//! ProductAtomicV1 storage owner. This module deliberately exposes typed
//! operations only; it never returns its SQLite connection or transaction.
#![allow(clippy::collapsible_if)] // W6a clippy debt: preserve explicit branch and short-circuit ordering
#![allow(dead_code)] // W6a clippy debt: retain conditional helpers used by other feature and test configurations
#![allow(clippy::large_enum_variant)] // W6a clippy debt: preserve the existing public enum representation and matching behavior
#![allow(clippy::manual_contains)] // W6a clippy debt: preserve the existing predicate expression and evaluation order
#![allow(clippy::too_many_arguments)] // W6a clippy debt: preserve the established operation binding and call shape
#![allow(clippy::type_complexity)] // W6a clippy debt: preserve the existing internal tuple and storage representation

use crate::product_scope::{
    ProductCapability, RepositoryIdentity, ScopeGrantManifest, ScopeGrantReceipt, ScopeGrantView,
    ScopeMigrationManifest, ScopeMigrationReceipt, ScopeMutation, ScopeMutationReceiptKind,
    ScopeSourceManifest, ScopeSourceView, ScopedEvidenceSnapshot, ScopedOrderView,
};
use chrono::{DateTime, Utc};
use foundry_approval::product::{
    ProductApprovalAnchor, ProductGrantBinding, consume_product_grant_fenced_in,
    consume_product_token_in, issue_product_grant_in, prepare_product_in,
    validate_approval_anchor_in,
};
use foundry_domain::OrderEvent;
use foundry_domain::ports::{ActorId, ActorKind};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::path::{Path, PathBuf};
use thiserror::Error;

// The capture readers are private to the isolated v3 claim path.
#[cfg(feature = "w1-isolated-test-support")]
#[derive(Debug, PartialEq, Eq)]
struct W1ServiceAuthority {
    service_lease_until: String,
    credential_expires_at: String,
    grant_revision: i64,
    grant_action: String,
}

#[cfg(feature = "w1-isolated-test-support")]
fn w1_canonical_time(value: &str) -> Result<DateTime<Utc>, ProductError> {
    let time = DateTime::parse_from_rfc3339(value)
        .map_err(|_| ProductError::ContractMismatch)?
        .with_timezone(&Utc);
    if time.to_rfc3339() != value { return Err(ProductError::ContractMismatch); }
    Ok(time)
}

#[cfg(feature = "w1-isolated-test-support")]
fn w1_service_authority_in(tx: &rusqlite::Transaction<'_>, context: &ProductServiceContext, now: DateTime<Utc>) -> Result<W1ServiceAuthority, ProductError> {
    let mut statement = tx.prepare("SELECT s.lease_until,c.expires_at,g.grant_revision,g.action FROM product_services_v1 s JOIN product_service_credentials_v1 c USING(service_id) JOIN product_service_grants_v1 g ON g.service_id=s.service_id JOIN product_scope_source_instances_v1 i ON i.source_instance_id=g.source_instance_id JOIN product_scope_current_sources_v1 cs ON cs.connector_id=i.connector_id AND cs.source_instance_id=i.source_instance_id JOIN product_scope_repositories_v1 r ON r.repository_key=i.repository_key JOIN product_scope_api_locators_v1 l USING(source_instance_id) JOIN product_sources_v1 ps ON ps.connector_id=i.connector_id WHERE s.service_id=?1 AND s.revision=?2 AND s.enabled=1 AND c.credential_revision=?3 AND g.source_instance_id=?4 AND g.action='drive_review' AND g.enabled=1 AND i.enabled=1 AND i.generation=?5 AND i.revision=?6 AND ps.revision=?7 AND ps.enabled=1 AND i.connector_id=?8 AND r.api_origin=?9 AND r.stable_repository_id=?10 AND ps.origin=?9 AND l.api_owner=?11 COLLATE NOCASE AND l.api_repo=?12 COLLATE NOCASE")?;
    let mut rows = statement.query(params![context.service_id,context.service_revision,context.credential_revision,context.source_instance_id,context.source_generation,context.source_instance_revision,context.connector_revision,context.connector_id,context.api_origin,context.repository_id,context.api_owner,context.api_repo])?;
    let denied = || ProductError::Invariant("service authority is absent, expired, or revoked");
    let row = rows.next()?.ok_or_else(denied)?;
    let authority = W1ServiceAuthority { service_lease_until: row.get(0).map_err(isolated_schema_error)?, credential_expires_at: row.get(1).map_err(isolated_schema_error)?, grant_revision: row.get(2).map_err(isolated_schema_error)?, grant_action: row.get(3).map_err(isolated_schema_error)? };
    if authority.grant_action != "drive_review" { return Err(ProductError::ContractMismatch); }
    if rows.next()?.is_some() { return Err(ProductError::ContractMismatch); }
    let service_expiry = w1_canonical_time(&authority.service_lease_until)?;
    let credential_expiry = w1_canonical_time(&authority.credential_expires_at)?;
    if service_expiry <= now || credential_expiry <= now { return Err(denied()); }
    Ok(authority)
}

#[cfg(feature = "w1-isolated-test-support")]
#[derive(Debug, PartialEq, Eq)]
struct W1ClaimCandidate {
    order_id: String, lifecycle: i64, delivery_id: String, task_kind: String,
    payload: String, source_revision: i64, admission_seq: i64, admission_hash: String,
    old_epoch: Option<i64>, attempts: i64, provenance_seq: i64, provenance_hash: String, available_at: DateTime<Utc>, old_lease: Option<DateTime<Utc>>,
    current_revision: i64, enabled: i64, state: String,
}

#[cfg(feature = "w1-isolated-test-support")]
fn w1_claim_candidate_in(tx: &rusqlite::Transaction<'_>, context: &ProductServiceContext, now: DateTime<Utc>) -> Result<Option<W1ClaimCandidate>, ProductError> {
    let mut statement = tx.prepare("SELECT o.order_id,o.lifecycle,b.delivery_id,o.task_kind,o.payload_snapshot,o.source_revision,o.admission_seq,o.admission_hash,s.revision,s.enabled,c.epoch,c.lease_until,b.available_at,b.state,c.order_id,b.attempts,p.admission_seq,p.admission_hash FROM product_work_backlog_v1 b JOIN product_orders_v1 o ON o.order_id=b.order_id AND o.lifecycle=b.lifecycle JOIN product_sources_v1 s ON s.connector_id=o.connector_id JOIN product_scope_order_provenance_v1 p ON p.order_id=o.order_id AND p.lifecycle=o.lifecycle AND p.verified=1 AND p.source_instance_id=?1 LEFT JOIN product_work_claims_v1 c ON c.order_id=b.order_id AND c.lifecycle=b.lifecycle WHERE o.connector_id=?2 AND o.repository_id=?3 AND b.state IN ('ready','retryable','claimed','completed')")?;
    let mut rows = statement.query(params![context.source_instance_id,context.connector_id,context.repository_id])?;
    let mut candidates = Vec::new();
    while let Some(row) = rows.next()? {
        let read = || -> Result<W1ClaimCandidate, rusqlite::Error> {
            let old_lease: Option<String> = row.get(11)?;
            let available: String = row.get(12)?;
            let claim_id: Option<String> = row.get(14)?;
            let epoch: Option<i64> = row.get(10)?;
            if claim_id.is_some() != epoch.is_some() { return Err(rusqlite::Error::InvalidColumnType(10, "epoch".into(), rusqlite::types::Type::Null)); }
            Ok(W1ClaimCandidate { order_id: row.get(0)?, lifecycle: row.get(1)?, delivery_id: row.get(2)?, task_kind: row.get(3)?, payload: row.get(4)?, source_revision: row.get(5)?, admission_seq: row.get(6)?, admission_hash: row.get(7)?, current_revision: row.get(8)?, enabled: row.get(9)?, old_epoch: epoch, attempts: row.get(15)?, provenance_seq: row.get(16)?, provenance_hash: row.get(17)?, old_lease: old_lease.as_deref().map(w1_canonical_time).transpose().map_err(|e| rusqlite::Error::FromSqlConversionFailure(11, rusqlite::types::Type::Text, Box::new(e)))?, available_at: w1_canonical_time(&available).map_err(|e| rusqlite::Error::FromSqlConversionFailure(12, rusqlite::types::Type::Text, Box::new(e)))?, state: row.get(13)? })
        };
        let candidate = read().map_err(isolated_schema_error)?;
        if candidate.attempts < 0 || candidate.old_epoch.is_some_and(|epoch| epoch <= 0) { return Err(ProductError::ContractMismatch); }
        if candidate.available_at <= now && (matches!(candidate.state.as_str(), "ready" | "retryable") || candidate.old_lease.is_none_or(|lease| lease <= now)) { candidates.push(candidate); }
    }
    candidates.sort_by(|a,b| a.available_at.cmp(&b.available_at).then_with(|| a.order_id.cmp(&b.order_id)).then_with(|| a.lifecycle.cmp(&b.lifecycle)));
    let Some(candidate) = candidates.into_iter().next() else { return Ok(None); };
    if candidate.enabled != 1 || candidate.current_revision != candidate.source_revision || candidate.source_revision != context.source_revision { return Err(ProductError::Invariant("work source was disabled or rebound")); }
    if candidate.provenance_seq != candidate.admission_seq || candidate.provenance_hash != candidate.admission_hash { return Err(ProductError::Invariant("order provenance binding changed")); }
    let anchor: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM product_transition_receipts_v1 WHERE seq=?1 AND event_hash=?2 AND order_id=?3 AND lifecycle=?4 AND operation='github-admission')", params![candidate.admission_seq,candidate.admission_hash,candidate.order_id,candidate.lifecycle], |r| r.get(0))?;
    if !anchor { return Err(ProductError::Invariant("work admission anchor is not authoritative")); }
    if candidate.old_lease.is_some_and(|lease| lease > now) { return Ok(None); }
    Ok(Some(candidate))
}

// The callback is installed only for an explicitly armed v3 claim. SQLite invokes
// it on the claiming thread, so it can report the first actual busy result.
#[cfg(feature = "w1-isolated-test-support")]
std::thread_local! {
    static W1_BUSY_WAIT: std::cell::RefCell<Option<(std::sync::mpsc::SyncSender<()>, std::time::Instant)>> = const { std::cell::RefCell::new(None) };
}

#[cfg(feature = "w1-isolated-test-support")]
fn w1_busy_handler(attempts: i32) -> bool {
    W1_BUSY_WAIT.try_with(|slot| {
        let Ok(mut slot) = slot.try_borrow_mut() else { return false; };
        let Some((sender, started)) = slot.as_mut() else { return false; };
        if attempts == 0 { let _ = sender.try_send(()); }
        let remaining = std::time::Duration::from_secs(5).saturating_sub(started.elapsed());
        if remaining.is_zero() { return false; }
        std::thread::sleep(remaining.min(std::time::Duration::from_millis(10)));
        true
    }).unwrap_or(false)
}

#[cfg(feature = "w1-isolated-test-support")]
struct W1BusyGuard<'a> { store: &'a mut ProductStore, restored: bool }

#[cfg(feature = "w1-isolated-test-support")]
impl W1BusyGuard<'_> {
    fn restore(&mut self) -> rusqlite::Result<()> {
        let _ = W1_BUSY_WAIT.try_with(|slot| { if let Ok(mut slot) = slot.try_borrow_mut() { *slot = None; } });
        let result = self.store.conn.busy_timeout(std::time::Duration::from_secs(5));
        self.restored = result.is_ok();
        result
    }
}

#[cfg(feature = "w1-isolated-test-support")]
impl Drop for W1BusyGuard<'_> {
    fn drop(&mut self) {
        if !self.restored { let _ = self.restore(); }
    }
}

// This private entry point owns the IMMEDIATE transaction and the single clock
// sample.
#[cfg(feature = "w1-isolated-test-support")]
fn w1_capture_claim_private(store: &mut ProductStore, context: &ProductServiceContext, worker: &str, lease: chrono::Duration) -> Result<ProductWorkClaimResult, ProductError> {
    if !store.open_mode.is_capture() { return Err(ProductError::ContractMismatch); }
    if worker.trim().is_empty() || worker.len() > 128 || lease <= chrono::Duration::zero() || lease > chrono::Duration::hours(1) { return Err(ProductError::InvalidConfig); }
    if let Some(sender) = store.w1_capture_busy_observer.take() {
        W1_BUSY_WAIT.with(|slot| *slot.borrow_mut() = Some((sender, std::time::Instant::now())));
        let mut guard = W1BusyGuard { store, restored: false };
        guard.store.conn.busy_handler(Some(w1_busy_handler))?;
        let result = w1_capture_claim_inner(guard.store, context, worker, lease);
        guard.restore()?;
        return result;
    }
    w1_capture_claim_inner(store, context, worker, lease)
}

#[cfg(feature = "w1-isolated-test-support")]
fn w1_capture_claim_inner(store: &mut ProductStore, context: &ProductServiceContext, worker: &str, lease: chrono::Duration) -> Result<ProductWorkClaimResult, ProductError> {
    let clock = store.clock.clone();
    let tx = store.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let now = clock.now();
    validate_isolated_pragmas(&tx)?;
    validate_isolated_marker_version(&tx, store.open_mode)?;
    validate_w1_operations(&tx)?;
    validate_w1_capture_schema(&tx)?;
    let authority = w1_service_authority_in(&tx, context, now)?;
    let Some(c) = w1_claim_candidate_in(&tx, context, now)? else { return Ok(ProductWorkClaimResult::Empty); };
    // Both supported transitions into claimed/completed retain a claim row.
    if matches!(c.state.as_str(), "claimed" | "completed") && c.old_epoch.is_none() { return Err(ProductError::ContractMismatch); }
    if c.lifecycle <= 0 { return Err(ProductError::ContractMismatch); }
    let epoch = c.old_epoch.unwrap_or(0).checked_add(1).ok_or(ProductError::InvalidConfig)?;
    let attempts = c.attempts.checked_add(1).ok_or(ProductError::InvalidConfig)?;
    let lease_until = now.checked_add_signed(lease).ok_or(ProductError::InvalidConfig)?;
    let old_fence: i64 = tx.query_row("SELECT next FROM product_fence_counter_v1 WHERE singleton=1", [], |r| r.get(0)).map_err(|error| match error {
        rusqlite::Error::QueryReturnedNoRows => ProductError::ContractMismatch,
        other => isolated_schema_error(other),
    })?;
    if old_fence < 0 { return Err(ProductError::ContractMismatch); }
    let fence = old_fence.checked_add(1).ok_or(ProductError::InvalidConfig)?;
    let digest = payload_digest(&c.task_kind, &c.payload);
    let changed = tx.execute("UPDATE product_fence_counter_v1 SET next=?1 WHERE singleton=1 AND next=?2", params![fence, old_fence])?;
    if changed != 1 { return Err(ProductError::ContractMismatch); }
    let changed = tx.execute("INSERT INTO product_work_claims_v1(order_id,lifecycle,owner_id,epoch,fence,state,lease_until,source_revision,payload_digest) VALUES(?1,?2,?3,?4,?5,'claimed',?6,?7,?8) ON CONFLICT(order_id,lifecycle) DO UPDATE SET owner_id=excluded.owner_id,epoch=excluded.epoch,fence=excluded.fence,state='claimed',lease_until=excluded.lease_until,source_revision=excluded.source_revision,payload_digest=excluded.payload_digest WHERE product_work_claims_v1.epoch=?9", params![c.order_id,c.lifecycle,worker,epoch,fence,lease_until.to_rfc3339(),c.source_revision,digest,c.old_epoch])?;
    if changed != 1 { return Err(ProductError::ContractMismatch); }
    let changed = tx.execute("UPDATE product_work_backlog_v1 SET state='claimed',attempts=?1 WHERE order_id=?2 AND lifecycle=?3 AND delivery_id=?4 AND state=?5 AND attempts=?6", params![attempts,c.order_id,c.lifecycle,c.delivery_id,c.state,c.attempts])?;
    if changed != 1 { return Err(ProductError::ContractMismatch); }
    let changed = tx.execute("INSERT INTO product_service_work_bindings_v1(order_id,lifecycle,service_id,service_revision,credential_revision,source_instance_id) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(order_id,lifecycle) DO UPDATE SET service_id=excluded.service_id,service_revision=excluded.service_revision,credential_revision=excluded.credential_revision,source_instance_id=excluded.source_instance_id", params![c.order_id,c.lifecycle,context.service_id,context.service_revision,context.credential_revision,context.source_instance_id])?;
    if changed != 1 { return Err(ProductError::ContractMismatch); }
    if std::mem::take(&mut store.w1_capture_fail_after_legacy_writes) { return Err(ProductError::Invariant("w1 capture injected failure")); }
    let changed = tx.execute("INSERT INTO product_w1_claim_authority_v1(order_id,lifecycle,epoch,fence,installation_id,database_id,delivery_id,worker_id,claimed_at,claim_lease_until,payload_digest,admission_seq,admission_hash,service_id,service_revision,service_lease_until,credential_revision,credential_expires_at,grant_action,grant_revision,source_instance_id,source_generation,source_instance_revision,connector_id,connector_revision,api_origin,repository_id,api_owner,api_repo) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29)", params![c.order_id,c.lifecycle,epoch,fence,store.installation_id,store.database_id,c.delivery_id,worker,now.to_rfc3339(),lease_until.to_rfc3339(),digest,c.admission_seq,c.admission_hash,context.service_id,context.service_revision,authority.service_lease_until,context.credential_revision,authority.credential_expires_at,authority.grant_action,authority.grant_revision,context.source_instance_id,context.source_generation,context.source_instance_revision,context.connector_id,context.connector_revision,context.api_origin,context.repository_id,context.api_owner,context.api_repo])?;
    if changed != 1 { return Err(ProductError::ContractMismatch); }
    let claim = ProductWorkClaim { order_id:c.order_id, lifecycle:c.lifecycle, delivery_id:c.delivery_id, worker_id:worker.to_owned(), epoch, fence, source_revision:c.source_revision, payload_digest:digest, service_binding:Some(foundry_exec::ReviewServiceBinding { service_id:context.service_id.clone(), service_revision:context.service_revision, credential_revision:context.credential_revision, source_instance_id:context.source_instance_id.clone() }), lease_until, task_kind:c.task_kind, payload_snapshot:c.payload };
    tx.commit()?;
    Ok(ProductWorkClaimResult::Claimed(claim))
}

#[path = "product/result.rs"]
mod result;
#[path = "product/review.rs"]
mod review;
#[cfg(any(test, feature = "p2-test-support"))]
pub use review::{ChargeBudgetObservation, ChargeBudgetObserver};
#[path = "product_service.rs"]
mod service;
#[cfg(feature = "p2-test-support")]
pub use result::P2ReviewPreparation;
pub use result::{
    ProductResultArtifactContent, ProductResultError, ProductResultExpectation, ProductResultInfo,
    product_host_result_digest,
};
pub use service::{
    ProductServiceAction, ProductServiceContext, ProductServiceCredential, ProductServiceGrant,
    ProductServiceReceipt, ProductServiceView,
};

#[derive(Debug, Error)]
pub enum ProductError {
    #[error("product store contract mismatch")]
    ContractMismatch,
    #[error("invalid product store configuration")]
    InvalidConfig,
    #[error("product store invariant failed: {0}")]
    Invariant(&'static str),
    #[error("product store database: {0}")]
    Database(#[from] rusqlite::Error),
}

#[cfg(feature = "w1-isolated-test-support")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum W1Operation { Approve, Revoke, Select }

#[cfg(feature = "w1-isolated-test-support")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct W1OperationReceipt {
    pub canonical: Vec<u8>,
    pub operation: W1Operation,
    pub installation_id: String,
    pub database_id: String,
    pub actor_id: String,
    pub authority_revision: i64,
    pub source_instance_id: String,
    pub digest: String,
    pub expected_revision: i64,
    pub result_revision: i64,
    pub request_key: String,
    pub order_id: Option<String>,
    pub lifecycle: Option<i64>,
    pub approval_revision: Option<i64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StoreOpenMode {
    Normal,
    #[cfg(feature = "a01-harness-support")]
    A01Create,
    #[cfg(feature = "a01-harness-support")]
    A01Reopen,
    #[cfg(feature = "a01-harness-support")]
    A01TaskCreate,
    #[cfg(feature = "a01-harness-support")]
    A01TaskReopen,
    #[cfg(feature = "w1-isolated-test-support")]
    IsolatedCreate,
    #[cfg(feature = "w1-isolated-test-support")]
    IsolatedReopen,
    #[cfg(feature = "w1-isolated-test-support")]
    CaptureCreate,
    #[cfg(feature = "w1-isolated-test-support")]
    CaptureReopen,
    #[cfg(feature = "w1-isolated-test-support")]
    PinCreate,
    #[cfg(feature = "w1-isolated-test-support")]
    PinReopen,
}

#[cfg(feature = "a01-harness-support")]
impl StoreOpenMode {
    fn is_a01_task(self) -> bool { matches!(self, Self::A01TaskCreate | Self::A01TaskReopen) }
    fn is_a01(self) -> bool {
        matches!(self, Self::A01Create | Self::A01Reopen | Self::A01TaskCreate | Self::A01TaskReopen)
    }
}

#[cfg(feature = "w1-isolated-test-support")]
impl StoreOpenMode {
    fn is_reopen(self) -> bool { matches!(self, Self::IsolatedReopen | Self::CaptureReopen | Self::PinReopen) }
    fn is_create(self) -> bool { matches!(self, Self::IsolatedCreate | Self::CaptureCreate | Self::PinCreate) }
    fn is_capture(self) -> bool { matches!(self, Self::CaptureCreate | Self::CaptureReopen) }
    fn is_pin(self) -> bool { matches!(self, Self::PinCreate | Self::PinReopen) }
}

/// Human decision accepted for one exact, already persisted product hold.
/// This records acceptance only; continuing the runner is a separate phase.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", content = "target", rename_all = "snake_case")]
pub enum ProductHoldDecision {
    Resume(String),
    CommitPass,
    CommitFail,
    Abort,
}

/// Immutable acknowledgement of a product hold decision. The acceptance
/// receipt is historical; callers still need current authority for status or
/// continuation operations.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProductHoldAcceptance {
    pub operation_id: String,
    pub actor_id: String,
    pub order_id: String,
    pub lifecycle: i64,
    pub hold_instance_id: String,
    pub hold_revision: i64,
    pub attempt: i64,
    pub originating_run: i64,
    pub activation: i64,
    pub host_epoch: i64,
    pub decision: ProductHoldDecision,
    pub source_revision: i64,
    pub authority_revision: i64,
    /// Additive W2 authority binding. Historical receipts deserialize with
    /// absent values and are not eligible for new continuation work.
    #[serde(default)]
    pub source_instance_id: String,
    #[serde(default)]
    pub scope_revision: i64,
    #[serde(default)]
    pub grant_revision: i64,
    pub predecessor_seq: i64,
    pub reserved_run_id: i64,
    pub accepted_at: String,
}

/// Client-observed immutable hold coordinates. The store compares every
/// supplied value inside the acceptance transaction before recording a human
/// decision, so a stale detail page cannot approve a replacement hold.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProductHoldExpectation {
    pub lifecycle: i64,
    pub attempt: i64,
    pub originating_run: i64,
    pub activation: i64,
    pub host_epoch: i64,
    pub hold_revision: i64,
    pub source_revision: i64,
    pub predecessor_seq: i64,
    pub snapshot_digest: String,
}

/// Sanitized, immutable coordinates needed to present and accept an active
/// ProductAtomicV1 review hold. The pack snapshot itself stays in the
/// restricted review tables.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProductReviewHoldView {
    pub order_id: String,
    pub hold_instance_id: String,
    pub expectation: ProductHoldExpectation,
    pub allowed_targets: Vec<String>,
    pub deadline: String,
}

/// Read-only state for an accepted decision. A phase of `recovery_required`
/// means no automatic continuation is permitted.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProductReviewProgress {
    pub receipt: ProductHoldAcceptance,
    pub phase: String,
    pub progress_revision: i64,
    pub recovery_required: bool,
}

#[derive(Debug, Clone)]
pub struct ProductConfig {
    pub installation_id: String,
    pub database_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionReceipt {
    pub order_id: String,
    pub lifecycle: i64,
    pub sequence: i64,
    pub event_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductPrincipal {
    pub actor_id: String,
    pub role: String,
    pub authority_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProductApprovalReceipt {
    pub operation_id: String,
    pub actor_id: String,
    pub order_id: String,
    pub lifecycle: i64,
    pub sequence: i64,
    pub event_hash: String,
    pub artifact_digest: String,
    /// Full immutable tuple binding approved in the same transaction.
    pub attempt: i64,
    pub run_id: i64,
    pub generation: i64,
    pub tuple_digest: String,
    pub source_revision: i64,
}

/// Public artifact identity/provenance exposed only after a current scoped
/// ReadArtifact check. Filesystem locations remain private to ProductStore.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProductArtifactInfo {
    pub artifact_id: String,
    pub order_id: String,
    pub lifecycle: i64,
    pub attempt: i64,
    pub run_id: String,
    pub generation: i64,
    pub stage_sequence: i64,
    pub stage_id: String,
    pub sha256: String,
}

/// Fully verified artifact bytes. Constructed only by ProductStore after
/// checking current scope and the immutable registered digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductArtifactContent {
    pub info: ProductArtifactInfo,
    pub bytes: Vec<u8>,
}

/// Unforgeable authorization to begin the separate external publish effect.
/// Constructed only after a fresh ProductStore transaction verifies that the
/// exact approval receipt still names the current Publishing lifecycle and
/// the currently promoted artifact tuple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductPublishAuthorization {
    receipt: ProductApprovalReceipt,
}

impl ProductPublishAuthorization {
    pub fn receipt(&self) -> &ProductApprovalReceipt {
        &self.receipt
    }
}

/// Result of the durable admission check for a separate publish effect.
/// Existing pending work must be reconciled before another publish attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProductPublishStart {
    Started,
    Reconcile,
    Confirmed { pr_url: String },
}

/// Work that is durably waiting for an adapter, or a fenced claim that a
/// configured adapter may execute. Claim proofs cannot be constructed by
/// callers and are bound to the immutable admitted snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProductWorkClaimResult {
    Empty,
    Unsupported { order_id: String, task_kind: String },
    Claimed(ProductWorkClaim),
}

type ScopedIntakeBinding = (
    String,
    String,
    i64,
    Option<String>,
    Option<String>,
    i64,
    i64,
    i64,
);

#[derive(Clone, PartialEq, Eq)]
pub struct ProductWorkClaim {
    order_id: String,
    lifecycle: i64,
    delivery_id: String,
    worker_id: String,
    epoch: i64,
    fence: i64,
    source_revision: i64,
    payload_digest: String,
    service_binding: Option<foundry_exec::ReviewServiceBinding>,
    lease_until: DateTime<Utc>,
    task_kind: String,
    payload_snapshot: String,
}

impl std::fmt::Debug for ProductWorkClaim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProductWorkClaim")
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

impl ProductWorkClaim {
    pub fn order_id(&self) -> &str {
        &self.order_id
    }
    pub fn lifecycle(&self) -> i64 {
        self.lifecycle
    }
    pub fn delivery_id(&self) -> &str {
        &self.delivery_id
    }
    pub fn task_kind(&self) -> &str {
        &self.task_kind
    }
    pub fn payload_snapshot(&self) -> &str {
        &self.payload_snapshot
    }
    pub fn lease_until(&self) -> DateTime<Utc> {
        self.lease_until
    }

    /// Convert the opaque ledger claim to the host's typed binding request.
    /// ProductStore still revalidates every field under its admission/write
    /// transaction; this conversion does not itself authorize execution.
    pub fn review_binding(&self) -> foundry_exec::ReviewWorkClaim {
        foundry_exec::ReviewWorkClaim {
            order_id: self.order_id.clone(),
            lifecycle: self.lifecycle,
            delivery_id: self.delivery_id.clone(),
            worker_id: self.worker_id.clone(),
            epoch: self.epoch,
            fence: self.fence,
            source_revision: self.source_revision,
            payload_digest: self.payload_digest.clone(),
            lease_until: self.lease_until,
            task_kind: self.task_kind.clone(),
            payload_snapshot: self.payload_snapshot.clone(),
            service_binding: self.service_binding.clone(),
        }
    }
}

/// Verified S1 projection input captured from one ProductAtomicV1 read
/// transaction. Rows are sequence ordered and replayed before returning.
pub struct ProductProjectionSnapshot {
    pub rows: Vec<crate::LedgerRow>,
    pub lifecycles: Vec<crate::replay::ReplayedOrder>,
    pub head_sequence: i64,
    pub head_hash: String,
}

/// Values describing the immutable promoted execution that the approver saw.
/// The store independently verifies each value against its own SQLite rows.
#[derive(Debug, Clone)]
pub struct ProductApprovalIntent {
    pub request_key: String,
    pub grant_id: String,
    pub token_digest: String,
    pub order_id: String,
    pub expected_source_revision: i64,
    pub expected_attempt: i64,
    pub run_id: i64,
    pub generation: i64,
    pub artifact_digest: String,
    pub tuple_digest: String,
    pub intent: String,
    pub now: DateTime<Utc>,
}

/// Unforgeable handle for one prepared reservation claim. Its fields and
/// constructor stay private so a caller cannot substitute a newer fence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductReservationProof {
    operation_id: String,
    owner_id: String,
    epoch: i64,
    fence: i64,
}

impl ProductReservationProof {
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }
}

/// Opaque session bearer. The secret is excluded from Debug and serde.
pub struct ProductSession {
    bearer: String,
    digest: String,
}

impl ProductSession {
    pub fn bearer(&self) -> &str {
        &self.bearer
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
}

pub struct ProductStore {
    conn: Connection,
    open_mode: StoreOpenMode,
    #[cfg(feature = "a01-harness-support")]
    a01_identity: Option<A01FileIdentity>,
    path: PathBuf,
    installation_id: String,
    database_id: String,
    clock: std::sync::Arc<dyn ProductClock>,
    #[cfg(feature = "w1-isolated-test-support")]
    w1_capture_fail_after_legacy_writes: bool,
    #[cfg(feature = "w1-isolated-test-support")]
    w1_capture_busy_observer: Option<std::sync::mpsc::SyncSender<()>>,
    review_config: Option<ProductReviewConfig>,
    artifact_root: Option<PathBuf>,
    artifact_root_handle: Option<std::sync::Arc<std::fs::File>>,
    active_review_claim: Option<ProductReviewClaim>,
    active_review_claim_lock: Option<std::fs::File>,
    #[cfg(any(test, feature = "p2-test-support"))]
    charge_budget_observer: Option<review::ChargeBudgetObserver>,
}

/// Injectable wall clock used at authorization and lease transaction
/// boundaries. Production stores use `SystemProductClock`; deterministic
/// tests can provide a bounded clock without sleeping.
pub trait ProductClock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProductClock;

impl ProductClock for SystemProductClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[derive(Clone)]
struct ProductReviewClaim {
    operation_id: String,
    owner_id: String,
    epoch: i64,
    fence: i64,
    reserved_run_id: i64,
}

#[derive(Debug, Clone)]
pub struct ProductReviewConfig {
    pub artifact_root: PathBuf,
    pub session_root: PathBuf,
    pub claim_root: PathBuf,
    pub pack_digest: String,
}

impl ProductStore {
    /// Exclusively create a disposable A01 store in a caller-controlled private
    /// directory. This mode denies all work claims and review admission.
    /// The caller must exclude concurrent same-UID pathname/schema writers.
    #[cfg(feature = "a01-harness-support")]
    pub fn create_a01_isolated_test_store(
        directory: &Path,
        name: &str,
        config: ProductConfig,
        clock: std::sync::Arc<dyn ProductClock>,
    ) -> Result<ProductStore, ProductError> {
        Self::create_a01_isolated_test_store_with_checkpoint(
            directory, name, config, clock,
            #[cfg(all(test, feature = "a01-harness-support", unix))]
            None,
        )
    }

    #[cfg(feature = "a01-harness-support")]
    
fn create_a01_isolated_test_store_with_checkpoint(
        directory: &Path,
        name: &str,
        config: ProductConfig,
        clock: std::sync::Arc<dyn ProductClock>,
            #[cfg(all(test, feature = "a01-harness-support", unix))] checkpoint: Option<usize>,
    ) -> Result<ProductStore, ProductError> {
        validate_a01_config(&config)?;
        let path = a01_test_path(directory, name)?;
        a01_require_absent(&path)?;
        a01_check_sidecars(&path)?;
        #[cfg(not(unix))]
        { let _ = clock; Err(ProductError::InvalidConfig) }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let held = std::fs::OpenOptions::new().read(true).write(true)
                .create_new(true).mode(0o600).open(&path)
                .map_err(|_| ProductError::Invariant("A01 exclusive file creation failed"))?;
            let identity = a01_checked_file(&path, &held)?;
            // The fresh schema and discriminator share open_internal's one
            // IMMEDIATE initialization transaction. No ordinary v3 is committed.
            let mut store = Self::open_internal_with_checkpoint(
            &path, config, clock, StoreOpenMode::A01Create,
            #[cfg(all(test, feature = "a01-harness-support", unix))]
            checkpoint,
        )?;
            if a01_checked_file(&path, &held)? != identity {
                return Err(ProductError::Invariant("A01 physical identity changed"));
            }
            a01_check_sidecars(&path)?;
            store.a01_identity = Some(identity);
            Ok(store)
        }
    }

    /// Validate and reopen an A01 store without creating or repairing schema.
    /// Assembly must additionally compare its privately retained physical
    /// identity; logical IDs alone cannot distinguish a copied database.
    #[cfg(feature = "a01-harness-support")]
    pub fn reopen_a01_isolated_test_store(
        directory: &Path,
        name: &str,
        config: ProductConfig,
        clock: std::sync::Arc<dyn ProductClock>,
    ) -> Result<ProductStore, ProductError> {
        validate_a01_config(&config)?;
        let path = a01_test_path(directory, name)?;
        a01_check_sidecars(&path)?;
        a01_file_identity(&path)?;
        let held = std::fs::File::open(&path)
            .map_err(|_| ProductError::Invariant("A01 file inspection failed"))?;
        let identity = a01_checked_file(&path, &held)?;
        // Reject WAL format before SQLite inspection can create WAL/SHM files.
        #[cfg(unix)]
        {
            let header = a01_read_header(&held)?;
            if &header[..16] != b"SQLite format 3\0" || header[18..20] != [1, 1] {
                return Err(ProductError::ContractMismatch);
            }
        }
        {
            let inspection = Connection::open_with_flags(
                &path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            inspection.busy_timeout(std::time::Duration::from_secs(5))?;
            let tx = inspection.unchecked_transaction()?;
            let journal: String = tx.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
            if journal != "delete" { return Err(ProductError::ContractMismatch); }
            validate_a01_contract(&tx, &config)?;
            tx.commit()?;
        }
        if a01_checked_file(&path, &held)? != identity {
            return Err(ProductError::Invariant("A01 physical identity changed"));
        }
        a01_check_sidecars(&path)?;
        let conn = Connection::open_with_flags(
            &path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        )?;
        configure_a01_connection(&conn)?;
        {
            let tx = conn.unchecked_transaction()?;
            validate_a01_connection(&tx, &config)?;
            tx.commit()?;
        }
        if a01_checked_file(&path, &held)? != identity {
            return Err(ProductError::Invariant("A01 physical identity changed"));
        }
        a01_check_sidecars(&path)?;
        Ok(Self {
            conn,
            open_mode: StoreOpenMode::A01Reopen,
            a01_identity: Some(identity),
            path,
            installation_id: config.installation_id,
            database_id: config.database_id,
            clock,
            #[cfg(feature = "w1-isolated-test-support")]
            w1_capture_fail_after_legacy_writes: false,
            #[cfg(feature = "w1-isolated-test-support")]
            w1_capture_busy_observer: None,
            review_config: None,
            artifact_root: None,
            artifact_root_handle: None,
            active_review_claim: None,
            active_review_claim_lock: None,
            #[cfg(any(test, feature = "p2-test-support"))]
            charge_budget_observer: None,
        })
    }

    /// Arm a single pre-commit capture failure on an isolated v3 store.
    #[cfg(feature = "w1-isolated-test-support")]
    pub fn w1_capture_fail_after_legacy_writes_once(&mut self) -> Result<(), ProductError> {
        if !self.open_mode.is_capture() { return Err(ProductError::ContractMismatch); }
        self.w1_capture_fail_after_legacy_writes = true;
        Ok(())
    }
    /// Observe the first SQLite busy callback on the next v3 claim.
    #[cfg(feature = "w1-isolated-test-support")]
    pub fn w1_capture_observe_busy_once(&mut self, sender: std::sync::mpsc::SyncSender<()>) -> Result<(), ProductError> {
        if !self.open_mode.is_capture() { return Err(ProductError::ContractMismatch); }
        self.w1_capture_busy_observer = Some(sender);
        Ok(())
    }
    /// Read the actual v3 connection timeout after a test claim.
    #[cfg(feature = "w1-isolated-test-support")]
    pub fn w1_capture_busy_timeout_ms(&self) -> Result<i64, ProductError> {
        if !self.open_mode.is_capture() { return Err(ProductError::ContractMismatch); }
        Ok(self.conn.query_row("PRAGMA busy_timeout", [], |row| row.get(0))?)
    }
    /// Explicit maintenance-only upgrade from ProductAtomicV1 schema v1 to
    /// scoped v2. Normal `open` refuses v1, so callers must stop old
    /// processes and present a reviewed finite manifest before the version is
    /// advanced. The migration is additive and preserves S1/evidence bytes.
    pub fn migrate_scope_v1(
        path: &Path,
        config: &ProductConfig,
        manifest: &ScopeMigrationManifest,
        now: DateTime<Utc>,
    ) -> Result<ScopeMigrationReceipt, ProductError> {
        if manifest.manifest_id.trim().is_empty()
            || manifest.reviewed_by.trim().is_empty()
            || manifest.sources.is_empty()
        {
            return Err(ProductError::InvalidConfig);
        }
        reject_isolated_before_path_creation(path)?;
        let path = normalized_path(path)?;
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let contract: Option<String> = tx
            .query_row(
                "SELECT contract FROM foundry_contract WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let meta: Option<(String, String, i64)> = tx
            .query_row(
                "SELECT installation_id,database_id,schema_version FROM product_meta_v1 WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if contract.as_deref() != Some("ProductAtomicV1")
            || !matches!(meta, Some((ref installation, ref database, 1))
                if installation == &config.installation_id && database == &config.database_id)
        {
            return Err(ProductError::ContractMismatch);
        }
        crate::product_scope::create_schema(&tx)?;
        let operation_columns = {
            let mut stmt = tx.prepare("PRAGMA table_info(product_operations_v1)")?;
            stmt.query_map([], |r| r.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()?
        };
        if !operation_columns
            .iter()
            .any(|column| column == "scope_revision")
        {
            tx.execute(
                "ALTER TABLE product_operations_v1 ADD COLUMN scope_revision INTEGER NOT NULL DEFAULT -1",
                [],
            )?;
        }
        let manifest_json =
            serde_json::to_string(manifest).map_err(|_| ProductError::InvalidConfig)?;
        let manifest_digest = crate::product_scope::framed_hash(
            "foundry-product-scope-migration-v1",
            &[&manifest_json],
        );
        if tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM product_scope_migrations_v1 WHERE manifest_id=?1)",
            [&manifest.manifest_id],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(ProductError::Invariant("scope migration already recorded"));
        }

        let mut source_by_id = std::collections::BTreeMap::new();
        let mut current_repo = std::collections::BTreeSet::new();
        for source in &manifest.sources {
            validate_scope_source(source)?;
            let identity = canonical_scope_identity(&source.identity)?;
            if source_by_id
                .insert(
                    source.source_instance_id.as_str(),
                    (source, identity.clone()),
                )
                .is_some()
            {
                return Err(ProductError::InvalidConfig);
            }
            if source.enabled && !current_repo.insert(identity.key()) {
                return Err(ProductError::Invariant(
                    "only one active intake binding per canonical repository is supported",
                ));
            }
            let legacy: Option<(String, String, String, String, i64, i64)> = tx
                .query_row(
                    "SELECT provider,origin,repository_id,scope,revision,enabled FROM product_sources_v1 WHERE connector_id=?1",
                    [&source.connector_id],
                    |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
                )
                .optional()?;
            let Some((provider, origin, repository_id, scope, _revision, old_enabled)) = legacy
            else {
                return Err(ProductError::Invariant(
                    "manifest source has no matching legacy connector",
                ));
            };
            let old_identity = RepositoryIdentity::canonicalized(
                &identity.organization,
                provider,
                origin,
                &identity.stable_repository_id,
            )
            .map_err(|_| ProductError::Invariant("legacy source identity is invalid"))?;
            if old_identity.organization != identity.organization
                || old_identity.provider != identity.provider
                || old_identity.api_origin != identity.api_origin
                // The v1 `repository_id` value was historically overloaded as
                // an owner/name locator. The reviewed manifest supplies the
                // explicit mapping to the canonical stable provider ID.
                || repository_id != source.legacy_repository_locator
                || scope != source.scope_query
                || i64::from(source.enabled) != old_enabled
            {
                return Err(ProductError::Invariant(
                    "manifest source does not match configured legacy source",
                ));
            }
            insert_scope_source(&tx, source, &identity)?;
            tx.execute(
                "INSERT INTO product_scope_current_sources_v1(connector_id,source_instance_id) VALUES(?1,?2)",
                params![source.connector_id,source.source_instance_id],
            )?;
        }

        let mut verified = std::collections::BTreeSet::new();
        for order in &manifest.verified_orders {
            if order.order_id.trim().is_empty()
                || order.lifecycle <= 0
                || order.admission_sequence <= 0
                || !is_sha256_hex(&order.admission_hash)
                || !verified.insert((order.order_id.as_str(), order.lifecycle))
            {
                return Err(ProductError::InvalidConfig);
            }
            let Some((source, identity)) = source_by_id.get(order.source_instance_id.as_str())
            else {
                return Err(ProductError::Invariant(
                    "verified order names an unknown source instance",
                ));
            };
            let old: Option<(i64, String, String, String, i64, String)> = tx
                .query_row(
                    "SELECT lifecycle,connector_id,repository_id,issue_id,admission_seq,admission_hash FROM product_orders_v1 WHERE order_id=?1",
                    [&order.order_id],
                    |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
                )
                .optional()?;
            let Some((lifecycle, connector, repository, _issue, seq, hash)) = old else {
                return Err(ProductError::Invariant(
                    "manifest names an unknown historical order",
                ));
            };
            if lifecycle != order.lifecycle
                || connector != source.connector_id
                || repository != source.legacy_repository_locator
                || seq != order.admission_sequence
                || hash != order.admission_hash
            {
                return Err(ProductError::Invariant(
                    "verified order provenance does not match stored admission",
                ));
            }
            let (last_seq, replay_lifecycle, _) = verified_order_state_in(&tx, &order.order_id)?;
            if i64::from(replay_lifecycle) + 1 != order.lifecycle
                || last_seq < order.admission_sequence
            {
                return Err(ProductError::Invariant(
                    "historical S1 lifecycle does not match manifest",
                ));
            }
            let admission_is_receipted: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM product_transition_receipts_v1 WHERE seq=?1 AND event_hash=?2 AND order_id=?3 AND lifecycle=?4 AND operation='github-admission')",
                params![order.admission_sequence,order.admission_hash,order.order_id,order.lifecycle],
                |r| r.get(0),
            )?;
            if !admission_is_receipted {
                return Err(ProductError::Invariant(
                    "historical admission has no matching transition receipt",
                ));
            }
            let _ = identity;
            tx.execute(
                "INSERT INTO product_scope_order_provenance_v1(order_id,lifecycle,source_instance_id,admission_seq,admission_hash,verified) VALUES(?1,?2,?3,?4,?5,1)",
                params![order.order_id,order.lifecycle,order.source_instance_id,order.admission_sequence,order.admission_hash],
            )?;
        }

        let mut grant_count = 0usize;
        for grant in &manifest.grants {
            if grant.capabilities.is_empty()
                || !source_by_id.contains_key(grant.source_instance_id.as_str())
            {
                return Err(ProductError::InvalidConfig);
            }
            let role: Option<String> = tx
                .query_row(
                    "SELECT role FROM product_accounts_v1 WHERE actor_id=?1 AND enabled=1",
                    [&grant.actor_id],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(role) = role else {
                return Err(ProductError::Invariant(
                    "migration grant names an unavailable account",
                ));
            };
            for capability in &grant.capabilities {
                if !capability.allowed_for_role(&role) {
                    return Err(ProductError::Invariant(
                        "migration grant exceeds the account role ceiling",
                    ));
                }
                insert_initial_scope_grant(
                    &tx,
                    &grant.actor_id,
                    &grant.source_instance_id,
                    *capability,
                    &manifest.reviewed_by,
                    &manifest.manifest_id,
                )?;
                grant_count += 1;
            }
        }

        let mut orders = tx.prepare(
            "SELECT order_id,lifecycle,connector_id,repository_id,admission_seq,admission_hash FROM product_orders_v1 ORDER BY order_id,lifecycle",
        )?;
        let legacy_orders = orders
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(orders);
        let mut quarantined = 0usize;
        for (order_id, lifecycle, connector, repository, seq, hash) in legacy_orders {
            if verified.contains(&(order_id.as_str(), lifecycle)) {
                continue;
            }
            let evidence_digest = crate::product_scope::framed_hash(
                "foundry-product-scope-quarantine-v1",
                &[
                    &order_id,
                    &lifecycle.to_string(),
                    &connector,
                    &repository,
                    &seq.to_string(),
                    &hash,
                ],
            );
            tx.execute(
                "INSERT INTO product_scope_quarantine_v1(order_id,lifecycle,evidence_digest,reason,recorded_at) VALUES(?1,?2,?3,'provenance_not_explicitly_verified',?4)",
                params![order_id,lifecycle,evidence_digest,now.to_rfc3339()],
            )?;
            quarantined += 1;
        }

        // Existing bearer sessions and unconsumed legacy grants cannot imply
        // scope authority. Preserve rows and consumption history while
        // invalidating only live credentials/reservations.
        tx.execute(
            "UPDATE product_sessions_v1 SET revoked=1 WHERE revoked=0",
            [],
        )?;
        tx.execute(
            "UPDATE product_grants_v1 SET state='invalidated' WHERE state IN ('issued','reserved')",
            [],
        )?;
        tx.execute(
            "UPDATE product_operations_v1 SET phase='fenced' WHERE phase='prepared'",
            [],
        )?;
        tx.execute("UPDATE product_sources_v1 SET enabled=0", [])?;
        for source in &manifest.sources {
            tx.execute(
                "UPDATE product_sources_v1 SET enabled=?1 WHERE connector_id=?2",
                params![i64::from(source.enabled), source.connector_id],
            )?;
        }
        tx.execute(
            "UPDATE product_scope_policy_state_v1 SET mode='enforced' WHERE singleton=1",
            [],
        )?;
        tx.execute(
            "UPDATE product_meta_v1 SET schema_version=2 WHERE singleton=1 AND schema_version=1",
            [],
        )?;
        tx.execute(
            "INSERT INTO product_scope_migrations_v1(manifest_id,manifest_digest,reviewed_by,migrated_at,verified_orders,quarantined_orders) VALUES(?1,?2,?3,?4,?5,?6)",
            params![manifest.manifest_id,manifest_digest,manifest.reviewed_by,now.to_rfc3339(),verified.len() as i64,quarantined as i64],
        )?;
        tx.commit()?;
        Ok(ScopeMigrationReceipt {
            manifest_digest,
            source_instances: manifest.sources.len(),
            verified_orders: verified.len(),
            quarantined_orders: quarantined,
            grants: grant_count,
        })
    }

    #[cfg(feature = "w1-isolated-test-support")]
    pub fn w1_change_pack_approval(
        &mut self, session_digest: &str, source_instance_id: &str, digest: &str,
        expected_approval_revision: i64, approve: bool, request_key: &str, now: DateTime<Utc>,
    ) -> Result<W1OperationReceipt, ProductError> {
        use w1_owner_codec::{Request, Receipt, encode_request, encode_receipt};
        if !w1_text(source_instance_id) || !w1_text(request_key)
            || digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || expected_approval_revision < 0 || expected_approval_revision.checked_add(1).is_none() {
            return Err(ProductError::InvalidConfig);
        }
        self.conn.pragma_update(None, "foreign_keys", "ON").map_err(isolated_schema_error)?;
        self.conn.pragma_update(None, "recursive_triggers", "ON").map_err(isolated_schema_error)?;
        validate_isolated_pragmas(&self.conn)?;
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if self.open_mode == StoreOpenMode::Normal { return Err(ProductError::ContractMismatch); }
        validate_isolated_pragmas(&tx)?;
        validate_isolated_marker_version(&tx, self.open_mode)?;
        validate_w1_operations(&tx)?;
        let principal = resolve_session_in(&tx, session_digest, now).map_err(|e| match e {
            ProductError::Invariant("session invalid" | "session expiry invalid" | "session expired, revoked, or stale") => w1_authorization_error(), other => other,
        })?;
        if principal.role != "owner" { return Err(w1_authorization_error()); }
        w1_authorize_source(&tx, &principal.actor_id, source_instance_id)?;
        let operation = if approve { "approve" } else { "revoke" };
        let request = Request {
            installation: self.installation_id.clone(), database: self.database_id.clone(),
            operation: operation.into(), actor: principal.actor_id.clone(), source: source_instance_id.into(),
            digest: digest.into(), expected: expected_approval_revision, key: request_key.into(),
            order: None, lifecycle: None,
        };
        let request_bytes = encode_request(&request).map_err(|_| ProductError::InvalidConfig)?;
        if let Some(receipt) = w1_replay(&tx, &principal, &request, &request_bytes)? {
            tx.commit()?;
            return Ok(receipt);
        }
        let (latest, active, _) = w1_validate_histories(&tx, &self.installation_id, &self.database_id, source_instance_id, digest, None)?;
        if latest != expected_approval_revision { return Err(ProductError::Invariant("w1 stale revision")); }
        if active == approve { return Err(ProductError::Invariant("w1 approval state invalid")); }
        let receipt = Receipt {
            installation: request.installation.clone(), database: request.database.clone(), operation: operation.into(),
            actor: request.actor.clone(), authority: principal.authority_revision, source: request.source.clone(),
            digest: request.digest.clone(), expected: request.expected, result: request.expected + 1,
            key: request.key.clone(), order: None, lifecycle: None, approval: None,
        };
        let receipt_bytes = encode_receipt(&receipt).map_err(|_| ProductError::ContractMismatch)?;
        let public = w1_public_receipt(receipt_bytes.clone())?;
        tx.execute("INSERT INTO product_w1_operations_v1(actor_id,request_key,operation,authority_revision,source_instance_id,digest,expected_revision,approval_result_revision,order_id,lifecycle,selection_result_revision,referenced_approval_revision,referenced_operation,request,receipt) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,NULL,NULL,NULL,NULL,NULL,?9,?10)",
            params![principal.actor_id, request_key, operation, principal.authority_revision, source_instance_id, digest, expected_approval_revision, receipt.result, request_bytes, receipt_bytes])?;
        tx.commit()?;
        Ok(public)
    }

    #[cfg(feature = "w1-isolated-test-support")]
    pub fn w1_select_pack(
        &mut self, session_digest: &str, order_id: &str, lifecycle: i64,
        source_instance_id: &str, digest: &str, expected_selection_revision: i64,
        request_key: &str, now: DateTime<Utc>,
    ) -> Result<W1OperationReceipt, ProductError> {
        use w1_owner_codec::{Request, Receipt, encode_request, encode_receipt};
        if !w1_text(order_id) || lifecycle <= 0 || !w1_text(source_instance_id) || !w1_text(request_key)
            || digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || expected_selection_revision < 0 || expected_selection_revision.checked_add(1).is_none() {
            return Err(ProductError::InvalidConfig);
        }
        self.conn.pragma_update(None, "foreign_keys", "ON").map_err(isolated_schema_error)?;
        self.conn.pragma_update(None, "recursive_triggers", "ON").map_err(isolated_schema_error)?;
        validate_isolated_pragmas(&self.conn)?;
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if self.open_mode == StoreOpenMode::Normal { return Err(ProductError::ContractMismatch); }
        validate_isolated_pragmas(&tx)?;
        validate_isolated_marker_version(&tx, self.open_mode)?;
        validate_w1_operations(&tx)?;
        let principal = resolve_session_in(&tx, session_digest, now).map_err(|e| match e {
            ProductError::Invariant("session invalid" | "session expiry invalid" | "session expired, revoked, or stale") => w1_authorization_error(), other => other,
        })?;
        if principal.role != "owner" { return Err(w1_authorization_error()); }
        w1_authorize_source(&tx, &principal.actor_id, source_instance_id)?;
        w1_authorize_order(&tx, &principal, order_id, lifecycle, source_instance_id)?;
        let request = Request { installation: self.installation_id.clone(), database: self.database_id.clone(),
            operation: "select".into(), actor: principal.actor_id.clone(), source: source_instance_id.into(),
            digest: digest.into(), expected: expected_selection_revision, key: request_key.into(),
            order: Some(order_id.into()), lifecycle: Some(lifecycle) };
        let request_bytes = encode_request(&request).map_err(|_| ProductError::InvalidConfig)?;
        if let Some(receipt) = w1_replay(&tx, &principal, &request, &request_bytes)? {
            tx.commit()?;
            return Ok(receipt);
        }
        let (approval_revision, active, selection_revision) = w1_validate_histories(
            &tx, &self.installation_id, &self.database_id, source_instance_id, digest, Some((order_id, lifecycle)))?;
        let blocked: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM product_review_runs_v1 WHERE order_id=?1 AND product_lifecycle=?2) OR EXISTS(SELECT 1 FROM product_review_holds_v1 WHERE order_id=?1 AND lifecycle=?2) OR EXISTS(SELECT 1 FROM product_review_active_v1 WHERE order_id=?1)", params![order_id, lifecycle], |r| r.get(0))?;
        if blocked { return Err(ProductError::Invariant("w1 selection blocked by review")); }
        if selection_revision != expected_selection_revision { return Err(ProductError::Invariant("w1 stale revision")); }
        if !active { return Err(ProductError::Invariant("w1 approval inactive")); }
        let receipt = Receipt { installation: request.installation.clone(), database: request.database.clone(),
            operation: request.operation.clone(), actor: request.actor.clone(), authority: principal.authority_revision,
            source: request.source.clone(), digest: request.digest.clone(), expected: request.expected,
            result: request.expected + 1, key: request.key.clone(), order: request.order.clone(),
            lifecycle: request.lifecycle, approval: Some(approval_revision) };
        let receipt_bytes = encode_receipt(&receipt).map_err(|_| ProductError::ContractMismatch)?;
        let public = w1_public_receipt(receipt_bytes.clone())?;
        tx.execute("INSERT INTO product_w1_operations_v1(actor_id,request_key,operation,authority_revision,source_instance_id,digest,expected_revision,approval_result_revision,order_id,lifecycle,selection_result_revision,referenced_approval_revision,referenced_operation,request,receipt) VALUES(?1,?2,'select',?3,?4,?5,?6,NULL,?7,?8,?9,?10,'approve',?11,?12)",
            params![principal.actor_id, request_key, principal.authority_revision, source_instance_id, digest, expected_selection_revision, order_id, lifecycle, receipt.result, approval_revision, request_bytes, receipt_bytes])?;
        tx.commit()?;
        Ok(public)
    }

    pub fn open(path: &Path, config: ProductConfig) -> Result<Self, ProductError> {
        Self::open_with_clock(path, config, std::sync::Arc::new(SystemProductClock))
    }

    pub fn open_with_clock(
        path: &Path,
        config: ProductConfig,
        clock: std::sync::Arc<dyn ProductClock>,
    ) -> Result<Self, ProductError> {
        Self::open_internal(path, config, clock, StoreOpenMode::Normal)
    }

    /// Create a fresh isolated test store in a private directory exclusively controlled
    /// by the caller, including against same-UID interference, with no other writers.
    /// `name` must be a single file name; an existing entry is never opened.
    #[cfg(feature = "w1-isolated-test-support")]
    pub fn create_isolated_test_store(directory: &Path, name: &str, config: ProductConfig) -> Result<Self, ProductError> {
        Self::create_isolated_with_mode(directory, name, config, std::sync::Arc::new(SystemProductClock), StoreOpenMode::IsolatedCreate)
    }

    #[cfg(feature = "w1-isolated-test-support")]
    pub fn create_w1_capture_test_store(directory: &Path, name: &str, config: ProductConfig, clock: std::sync::Arc<dyn ProductClock>) -> Result<Self, ProductError> {
        Self::create_isolated_with_mode(directory, name, config, clock, StoreOpenMode::CaptureCreate)
    }

    /// Fresh, empty test-only storage; reference guards and pin writer are deferred.
    #[cfg(feature = "w1-isolated-test-support")]
    pub fn create_w1_pin_test_store(directory: &Path, name: &str, config: ProductConfig) -> Result<Self, ProductError> {
        Self::create_isolated_with_mode(directory, name, config, std::sync::Arc::new(SystemProductClock), StoreOpenMode::PinCreate)
    }

    #[cfg(feature = "w1-isolated-test-support")]
    fn create_isolated_with_mode(directory: &Path, name: &str, config: ProductConfig, clock: std::sync::Arc<dyn ProductClock>, mode: StoreOpenMode) -> Result<Self, ProductError> {
        let path = isolated_test_path(directory, name)?;
        for suffix in ["-journal", "-wal", "-shm"] {
            let sidecar = path.with_file_name(format!("{}{}", name, suffix));
            match std::fs::symlink_metadata(sidecar) {
                Ok(_) => return Err(ProductError::InvalidConfig),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(ProductError::Invariant("isolated store sidecar inspection failed")),
            }
        }
        let _exclusive_file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)
            .map_err(|_| ProductError::Invariant("isolated store file creation failed"))?;
        Self::open_internal(&path, config, clock, mode)
    }

    /// Reopen a marked isolated test store in a private directory exclusively controlled
    /// by the caller, including against same-UID interference, with no other writers.
    #[cfg(feature = "w1-isolated-test-support")]
    pub fn reopen_isolated_test_store(directory: &Path, name: &str, config: ProductConfig) -> Result<Self, ProductError> {
        Self::reopen_isolated_with_mode(directory, name, config, std::sync::Arc::new(SystemProductClock), StoreOpenMode::IsolatedReopen)
    }

    #[cfg(feature = "w1-isolated-test-support")]
    pub fn reopen_w1_capture_test_store(directory: &Path, name: &str, config: ProductConfig, clock: std::sync::Arc<dyn ProductClock>) -> Result<Self, ProductError> {
        Self::reopen_isolated_with_mode(directory, name, config, clock, StoreOpenMode::CaptureReopen)
    }

    #[cfg(feature = "w1-isolated-test-support")]
    pub fn reopen_w1_pin_test_store(directory: &Path, name: &str, config: ProductConfig) -> Result<Self, ProductError> {
        Self::reopen_isolated_with_mode(directory, name, config, std::sync::Arc::new(SystemProductClock), StoreOpenMode::PinReopen)
    }

    #[cfg(feature = "w1-isolated-test-support")]
    fn reopen_isolated_with_mode(directory: &Path, name: &str, config: ProductConfig, clock: std::sync::Arc<dyn ProductClock>, mode: StoreOpenMode) -> Result<Self, ProductError> {
        let path = isolated_test_path(directory, name)?;
        for suffix in ["-journal", "-wal", "-shm"] {
            let sidecar = path.with_file_name(format!("{}{}", name, suffix));
            match std::fs::symlink_metadata(sidecar) {
                Ok(_) => return Err(ProductError::InvalidConfig),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(ProductError::Invariant("isolated store sidecar inspection failed")),
            }
        }
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|_| ProductError::Invariant("isolated store file inspection failed"))?;
        if !metadata.file_type().is_file() {
            return Err(ProductError::InvalidConfig);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.nlink() != 1 { return Err(ProductError::InvalidConfig); }
        }
        Self::open_internal(&path, config, clock, mode)
    }

    fn open_internal(
        path: &Path,
        config: ProductConfig,
        clock: std::sync::Arc<dyn ProductClock>,
        mode: StoreOpenMode,
    ) -> Result<Self, ProductError> {
        Self::open_internal_with_checkpoint(
            path, config, clock, mode,
            #[cfg(all(test, feature = "a01-harness-support", unix))]
            None,
        )
    }

    
fn open_internal_with_checkpoint(
        path: &Path,
        config: ProductConfig,
        clock: std::sync::Arc<dyn ProductClock>,
        mode: StoreOpenMode,
            #[cfg(all(test, feature = "a01-harness-support", unix))] checkpoint: Option<usize>,
    ) -> Result<Self, ProductError> {
        if path.as_os_str().is_empty()
            || config.installation_id.trim().is_empty()
            || config.database_id.trim().is_empty()
        {
            return Err(ProductError::InvalidConfig);
        }
        if mode == StoreOpenMode::Normal {
            reject_isolated_before_path_creation(path)?;
        }
        let path = normalized_path(path)?;
        // Inspect an existing database read-only before a writable SQLite open,
        // which can otherwise perform recovery before the marker is checked.
        if path.exists() {
            let inspection = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            let marker: bool = inspection.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='product_w1_isolated_marker_v1')",
                [], |r| r.get(0),
            )?;
            if mode == StoreOpenMode::Normal && marker {
                return Err(ProductError::ContractMismatch);
            }
            // A01 uses the product schema version as its discriminator. Check
            // it on this read-only connection before SQLite can open writable.
            if mode == StoreOpenMode::Normal {
                let has_meta: bool = inspection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='product_meta_v1')",
                    [], |r| r.get(0),
                )?;
                if has_meta {
                    let version: Option<i64> = inspection.query_row(
                        "SELECT schema_version FROM product_meta_v1 WHERE singleton=1",
                        [], |r| r.get(0),
                    ).optional()?;
                    if version == Some(0x0A01) {
                        return Err(ProductError::ContractMismatch);
                    }
                }
            }
            #[cfg(feature = "w1-isolated-test-support")]
            if mode.is_reopen() {
                validate_isolated_marker_version(&inspection, mode)?;
            }
        }
        let mut conn = if mode == StoreOpenMode::Normal {
            Connection::open(&path)?
        } else {
            Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?
        };
        let isolated_marker: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='product_w1_isolated_marker_v1')",
            [], |r| r.get(0),
        )?;
        match mode {
            StoreOpenMode::Normal if isolated_marker => return Err(ProductError::ContractMismatch),
            #[cfg(feature = "w1-isolated-test-support")]
            StoreOpenMode::IsolatedReopen | StoreOpenMode::CaptureReopen | StoreOpenMode::PinReopen if !isolated_marker => return Err(ProductError::ContractMismatch),
            #[cfg(feature = "w1-isolated-test-support")]
            StoreOpenMode::IsolatedCreate | StoreOpenMode::CaptureCreate | StoreOpenMode::PinCreate if isolated_marker => return Err(ProductError::ContractMismatch),
            _ => {}
        }
        #[cfg(feature = "w1-isolated-test-support")]
        if mode.is_reopen() {
            validate_isolated_marker_version(&conn, mode)?;
        }
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        #[cfg(feature = "w1-isolated-test-support")]
        if mode != StoreOpenMode::Normal {
            conn.pragma_update(None, "foreign_keys", "ON").map_err(isolated_schema_error)?;
        } else {
            conn.pragma_update(None, "foreign_keys", "ON")?;
        }
        #[cfg(not(feature = "w1-isolated-test-support"))]
        conn.pragma_update(None, "foreign_keys", "ON")?;
        #[cfg(feature = "w1-isolated-test-support")]
        if mode != StoreOpenMode::Normal {
            conn.pragma_update(None, "recursive_triggers", "ON").map_err(isolated_schema_error)?;
            validate_isolated_pragmas(&conn)?;
        }
        #[cfg(feature = "a01-harness-support")]
        if mode.is_a01() {
            configure_a01_connection(&conn)?;
        }
        let has_contract: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='foundry_contract')",
            [], |r| r.get(0),
        )?;
        let existing: Option<String> = if has_contract {
            conn.query_row(
                "SELECT contract FROM foundry_contract WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?
        } else {
            None
        };
        #[cfg(feature = "w1-isolated-test-support")]
        if mode.is_reopen() && existing.as_deref() != Some("ProductAtomicV1") {
            return Err(ProductError::ContractMismatch);
        }
        if existing.as_deref().is_some_and(|v| v != "ProductAtomicV1") {
            return Err(ProductError::ContractMismatch);
        }
        if has_contract && existing.is_none() {
            return Err(ProductError::ContractMismatch);
        }
        let has_meta: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='product_meta_v1')",
            [],
            |r| r.get(0),
        )?;
        if existing.as_deref() == Some("ProductAtomicV1") {
            if !has_meta {
                return Err(ProductError::ContractMismatch);
            }
            let meta: Option<(String, String, i64)> = conn
                .query_row(
                    "SELECT installation_id,database_id,schema_version FROM product_meta_v1 WHERE singleton=1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            if !matches!(meta.as_ref(), Some((installation, database, version))
                if installation == &config.installation_id
                    && database == &config.database_id
                    && *version == 3)
            {
                return Err(ProductError::ContractMismatch);
            }
            // Product v3 keeps its result schema stable. The publication
            // ledger is an additive v3-compatible extension.
            result::validate_v3_connection(&conn, &config.installation_id, &config.database_id)?;
            if mode == StoreOpenMode::Normal {
                let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
                create_product_publish_schema(&tx)?;
                tx.commit()?;
            }
            #[cfg(feature = "w1-isolated-test-support")]
            if mode.is_reopen() {
                let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
                validate_isolated_pragmas(&tx)?;
                validate_isolated_marker_version(&tx, mode)?;
                validate_w1_operations(&tx)?;
                if mode.is_capture() || mode.is_pin() { validate_w1_capture_schema(&tx)?; }
                if mode.is_pin() { validate_w1_pin_schema(&tx)?; }
                tx.commit()?;
            }
            return Ok(Self {
                conn,
                open_mode: mode,
                #[cfg(feature = "a01-harness-support")]
                a01_identity: None,
                path,
                installation_id: config.installation_id,
                database_id: config.database_id,
                clock,
                #[cfg(feature = "w1-isolated-test-support")]
                w1_capture_fail_after_legacy_writes: false,
                #[cfg(feature = "w1-isolated-test-support")]
                w1_capture_busy_observer: None,
                review_config: None,
                artifact_root: None,
                artifact_root_handle: None,
                active_review_claim: None,
                active_review_claim_lock: None,
                #[cfg(any(test, feature = "p2-test-support"))]
                charge_budget_observer: None,
            });
        }
        if !has_contract {
            // A contract-less database is fresh only when it has no user
            // tables. In particular, never adopt a standalone review-host or
            // tuple database and then add ProductAtomicV1 tables to it.
            let non_system_tables: i64 = conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
                [],
                |r| r.get(0),
            )?;
            if non_system_tables != 0 || has_meta {
                return Err(ProductError::ContractMismatch);
            }
        }
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        initialize_product_schema_with_a01_checkpoint_in(
                &tx, &config, mode,
                #[cfg(all(test, feature = "a01-harness-support", unix))]
                checkpoint,
            )?;
        #[cfg(feature = "a01-harness-support")]
        if mode.is_a01() {
            if mode.is_a01_task() { a01_task::validate_a01_task_connection(&tx, &config)?; }
            else { validate_a01_connection(&tx, &config)?; }
        }
        #[cfg(feature = "w1-isolated-test-support")]
        if mode.is_create() {
            validate_isolated_pragmas(&tx)?;
            validate_isolated_marker_version(&tx, mode)?;
            validate_w1_operations(&tx)?;
            if mode.is_capture() || mode.is_pin() { validate_w1_capture_schema(&tx)?; }
            if mode.is_pin() { validate_w1_pin_schema(&tx)?; }
            result::validate_v3_connection(&tx, &config.installation_id, &config.database_id)?;
        }
        #[cfg(all(test, feature = "a01-harness-support", unix))]
            if mode.is_a01() {
                a01_initialization_checkpoint(checkpoint, 6)?;
                a01_initialization_checkpoint(checkpoint, 7)?;
            }
            tx.commit()?;
        Ok(Self {
            conn,
            open_mode: mode,
            #[cfg(feature = "a01-harness-support")]
            a01_identity: None,
            path,
            installation_id: config.installation_id,
            database_id: config.database_id,
            clock,
            #[cfg(feature = "w1-isolated-test-support")]
            w1_capture_fail_after_legacy_writes: false,
            #[cfg(feature = "w1-isolated-test-support")]
            w1_capture_busy_observer: None,
            review_config: None,
            artifact_root: None,
            artifact_root_handle: None,
            active_review_claim: None,
            active_review_claim_lock: None,
            #[cfg(any(test, feature = "p2-test-support"))]
            charge_budget_observer: None,
        })
    }

    /// Installs a per-store observation hook available only to test builds or
    /// the explicit test-support feature. Notifications cannot influence the
    /// transaction or its authorization decisions.
    #[cfg(any(test, feature = "p2-test-support"))]
    pub fn set_charge_budget_observer(&mut self, observer: Option<review::ChargeBudgetObserver>) {
        self.charge_budget_observer = observer;
    }

    /// Pin the shared review host to an already configured ProductAtomicV1
    /// store. Artifact/session/claim roots remain separate from the database.
    pub fn configure_review_backend(
        &mut self,
        config: ProductReviewConfig,
    ) -> Result<(), ProductError> {
        // Configuration pins paths and pack only. It does not grant service
        // authority; admission and every new stage validate a live service
        // claim inside their write transaction.
        if config.pack_digest.trim().is_empty()
            || [
                &config.artifact_root,
                &config.session_root,
                &config.claim_root,
            ]
            .iter()
            .any(|path| !path.is_absolute())
        {
            return Err(ProductError::InvalidConfig);
        }
        let roots = [
            normalized_path(&config.artifact_root)?,
            normalized_path(&config.session_root)?,
            normalized_path(&config.claim_root)?,
        ];
        if roots.iter().any(|root| *root == self.path)
            || roots
                .iter()
                .enumerate()
                .any(|(index, root)| roots[..index].contains(root))
        {
            return Err(ProductError::InvalidConfig);
        }
        self.configure_product_artifact_root(config.artifact_root.clone())?;
        self.review_config = Some(config);
        Ok(())
    }

    /// Pin the product artifact root to this ProductAtomicV1 installation.
    /// The path is separate from the SQLite database and cannot be silently
    /// changed by another handle or server process.
    pub fn configure_product_artifact_root(
        &mut self,
        root: impl AsRef<Path>,
    ) -> Result<(), ProductError> {
        let root = normalized_path(root.as_ref())?;
        if root == self.path {
            return Err(ProductError::InvalidConfig);
        }
        std::fs::create_dir_all(&root).map_err(|_| ProductError::InvalidConfig)?;
        let pinned_root = std::sync::Arc::new(review::pin_artifact_root(&root)?);
        let root_identity = review::artifact_root_identity(&pinned_root)?;
        let root_text = root.to_string_lossy().into_owned();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old: Option<String> = tx
            .query_row(
                "SELECT root_path FROM product_artifact_root_v1 WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        match old {
            Some(old) if old != root_text => return Err(ProductError::ContractMismatch),
            Some(_) => {}
            None => {
                tx.execute(
                    "INSERT INTO product_artifact_root_v1(singleton,root_path) VALUES(1,?1)",
                    [&root_text],
                )?;
            }
        }
        let old_identity: Option<(i64, i64)> = tx
            .query_row(
                "SELECT device,inode FROM product_artifact_root_identity_v1 WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match old_identity {
            Some((device, inode)) if (device, inode) != root_identity => {
                return Err(ProductError::ContractMismatch);
            }
            Some(_) => {}
            None => {
                tx.execute("INSERT INTO product_artifact_root_identity_v1(singleton,device,inode) VALUES(1,?1,?2)", params![root_identity.0,root_identity.1])?;
            }
        }
        tx.commit()?;
        self.artifact_root = Some(root);
        self.artifact_root_handle = Some(pinned_root);
        Ok(())
    }

    /// Read and verify the authoritative product journal under one SQLite
    /// snapshot. This is for projection/recovery only; callers cannot append
    /// rows through this API.
    pub fn projection_snapshot(&mut self) -> Result<ProductProjectionSnapshot, ProductError> {
        let scope_mode: String = self.conn.query_row(
            "SELECT mode FROM product_scope_policy_state_v1 WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if scope_mode == "enforced" {
            return Err(ProductError::Invariant(
                "unscoped product projection is unavailable under scoped policy",
            ));
        }
        let tx = self.conn.transaction()?;
        let mut statement = tx.prepare("SELECT seq,order_id,from_state,to_state,event,actor,intent,at,prev_hash,hash,event_json FROM ledger ORDER BY seq")?;
        let rows = statement
            .query_map([], |r| {
                Ok(crate::LedgerRow {
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
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if !crate::sqlite::SqliteLedger::verify_rows(&rows) {
            return Err(ProductError::Invariant(
                "product S1 chain verification failed",
            ));
        }
        let lifecycles = crate::replay::replay(&rows, crate::sqlite::SqliteLedger::verify_rows)
            .map_err(|_| ProductError::Invariant("product S1 lifecycle replay failed"))?;
        let head_sequence = rows.last().map(|r| r.seq).unwrap_or(0);
        let head_hash = rows.last().map(|r| r.hash.clone()).unwrap_or_default();
        drop(statement);
        tx.commit()?;
        Ok(ProductProjectionSnapshot {
            rows,
            lifecycles,
            head_sequence,
            head_hash,
        })
    }

    /// Revalidate a committed approval immediately before a distinct publish
    /// side effect. A receipt supplied by a caller is only a lookup key: its
    /// exact bytes must match the immutable outcome in this ProductStore, and
    /// the current lifecycle, source binding and promoted tuple are checked
    /// under one SQLite read transaction.
    pub fn authorize_product_publish(
        &self,
        supplied: &ProductApprovalReceipt,
    ) -> Result<ProductPublishAuthorization, ProductError> {
        let tx = self.conn.unchecked_transaction()?;
        if scope_enforced_in(&tx)? {
            return Err(ProductError::Invariant(
                "publish requires an authenticated scoped authorization",
            ));
        }
        let result_schema: i64 = tx.query_row(
            "SELECT schema_version FROM product_meta_v1 WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if result_schema >= 3 {
            return Err(ProductError::Invariant(
                "legacy publish authorization is disabled for ProductAtomicV1 v3",
            ));
        }
        validate_product_publish_receipt_in(&tx, supplied)?;
        tx.commit()?;
        Ok(ProductPublishAuthorization {
            receipt: supplied.clone(),
        })
    }

    /// Atomically revalidate current scoped authority and the committed v3
    /// approval, then persist a durable publish start. The returned state is
    /// committed before a caller may contact its Publisher.
    pub fn begin_product_publish(
        &mut self,
        session_digest: &str,
        supplied: &ProductApprovalReceipt,
        now: DateTime<Utc>,
    ) -> Result<ProductPublishStart, ProductError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Authentication is always required: only a resolved principal may
        // read or extend publish state.
        let _principal = resolve_session_in(&tx, session_digest, now)?;
        let receipt_json = serde_json::to_string(supplied)
            .map_err(|_| ProductError::Invariant("approval receipt serialization failed"))?;
        let existing: Option<(String, String, Option<String>)> = tx
            .query_row(
                "SELECT receipt_json,state,pr_url FROM product_publish_outcomes_v1 WHERE operation_id=?1",
                [&supplied.operation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((stored_receipt, state, pr_url)) = existing {
            // Read/reconciliation path: an already-started operation is
            // distinguished from permission to execute again. The committed
            // start row plus exact receipt binding is the authority; fresh
            // execution authority (capability + current lifecycle) is NOT
            // re-required for retrieving or reconciling an existing effect.
            if stored_receipt != receipt_json {
                return Err(ProductError::Invariant(
                    "publish operation receipt binding conflict",
                ));
            }
            let start = match state.as_str() {
                "pending" => ProductPublishStart::Reconcile,
                "confirmed" => ProductPublishStart::Confirmed {
                    pr_url: pr_url
                        .ok_or(ProductError::Invariant("confirmed publish URL is missing"))?,
                },
                _ => return Err(ProductError::Invariant("publish state is invalid")),
            };
            tx.commit()?;
            return Ok(start);
        }
        // Fresh execution path: no durable start exists yet, so current
        // scoped execution authority and full receipt revalidation gate the
        // INSERT.
        require_order_capability_in(
            &tx,
            &_principal,
            &supplied.order_id,
            ProductCapability::PublishApproved,
            true,
        )?;
        validate_product_publish_receipt_in(&tx, supplied)?;
        tx.execute(
            "INSERT INTO product_publish_outcomes_v1(operation_id,receipt_json,state,pr_url,started_at,settled_at) VALUES(?1,?2,'pending',NULL,?3,NULL)",
            params![supplied.operation_id, receipt_json, now.timestamp()],
        )?;
        tx.commit()?;
        Ok(ProductPublishStart::Started)
    }

    /// Re-fence a committed pending publish before any external retry (used
    /// when a reconciler reports NotFound). Returns `Confirmed` when another
    /// caller has already settled the operation, `Started` only while the
    /// durable row is still pending with the exact bound receipt, and errors
    /// when no start exists or the receipt binding conflicts. No publisher
    /// call is permitted without passing this fence.
    pub fn claim_product_publish_retry(
        &mut self,
        supplied: &ProductApprovalReceipt,
    ) -> Result<ProductPublishStart, ProductError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let receipt_json = serde_json::to_string(supplied)
            .map_err(|_| ProductError::Invariant("approval receipt serialization failed"))?;
        let row: Option<(String, String, Option<String>)> = tx
            .query_row(
                "SELECT receipt_json,state,pr_url FROM product_publish_outcomes_v1 WHERE operation_id=?1",
                [&supplied.operation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((stored_receipt, state, pr_url)) = row else {
            return Err(ProductError::Invariant(
                "publish operation start is missing for retry",
            ));
        };
        if stored_receipt != receipt_json {
            return Err(ProductError::Invariant(
                "publish operation receipt binding conflict",
            ));
        }
        let start = match state.as_str() {
            "pending" => ProductPublishStart::Started,
            "confirmed" => ProductPublishStart::Confirmed {
                pr_url: pr_url
                    .ok_or(ProductError::Invariant("confirmed publish URL is missing"))?,
            },
            _ => return Err(ProductError::Invariant("publish state is invalid")),
        };
        tx.commit()?;
        Ok(start)
    }

    /// Settle one previously committed pending publish start. Idempotent for
    /// an already-confirmed row with the identical URL (concurrent
    /// reconcilers observing the same confirmed effect both succeed); a
    /// different URL or an unknown operation is rejected.
    pub fn settle_product_publish(
        &mut self,
        operation_id: &str,
        pr_url: &str,
    ) -> Result<(), ProductError> {
        if operation_id.trim().is_empty() || pr_url.trim().is_empty() {
            return Err(ProductError::InvalidConfig);
        }
        let now = self.clock.now().timestamp();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE product_publish_outcomes_v1 SET state='confirmed',pr_url=?1,settled_at=?2 WHERE operation_id=?3 AND state='pending' AND pr_url IS NULL AND settled_at IS NULL",
            params![pr_url, now, operation_id],
        )?;
        if changed != 1 {
            let existing: Option<(String, Option<String>)> = tx
                .query_row(
                    "SELECT state,pr_url FROM product_publish_outcomes_v1 WHERE operation_id=?1",
                    [operation_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if existing
                .is_some_and(|(state, url)| state == "confirmed" && url.as_deref() == Some(pr_url))
            {
                // Concurrent settlement of the same confirmed effect: success.
                tx.commit()?;
                return Ok(());
            }
            return Err(ProductError::Invariant(
                "publish operation is unknown or already settled",
            ));
        }
        tx.commit()?;
        Ok(())
    }

    /// Register/update a verified connector's source scope. Caller input is
    /// configuration only; delivery bodies cannot change this row.
    pub fn bind_source(
        &mut self,
        connector: &str,
        provider: &str,
        origin: &str,
        repository_id: &str,
        scope: &str,
    ) -> Result<i64, ProductError> {
        if [connector, provider, origin, repository_id, scope]
            .iter()
            .any(|v| v.trim().is_empty())
        {
            return Err(ProductError::InvalidConfig);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if scope_enforced_in(&tx)? {
            return Err(ProductError::Invariant(
                "legacy source binding is unavailable after scope activation",
            ));
        }
        tx.execute("INSERT INTO product_sources_v1(connector_id,provider,origin,repository_id,scope,revision,enabled) VALUES(?1,?2,?3,?4,?5,1,1) ON CONFLICT(connector_id) DO UPDATE SET provider=excluded.provider,origin=excluded.origin,repository_id=excluded.repository_id,scope=excluded.scope,revision=product_sources_v1.revision+1,enabled=1", params![connector, provider, origin, repository_id, scope])?;
        let rev = tx.query_row(
            "SELECT revision FROM product_sources_v1 WHERE connector_id=?1",
            [connector],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(rev)
    }

    /// Configure a canonical repository binding under the current Owner's
    /// management authority. A rebind creates a new immutable source instance;
    /// existing grants remain attached to the retired instance.
    pub fn configure_scoped_source(
        &mut self,
        session_digest: &str,
        connector: &str,
        identity: RepositoryIdentity,
        api_owner: &str,
        api_repo: &str,
        legacy_repository_locator: &str,
        scope_query: &str,
        enabled: bool,
        expected_revision: i64,
        request_key: &str,
        now: DateTime<Utc>,
    ) -> Result<ScopeSourceManifest, ProductError> {
        if connector.trim().is_empty()
            || legacy_repository_locator.trim().is_empty()
            || scope_query.trim().is_empty()
            || expected_revision < 0
            || request_key.trim().is_empty()
            || request_key.len() > 256
        {
            return Err(ProductError::InvalidConfig);
        }
        let identity = canonical_scope_identity(&identity)?;
        validate_api_locator_parts(api_owner, api_repo)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let principal = resolve_session_in(&tx, session_digest, now)?;
        if principal.role != "owner" {
            return Err(ProductError::Invariant(
                "current role cannot administer sources",
            ));
        }
        tx.execute(
            "UPDATE product_scope_policy_state_v1 SET mode='enforced' WHERE singleton=1",
            [],
        )?;
        let source_request_digest = crate::product_scope::framed_hash(
            "foundry-product-source-change-v1",
            &[
                &principal.actor_id,
                &principal.authority_revision.to_string(),
                connector,
                &identity.key(),
                api_owner,
                api_repo,
                legacy_repository_locator,
                scope_query,
                if enabled { "enabled" } else { "disabled" },
                &expected_revision.to_string(),
            ],
        );
        if let Some((old_digest, receipt_json)) = tx
            .query_row(
                "SELECT request_digest,receipt_json FROM product_scope_operations_v1 WHERE actor_id=?1 AND request_key=?2",
                params![principal.actor_id,request_key],
                |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)),
            )
            .optional()?
        {
            if old_digest != source_request_digest {
                return Err(ProductError::Invariant("scope idempotency key binding conflict"));
            }
            let source: ScopeSourceManifest = serde_json::from_str(&receipt_json)
                .map_err(|_| ProductError::Invariant("stored source receipt is corrupt"))?;
            if source.api_owner.is_none() || source.api_repo.is_none() {
                return Err(ProductError::Invariant(
                    "legacy source receipt needs an explicit locator rebind",
                ));
            }
            tx.commit()?;
            return Ok(source);
        }
        let legacy_revision: i64 = tx
            .query_row(
                "SELECT revision FROM product_sources_v1 WHERE connector_id=?1",
                [&connector],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        if legacy_revision != expected_revision {
            return Err(ProductError::Invariant("source revision conflict"));
        }
        let current: Option<(String, String, String, String, String, i64)> = tx
            .query_row(
                "SELECT i.source_instance_id,r.organization,r.provider,r.api_origin,r.stable_repository_id,i.enabled
                 FROM product_scope_current_sources_v1 c
                 JOIN product_scope_source_instances_v1 i USING(source_instance_id)
                 JOIN product_scope_repositories_v1 r USING(repository_key)
                 WHERE c.connector_id=?1",
                [&connector],
                |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
            )
            .optional()?;
        let old_source_instance_id = current.as_ref().map(|row| row.0.clone());
        if let Some((old_id, ..)) = current {
            tx.execute(
                "UPDATE product_scope_source_instances_v1 SET enabled=0,revision=revision+1,retired_at=?1 WHERE source_instance_id=?2",
                params![now.to_rfc3339(),old_id],
            )?;
            tx.execute(
                "UPDATE product_grants_v1 SET state='invalidated' WHERE state IN ('issued','reserved') AND order_id IN (SELECT order_id FROM product_scope_order_provenance_v1 WHERE source_instance_id=?1)",
                [&old_id],
            )?;
            tx.execute(
                "UPDATE product_operations_v1 SET phase='fenced' WHERE phase='prepared' AND order_id IN (SELECT order_id FROM product_scope_order_provenance_v1 WHERE source_instance_id=?1)",
                [&old_id],
            )?;
        }
        if enabled {
            let collision: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM product_scope_current_sources_v1 c JOIN product_scope_source_instances_v1 i USING(source_instance_id) WHERE i.repository_key=?1 AND i.enabled=1 AND c.connector_id<>?2)",
                params![identity.key(),connector], |r| r.get(0),
            )?;
            if collision {
                return Err(ProductError::Invariant(
                    "canonical repository already has an active intake binding",
                ));
            }
        }
        let generation: i64 = tx.query_row(
            "SELECT COALESCE(MAX(generation),0)+1 FROM product_scope_source_instances_v1 WHERE connector_id=?1",
            [&connector], |r| r.get(0),
        )?;
        let generation_text = generation.to_string();
        let repository_key = identity.key();
        let source_instance_id = crate::product_scope::framed_hash(
            "foundry-product-source-instance-v1",
            &[
                &self.installation_id,
                connector,
                &generation_text,
                &repository_key,
            ],
        );
        let manifest_source = ScopeSourceManifest {
            connector_id: connector.into(),
            source_instance_id: source_instance_id.clone(),
            generation,
            identity: identity.clone(),
            api_owner: Some(api_owner.to_owned()),
            api_repo: Some(api_repo.to_owned()),
            legacy_repository_locator: legacy_repository_locator.into(),
            scope_query: scope_query.into(),
            enabled,
        };
        insert_scope_source(&tx, &manifest_source, &identity)?;
        tx.execute(
            "INSERT INTO product_scope_current_sources_v1(connector_id,source_instance_id) VALUES(?1,?2) ON CONFLICT(connector_id) DO UPDATE SET source_instance_id=excluded.source_instance_id",
            params![connector,source_instance_id],
        )?;
        if legacy_revision == 0 {
            tx.execute(
                "INSERT INTO product_sources_v1(connector_id,provider,origin,repository_id,scope,revision,enabled) VALUES(?1,?2,?3,?4,?5,1,?6)",
                params![connector,identity.provider,identity.api_origin,identity.stable_repository_id,scope_query,i64::from(enabled)],
            )?;
        } else {
            tx.execute(
                "UPDATE product_sources_v1 SET provider=?1,origin=?2,repository_id=?3,scope=?4,revision=revision+1,enabled=?5 WHERE connector_id=?6",
                params![identity.provider,identity.api_origin,identity.stable_repository_id,scope_query,i64::from(enabled),connector],
            )?;
        }
        let source_manifest = ScopeSourceManifest {
            connector_id: connector.into(),
            source_instance_id: source_instance_id.clone(),
            generation,
            identity,
            api_owner: Some(api_owner.to_owned()),
            api_repo: Some(api_repo.to_owned()),
            legacy_repository_locator: legacy_repository_locator.into(),
            scope_query: scope_query.into(),
            enabled,
        };
        let source_json = serde_json::to_string(&source_manifest)
            .map_err(|_| ProductError::Invariant("source receipt encoding failed"))?;
        let source_revision: i64 = tx.query_row(
            "SELECT revision FROM product_sources_v1 WHERE connector_id=?1",
            [&connector],
            |r| r.get(0),
        )?;
        let operation = if old_source_instance_id.is_none() {
            "created"
        } else {
            "rebound"
        };
        tx.execute(
            "INSERT INTO product_scope_source_events_v1(actor_id,connector_id,old_source_instance_id,new_source_instance_id,operation,source_revision,request_key,request_digest,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![principal.actor_id,connector,old_source_instance_id,source_instance_id,operation,source_revision,request_key,source_request_digest,now.to_rfc3339()],
        )?;
        tx.execute(
            "INSERT INTO product_scope_operations_v1(actor_id,request_key,request_digest,receipt_json) VALUES(?1,?2,?3,?4)",
            params![principal.actor_id,request_key,source_request_digest,source_json],
        )?;
        tx.commit()?;
        Ok(source_manifest)
    }

    /// Owner-only source metadata query. Includes retired generations so a
    /// rebind never hides the identity holding historical orders and grants.
    pub fn list_scoped_sources(
        &mut self,
        session_digest: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<ScopeSourceView>, ProductError> {
        let tx = self.conn.transaction()?;
        let principal = resolve_session_in(&tx, session_digest, now)?;
        if principal.role != "owner" {
            return Err(ProductError::Invariant(
                "current role cannot administer sources",
            ));
        }
        let mut stmt = tx.prepare(
            "SELECT i.source_instance_id,i.connector_id,i.generation,COALESCE((SELECT s.revision FROM product_sources_v1 s WHERE s.connector_id=i.connector_id AND c.source_instance_id=i.source_instance_id),i.revision),r.organization,r.provider,r.api_origin,r.stable_repository_id,i.legacy_repository_locator,i.scope_query,i.enabled,COALESCE(c.source_instance_id=i.source_instance_id,0),l.api_owner,l.api_repo,i.revision
             FROM product_scope_source_instances_v1 i
             JOIN product_scope_repositories_v1 r USING(repository_key)
             LEFT JOIN product_scope_api_locators_v1 l USING(source_instance_id)
             LEFT JOIN product_scope_current_sources_v1 c ON c.connector_id=i.connector_id
             ORDER BY i.connector_id,i.generation",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ScopeSourceView {
                source: ScopeSourceManifest {
                    source_instance_id: row.get(0)?,
                    connector_id: row.get(1)?,
                    generation: row.get(2)?,
                    identity: RepositoryIdentity {
                        organization: row.get(4)?,
                        provider: row.get(5)?,
                        api_origin: row.get(6)?,
                        stable_repository_id: row.get(7)?,
                    },
                    api_owner: row.get(12)?,
                    api_repo: row.get(13)?,
                    legacy_repository_locator: row.get(8)?,
                    scope_query: row.get(9)?,
                    enabled: row.get::<_, i64>(10)? == 1,
                },
                source_instance_revision: row.get(14)?,
                connector_revision: row.get(3)?,
                source_revision: row.get(3)?,
                current: row.get::<_, i64>(11)? == 1,
            })
        })?;
        let result = rows.collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        tx.commit()?;
        Ok(result)
    }

    /// Owner-only current capability query for one known account/source.
    pub fn list_product_scope_grants(
        &mut self,
        session_digest: &str,
        subject_id: &str,
        source_instance_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<ScopeGrantView>, ProductError> {
        let tx = self.conn.transaction()?;
        let principal = resolve_session_in(&tx, session_digest, now)?;
        if principal.role != "owner" {
            return Err(ProductError::Invariant(
                "current role cannot administer repository grants",
            ));
        }
        let known: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM product_accounts_v1 WHERE actor_id=?1) AND EXISTS(SELECT 1 FROM product_scope_source_instances_v1 WHERE source_instance_id=?2)",
            params![subject_id, source_instance_id],
            |row| row.get(0),
        )?;
        if !known {
            return Err(ProductError::Invariant("scope subject unavailable"));
        }
        let scope_revision: i64 = tx.query_row(
            "SELECT revision FROM product_scope_subject_revisions_v1 WHERE subject_id=?1 AND source_instance_id=?2",
            params![subject_id, source_instance_id],
            |row| row.get(0),
        ).optional()?.unwrap_or(0);
        let mut stmt = tx.prepare(
            "SELECT capability,grant_revision,enabled FROM product_scope_grants_v1 WHERE subject_id=?1 AND source_instance_id=?2 ORDER BY capability",
        )?;
        let rows = stmt.query_map(params![subject_id, source_instance_id], |row| {
            let capability: String = row.get(0)?;
            Ok(ScopeGrantView {
                actor_id: subject_id.to_owned(),
                source_instance_id: source_instance_id.to_owned(),
                capability: ProductCapability::parse(&capability)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                grant_revision: row.get(1)?,
                scope_revision,
                enabled: row.get::<_, i64>(2)? == 1,
            })
        })?;
        let result = rows.collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        tx.commit()?;
        Ok(result)
    }

    /// Apply one explicit repository capability change. Owner management is
    /// separate from data access; even an Owner needs this grant to read data.
    pub fn change_product_scope(
        &mut self,
        session_digest: &str,
        subject_id: &str,
        source_instance_id: &str,
        capability: ProductCapability,
        mutation: ScopeMutation,
        expected_scope_revision: i64,
        request_key: &str,
        now: DateTime<Utc>,
    ) -> Result<ScopeGrantReceipt, ProductError> {
        if subject_id.trim().is_empty()
            || source_instance_id.trim().is_empty()
            || request_key.trim().is_empty()
            || request_key.len() > 256
            || expected_scope_revision < 0
        {
            return Err(ProductError::InvalidConfig);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let principal = resolve_session_in(&tx, session_digest, now)?;
        if principal.role != "owner" {
            return Err(ProductError::Invariant(
                "current role cannot administer repository grants",
            ));
        }
        tx.execute(
            "UPDATE product_scope_policy_state_v1 SET mode='enforced' WHERE singleton=1",
            [],
        )?;
        let digest = crate::product_scope::framed_hash(
            "foundry-product-scope-mutation-v1",
            &[
                &principal.actor_id,
                &principal.authority_revision.to_string(),
                subject_id,
                source_instance_id,
                capability.as_str(),
                mutation.as_str(),
                &expected_scope_revision.to_string(),
            ],
        );
        if let Some((old_digest, receipt_json)) = tx
            .query_row(
                "SELECT request_digest,receipt_json FROM product_scope_operations_v1 WHERE actor_id=?1 AND request_key=?2",
                params![principal.actor_id,request_key],
                |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)),
            )
            .optional()?
        {
            if old_digest != digest {
                return Err(ProductError::Invariant(
                    "scope idempotency key binding conflict",
                ));
            }
            let receipt: ScopeGrantReceipt = serde_json::from_str(&receipt_json)
                .map_err(|_| ProductError::Invariant("stored scope receipt is corrupt"))?;
            tx.commit()?;
            return Ok(receipt);
        }
        let target_role: Option<String> = tx
            .query_row(
                "SELECT role FROM product_accounts_v1 WHERE actor_id=?1 AND enabled=1",
                [subject_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(target_role) = target_role else {
            return Err(ProductError::Invariant("scope subject unavailable"));
        };
        if !capability.allowed_for_role(&target_role) {
            return Err(ProductError::Invariant(
                "capability exceeds target role ceiling",
            ));
        }
        if principal.actor_id == subject_id && mutation == ScopeMutation::Grant {
            let already_enabled: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM product_scope_grants_v1 WHERE subject_id=?1 AND source_instance_id=?2 AND capability=?3 AND enabled=1)",
                params![subject_id,source_instance_id,capability.as_str()], |r| r.get(0),
            )?;
            if !already_enabled {
                return Err(ProductError::Invariant(
                    "remote self scope expansion is forbidden",
                ));
            }
        }
        let source_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM product_scope_source_instances_v1 WHERE source_instance_id=?1)",
            [source_instance_id],
            |r| r.get(0),
        )?;
        if !source_exists {
            return Err(ProductError::Invariant("scope source instance unavailable"));
        }
        let revision: i64 = tx
            .query_row(
                "SELECT revision FROM product_scope_subject_revisions_v1 WHERE subject_id=?1 AND source_instance_id=?2",
                params![subject_id,source_instance_id], |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        if revision != expected_scope_revision {
            return Err(ProductError::Invariant("scope revision conflict"));
        }
        let old: Option<(String, i64, i64)> = tx
            .query_row(
                "SELECT grant_id,grant_revision,enabled FROM product_scope_grants_v1 WHERE subject_id=?1 AND source_instance_id=?2 AND capability=?3",
                params![subject_id,source_instance_id,capability.as_str()],
                |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
            )
            .optional()?;
        if mutation == ScopeMutation::Revoke
            && !old.as_ref().is_some_and(|(_, _, enabled)| *enabled == 1)
        {
            return Err(ProductError::Invariant("active scope grant unavailable"));
        }
        let (scope_revision, grant_revision) = if capability == ProductCapability::A01BridgePassed {
            (revision.checked_add(1).ok_or(ProductError::Invariant("scope revision exhausted"))?,
             old.as_ref().map_or(Some(1), |(_, revision, _)| revision.checked_add(1))
                .ok_or(ProductError::Invariant("scope revision exhausted"))?)
        } else {
            (revision + 1, old.map(|(_, revision, _)| revision + 1).unwrap_or(1))
        };
        let grant_id = crate::product_scope::framed_hash(
            "foundry-product-scope-grant-v1",
            &[
                subject_id,
                source_instance_id,
                capability.as_str(),
                &grant_revision.to_string(),
            ],
        );
        let enabled = i64::from(mutation == ScopeMutation::Grant);
        tx.execute(
            "INSERT INTO product_scope_subject_revisions_v1(subject_id,source_instance_id,revision) VALUES(?1,?2,?3)
             ON CONFLICT(subject_id,source_instance_id) DO UPDATE SET revision=excluded.revision",
            params![subject_id,source_instance_id,scope_revision],
        )?;
        tx.execute(
            "INSERT INTO product_scope_grants_v1(grant_id,subject_id,source_instance_id,capability,grant_revision,enabled,created_by,request_key)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(subject_id,source_instance_id,capability) DO UPDATE SET grant_id=excluded.grant_id,grant_revision=excluded.grant_revision,enabled=excluded.enabled,created_by=excluded.created_by,request_key=excluded.request_key",
            params![grant_id,subject_id,source_instance_id,capability.as_str(),grant_revision,enabled,principal.actor_id,request_key],
        )?;
        tx.execute(
            "INSERT INTO product_scope_events_v1(actor_id,subject_id,source_instance_id,capability,operation,scope_revision,grant_revision,request_key,request_digest,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![principal.actor_id,subject_id,source_instance_id,capability.as_str(),mutation.as_str(),scope_revision,grant_revision,request_key,digest,now.to_rfc3339()],
        )?;
        let receipt = ScopeGrantReceipt {
            actor_id: principal.actor_id,
            subject_id: subject_id.into(),
            source_instance_id: source_instance_id.into(),
            capability,
            mutation: ScopeMutationReceiptKind::from(mutation),
            scope_revision,
            grant_revision,
            request_key: request_key.into(),
        };
        let receipt_json = serde_json::to_string(&receipt)
            .map_err(|_| ProductError::Invariant("scope receipt encoding failed"))?;
        tx.execute(
            "INSERT INTO product_scope_operations_v1(actor_id,request_key,request_digest,receipt_json) VALUES(?1,?2,?3,?4)",
            params![receipt.actor_id,request_key,digest,receipt_json],
        )?;
        tx.execute(
            "UPDATE product_grants_v1 SET state='invalidated' WHERE state IN ('issued','reserved') AND actor_id=?1 AND order_id IN (SELECT order_id FROM product_scope_order_provenance_v1 WHERE source_instance_id=?2)",
            params![subject_id,source_instance_id],
        )?;
        tx.execute(
            "UPDATE product_operations_v1 SET phase='fenced' WHERE phase='prepared' AND actor_id=?1 AND order_id IN (SELECT order_id FROM product_scope_order_provenance_v1 WHERE source_instance_id=?2)",
            params![subject_id,source_instance_id],
        )?;
        tx.commit()?;
        Ok(receipt)
    }

    /// Return one order only when the current authenticated principal has an
    /// explicit capability grant for its immutable source provenance.
    pub fn read_scoped_order(
        &mut self,
        session_digest: &str,
        order_id: &str,
        now: DateTime<Utc>,
    ) -> Result<ScopedOrderView, ProductError> {
        let tx = self.conn.transaction()?;
        let principal = resolve_session_in(&tx, session_digest, now)?;
        let view = scoped_order_in(&tx, &principal, order_id, ProductCapability::ReadOrder)?;
        tx.commit()?;
        Ok(view)
    }

    /// List only orders covered by the caller's current explicit ReadOrder
    /// grants. No global counts or foreign order IDs leave this transaction.
    pub fn list_scoped_orders(
        &mut self,
        session_digest: &str,
        after_order_id: Option<&str>,
        limit: usize,
        now: DateTime<Utc>,
    ) -> Result<Vec<ScopedOrderView>, ProductError> {
        if !(1..=100).contains(&limit) {
            return Err(ProductError::InvalidConfig);
        }
        let tx = self.conn.transaction()?;
        let principal = resolve_session_in(&tx, session_digest, now)?;
        if !scope_enforced_in(&tx)? {
            return Err(ProductError::Invariant(
                "product scope policy is not active",
            ));
        }
        if !ProductCapability::ReadOrder.allowed_for_role(&principal.role) {
            return Err(ProductError::Invariant("role does not permit order reads"));
        }
        if let Some(cursor) = after_order_id {
            // A cursor is accepted only if it is itself visible to this
            // principal, avoiding an existence/order-position oracle.
            scoped_order_in(&tx, &principal, cursor, ProductCapability::ReadOrder)?;
        }
        let mut stmt = tx.prepare(
            "SELECT o.order_id FROM product_orders_v1 o
             JOIN product_scope_order_provenance_v1 p ON p.order_id=o.order_id AND p.lifecycle=o.lifecycle AND p.verified=1
             JOIN product_scope_grants_v1 g ON g.source_instance_id=p.source_instance_id AND g.subject_id=?1 AND g.capability='read_order' AND g.enabled=1
             WHERE (?2 IS NULL OR o.order_id>?2)
             ORDER BY o.order_id LIMIT ?3",
        )?;
        let ids = stmt
            .query_map(
                params![principal.actor_id, after_order_id, limit as i64],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        let mut views = Vec::with_capacity(ids.len());
        for id in ids {
            views.push(scoped_order_in(
                &tx,
                &principal,
                &id,
                ProductCapability::ReadOrder,
            )?);
        }
        tx.commit()?;
        Ok(views)
    }

    /// Read one bounded page of evidence covered by the caller's current
    /// explicit export or operational-audit grant. Authentication, capability,
    /// lifecycle provenance, chain verification, and row selection share this
    /// SQLite read transaction. The returned rows are inputs to the serve
    /// pseudonymizer and must not be serialized as LedgerRow values.
    pub fn scoped_evidence_snapshot(
        &mut self,
        session_digest: &str,
        capability: ProductCapability,
        limit: usize,
        now: DateTime<Utc>,
    ) -> Result<ScopedEvidenceSnapshot, ProductError> {
        if !matches!(
            capability,
            ProductCapability::ExportEvidence | ProductCapability::ReadOperationalAudit
        ) || !(1..=100).contains(&limit)
        {
            return Err(ProductError::InvalidConfig);
        }
        let tx = self.conn.transaction()?;
        let principal = resolve_session_in(&tx, session_digest, now)?;
        if !scope_enforced_in(&tx)? || !capability.allowed_for_role(&principal.role) {
            return Err(ProductError::Invariant(
                "scoped evidence capability unavailable",
            ));
        }
        let has_capability: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM product_scope_grants_v1 g
             JOIN product_scope_source_instances_v1 s USING(source_instance_id)
             WHERE g.subject_id=?1 AND g.capability=?2 AND g.enabled=1)",
            params![principal.actor_id, capability.as_str()],
            |r| r.get(0),
        )?;
        if !has_capability {
            return Err(ProductError::Invariant(
                "scoped evidence capability unavailable",
            ));
        }

        let mut order_stmt = tx.prepare(
            "SELECT DISTINCT p.order_id FROM product_scope_order_provenance_v1 p
             JOIN product_scope_grants_v1 g ON g.source_instance_id=p.source_instance_id
                 AND g.subject_id=?1 AND g.capability=?2 AND g.enabled=1
             JOIN product_transition_receipts_v1 r ON r.order_id=p.order_id
                 AND r.lifecycle=p.lifecycle AND r.seq=p.admission_seq
                 AND r.event_hash=p.admission_hash AND r.operation='github-admission'
             WHERE p.verified=1
             ORDER BY p.order_id LIMIT ?3",
        )?;
        let visible_orders = order_stmt
            .query_map(
                params![principal.actor_id, capability.as_str(), (limit + 1) as i64],
                |r| r.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(order_stmt);
        let has_more = visible_orders.len() > limit;
        // Validate all S1 rows in this read snapshot without retaining the
        // whole journal. The seq scan checks the exact existing v1/v2 hash
        // implementation and global sequence continuity; a second indexed,
        // order-grouped scan reuses replay's event decoder and domain
        // transition implementation while retaining one order state.
        let scan_stats = verify_scoped_s1(&tx)?
            .ok_or(ProductError::Invariant("product S1 verification failed"))?;
        debug_assert!(scan_stats.peak_retained_rows <= 1);
        debug_assert!(scan_stats.peak_order_states <= 1);

        // Only selected, explicitly granted, lifecycle-proven rows are
        // materialized for the pseudonymizer. Visible output can grow with
        // the selected orders' history; verifier auxiliary memory does not
        // grow with foreign order count.
        let selected_orders_sql =
            "SELECT DISTINCT q.order_id FROM product_scope_order_provenance_v1 q
             JOIN product_scope_grants_v1 qg ON qg.source_instance_id=q.source_instance_id
                 AND qg.subject_id=?1 AND qg.capability=?2 AND qg.enabled=1
             JOIN product_transition_receipts_v1 qa ON qa.seq=q.admission_seq
                 AND qa.event_hash=q.admission_hash AND qa.order_id=q.order_id
                 AND qa.lifecycle=q.lifecycle AND qa.operation='github-admission'
             WHERE q.verified=1 ORDER BY q.order_id LIMIT ?3";
        let selected_total: i64 = tx.query_row(
            &format!("SELECT COUNT(*) FROM ledger l JOIN ({selected_orders_sql}) s ON s.order_id=l.order_id"),
            params![principal.actor_id, capability.as_str(), limit as i64],
            |r| r.get(0),
        )?;
        let mut evidence_stmt = tx.prepare(&format!(
            "SELECT DISTINCT l.seq,l.order_id,l.from_state,l.to_state,l.event,l.actor,l.intent,l.at,l.prev_hash,l.hash,l.event_json
             FROM ledger l
             JOIN product_transition_receipts_v1 r ON r.seq=l.seq AND r.event_hash=l.hash AND r.order_id=l.order_id
             JOIN product_scope_order_provenance_v1 p ON p.order_id=r.order_id AND p.lifecycle=r.lifecycle AND p.verified=1
             JOIN product_transition_receipts_v1 admission ON admission.seq=p.admission_seq
                 AND admission.event_hash=p.admission_hash AND admission.order_id=p.order_id
                 AND admission.lifecycle=p.lifecycle AND admission.operation='github-admission'
             JOIN product_scope_grants_v1 g ON g.source_instance_id=p.source_instance_id
                 AND g.subject_id=?1 AND g.capability=?2 AND g.enabled=1
             JOIN ({selected_orders_sql}) selected ON selected.order_id=p.order_id
             ORDER BY l.seq"
        ))?;
        let rows = evidence_stmt
            .query_map(
                params![principal.actor_id, capability.as_str(), limit as i64],
                ledger_row_from_sql,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(evidence_stmt);
        let coverage_complete = rows.len() as i64 == selected_total;
        let (audit_sources, event_scope, casting_journal_present) =
            crate::product_scope::scoped_audit_sources_in(&tx, &principal.actor_id)?;
        tx.commit()?;
        Ok(ScopedEvidenceSnapshot {
            rows,
            has_more,
            coverage_complete,
            audit_sources,
            event_scope,
            casting_journal_present,
        })
    }

    /// Local bootstrap primitive. The caller performs the KDF before entering
    /// this write transaction; a second bootstrap can never replace an owner.
    pub fn bootstrap_owner(
        &mut self,
        actor_id: &str,
        argon2id_phc: &str,
    ) -> Result<(), ProductError> {
        // Compatibility bootstrap retained for isolated pre-scope library
        // fixtures. Product CLI/HTTP setup uses bootstrap_owner_scoped, which
        // always activates enforced policy, including an empty grant set.
        self.bootstrap_owner_with_mode(actor_id, argon2id_phc, &[], &[], false)
    }

    /// Fresh-install bootstrap with an explicit finite source and data-grant
    /// manifest. It is the only path that may give the initial Owner data
    /// access without a second Owner approving that scope.
    pub fn bootstrap_owner_scoped(
        &mut self,
        actor_id: &str,
        argon2id_phc: &str,
        sources: &[ScopeSourceManifest],
        grants: &[ScopeGrantManifest],
    ) -> Result<(), ProductError> {
        self.bootstrap_owner_with_mode(actor_id, argon2id_phc, sources, grants, true)
    }

    fn bootstrap_owner_with_mode(
        &mut self,
        actor_id: &str,
        argon2id_phc: &str,
        sources: &[ScopeSourceManifest],
        grants: &[ScopeGrantManifest],
        enforce_scope: bool,
    ) -> Result<(), ProductError> {
        if actor_id.trim().is_empty()
            || !crate::product_identity::ProductPasswordKdf::validate_phc(argon2id_phc)
        {
            return Err(ProductError::InvalidConfig);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let owners: i64 = tx.query_row(
            "SELECT count(*) FROM product_accounts_v1 WHERE role='owner' AND enabled=1",
            [],
            |r| r.get(0),
        )?;
        if owners != 0 {
            return Err(ProductError::Invariant(
                "product owner already bootstrapped",
            ));
        }
        tx.execute("INSERT INTO product_accounts_v1(actor_id,role,enabled,revision) VALUES(?1,'owner',1,1)", [actor_id])?;
        tx.execute(
            "INSERT INTO product_credentials_v1(actor_id,argon2id_phc,revision) VALUES(?1,?2,1)",
            params![actor_id, argon2id_phc],
        )?;
        if sources.is_empty() && !grants.is_empty() {
            return Err(ProductError::Invariant(
                "bootstrap grants require explicit source instances",
            ));
        }
        let mut active_repositories = std::collections::BTreeSet::new();
        let mut source_ids = std::collections::BTreeSet::new();
        for source in sources {
            validate_scope_source(source)?;
            if source.connector_id.trim().is_empty()
                || source.generation != 1
                || !source_ids.insert(source.source_instance_id.as_str())
                || (source.enabled && !active_repositories.insert(source.identity.key()))
            {
                return Err(ProductError::InvalidConfig);
            }
            let identity = canonical_scope_identity(&source.identity)?;
            insert_scope_source(&tx, source, &identity)?;
            tx.execute(
                "INSERT INTO product_scope_current_sources_v1(connector_id,source_instance_id) VALUES(?1,?2)",
                params![source.connector_id,source.source_instance_id],
            )?;
            tx.execute(
                "INSERT INTO product_sources_v1(connector_id,provider,origin,repository_id,scope,revision,enabled) VALUES(?1,?2,?3,?4,?5,1,?6)",
                params![source.connector_id,identity.provider,identity.api_origin,identity.stable_repository_id,source.scope_query,i64::from(source.enabled)],
            )?;
        }
        for grant in grants {
            if grant.actor_id != actor_id
                || !source_ids.contains(grant.source_instance_id.as_str())
                || grant.capabilities.is_empty()
            {
                return Err(ProductError::Invariant(
                    "bootstrap manifest grants must explicitly name the initial Owner and configured source",
                ));
            }
            let mut capabilities = std::collections::BTreeSet::new();
            for capability in &grant.capabilities {
                if !capability.allowed_for_role("owner") || !capabilities.insert(*capability) {
                    return Err(ProductError::InvalidConfig);
                }
                insert_initial_scope_grant(
                    &tx,
                    actor_id,
                    &grant.source_instance_id,
                    *capability,
                    actor_id,
                    &format!(
                        "initial-bootstrap:{}:{}",
                        grant.source_instance_id,
                        capability.as_str()
                    ),
                )?;
            }
        }
        if enforce_scope {
            // An explicitly empty bootstrap still activates deny-by-default
            // scope; compatibility mode is only for existing isolated fixtures.
            tx.execute(
                "UPDATE product_scope_policy_state_v1 SET mode='enforced' WHERE singleton=1",
                [],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Local recovery primitive. The caller must perform the KDF before this
    /// method; incrementing both revisions invalidates all existing sessions.
    /// There is intentionally no HTTP handler for this operation.
    pub fn reset_credentials_local(
        &mut self,
        actor_id: &str,
        argon2id_phc: &str,
    ) -> Result<(), ProductError> {
        if actor_id.trim().is_empty()
            || !crate::product_identity::ProductPasswordKdf::validate_phc(argon2id_phc)
        {
            return Err(ProductError::InvalidConfig);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE product_credentials_v1 SET argon2id_phc=?1,revision=revision+1 WHERE actor_id=?2 AND EXISTS(SELECT 1 FROM product_accounts_v1 WHERE actor_id=?2)",
            params![argon2id_phc, actor_id],
        )?;
        if changed != 1 {
            return Err(ProductError::Invariant("account unavailable"));
        }
        tx.execute(
            "UPDATE product_accounts_v1 SET revision=revision+1 WHERE actor_id=?1",
            [actor_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Change a product account's live authority. The revision increment
    /// invalidates every previously issued session in the same SQLite commit.
    /// The final enabled Owner is protected under the write lock.
    pub fn set_account_authority(
        &mut self,
        session_digest: &str,
        target_actor: &str,
        role: &str,
        enabled: bool,
        now: DateTime<Utc>,
    ) -> Result<(), ProductError> {
        if !matches!(role, "owner" | "approver" | "operator" | "viewer") {
            return Err(ProductError::InvalidConfig);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let principal = resolve_session_in(&tx, session_digest, now)?;
        if principal.role != "owner" {
            return Err(ProductError::Invariant(
                "current role cannot administer accounts",
            ));
        }
        let current: Option<(String, i64, i64)> = tx
            .query_row(
                "SELECT role,enabled,revision FROM product_accounts_v1 WHERE actor_id=?1",
                [target_actor],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((old_role, old_enabled, old_revision)) = current else {
            return Err(ProductError::Invariant("account unavailable"));
        };
        if old_role == "owner" && old_enabled == 1 && (!enabled || role != "owner") {
            let others: i64 = tx.query_row(
                "SELECT count(*) FROM product_accounts_v1 WHERE role='owner' AND enabled=1 AND actor_id<>?1",
                [target_actor], |r| r.get(0),
            )?;
            if others == 0 {
                return Err(ProductError::Invariant(
                    "cannot remove the last enabled Owner",
                ));
            }
        }
        if (old_role != role || old_enabled != i64::from(enabled)) && scope_enforced_in(&tx)? {
            tx.execute(
                "UPDATE product_grants_v1 SET state='invalidated' WHERE state IN ('issued','reserved') AND actor_id=?1",
                [target_actor],
            )?;
            tx.execute(
                "UPDATE product_operations_v1 SET phase='fenced' WHERE phase='prepared' AND actor_id=?1",
                [target_actor],
            )?;
            let mut grants = tx.prepare(
                "SELECT source_instance_id,capability,grant_revision FROM product_scope_grants_v1 WHERE subject_id=?1 AND enabled=1 ORDER BY source_instance_id,capability",
            )?;
            let rows = grants
                .query_map([target_actor], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(grants);
            for (source_id, capability, grant_revision) in rows {
                let scope_revision: i64 = tx
                    .query_row(
                        "SELECT revision FROM product_scope_subject_revisions_v1 WHERE subject_id=?1 AND source_instance_id=?2",
                        params![target_actor,source_id],
                        |r| r.get(0),
                    )
                    .optional()?
                    .unwrap_or(0)
                    + 1;
                let next_grant_revision = grant_revision + 1;
                let request_key =
                    format!("account-authority:{}:{}", target_actor, old_revision + 1);
                let digest = crate::product_scope::framed_hash(
                    "foundry-product-account-scope-revocation-v1",
                    &[
                        &principal.actor_id,
                        target_actor,
                        role,
                        if enabled { "enabled" } else { "disabled" },
                        &source_id,
                        &capability,
                    ],
                );
                tx.execute(
                    "UPDATE product_scope_subject_revisions_v1 SET revision=?1 WHERE subject_id=?2 AND source_instance_id=?3",
                    params![scope_revision,target_actor,source_id],
                )?;
                tx.execute(
                    "UPDATE product_scope_grants_v1 SET enabled=0,grant_revision=?1 WHERE subject_id=?2 AND source_instance_id=?3 AND capability=?4 AND enabled=1",
                    params![next_grant_revision,target_actor,source_id,capability],
                )?;
                tx.execute(
                    "INSERT INTO product_scope_events_v1(actor_id,subject_id,source_instance_id,capability,operation,scope_revision,grant_revision,request_key,request_digest,created_at) VALUES(?1,?2,?3,?4,'revoke',?5,?6,?7,?8,?9)",
                    params![principal.actor_id,target_actor,source_id,capability,scope_revision,next_grant_revision,request_key,digest,now.to_rfc3339()],
                )?;
            }
        }
        tx.execute("UPDATE product_accounts_v1 SET role=?1,enabled=?2,revision=revision+1 WHERE actor_id=?3", params![role, i64::from(enabled), target_actor])?;
        tx.commit()?;
        Ok(())
    }

    /// Add an account under the currently authenticated Owner's authority.
    /// Password KDF work is performed by the caller before taking this write
    /// transaction; the PHC parameters are revalidated here.
    pub fn create_account(
        &mut self,
        session_digest: &str,
        actor_id: &str,
        role: &str,
        argon2id_phc: &str,
        now: DateTime<Utc>,
    ) -> Result<(), ProductError> {
        if actor_id.trim().is_empty()
            || actor_id.len() > 256
            || !matches!(role, "owner" | "approver" | "operator" | "viewer")
            || !crate::product_identity::ProductPasswordKdf::validate_phc(argon2id_phc)
        {
            return Err(ProductError::InvalidConfig);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let principal = resolve_session_in(&tx, session_digest, now)?;
        if principal.role != "owner" {
            return Err(ProductError::Invariant(
                "current role cannot administer accounts",
            ));
        }
        tx.execute(
            "INSERT INTO product_accounts_v1(actor_id,role,enabled,revision) VALUES(?1,?2,1,1)",
            params![actor_id, role],
        )?;
        tx.execute(
            "INSERT INTO product_credentials_v1(actor_id,argon2id_phc,revision) VALUES(?1,?2,1)",
            params![actor_id, argon2id_phc],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Disable or rebind a configured connector in a single DB transaction.
    /// Approval rechecks its revision and enabled bit under the same lock.
    pub fn configure_source(
        &mut self,
        session_digest: &str,
        connector: &str,
        provider: &str,
        origin: &str,
        repository_id: &str,
        scope: &str,
        enabled: bool,
        now: DateTime<Utc>,
    ) -> Result<i64, ProductError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let principal = resolve_session_in(&tx, session_digest, now)?;
        if scope_enforced_in(&tx)? {
            return Err(ProductError::Invariant(
                "legacy source mutation is unavailable under scoped policy",
            ));
        }
        if principal.role != "owner" {
            return Err(ProductError::Invariant(
                "current role cannot administer sources",
            ));
        }
        let n = tx.execute("UPDATE product_sources_v1 SET provider=?1,origin=?2,repository_id=?3,scope=?4,revision=revision+1,enabled=?5 WHERE connector_id=?6", params![provider,origin,repository_id,scope,i64::from(enabled),connector])?;
        if n != 1 {
            return Err(ProductError::Invariant("connector unavailable"));
        }
        let revision = tx.query_row(
            "SELECT revision FROM product_sources_v1 WHERE connector_id=?1",
            [connector],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(revision)
    }

    /// Fence all outstanding prepared approvals during explicit recovery.
    /// This never approves anything; the next authenticated retry must prepare
    /// a new five minute reservation.
    pub fn fence_prepared_approvals(&mut self) -> Result<i64, ProductError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let epoch: i64 = tx.query_row(
            "UPDATE product_meta_v1 SET recovery_epoch=recovery_epoch+1 WHERE singleton=1 RETURNING recovery_epoch",
            [], |r| r.get(0),
        )?;
        tx.execute("UPDATE product_grants_v1 SET state='issued',fence=0 WHERE state='reserved' AND grant_id IN (SELECT grant_id FROM product_operations_v1 WHERE phase='prepared' AND grant_id IS NOT NULL)", [])?;
        tx.execute(
            "UPDATE product_operations_v1 SET phase='fenced' WHERE phase='prepared'",
            [],
        )?;
        tx.commit()?;
        Ok(epoch)
    }

    /// KDF verification occurs before the write lock. The credential revision
    /// is checked again while the random session is inserted.
    pub fn login(
        &mut self,
        actor_id: &str,
        password: &str,
        now: DateTime<Utc>,
    ) -> Result<ProductSession, ProductError> {
        // The throttle is keyed by the normalized installation-local account
        // identifier and is durable across process restarts. It is checked
        // before the expensive KDF and updated atomically after each failure.
        let actor_key = actor_id.trim().to_lowercase();
        if actor_key.is_empty() || actor_key.len() > 256 {
            return Err(ProductError::Invariant("credentials invalid"));
        }
        self.prune_login_throttle(now)?;
        let blocked_until: Option<String> = self
            .conn
            .query_row(
                "SELECT blocked_until FROM product_login_throttle_v1 WHERE actor_id=?1",
                [&actor_key],
                |r| r.get(0),
            )
            .optional()?;
        if blocked_until.is_none() {
            let count: i64 =
                self.conn
                    .query_row("SELECT count(*) FROM product_login_throttle_v1", [], |r| {
                        r.get(0)
                    })?;
            if count >= 1024 {
                return Err(ProductError::Invariant("credentials invalid"));
            }
        }
        if let Some(value) = blocked_until {
            let until = DateTime::parse_from_rfc3339(&value)
                .map_err(|_| ProductError::Invariant("login throttle state invalid"))?
                .with_timezone(&Utc);
            if now < until {
                return Err(ProductError::Invariant("credentials invalid"));
            }
        }
        let credentials: Option<(String, i64)> = self.conn.query_row(
            "SELECT c.argon2id_phc,c.revision FROM product_credentials_v1 c JOIN product_accounts_v1 a ON a.actor_id=c.actor_id WHERE c.actor_id=?1 AND a.enabled=1",
            [actor_id], |r| Ok((r.get(0)?,r.get(1)?)),
        ).optional()?;
        let Some((phc, credential_revision)) = credentials else {
            self.record_login_failure(&actor_key, now)?;
            return Err(ProductError::Invariant("credentials invalid"));
        };
        let verified = crate::product_identity::ProductPasswordKdf::verify(password, &phc)
            .map_err(|_| ProductError::Invariant("credentials invalid"))?;
        if !verified {
            self.record_login_failure(&actor_key, now)?;
            return Err(ProductError::Invariant("credentials invalid"));
        }
        use rand_core::RngCore as _;
        let mut bytes = [0u8; 32];
        rand_core::OsRng
            .try_fill_bytes(&mut bytes)
            .map_err(|_| ProductError::Invariant("secure session generation failed"))?;
        let bearer = hex::encode(bytes);
        let session_digest = sha256_hex(bearer.as_bytes());
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<(i64,i64)> = tx.query_row(
            "SELECT a.revision,c.revision FROM product_accounts_v1 a JOIN product_credentials_v1 c ON c.actor_id=a.actor_id WHERE a.actor_id=?1 AND a.enabled=1",
            [actor_id], |r| Ok((r.get(0)?,r.get(1)?)),
        ).optional()?;
        let Some((authority_revision, credential_rev)) = current else {
            return Err(ProductError::Invariant(
                "account is disabled or unconfigured",
            ));
        };
        if credential_rev != credential_revision {
            return Err(ProductError::Invariant(
                "credential revision changed during login",
            ));
        }
        tx.execute("INSERT INTO product_sessions_v1(session_digest,actor_id,authority_revision,expires_at,revoked) VALUES(?1,?2,?3,?4,0)", params![session_digest,actor_id,authority_revision,(now+chrono::Duration::hours(8)).to_rfc3339()])?;
        tx.execute(
            "DELETE FROM product_login_throttle_v1 WHERE actor_id=?1",
            [&actor_key],
        )?;
        tx.commit()?;
        Ok(ProductSession {
            bearer,
            digest: session_digest,
        })
    }

    fn record_login_failure(
        &mut self,
        actor_key: &str,
        now: DateTime<Utc>,
    ) -> Result<(), ProductError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row: Option<(String, i64, String)> = tx.query_row(
            "SELECT window_started_at,failures,blocked_until FROM product_login_throttle_v1 WHERE actor_id=?1",
            [actor_key], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        ).optional()?;
        if row.is_none() {
            let count: i64 =
                tx.query_row("SELECT count(*) FROM product_login_throttle_v1", [], |r| {
                    r.get(0)
                })?;
            if count >= 1024 {
                return Err(ProductError::Invariant("credentials invalid"));
            }
        }
        let fresh = now.to_rfc3339();
        let (window, failures, old_block) = match row {
            Some((started, failures, blocked)) => {
                let started_at = DateTime::parse_from_rfc3339(&started)
                    .map_err(|_| ProductError::Invariant("login throttle state invalid"))?
                    .with_timezone(&Utc);
                if now >= started_at + chrono::Duration::minutes(5) {
                    (fresh.clone(), 0, String::new())
                } else {
                    (started, failures, blocked)
                }
            }
            None => (fresh.clone(), 0, String::new()),
        };
        let next = failures + 1;
        let blocked = if next >= 5 {
            let prior = DateTime::parse_from_rfc3339(&old_block)
                .ok()
                .map(|v| v.with_timezone(&Utc));
            let candidate = now + chrono::Duration::minutes(5);
            prior
                .filter(|v| *v > candidate)
                .unwrap_or(candidate)
                .to_rfc3339()
        } else if old_block.is_empty() {
            fresh
        } else {
            old_block
        };
        tx.execute(
            "INSERT INTO product_login_throttle_v1(actor_id,window_started_at,failures,blocked_until) VALUES(?1,?2,?3,?4) ON CONFLICT(actor_id) DO UPDATE SET window_started_at=excluded.window_started_at,failures=excluded.failures,blocked_until=excluded.blocked_until",
            params![actor_key, window, next, blocked],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn prune_login_throttle(&mut self, now: DateTime<Utc>) -> Result<(), ProductError> {
        let mut stmt = self.conn.prepare(
            "SELECT actor_id,window_started_at,blocked_until FROM product_login_throttle_v1",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut expired = Vec::new();
        for row in rows {
            let (actor, started, blocked) = row?;
            let started = DateTime::parse_from_rfc3339(&started)
                .map_err(|_| ProductError::Invariant("login throttle state invalid"))?
                .with_timezone(&Utc);
            let blocked = DateTime::parse_from_rfc3339(&blocked)
                .map_err(|_| ProductError::Invariant("login throttle state invalid"))?
                .with_timezone(&Utc);
            if now >= started + chrono::Duration::minutes(5) && now >= blocked {
                expired.push(actor);
            }
        }
        drop(stmt);
        if !expired.is_empty() {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            for actor in expired {
                tx.execute(
                    "DELETE FROM product_login_throttle_v1 WHERE actor_id=?1",
                    [actor],
                )?;
            }
            tx.commit()?;
        }
        Ok(())
    }

    /// Resolve current authority under the caller's transaction boundary by
    /// joining the live account revision; stale or expired sessions fail closed.
    pub fn resolve_session(
        &self,
        session_digest: &str,
        now: DateTime<Utc>,
    ) -> Result<ProductPrincipal, ProductError> {
        if !is_sha256_hex(session_digest) {
            return Err(ProductError::Invariant("session invalid"));
        }
        let row: Option<(String,String,i64,i64,String,i64)> = self.conn.query_row(
            "SELECT s.actor_id,a.role,s.authority_revision,a.revision,s.expires_at,s.revoked FROM product_sessions_v1 s JOIN product_accounts_v1 a ON a.actor_id=s.actor_id WHERE s.session_digest=?1 AND a.enabled=1",
            [session_digest], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
        ).optional()?;
        let Some((actor_id, role, session_revision, authority_revision, expires_at, revoked)) = row
        else {
            return Err(ProductError::Invariant("session invalid"));
        };
        let expires_at = DateTime::parse_from_rfc3339(&expires_at)
            .map_err(|_| ProductError::Invariant("session expiry invalid"))?
            .with_timezone(&Utc);
        if revoked != 0 || session_revision != authority_revision || now >= expires_at {
            return Err(ProductError::Invariant(
                "session expired, revoked, or stale",
            ));
        }
        Ok(ProductPrincipal {
            actor_id,
            role,
            authority_revision,
        })
    }

    /// List active product review holds visible to a current Owner or Approver.
    /// The immutable runner snapshot remains restricted to the storage owner.
    pub fn list_product_review_holds(
        &mut self,
        session_digest: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<ProductReviewHoldView>, ProductError> {
        let tx = self.conn.transaction()?;
        let principal = resolve_session_in(&tx, session_digest, now)?;
        if principal.role != "owner" && principal.role != "approver" {
            return Err(ProductError::Invariant(
                "current role cannot read review holds",
            ));
        }
        let scoped = scope_enforced_in(&tx)?;
        let query = if scoped {
            "SELECT h.order_id,h.hold_instance_id,h.lifecycle,h.attempt,h.originating_run,h.activation,h.host_epoch,h.hold_revision,h.source_revision,h.expected_predecessor,h.snapshot_digest,h.allowed_targets_json,h.deadline
             FROM product_review_holds_v1 h
             JOIN product_orders_v1 o ON o.order_id=h.order_id AND o.lifecycle=h.lifecycle
             JOIN product_scope_order_provenance_v1 p ON p.order_id=o.order_id AND p.lifecycle=o.lifecycle AND p.verified=1
             WHERE h.state='active' AND EXISTS(SELECT 1 FROM product_scope_grants_v1 g WHERE g.subject_id=?1 AND g.source_instance_id=p.source_instance_id AND g.capability='read_review' AND g.enabled=1)
             ORDER BY h.order_id,h.lifecycle,h.attempt,h.originating_run,h.hold_instance_id"
        } else {
            "SELECT h.order_id,h.hold_instance_id,h.lifecycle,h.attempt,h.originating_run,h.activation,h.host_epoch,h.hold_revision,h.source_revision,h.expected_predecessor,h.snapshot_digest,h.allowed_targets_json,h.deadline
             FROM product_review_holds_v1 h
             JOIN product_orders_v1 o ON o.order_id=h.order_id AND o.lifecycle=h.lifecycle
             JOIN product_sources_v1 s ON s.connector_id=o.connector_id AND s.repository_id=o.repository_id
             WHERE h.state='active' AND s.enabled=1 AND s.revision=h.source_revision
             ORDER BY h.order_id,h.lifecycle,h.attempt,h.originating_run,h.hold_instance_id"
        };
        let mut stmt = tx.prepare(query)?;
        let mut map_hold = |r: &rusqlite::Row<'_>| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                ProductHoldExpectation {
                    lifecycle: r.get(2)?,
                    attempt: r.get(3)?,
                    originating_run: r.get(4)?,
                    activation: r.get(5)?,
                    host_epoch: r.get(6)?,
                    hold_revision: r.get(7)?,
                    source_revision: r.get(8)?,
                    predecessor_seq: r.get(9)?,
                    snapshot_digest: r.get(10)?,
                },
                r.get::<_, String>(11)?,
                r.get::<_, String>(12)?,
            ))
        };
        let rows = if scoped {
            stmt.query_map([principal.actor_id.as_str()], &mut map_hold)?
        } else {
            stmt.query_map([], &mut map_hold)?
        };
        let mut holds = Vec::new();
        for row in rows {
            let (order_id, hold_instance_id, expectation, targets, deadline) = row?;
            let allowed_targets: Vec<String> = serde_json::from_str(&targets)
                .map_err(|_| ProductError::Invariant("stored hold targets are corrupt"))?;
            holds.push(ProductReviewHoldView {
                order_id,
                hold_instance_id,
                expectation,
                allowed_targets,
                deadline,
            });
        }
        drop(stmt);
        tx.commit()?;
        Ok(holds)
    }

    /// Return progress only to the actor whose authenticated session made the
    /// immutable decision. The caller may display the receipt but cannot use
    /// the status to restart ambiguous work.
    pub fn product_review_progress(
        &mut self,
        session_digest: &str,
        operation_id: &str,
        now: DateTime<Utc>,
    ) -> Result<ProductReviewProgress, ProductError> {
        let tx = self.conn.transaction()?;
        let principal = resolve_session_in(&tx, session_digest, now)?;
        if principal.role != "owner" && principal.role != "approver" {
            return Err(ProductError::Invariant(
                "current role cannot read review progress",
            ));
        }
        let (receipt_json, phase, revision): (String, String, i64) = tx.query_row(
            "SELECT a.receipt_json,p.phase,p.progress_revision
             FROM product_review_hold_acceptances_v1 a
             JOIN product_review_hold_progress_v1 p ON p.operation_id=a.operation_id
             WHERE a.operation_id=?1",
            [operation_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let receipt: ProductHoldAcceptance = serde_json::from_str(&receipt_json)
            .map_err(|_| ProductError::Invariant("stored review receipt is corrupt"))?;
        if receipt.actor_id != principal.actor_id {
            return Err(ProductError::Invariant(
                "review progress belongs to another actor",
            ));
        }
        if scope_enforced_in(&tx)? {
            require_order_capability_in(
                &tx,
                &principal,
                &receipt.order_id,
                ProductCapability::ReadReview,
                false,
            )?;
        }
        tx.commit()?;
        Ok(ProductReviewProgress {
            recovery_required: phase == "recovery_required",
            receipt,
            phase,
            progress_revision: revision,
        })
    }

    /// Atomically accept a human decision for one exact hold snapshot already
    /// recorded by the product review storage adapter. This commits authority
    /// revalidation, the hold CAS, immutable receipt, progress row and one
    /// followup-run reservation together. It deliberately does not run or
    /// redispatch the review engine.
    pub fn accept_product_review_hold(
        &mut self,
        session_digest: &str,
        order_id: &str,
        hold_instance_id: &str,
        expected: &ProductHoldExpectation,
        request_key: &str,
        decision: ProductHoldDecision,
        now: DateTime<Utc>,
    ) -> Result<ProductHoldAcceptance, ProductError> {
        if order_id.is_empty()
            || hold_instance_id.is_empty()
            || request_key.trim().is_empty()
            || request_key.len() > 256
        {
            return Err(ProductError::InvalidConfig);
        }
        if let ProductHoldDecision::Resume(target) = &decision {
            if target.trim().is_empty() || target.len() > 256 {
                return Err(ProductError::InvalidConfig);
            }
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let principal = resolve_session_in(&tx, session_digest, now)?;
        if principal.role != "owner" && principal.role != "approver" {
            return Err(ProductError::Invariant(
                "current role cannot resolve review hold",
            ));
        }
        let scoped_authority = if scope_enforced_in(&tx)? {
            Some(require_order_capability_in(
                &tx,
                &principal,
                order_id,
                ProductCapability::ResolveReviewHold,
                true,
            )?)
        } else {
            None
        };

        // Resolve the hold only through its immutable product binding. The
        // ProductStore-backed frozen-pack host writes this evidence through
        // the same private store; callers cannot submit a replacement snapshot.
        let hold: Option<(i64,i64,i64,i64,i64,String,String,i64,i64,String,i64,String,String,String,i64,String,i64)> = tx.query_row(
            "SELECT h.lifecycle,h.attempt,h.originating_run,h.activation,h.host_epoch,h.hold_instance_id,h.snapshot_digest,h.hold_revision,h.expected_predecessor,h.snapshot_json,h.source_revision,h.deadline,h.allowed_targets_json,h.state,h.admission_seq,h.admission_hash,o.source_revision FROM product_review_holds_v1 h JOIN product_orders_v1 o ON o.order_id=h.order_id AND o.lifecycle=h.lifecycle WHERE h.order_id=?1 AND h.hold_instance_id=?2",
            params![order_id,hold_instance_id],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get(10)?,r.get(11)?,r.get(12)?,r.get(13)?,r.get(14)?,r.get(15)?,r.get(16)?)),
        ).optional()?;
        let Some((
            lifecycle,
            attempt,
            originating_run,
            activation,
            host_epoch,
            stored_hold_id,
            snapshot_digest,
            hold_revision,
            expected_predecessor,
            snapshot_json,
            hold_source_revision,
            deadline,
            allowed_targets_json,
            hold_state,
            admission_seq,
            admission_hash,
            order_source_revision,
        )) = hold
        else {
            return Err(ProductError::Invariant("product review hold unavailable"));
        };
        if expected.lifecycle != lifecycle
            || expected.attempt != attempt
            || expected.originating_run != originating_run
            || expected.activation != activation
            || expected.host_epoch != host_epoch
            || expected.hold_revision != hold_revision
            || expected.source_revision != hold_source_revision
            || expected.predecessor_seq != expected_predecessor
            || expected.snapshot_digest != snapshot_digest
        {
            return Err(ProductError::Invariant(
                "stale product review hold expectation",
            ));
        }
        if stored_hold_id != hold_instance_id || hold_state != "active" {
            // An exact replay is handled below, but a changed/current hold is
            // never allowed to create a second acceptance.
            let current_source: Option<(String,String,i64,i64)> = tx.query_row(
                "SELECT o.repository_id,s.repository_id,s.revision,s.enabled FROM product_orders_v1 o JOIN product_sources_v1 s ON s.connector_id=o.connector_id WHERE o.order_id=?1 AND o.lifecycle=?2",
                params![order_id,lifecycle], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
            ).optional()?;
            let Some((order_repo, source_repo, source_revision, enabled)) = current_source else {
                return Err(ProductError::Invariant("product review source unavailable"));
            };
            if enabled != 1
                || order_repo != source_repo
                || source_revision != order_source_revision
                || source_revision != hold_source_revision
            {
                return Err(ProductError::Invariant("product review source changed"));
            }
            let request_digest = product_hold_request_digest(
                &self.installation_id,
                &principal.actor_id,
                principal.authority_revision,
                request_key,
                order_id,
                lifecycle,
                hold_instance_id,
                hold_revision,
                attempt,
                originating_run,
                activation,
                host_epoch,
                source_revision,
                expected_predecessor,
                &snapshot_digest,
                &decision,
                scoped_authority
                    .as_ref()
                    .map(|v| v.source_instance_id.as_str())
                    .unwrap_or(""),
                scoped_authority
                    .as_ref()
                    .map(|v| v.scope_revision)
                    .unwrap_or(-1),
                scoped_authority
                    .as_ref()
                    .map(|v| v.grant_revision)
                    .unwrap_or(-1),
            );
            let old: Option<(String,String)> = tx.query_row(
                "SELECT request_digest,receipt_json FROM product_review_hold_acceptances_v1 WHERE actor_id=?1 AND request_key=?2",
                params![principal.actor_id,request_key], |r| Ok((r.get(0)?,r.get(1)?)),
            ).optional()?;
            if let Some((old_digest, receipt_json)) = old {
                if old_digest != request_digest {
                    return Err(ProductError::Invariant(
                        "review hold idempotency binding conflict",
                    ));
                }
                let receipt: ProductHoldAcceptance = serde_json::from_str(&receipt_json)
                    .map_err(|_| ProductError::Invariant("stored review receipt is corrupt"))?;
                if receipt.operation_id
                    != product_hold_operation_id(
                        &self.installation_id,
                        &principal.actor_id,
                        request_key,
                    )
                    || receipt.actor_id != principal.actor_id
                    || receipt.order_id != order_id
                    || receipt.lifecycle != lifecycle
                    || receipt.hold_instance_id != hold_instance_id
                    || receipt.hold_revision != hold_revision
                    || receipt.attempt != attempt
                    || receipt.originating_run != originating_run
                    || receipt.activation != activation
                    || receipt.host_epoch != host_epoch
                    || receipt.source_revision != source_revision
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
                    || receipt.predecessor_seq != expected_predecessor
                    || receipt.decision != decision
                {
                    return Err(ProductError::Invariant(
                        "stored review receipt binding is invalid",
                    ));
                }
                tx.commit()?;
                return Ok(receipt);
            }
            return Err(ProductError::Invariant(
                "product review hold already accepted or inactive",
            ));
        }
        if order_source_revision != hold_source_revision {
            return Err(ProductError::Invariant(
                "product review source revision changed",
            ));
        }
        let (source_connector,source_repo,source_revision,source_enabled): (String,String,i64,i64) = tx.query_row(
            "SELECT o.connector_id,o.repository_id,s.revision,s.enabled FROM product_orders_v1 o JOIN product_sources_v1 s ON s.connector_id=o.connector_id WHERE o.order_id=?1 AND o.lifecycle=?2 AND s.repository_id=o.repository_id",
            params![order_id,lifecycle], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
        ).optional()?.ok_or(ProductError::Invariant("product review source binding unavailable"))?;
        let _ = (source_connector, source_repo);
        if source_enabled != 1 || source_revision != hold_source_revision {
            return Err(ProductError::Invariant(
                "product review source is disabled or stale",
            ));
        }
        let (predecessor, current_lifecycle, current_state) =
            verified_order_state_in(&tx, order_id)?;
        if i64::from(current_lifecycle) + 1 != lifecycle
            || (current_state != "waiting" && !a01_hold_review_in(&tx,order_id,&current_state)?)
            || predecessor != expected_predecessor
        {
            return Err(ProductError::Invariant(
                "product review S1 lifecycle is not held",
            ));
        }
        let admission_is_exact: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM product_transition_receipts_v1 r JOIN product_orders_v1 o ON o.order_id=r.order_id WHERE r.seq=?1 AND r.event_hash=?2 AND r.order_id=?3 AND r.lifecycle=?4 AND r.operation='github-admission' AND o.admission_seq=r.seq AND o.admission_hash=r.event_hash)",
            params![admission_seq,admission_hash,order_id,lifecycle], |r| r.get(0),
        )?;
        if !admission_is_exact {
            return Err(ProductError::Invariant(
                "product review admission anchor is invalid",
            ));
        }
        if !is_sha256_hex(&snapshot_digest)
            || product_review_snapshot_digest(&snapshot_json) != snapshot_digest
        {
            return Err(ProductError::Invariant(
                "product review snapshot digest is invalid",
            ));
        }
        if !deadline.is_empty() {
            let deadline = DateTime::parse_from_rfc3339(&deadline)
                .map_err(|_| ProductError::Invariant("product review deadline is invalid"))?
                .with_timezone(&Utc);
            if now >= deadline {
                return Err(ProductError::Invariant("product review hold expired"));
            }
        }
        if let ProductHoldDecision::Resume(target) = &decision {
            let allowed: Vec<String> = serde_json::from_str(&allowed_targets_json)
                .map_err(|_| ProductError::Invariant("product review target list is corrupt"))?;
            if !allowed.iter().any(|item| item == target) {
                return Err(ProductError::Invariant(
                    "product review target is not allowed",
                ));
            }
        }
        let request_digest = product_hold_request_digest(
            &self.installation_id,
            &principal.actor_id,
            principal.authority_revision,
            request_key,
            order_id,
            lifecycle,
            hold_instance_id,
            hold_revision,
            attempt,
            originating_run,
            activation,
            host_epoch,
            source_revision,
            expected_predecessor,
            &snapshot_digest,
            &decision,
            scoped_authority
                .as_ref()
                .map(|v| v.source_instance_id.as_str())
                .unwrap_or(""),
            scoped_authority
                .as_ref()
                .map(|v| v.scope_revision)
                .unwrap_or(-1),
            scoped_authority
                .as_ref()
                .map(|v| v.grant_revision)
                .unwrap_or(-1),
        );
        let operation_id =
            product_hold_operation_id(&self.installation_id, &principal.actor_id, request_key);
        if let Some((old_digest,receipt_json)) = tx.query_row(
            "SELECT request_digest,receipt_json FROM product_review_hold_acceptances_v1 WHERE actor_id=?1 AND request_key=?2",
            params![principal.actor_id,request_key], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)),
        ).optional()? {
            if old_digest != request_digest { return Err(ProductError::Invariant("review hold idempotency binding conflict")); }
            let receipt = serde_json::from_str(&receipt_json).map_err(|_| ProductError::Invariant("stored review receipt is corrupt"))?;
            let receipt: ProductHoldAcceptance = receipt;
            if receipt.operation_id != operation_id || receipt.actor_id != principal.actor_id || receipt.order_id != order_id
                || receipt.lifecycle != lifecycle || receipt.hold_instance_id != hold_instance_id
                || receipt.hold_revision != hold_revision || receipt.attempt != attempt
                || receipt.originating_run != originating_run || receipt.activation != activation
                || receipt.host_epoch != host_epoch || receipt.source_revision != source_revision
                || receipt.authority_revision != principal.authority_revision
                || receipt.source_instance_id != scoped_authority.as_ref().map(|v|v.source_instance_id.clone()).unwrap_or_default()
                || receipt.scope_revision != scoped_authority.as_ref().map(|v|v.scope_revision).unwrap_or(-1)
                || receipt.grant_revision != scoped_authority.as_ref().map(|v|v.grant_revision).unwrap_or(-1)
                || receipt.predecessor_seq != expected_predecessor || receipt.decision != decision
            { return Err(ProductError::Invariant("stored review receipt binding is invalid")); }
            tx.commit()?;
            return Ok(receipt);
        }
        tx.execute("UPDATE product_review_holds_v1 SET state='accepted' WHERE order_id=?1 AND lifecycle=?2 AND attempt=?3 AND originating_run=?4 AND hold_instance_id=?5 AND hold_revision=?6 AND state='active'",params![order_id,lifecycle,attempt,originating_run,hold_instance_id,hold_revision])?;
        if tx.changes() != 1 {
            return Err(ProductError::Invariant("product review hold CAS lost"));
        }
        #[cfg(test)]
        product_hold_insert_killpoint("after_hold_cas");
        // The executor's run coordinate is per order. Allocate above every
        // observed originating and previously reserved followup run; the
        // SQLite reservation row id is an internal storage key only.
        let reserved_run_id: i64 = tx.query_row(
            "SELECT MAX(run_id)+1 FROM (SELECT originating_run AS run_id FROM product_review_holds_v1 WHERE order_id=?1 UNION ALL SELECT followup_run_id AS run_id FROM product_review_run_reservations_v1 WHERE order_id=?1 AND followup_run_id IS NOT NULL UNION ALL SELECT run_id FROM product_review_host_accept_v1 WHERE order_id=?1)",
            [order_id], |r| r.get::<_, Option<i64>>(0),
        )?.ok_or(ProductError::Invariant("product review run allocation overflow or missing origin"))?;
        tx.execute(
            "INSERT INTO product_review_run_reservations_v1(operation_id,order_id,lifecycle,attempt,originating_run,hold_instance_id,followup_run_id) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![operation_id,order_id,lifecycle,attempt,originating_run,hold_instance_id,reserved_run_id],
        )?;
        #[cfg(test)]
        product_hold_insert_killpoint("after_run_reservation");
        let receipt = ProductHoldAcceptance {
            operation_id: operation_id.clone(),
            actor_id: principal.actor_id.clone(),
            order_id: order_id.to_owned(),
            lifecycle,
            hold_instance_id: hold_instance_id.to_owned(),
            hold_revision,
            attempt,
            originating_run,
            activation,
            host_epoch,
            decision,
            source_revision,
            authority_revision: principal.authority_revision,
            source_instance_id: scoped_authority
                .as_ref()
                .map(|v| v.source_instance_id.clone())
                .unwrap_or_default(),
            scope_revision: scoped_authority
                .as_ref()
                .map(|v| v.scope_revision)
                .unwrap_or(-1),
            grant_revision: scoped_authority
                .as_ref()
                .map(|v| v.grant_revision)
                .unwrap_or(-1),
            predecessor_seq: predecessor,
            reserved_run_id,
            accepted_at: now.to_rfc3339(),
        };
        let receipt_json = serde_json::to_string(&receipt)
            .map_err(|_| ProductError::Invariant("review receipt encoding failed"))?;
        tx.execute("INSERT INTO product_review_hold_acceptances_v1(operation_id,actor_id,request_key,request_digest,receipt_json,order_id,lifecycle,attempt,originating_run,hold_instance_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![operation_id,principal.actor_id,request_key,request_digest,receipt_json,order_id,lifecycle,attempt,originating_run,hold_instance_id])?;
        #[cfg(test)]
        product_hold_insert_killpoint("after_acceptance_insert");
        let epoch: i64 = tx.query_row(
            "SELECT recovery_epoch FROM product_meta_v1 WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        let fence: i64 = tx.query_row(
            "UPDATE product_fence_counter_v1 SET next=next+1 WHERE singleton=1 RETURNING next",
            [],
            |r| r.get(0),
        )?;
        let owner = format!("{}:{epoch}:{fence}", receipt.operation_id);
        tx.execute("INSERT INTO product_review_hold_progress_v1(operation_id,phase,progress_revision,owner_id,epoch,fence,reserved_run_id,recovery_reason) VALUES(?1,'accepted',1,?2,?3,?4,?5,NULL)",params![receipt.operation_id,owner,epoch,fence,reserved_run_id])?;
        #[cfg(test)]
        product_hold_insert_killpoint("after_progress_insert");
        tx.commit()?;
        #[cfg(test)]
        product_hold_insert_killpoint("after_acceptance_commit");
        Ok(receipt)
    }

    /// Revoke one bearer session without exposing or persisting its secret.
    /// A missing, expired, or already revoked session is intentionally
    /// indistinguishable to the caller.
    pub fn revoke_session(
        &mut self,
        session_digest: &str,
        now: DateTime<Utc>,
    ) -> Result<(), ProductError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let principal = resolve_session_in(&tx, session_digest, now)?;
        let n = tx.execute(
            "UPDATE product_sessions_v1 SET revoked=1 WHERE session_digest=?1 AND actor_id=?2 AND revoked=0",
            params![session_digest, principal.actor_id],
        )?;
        if n != 1 {
            return Err(ProductError::Invariant("session invalid"));
        }
        tx.commit()?;
        Ok(())
    }

    /// Persist an approval reservation after checking the current authenticated
    /// principal. Preparation is durable but does not consume the grant/token.
    pub fn prepare_approval(
        &mut self,
        session_digest: &str,
        intent: &ProductApprovalIntent,
    ) -> Result<ProductReservationProof, ProductError> {
        if intent.request_key.is_empty() || !is_sha256_hex(&intent.token_digest) {
            return Err(ProductError::InvalidConfig);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let principal = resolve_session_in(&tx, session_digest, intent.now)?;
        if principal.role != "owner" && principal.role != "approver" {
            return Err(ProductError::Invariant("current role cannot approve"));
        }
        let operation_scope_revision = if scope_enforced_in(&tx)? {
            require_order_capability_in(
                &tx,
                &principal,
                &intent.order_id,
                ProductCapability::ApproveOrder,
                true,
            )?
            .scope_revision
        } else {
            -1
        };
        let operation_id = operation_id(
            &self.installation_id,
            &principal.actor_id,
            &intent.request_key,
        );
        let digest = approval_digest(&self.installation_id, &principal.actor_id, intent);
        let existing: Option<(String, String)> = tx.query_row(
            "SELECT operation_id,request_digest FROM product_operations_v1 WHERE actor_id=?1 AND request_key=?2",
            params![principal.actor_id, intent.request_key], |r| Ok((r.get(0)?,r.get(1)?)),
        ).optional()?;
        if let Some((old_id, old_digest)) = existing {
            if old_digest != digest {
                return Err(ProductError::Invariant(
                    "approval idempotency key binding conflict",
                ));
            }
            let old: (String, String, i64, Option<String>, Option<i64>, i64) = tx.query_row(
                "SELECT phase,expires_at,fence,owner_id,epoch,scope_revision FROM product_operations_v1 WHERE operation_id=?1", [&old_id],
                |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
            )?;
            if old.5 != operation_scope_revision {
                return Err(ProductError::Invariant(
                    "approval scope revision changed since reservation",
                ));
            }
            // A committed operation is a durable idempotency result. The
            // current authenticated actor may reacquire its private proof
            // after a lost HTTP response or process restart; this does not
            // reserve or consume the grant again.
            if old.0 == "completed" {
                let epoch = old
                    .4
                    .ok_or(ProductError::Invariant("approval epoch missing"))?;
                let owner = old
                    .3
                    .ok_or(ProductError::Invariant("approval owner missing"))?;
                let proof = ProductReservationProof {
                    operation_id: old_id,
                    owner_id: owner,
                    epoch,
                    fence: old.2,
                };
                tx.commit()?;
                return Ok(proof);
            }
            let expires = DateTime::parse_from_rfc3339(&old.1)
                .map_err(|_| ProductError::Invariant("approval expiry invalid"))?
                .with_timezone(&Utc);
            if old.0 == "prepared" && intent.now < expires {
                let epoch = old
                    .4
                    .ok_or(ProductError::Invariant("approval epoch missing"))?;
                let owner = old
                    .3
                    .ok_or(ProductError::Invariant("approval owner missing"))?;
                let proof = ProductReservationProof {
                    operation_id: old_id,
                    owner_id: owner,
                    epoch,
                    fence: old.2,
                };
                tx.commit()?;
                return Ok(proof);
            }
            // Expiry or a prior recovery epoch makes the old reservation
            // unusable. Reclaim only its matching fence, then issue a fresh
            // reservation while retaining the stable business operation ID.
            tx.execute("UPDATE product_grants_v1 SET state='issued',fence=0 WHERE grant_id=?1 AND state='reserved' AND fence=?2", params![intent.grant_id,old.2])?;
            tx.execute("UPDATE product_operations_v1 SET phase='fenced' WHERE operation_id=?1 AND phase='prepared'", [&old_id])?;
            let binding = ProductGrantBinding {
                grant_id: intent.grant_id.clone(),
                token_digest: intent.token_digest.clone(),
                actor_id: principal.actor_id.clone(),
                order_id: intent.order_id.clone(),
                operation_id: old_id.clone(),
            };
            let reservation = prepare_product_in(&tx, binding)
                .map_err(|_| ProductError::Invariant("approval grant unavailable"))?;
            let epoch: i64 = tx.query_row(
                "SELECT recovery_epoch FROM product_meta_v1 WHERE singleton=1",
                [],
                |r| r.get(0),
            )?;
            let owner = format!("{old_id}:{epoch}:{}", reservation.fence());
            tx.execute("UPDATE product_operations_v1 SET phase='prepared',grant_id=?1,owner_id=?2,epoch=?3,fence=?4,prepared_at=?5,expires_at=?6,scope_revision=?7 WHERE operation_id=?8", params![intent.grant_id,owner,epoch,reservation.fence(),intent.now.to_rfc3339(),(intent.now+chrono::Duration::minutes(5)).to_rfc3339(),operation_scope_revision,old_id])?;
            let proof = ProductReservationProof {
                operation_id: old_id,
                owner_id: owner,
                epoch,
                fence: reservation.fence(),
            };
            tx.commit()?;
            return Ok(proof);
        }
        let (source_revision, source_enabled): (i64, i64) = tx.query_row(
            "SELECT o.source_revision,s.enabled FROM product_orders_v1 o JOIN product_sources_v1 s ON s.connector_id=o.connector_id WHERE o.order_id=?1 AND o.lifecycle=1 AND s.repository_id=o.repository_id",
            [&intent.order_id], |r| Ok((r.get(0)?,r.get(1)?)),
        ).optional()?.ok_or(ProductError::Invariant("order source binding unavailable"))?;
        if source_enabled != 1 || source_revision != intent.expected_source_revision {
            return Err(ProductError::Invariant("source scope revision changed"));
        }
        let binding = ProductGrantBinding {
            grant_id: intent.grant_id.clone(),
            token_digest: intent.token_digest.clone(),
            actor_id: principal.actor_id.clone(),
            order_id: intent.order_id.clone(),
            operation_id: operation_id.clone(),
        };
        let reservation = prepare_product_in(&tx, binding)
            .map_err(|_| ProductError::Invariant("approval grant unavailable"))?;
        let epoch: i64 = tx.query_row(
            "SELECT recovery_epoch FROM product_meta_v1 WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        let owner = format!("{operation_id}:{epoch}:{}", reservation.fence());
        tx.execute("INSERT INTO product_operations_v1(operation_id,actor_id,request_key,request_digest,order_id,grant_id,phase,owner_id,epoch,fence,prepared_at,expires_at,scope_revision) VALUES(?1,?2,?3,?4,?5,?6,'prepared',?7,?8,?9,?10,?11,?12)",
            params![operation_id,principal.actor_id,intent.request_key,digest,intent.order_id,intent.grant_id,owner,epoch,reservation.fence(),intent.now.to_rfc3339(),(intent.now+chrono::Duration::minutes(5)).to_rfc3339(),operation_scope_revision])?;
        let proof = ProductReservationProof {
            operation_id,
            owner_id: owner,
            epoch,
            fence: reservation.fence(),
        };
        tx.commit()?;
        Ok(proof)
    }

    /// Issue a single-use approval grant after authenticating the live account.
    /// The caller keeps the opaque bearer secret; only its digest is persisted.
    pub fn issue_approval_grant(
        &mut self,
        session_digest: &str,
        grant_id: &str,
        token_digest: &str,
        order_id: &str,
        now: DateTime<Utc>,
    ) -> Result<(), ProductError> {
        if grant_id.is_empty() || !is_sha256_hex(token_digest) {
            return Err(ProductError::InvalidConfig);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let principal = resolve_session_in(&tx, session_digest, now)?;
        if principal.role != "owner" && principal.role != "approver" {
            return Err(ProductError::Invariant("current role cannot approve"));
        }
        if scope_enforced_in(&tx)? {
            require_order_capability_in(
                &tx,
                &principal,
                order_id,
                ProductCapability::ApproveOrder,
                true,
            )?;
        }
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM product_orders_v1 WHERE order_id=?1)",
            [order_id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(ProductError::Invariant("approval order unavailable"));
        }
        issue_product_grant_in(
            &tx,
            &ProductGrantBinding {
                grant_id: grant_id.into(),
                token_digest: token_digest.into(),
                actor_id: principal.actor_id,
                order_id: order_id.into(),
                operation_id: "pending".into(),
            },
        )
        .map_err(|_| ProductError::Invariant("approval grant issue failed"))?;
        tx.commit()?;
        Ok(())
    }

    /// Complete an approval entirely in ProductAtomicV1. The approval event,
    /// receipt, single-use grant/token consumption, and operation outcome are
    /// committed together. No caller-provided actor or cached tuple is trusted.
    pub fn approve_product(
        &mut self,
        session_digest: &str,
        intent: &ProductApprovalIntent,
        reservation: &ProductReservationProof,
    ) -> Result<ProductApprovalReceipt, ProductError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let principal = resolve_session_in(&tx, session_digest, intent.now)?;
        if principal.role != "owner" && principal.role != "approver" {
            return Err(ProductError::Invariant("current role cannot approve"));
        }
        let current_scope_revision = if scope_enforced_in(&tx)? {
            require_order_capability_in(
                &tx,
                &principal,
                &intent.order_id,
                ProductCapability::ApproveOrder,
                true,
            )?
            .scope_revision
        } else {
            -1
        };
        let operation_id = operation_id(
            &self.installation_id,
            &principal.actor_id,
            &intent.request_key,
        );
        if operation_id != reservation.operation_id {
            return Err(ProductError::Invariant(
                "approval operation binding mismatch",
            ));
        }
        let digest = approval_digest(&self.installation_id, &principal.actor_id, intent);
        let claim: Option<(String,i64,i64,String,String,i64)> = tx.query_row(
            "SELECT owner_id,epoch,fence,request_digest,phase,scope_revision FROM product_operations_v1 WHERE operation_id=?1 AND actor_id=?2 AND order_id=?3",
            params![operation_id,principal.actor_id,intent.order_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
        ).optional()?;
        let (
            claim_owner,
            claim_epoch,
            claim_fence,
            claim_digest,
            claim_phase,
            claim_scope_revision,
        ) = claim.ok_or(ProductError::Invariant("approval reservation missing"))?;
        if claim_owner != reservation.owner_id
            || claim_epoch != reservation.epoch
            || claim_fence != reservation.fence
            || claim_digest != digest
            || claim_scope_revision != current_scope_revision
        {
            return Err(ProductError::Invariant(
                "approval reservation proof is stale or mismatched",
            ));
        }
        if let Some(receipt_json) = tx
            .query_row(
                "SELECT receipt_json FROM product_outcomes_v1 WHERE operation_id=?1",
                [&operation_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            let receipt: ProductApprovalReceipt = serde_json::from_str(&receipt_json)
                .map_err(|_| ProductError::Invariant("stored approval outcome is corrupt"))?;
            let outcome: (i64, String) = tx.query_row(
                "SELECT seq,event_hash FROM product_outcomes_v1 WHERE operation_id=?1",
                [&operation_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if outcome.0 != receipt.sequence
                || outcome.1 != receipt.event_hash
                || receipt.operation_id != operation_id
                || receipt.order_id != intent.order_id
                || receipt.tuple_digest != intent.tuple_digest
                || receipt.artifact_digest != intent.artifact_digest
            {
                return Err(ProductError::Invariant(
                    "stored approval outcome binding is corrupt",
                ));
            }
            let side:Option<(String,String,String)>=tx.query_row("SELECT result_id,binding_digest,tuple_digest FROM product_approval_result_bindings_v1 WHERE operation_id=?1",[&operation_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            if let Some((result_id, binding_digest, tuple_digest)) = side {
                let exact = result::approval_result_in(
                    &tx,
                    &receipt.order_id,
                    receipt.lifecycle,
                    receipt.attempt,
                    receipt.run_id,
                    receipt.generation,
                    &receipt.tuple_digest,
                    &receipt.artifact_digest,
                )
                .map_err(|_| ProductError::Invariant("stored result binding is corrupt"))?;
                if exact != (result_id, binding_digest, tuple_digest) {
                    return Err(ProductError::Invariant("approval side binding mismatch"));
                }
            } else {
                let cutoff:i64=tx.query_row("SELECT legacy_approval_cutoff_seq FROM product_result_meta_v1 WHERE singleton=1",[],|r|r.get(0))?;
                if outcome.0 > cutoff {
                    return Err(ProductError::Invariant(
                        "post-cutoff approval lacks result side binding",
                    ));
                }
                let evidence:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ledger l JOIN product_transition_receipts_v1 r ON r.seq=l.seq AND r.event_hash=l.hash JOIN product_token_consumptions_v1 c ON c.operation_id=?1 AND c.seq=l.seq JOIN product_grants_v1 g ON g.grant_id=(SELECT grant_id FROM product_operations_v1 WHERE operation_id=?1) WHERE l.seq=?2 AND l.hash=?3 AND l.order_id=?4 AND l.to_state='publishing' AND l.event_json IS NOT NULL AND r.operation='human-approval' AND r.order_id=?4 AND r.lifecycle=?5 AND g.state='consumed')",params![operation_id,outcome.0,outcome.1,receipt.order_id,receipt.lifecycle],|r|r.get(0))?;
                if !evidence {
                    return Err(ProductError::Invariant(
                        "legacy approval evidence is incomplete",
                    ));
                }
            }
            let stored_digest: String = tx.query_row(
                "SELECT request_digest FROM product_operations_v1 WHERE operation_id=?1",
                [&operation_id],
                |r| r.get(0),
            )?;
            if stored_digest != digest
                || receipt.actor_id != principal.actor_id
                || receipt.order_id != intent.order_id
            {
                return Err(ProductError::Invariant("approval retry binding conflict"));
            }
            tx.commit()?;
            return Ok(receipt);
        }
        let (source_revision, source_enabled, lifecycle, admission_seq): (i64,i64,i64,i64) = tx.query_row(
            "SELECT o.source_revision,s.enabled,o.lifecycle,o.admission_seq FROM product_orders_v1 o JOIN product_sources_v1 s ON s.connector_id=o.connector_id WHERE o.order_id=?1 AND s.repository_id=o.repository_id",
            [&intent.order_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
        ).optional()?.ok_or(ProductError::Invariant("order source binding unavailable"))?;
        if source_enabled != 1
            || source_revision != intent.expected_source_revision
            || lifecycle != 1
        {
            return Err(ProductError::Invariant(
                "order lifecycle or source revision changed",
            ));
        }
        let op: Option<(String,String,i64,i64,String)> = tx.query_row(
            "SELECT request_digest,phase,epoch,fence,expires_at FROM product_operations_v1 WHERE operation_id=?1 AND actor_id=?2 AND order_id=?3",
            params![operation_id,principal.actor_id,intent.order_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
        ).optional()?;
        let (stored_digest, phase, epoch, fence, expires_at) =
            op.ok_or(ProductError::Invariant("approval was not prepared"))?;
        let expires_at = DateTime::parse_from_rfc3339(&expires_at)
            .map_err(|_| ProductError::Invariant("approval expiry invalid"))?
            .with_timezone(&Utc);
        let current_epoch: i64 = tx.query_row(
            "SELECT recovery_epoch FROM product_meta_v1 WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if stored_digest != digest
            || claim_phase != "prepared"
            || phase != "prepared"
            || epoch != current_epoch
            || intent.now >= expires_at
        {
            return Err(ProductError::Invariant("approval reservation is stale"));
        }
        let (predecessor, _head_hash, replay_lifecycle, actual_attempt, replay_state) =
            result::verified_order_in(&tx, &intent.order_id)
                .map_err(|_| ProductError::Invariant("approval S1 evidence is invalid"))?;
        if predecessor < admission_seq
            || i64::from(replay_lifecycle + 1) != lifecycle
            || i64::from(actual_attempt) != intent.expected_attempt
            || replay_state != "waiting"
        {
            return Err(ProductError::Invariant("order is not currently approvable"));
        }
        let (result_id, result_binding_digest, tuple_digest) = result::approval_result_in(
            &tx,
            &intent.order_id,
            lifecycle,
            intent.expected_attempt,
            intent.run_id,
            intent.generation,
            &intent.tuple_digest,
            &intent.artifact_digest,
        )
        .map_err(|_| {
            ProductError::Invariant("strict local result binding unavailable or invalid")
        })?;
        if tuple_digest != intent.tuple_digest {
            return Err(ProductError::Invariant("strict result tuple mismatch"));
        }
        let artifact_digest = intent.artifact_digest.clone();
        let seq: i64 = tx.query_row("SELECT COALESCE(MAX(seq),0)+1 FROM ledger", [], |r| {
            r.get(0)
        })?;
        let previous_hash: String = tx.query_row(
            "SELECT hash FROM ledger ORDER BY seq DESC LIMIT 1",
            [],
            |r| r.get(0),
        )?;
        let event = OrderEvent::Approved {
            intent: intent.intent.clone(),
            artifact_digest: artifact_digest.clone(),
        };
        let event_text = format!("{event:?}");
        let event_json = crate::event_json::event_to_json(&event);
        let actor = ActorId {
            kind: ActorKind::Human,
            source: "product-session".into(),
            id: principal.actor_id.clone(),
        };
        let actor_text = format!("{:?}/{}", actor.kind, actor.id);
        let at = intent.now.to_rfc3339();
        let v1 = format!(
            "{seq}|{}|waiting|publishing|{event_text}|{actor_text}|{}|{at}|{previous_hash}",
            intent.order_id, intent.intent
        );
        use sha2::Digest as _;
        let event_hash = hex::encode(sha2::Sha256::digest(
            crate::event_json::v2_hash_payload(&v1, &event_json).as_bytes(),
        ));
        tx.execute("INSERT INTO product_transition_intents_v1(order_id,lifecycle,expected_predecessor,operation) VALUES(?1,?2,?3,'human-approval')", params![intent.order_id,lifecycle,predecessor])?;
        tx.execute("INSERT INTO ledger(order_id,from_state,to_state,event,actor,intent,at,prev_hash,event_json,hash) VALUES(?1,'waiting','publishing',?2,?3,?4,?5,?6,?7,?8)", params![intent.order_id,event_text,actor_text,intent.intent,at,previous_hash,event_json,event_hash])?;
        #[cfg(test)]
        product_insert_killpoint("after_s1_insert");
        let anchor = ProductApprovalAnchor {
            sequence: seq,
            event_hash: event_hash.clone(),
            lifecycle,
            artifact_digest: artifact_digest.clone(),
        };
        let binding = ProductGrantBinding {
            grant_id: intent.grant_id.clone(),
            token_digest: intent.token_digest.clone(),
            actor_id: principal.actor_id.clone(),
            order_id: intent.order_id.clone(),
            operation_id: operation_id.clone(),
        };
        validate_approval_anchor_in(&tx, &binding, &anchor)
            .map_err(|_| ProductError::Invariant("approval participant anchor failed"))?;
        consume_product_grant_fenced_in(&tx, &binding, fence)
            .map_err(|_| ProductError::Invariant("approval grant consumption failed"))?;
        #[cfg(test)]
        product_insert_killpoint("after_grant_consumption");
        consume_product_token_in(&tx, &binding, &anchor)
            .map_err(|_| ProductError::Invariant("approval token consumption failed"))?;
        #[cfg(test)]
        product_insert_killpoint("after_token_insert");
        let receipt = ProductApprovalReceipt {
            operation_id: operation_id.clone(),
            actor_id: principal.actor_id,
            order_id: intent.order_id.clone(),
            lifecycle,
            sequence: seq,
            event_hash,
            artifact_digest,
            attempt: intent.expected_attempt,
            run_id: intent.run_id,
            generation: intent.generation,
            tuple_digest: intent.tuple_digest.clone(),
            source_revision: intent.expected_source_revision,
        };
        let receipt_json = serde_json::to_string(&receipt)
            .map_err(|_| ProductError::Invariant("approval receipt encoding failed"))?;
        tx.execute("INSERT INTO product_outcomes_v1(operation_id,seq,event_hash,receipt_json) VALUES(?1,?2,?3,?4)", params![operation_id,seq,receipt.event_hash,receipt_json])?;
        #[cfg(test)]
        product_insert_killpoint("after_outcome_insert");
        tx.execute("INSERT INTO product_approval_result_bindings_v1(operation_id,result_id,binding_digest,tuple_digest) VALUES(?1,?2,?3,?4)",params![operation_id,result_id,result_binding_digest,tuple_digest])?;
        #[cfg(test)]
        product_insert_killpoint("after_result_binding_insert");
        tx.execute("UPDATE product_operations_v1 SET phase='completed' WHERE operation_id=?1 AND phase='prepared' AND fence=?2", params![operation_id,fence])?;
        #[cfg(test)]
        product_insert_killpoint("before_approval_commit");
        #[cfg(test)]
        if std::env::var("B1_APPROVAL_COMMIT_OBSERVER").as_deref() == Ok("1") {
            use std::io::Write;
            println!("B1_APPROVAL_COMMIT_ATTEMPT");
            let _ = std::io::stdout().flush();
        }
        if let Err(commit_error) = tx.commit() {
            // A failed COMMIT can have an ambiguous outcome. Discard this
            // connection and inspect the authoritative operation on a fresh
            // handle; never rerun the approval blindly.
            let reopened = Connection::open(&self.path)?;
            reopened.busy_timeout(std::time::Duration::from_secs(5))?;
            let stored: Option<String> = reopened
                .query_row(
                    "SELECT receipt_json FROM product_outcomes_v1 WHERE operation_id=?1",
                    [&operation_id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(stored) = stored {
                let committed: ProductApprovalReceipt = serde_json::from_str(&stored)
                    .map_err(|_| ProductError::Invariant("stored approval outcome is corrupt"))?;
                if committed == receipt {
                    let recovery_tx = reopened.unchecked_transaction()?;
                    let side:Option<(String,String,String)>=recovery_tx.query_row("SELECT result_id,binding_digest,tuple_digest FROM product_approval_result_bindings_v1 WHERE operation_id=?1",[&operation_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
                    let Some((result_id, binding_digest, tuple_digest)) = side else {
                        return Err(ProductError::Invariant(
                            "ambiguous approval commit lacks result side binding",
                        ));
                    };
                    let exact = result::approval_result_in(
                        &recovery_tx,
                        &receipt.order_id,
                        receipt.lifecycle,
                        receipt.attempt,
                        receipt.run_id,
                        receipt.generation,
                        &receipt.tuple_digest,
                        &receipt.artifact_digest,
                    )
                    .map_err(|_| {
                        ProductError::Invariant("ambiguous approval result binding is corrupt")
                    })?;
                    if exact != (result_id, binding_digest, tuple_digest) {
                        return Err(ProductError::Invariant(
                            "ambiguous approval result side binding mismatch",
                        ));
                    }
                    recovery_tx.commit()?;
                    self.conn = reopened;
                    return Ok(committed);
                }
                return Err(ProductError::Invariant(
                    "ambiguous commit outcome binding mismatch",
                ));
            }
            self.conn = reopened;
            return Err(ProductError::Database(commit_error));
        }
        #[cfg(test)]
        product_insert_killpoint("after_approval_commit_before_response");
        Ok(receipt)
    }

    /// Admit one validated immutable task snapshot and its Queued S1 event in
    /// one BEGIN IMMEDIATE transaction. Replays of the same stable identity
    /// return its original receipt; changed payloads conflict.
    pub fn admit_order(
        &mut self,
        order_id: &str,
        connector: &str,
        repository_id: &str,
        issue_id: &str,
        task_kind: &str,
        payload: &str,
        at: DateTime<Utc>,
    ) -> Result<AdmissionReceipt, ProductError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let receipt = admit_order_in(
            &tx,
            order_id,
            connector,
            repository_id,
            issue_id,
            task_kind,
            payload,
            at,
        )?;
        tx.commit()?;
        Ok(receipt)
    }

    /// Persist one fully validated fetched page and its next-page cursor in a
    /// single transaction. Repeated delivery versions are idempotent by
    /// stable delivery ID; a conflicting reuse is rejected.
    pub fn persist_intake_page(
        &mut self,
        cursor: &crate::product_intake::ProductPollCursor,
        deliveries: &[crate::product_intake::ProductDelivery],
    ) -> Result<(), ProductError> {
        self.persist_intake_page_with_rejections(cursor, deliveries, &[])
    }

    /// Persist valid snapshots, privacy-safe poison-item digests, and cursor
    /// advancement together so a bad item cannot repeatedly starve the page.
    pub fn persist_intake_page_with_rejections(
        &mut self,
        cursor: &crate::product_intake::ProductPollCursor,
        deliveries: &[crate::product_intake::ProductDelivery],
        rejected_item_digests: &[String],
    ) -> Result<(), ProductError> {
        self.persist_intake_page_authorized(cursor, deliveries, rejected_item_digests, None)
    }

    pub fn persist_intake_page_for_service(
        &mut self,
        context: &ProductServiceContext,
        cursor: &crate::product_intake::ProductPollCursor,
        deliveries: &[crate::product_intake::ProductDelivery],
        rejected_item_digests: &[String],
        now: DateTime<Utc>,
    ) -> Result<(), ProductError> {
        self.persist_intake_page_authorized(
            cursor,
            deliveries,
            rejected_item_digests,
            Some((context, now)),
        )
    }

    fn persist_intake_page_authorized(
        &mut self,
        cursor: &crate::product_intake::ProductPollCursor,
        deliveries: &[crate::product_intake::ProductDelivery],
        rejected_item_digests: &[String],
        service: Option<(&ProductServiceContext, DateTime<Utc>)>,
    ) -> Result<(), ProductError> {
        if cursor.connector_id.trim().is_empty()
            || cursor.query_fingerprint.trim().is_empty()
            || cursor.sweep_id.trim().is_empty()
            || cursor.next_page == 0
            || cursor.page_ceiling == 0
            || cursor.next_page > cursor.page_ceiling.saturating_add(1)
        {
            return Err(ProductError::InvalidConfig);
        }
        if rejected_item_digests.len() > 100
            || rejected_item_digests.iter().any(|d| !is_sha256_hex(d))
        {
            return Err(ProductError::InvalidConfig);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((context, now)) = service {
            service::require_service_context_in(
                &tx,
                context,
                ProductServiceAction::IntakeRead,
                now,
            )?;
            if context.connector_id != cursor.connector_id {
                return Err(ProductError::Invariant(
                    "service source does not match intake cursor",
                ));
            }
        } else if scope_enforced_in(&tx)? {
            return Err(ProductError::Invariant("intake service authority required"));
        }
        let configured: Option<ScopedIntakeBinding> = if let Some((context, _)) = service {
            tx.query_row(
                "SELECT r.api_origin,r.stable_repository_id,ps.enabled,l.api_owner,l.api_repo,i.generation,i.revision,ps.revision
                 FROM product_scope_current_sources_v1 c JOIN product_scope_source_instances_v1 i USING(source_instance_id)
                 JOIN product_scope_repositories_v1 r USING(repository_key) JOIN product_scope_api_locators_v1 l USING(source_instance_id)
                 JOIN product_sources_v1 ps ON ps.connector_id=c.connector_id WHERE c.connector_id=?1 AND i.source_instance_id=?2",
                params![cursor.connector_id,context.source_instance_id],
                |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?)),
            ).optional()?
        } else {
            tx.query_row(
                "SELECT origin,repository_id,enabled,NULL,NULL,0,0,revision FROM product_sources_v1 WHERE connector_id=?1",
                [&cursor.connector_id],
                |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?)),
            ).optional()?
        };
        let Some((
            origin,
            configured_repository,
            enabled,
            api_owner,
            api_repo,
            generation,
            instance_revision,
            connector_revision,
        )) = configured
        else {
            return Err(ProductError::Invariant(
                "connector is disabled or unconfigured",
            ));
        };
        if enabled != 1 {
            return Err(ProductError::Invariant(
                "connector is disabled or unconfigured",
            ));
        }
        if let Some((context, _)) = service
            && (origin != context.api_origin
                || configured_repository != context.repository_id
                || api_owner
                    .as_deref()
                    .is_none_or(|v| !v.eq_ignore_ascii_case(&context.api_owner))
                || api_repo
                    .as_deref()
                    .is_none_or(|v| !v.eq_ignore_ascii_case(&context.api_repo))
                || generation != context.source_generation
                || instance_revision != context.source_instance_revision
                || connector_revision != context.connector_revision)
        {
            return Err(ProductError::Invariant(
                "verified source binding changed before page commit",
            ));
        }
        for item in deliveries {
            if item.connector_id != cursor.connector_id
                || [
                    item.delivery_id.as_str(),
                    item.repository_id.as_str(),
                    item.issue_id.as_str(),
                    item.observed_updated_at.as_str(),
                    item.task_kind.as_str(),
                ]
                .iter()
                .any(|v| v.trim().is_empty())
                || !is_sha256_hex(&item.version_digest)
            {
                return Err(ProductError::InvalidConfig);
            }
            if item.repository_id != configured_repository
                || item.delivery_id
                    != github_delivery_id(
                        &self.installation_id,
                        &item.connector_id,
                        &origin,
                        &item.repository_id,
                        &item.issue_id,
                        &item.version_digest,
                    )
            {
                return Err(ProductError::Invariant(
                    "delivery identity is outside configured connector namespace",
                ));
            }
            if item.version_digest
                != delivery_version_digest(
                    &item.issue_id,
                    &item.observed_updated_at,
                    &item.task_kind,
                    &item.payload,
                )
            {
                return Err(ProductError::Invariant("delivery version digest mismatch"));
            }
            if item.payload.len() > 1_000_000 {
                return Err(ProductError::InvalidConfig);
            }
            let existing: Option<(String,String,String,String)> = tx.query_row(
                "SELECT repository_id,issue_id,version_digest,payload FROM product_intake_inbox_v1 WHERE delivery_id=?1",
                [&item.delivery_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
            ).optional()?;
            if let Some((repo, issue, version, payload)) = existing {
                if repo != item.repository_id
                    || issue != item.issue_id
                    || version != item.version_digest
                    || payload != item.payload
                {
                    return Err(ProductError::Invariant("delivery identity conflict"));
                }
            } else {
                tx.execute("INSERT INTO product_intake_inbox_v1(delivery_id,connector_id,repository_id,issue_id,version_digest,payload,state,retry_at) VALUES(?1,?2,?3,?4,?5,?6,'fetched',NULL)", params![item.delivery_id,item.connector_id,item.repository_id,item.issue_id,item.version_digest,item.payload])?;
                tx.execute("INSERT OR IGNORE INTO product_intake_delivery_meta_v1(delivery_id,observed_updated_at,task_kind) VALUES(?1,?2,?3)", params![item.delivery_id,item.observed_updated_at,item.task_kind])?;
            }
            if let Some((context, _)) = service {
                let old:Option<(String,i64,i64,i64)>=tx.query_row("SELECT source_instance_id,source_revision,source_instance_revision,source_generation FROM product_intake_source_bindings_v1 WHERE delivery_id=?1",[&item.delivery_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
                if let Some((instance, revision, instance_revision, generation)) = old {
                    if instance != context.source_instance_id
                        || revision != context.connector_revision
                        || instance_revision != context.source_instance_revision
                        || generation != context.source_generation
                    {
                        return Err(ProductError::Invariant(
                            "delivery belongs to another source generation",
                        ));
                    }
                } else {
                    tx.execute("INSERT INTO product_intake_source_bindings_v1(delivery_id,source_instance_id,source_revision,source_instance_revision,source_generation) VALUES(?1,?2,?3,?4,?5)",params![item.delivery_id,context.source_instance_id,context.connector_revision,context.source_instance_revision,context.source_generation])?;
                }
            }
        }
        for digest in rejected_item_digests {
            tx.execute("INSERT OR IGNORE INTO product_intake_rejections_v1(connector_id,query_fingerprint,sweep_id,item_digest,recorded_at) VALUES(?1,?2,?3,?4,?5)", params![cursor.connector_id,cursor.query_fingerprint,cursor.sweep_id,digest,cursor.updated_at])?;
        }
        tx.execute("INSERT INTO product_poll_state_v1(connector_id,query_fingerprint,sweep_id,next_page,page_ceiling,updated_at) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(connector_id,query_fingerprint) DO UPDATE SET sweep_id=excluded.sweep_id,next_page=excluded.next_page,page_ceiling=excluded.page_ceiling,updated_at=excluded.updated_at", params![cursor.connector_id,cursor.query_fingerprint,cursor.sweep_id,cursor.next_page,cursor.page_ceiling,cursor.updated_at])?;
        if let Some((context, _)) = service {
            tx.execute("INSERT INTO product_poll_source_bindings_v1(connector_id,query_fingerprint,source_instance_id,source_revision,source_instance_revision,source_generation) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(connector_id,query_fingerprint) DO UPDATE SET source_instance_id=excluded.source_instance_id,source_revision=excluded.source_revision,source_instance_revision=excluded.source_instance_revision,source_generation=excluded.source_generation",params![cursor.connector_id,cursor.query_fingerprint,context.source_instance_id,context.connector_revision,context.source_instance_revision,context.source_generation])?;
        }
        tx.execute(
            "DELETE FROM product_poll_backoff_v1 WHERE connector_id=?1 AND query_fingerprint=?2",
            params![cursor.connector_id, cursor.query_fingerprint],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn intake_rejection_count(&self, connector: &str) -> Result<u64, ProductError> {
        if self.scope_policy_enforced()? {
            return Err(ProductError::Invariant("intake service authority required"));
        }
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM product_intake_rejections_v1 WHERE connector_id=?1",
            [connector],
            |r| r.get::<_, i64>(0),
        )? as u64)
    }

    pub fn intake_rejection_count_for_service(
        &self,
        context: &ProductServiceContext,
        now: DateTime<Utc>,
    ) -> Result<u64, ProductError> {
        let tx = self.conn.unchecked_transaction()?;
        service::require_service_context_in(
            &tx,
            context,
            ProductServiceAction::AdmitDelivery,
            now,
        )?;
        let count = tx.query_row(
            "SELECT COUNT(*) FROM product_intake_rejections_v1 WHERE connector_id=?1",
            [&context.connector_id],
            |r| r.get::<_, i64>(0),
        )? as u64;
        tx.commit()?;
        Ok(count)
    }

    /// Persist retry state for transport/rate-limit failures. Exponential
    /// delay is capped at one hour; explicit server reset can extend it up to
    /// 24 hours. All time comparisons use the supplied clock value.
    pub fn record_intake_poll_failure(
        &mut self,
        connector: &str,
        fingerprint: &str,
        now: DateTime<Utc>,
        server_retry_at: Option<DateTime<Utc>>,
    ) -> Result<DateTime<Utc>, ProductError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if scope_enforced_in(&tx)? {
            return Err(ProductError::Invariant("intake service authority required"));
        }
        let old: i64 = tx.query_row(
            "SELECT failures FROM product_poll_backoff_v1 WHERE connector_id=?1 AND query_fingerprint=?2",
            params![connector,fingerprint], |r| r.get(0),
        ).optional()?.unwrap_or(0);
        let failures = old.saturating_add(1).min(16);
        let seconds = 30i64
            .saturating_mul(1i64 << (failures.saturating_sub(1) as u32))
            .min(3600);
        let exponential = now + chrono::Duration::seconds(seconds);
        let server_cap = now + chrono::Duration::hours(24);
        let retry_at = server_retry_at
            .map(|t| t.min(server_cap).max(exponential))
            .unwrap_or(exponential);
        tx.execute("INSERT INTO product_poll_backoff_v1(connector_id,query_fingerprint,failures,retry_at) VALUES(?1,?2,?3,?4) ON CONFLICT(connector_id,query_fingerprint) DO UPDATE SET failures=excluded.failures,retry_at=excluded.retry_at", params![connector,fingerprint,failures,retry_at.to_rfc3339()])?;
        tx.commit()?;
        Ok(retry_at)
    }

    pub fn record_intake_poll_failure_for_service(
        &mut self,
        context: &ProductServiceContext,
        fingerprint: &str,
        now: DateTime<Utc>,
        server_retry_at: Option<DateTime<Utc>>,
    ) -> Result<DateTime<Utc>, ProductError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        service::require_service_context_in(&tx, context, ProductServiceAction::IntakeRead, now)?;
        let old:i64=tx.query_row("SELECT failures FROM product_poll_backoff_v1 WHERE connector_id=?1 AND query_fingerprint=?2",params![context.connector_id,fingerprint],|r|r.get(0)).optional()?.unwrap_or(0);
        let failures = old.saturating_add(1).min(16);
        let seconds = 30i64
            .saturating_mul(1i64 << (failures.saturating_sub(1) as u32))
            .min(3600);
        let exponential = now + chrono::Duration::seconds(seconds);
        let retry_at = server_retry_at
            .map(|t| t.min(now + chrono::Duration::hours(24)).max(exponential))
            .unwrap_or(exponential);
        tx.execute("INSERT INTO product_poll_backoff_v1(connector_id,query_fingerprint,failures,retry_at) VALUES(?1,?2,?3,?4) ON CONFLICT(connector_id,query_fingerprint) DO UPDATE SET failures=excluded.failures,retry_at=excluded.retry_at",params![context.connector_id,fingerprint,failures,retry_at.to_rfc3339()])?;
        tx.commit()?;
        Ok(retry_at)
    }

    pub fn intake_poll_retry_at(
        &self,
        connector: &str,
        fingerprint: &str,
    ) -> Result<Option<DateTime<Utc>>, ProductError> {
        if self.scope_policy_enforced()? {
            return Err(ProductError::Invariant("intake service authority required"));
        }
        let raw: Option<String> = self.conn.query_row(
            "SELECT retry_at FROM product_poll_backoff_v1 WHERE connector_id=?1 AND query_fingerprint=?2",
            params![connector,fingerprint], |r| r.get(0),
        ).optional()?;
        raw.map(|value| {
            DateTime::parse_from_rfc3339(&value)
                .map(|t| t.with_timezone(&Utc))
                .map_err(|_| ProductError::Invariant("invalid persisted poll retry timestamp"))
        })
        .transpose()
    }

    pub fn intake_poll_retry_at_for_service(
        &self,
        context: &ProductServiceContext,
        fingerprint: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>, ProductError> {
        let tx = self.conn.unchecked_transaction()?;
        service::require_service_context_in(&tx, context, ProductServiceAction::IntakeRead, now)?;
        let raw: Option<String> = tx.query_row("SELECT retry_at FROM product_poll_backoff_v1 WHERE connector_id=?1 AND query_fingerprint=?2", params![context.connector_id,fingerprint], |r|r.get(0)).optional()?;
        tx.commit()?;
        raw.map(|v| {
            DateTime::parse_from_rfc3339(&v)
                .map(|t| t.with_timezone(&Utc))
                .map_err(|_| ProductError::Invariant("invalid persisted poll retry timestamp"))
        })
        .transpose()
    }

    /// Return due fetched/ retryable inbox entries. Merely listing these does
    /// not change their state and is not an acknowledgement.
    pub fn due_intake_deliveries(
        &self,
        now: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<crate::product_intake::DueDelivery>, ProductError> {
        if limit == 0 || limit > 1000 {
            return Err(ProductError::InvalidConfig);
        }
        let mode: String = self.conn.query_row(
            "SELECT mode FROM product_scope_policy_state_v1 WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if mode == "enforced" {
            return Err(ProductError::Invariant(
                "intake service scope is not provisioned",
            ));
        }
        let mut stmt = self.conn.prepare("SELECT i.delivery_id,i.connector_id,i.repository_id,i.issue_id,m.observed_updated_at,i.version_digest,m.task_kind,i.payload,i.state,i.retry_at FROM product_intake_inbox_v1 i JOIN product_intake_delivery_meta_v1 m USING(delivery_id) WHERE i.state IN ('fetched','retryable','admitted') AND (i.retry_at IS NULL OR i.retry_at<=?1) ORDER BY i.delivery_id LIMIT ?2")?;
        let rows = stmt
            .query_map(params![now.to_rfc3339(), limit as i64], |r| {
                Ok(crate::product_intake::DueDelivery {
                    delivery: crate::product_intake::ProductDelivery {
                        delivery_id: r.get(0)?,
                        connector_id: r.get(1)?,
                        repository_id: r.get(2)?,
                        issue_id: r.get(3)?,
                        observed_updated_at: r.get(4)?,
                        version_digest: r.get(5)?,
                        task_kind: r.get(6)?,
                        payload: r.get(7)?,
                    },
                    state: r.get(8)?,
                    retry_at: r.get(9)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn due_intake_deliveries_for_service(
        &self,
        context: &ProductServiceContext,
        now: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<crate::product_intake::DueDelivery>, ProductError> {
        if limit == 0 || limit > 1000 {
            return Err(ProductError::InvalidConfig);
        }
        let tx = self.conn.unchecked_transaction()?;
        service::require_service_context_in(
            &tx,
            context,
            ProductServiceAction::AdmitDelivery,
            now,
        )?;
        let mut stmt = tx.prepare("SELECT i.delivery_id,i.connector_id,i.repository_id,i.issue_id,m.observed_updated_at,i.version_digest,m.task_kind,i.payload,i.state,i.retry_at FROM product_intake_inbox_v1 i JOIN product_intake_delivery_meta_v1 m USING(delivery_id) JOIN product_intake_source_bindings_v1 b USING(delivery_id) WHERE i.connector_id=?1 AND i.repository_id=?2 AND b.source_instance_id=?3 AND b.source_revision=?4 AND b.source_instance_revision=?5 AND b.source_generation=?6 AND i.state IN ('fetched','retryable','admitted') AND (i.retry_at IS NULL OR i.retry_at<=?7) ORDER BY i.delivery_id LIMIT ?8")?;
        let rows = stmt
            .query_map(
                params![
                    context.connector_id,
                    context.repository_id,
                    context.source_instance_id,
                    context.connector_revision,
                    context.source_instance_revision,
                    context.source_generation,
                    now.to_rfc3339(),
                    limit as i64
                ],
                |r| {
                    Ok(crate::product_intake::DueDelivery {
                        delivery: crate::product_intake::ProductDelivery {
                            delivery_id: r.get(0)?,
                            connector_id: r.get(1)?,
                            repository_id: r.get(2)?,
                            issue_id: r.get(3)?,
                            observed_updated_at: r.get(4)?,
                            version_digest: r.get(5)?,
                            task_kind: r.get(6)?,
                            payload: r.get(7)?,
                        },
                        state: r.get(8)?,
                        retry_at: r.get(9)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        tx.commit()?;
        Ok(rows)
    }

    /// Atomically admit the stable issue identity, append Queued to S1,
    /// create durable work, and bind the inbox delivery to the admission.
    pub fn admit_intake_delivery(
        &mut self,
        delivery_id: &str,
        order_id: &str,
        at: DateTime<Utc>,
    ) -> Result<AdmissionReceipt, ProductError> {
        self.admit_intake_delivery_authorized(delivery_id, order_id, at, None)
    }

    pub fn admit_intake_delivery_for_service(
        &mut self,
        context: &ProductServiceContext,
        delivery_id: &str,
        order_id: &str,
        at: DateTime<Utc>,
    ) -> Result<AdmissionReceipt, ProductError> {
        self.admit_intake_delivery_authorized(delivery_id, order_id, at, Some(context))
    }

    fn admit_intake_delivery_authorized(
        &mut self,
        delivery_id: &str,
        order_id: &str,
        at: DateTime<Utc>,
        service: Option<&ProductServiceContext>,
    ) -> Result<AdmissionReceipt, ProductError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(context) = service {
            service::require_service_context_in(
                &tx,
                context,
                ProductServiceAction::AdmitDelivery,
                at,
            )?;
        } else if scope_enforced_in(&tx)? {
            return Err(ProductError::Invariant("intake service authority required"));
        }
        let item: Option<(String,String,String,String,String,String,String,String)> = tx.query_row(
            "SELECT i.connector_id,i.repository_id,i.issue_id,i.version_digest,i.payload,m.task_kind,m.observed_updated_at,i.state FROM product_intake_inbox_v1 i JOIN product_intake_delivery_meta_v1 m USING(delivery_id) WHERE i.delivery_id=?1",
            [delivery_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?)),
        ).optional()?;
        let Some((
            connector,
            repository_id,
            issue_id,
            version_digest,
            payload,
            task_kind,
            _,
            state,
        )) = item
        else {
            return Err(ProductError::Invariant("delivery unavailable"));
        };
        if service.is_some_and(|context| {
            context.connector_id != connector || context.repository_id != repository_id
        }) {
            return Err(ProductError::Invariant(
                "delivery is outside service source",
            ));
        }
        if let Some(context) = service {
            let bound: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM product_intake_source_bindings_v1 WHERE delivery_id=?1 AND source_instance_id=?2 AND source_revision=?3 AND source_instance_revision=?4 AND source_generation=?5)",
                params![delivery_id, context.source_instance_id, context.connector_revision, context.source_instance_revision, context.source_generation],
                |r| r.get(0),
            )?;
            if !bound {
                return Err(ProductError::Invariant(
                    "delivery is outside service source generation",
                ));
            }
        }
        if state == "admitted" || state == "acked" {
            let row: (String,i64,String,i64) = tx.query_row("SELECT order_id,admission_seq,admission_hash,lifecycle FROM product_orders_v1 WHERE connector_id=?1 AND repository_id=?2 AND issue_id=?3", params![connector,repository_id,issue_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            if let Some(context) = service {
                verify_order_source_provenance_in(&tx, &row.0, row.3, context)?;
            }
            tx.commit()?;
            return Ok(AdmissionReceipt {
                order_id: row.0,
                lifecycle: row.3,
                sequence: row.1,
                event_hash: row.2,
            });
        }
        let observed_updated_at: String = tx.query_row(
            "SELECT observed_updated_at FROM product_intake_delivery_meta_v1 WHERE delivery_id=?1",
            [delivery_id],
            |r| r.get(0),
        )?;
        let expected_version =
            delivery_version_digest(&issue_id, &observed_updated_at, &task_kind, &payload);
        if version_digest != expected_version {
            return Err(ProductError::Invariant("delivery payload digest mismatch"));
        }
        // An issue edit/reopen is a new observed delivery version, not a new
        // order or lifecycle. Preserve the first admitted task snapshot and
        // bind this delivery to the same original S1 admission receipt.
        let prior: Option<(String, i64, String, i64)> = tx.query_row(
            "SELECT order_id,admission_seq,admission_hash,lifecycle FROM product_orders_v1 WHERE connector_id=?1 AND repository_id=?2 AND issue_id=?3",
            params![connector,repository_id,issue_id],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
        ).optional()?;
        if let Some((prior_order, sequence, event_hash, lifecycle)) = prior {
            if let Some(context) = service {
                verify_order_source_provenance_in(&tx, &prior_order, lifecycle, context)?;
            }
            tx.execute("UPDATE product_intake_inbox_v1 SET state='admitted',retry_at=NULL WHERE delivery_id=?1", [delivery_id])?;
            let receipt = AdmissionReceipt {
                order_id: prior_order,
                lifecycle,
                sequence,
                event_hash,
            };
            tx.commit()?;
            return Ok(receipt);
        }
        let receipt = admit_order_in(
            &tx,
            order_id,
            &connector,
            &repository_id,
            &issue_id,
            &task_kind,
            &payload,
            at,
        )?;
        tx.execute("UPDATE product_intake_inbox_v1 SET state='admitted',retry_at=NULL WHERE delivery_id=?1 AND state IN ('fetched','retryable')", [delivery_id])?;
        // Product intake currently preserves the task snapshot but has no
        // supported product execution adapter. Keep it durable and visibly
        // waiting; never represent an unimplemented run as successful work.
        tx.execute("INSERT OR IGNORE INTO product_work_backlog_v1(order_id,lifecycle,delivery_id,state,available_at,attempts) VALUES(?1,?2,?3,'waiting_unsupported',?4,0)", params![receipt.order_id,receipt.lifecycle,delivery_id,at.to_rfc3339()])?;
        tx.commit()?;
        Ok(receipt)
    }

    /// Mark only a delivery whose durable admission and S1 receipt still
    /// match. The caller invokes the remote ack only after admission commit.
    pub fn mark_intake_acked(
        &mut self,
        delivery_id: &str,
        receipt: &AdmissionReceipt,
    ) -> Result<(), ProductError> {
        self.mark_intake_acked_authorized(delivery_id, receipt, None, Utc::now())
    }

    pub fn mark_intake_acked_for_service(
        &mut self,
        context: &ProductServiceContext,
        delivery_id: &str,
        receipt: &AdmissionReceipt,
        now: DateTime<Utc>,
    ) -> Result<(), ProductError> {
        self.mark_intake_acked_authorized(delivery_id, receipt, Some(context), now)
    }

    fn mark_intake_acked_authorized(
        &mut self,
        delivery_id: &str,
        receipt: &AdmissionReceipt,
        service: Option<&ProductServiceContext>,
        now: DateTime<Utc>,
    ) -> Result<(), ProductError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(context) = service {
            service::require_service_context_in(
                &tx,
                context,
                ProductServiceAction::AckDelivery,
                now,
            )?;
        } else if scope_enforced_in(&tx)? {
            return Err(ProductError::Invariant("intake service authority required"));
        }
        let matches: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM product_intake_inbox_v1 i JOIN product_orders_v1 o ON o.connector_id=i.connector_id AND o.repository_id=i.repository_id AND o.issue_id=i.issue_id JOIN product_transition_receipts_v1 r ON r.seq=o.admission_seq AND r.event_hash=o.admission_hash LEFT JOIN product_intake_source_bindings_v1 b ON b.delivery_id=i.delivery_id WHERE i.delivery_id=?1 AND i.state IN ('admitted','retryable','acked') AND o.order_id=?2 AND o.lifecycle=?3 AND o.admission_seq=?4 AND o.admission_hash=?5 AND r.operation='github-admission' AND (?6 IS NULL OR (i.connector_id=?6 AND i.repository_id=?7 AND b.source_instance_id=?8 AND b.source_revision=?9 AND b.source_instance_revision=?10 AND b.source_generation=?11)))", params![delivery_id,receipt.order_id,receipt.lifecycle,receipt.sequence,receipt.event_hash,service.map(|c|c.connector_id.as_str()),service.map(|c|c.repository_id.as_str()),service.map(|c|c.source_instance_id.as_str()),service.map(|c|c.connector_revision),service.map(|c|c.source_instance_revision),service.map(|c|c.source_generation)], |r| r.get(0))?;
        if !matches {
            return Err(ProductError::Invariant(
                "ack requires exact durable admission receipt",
            ));
        }
        tx.execute(
            "UPDATE product_intake_inbox_v1 SET state='acked' WHERE delivery_id=?1",
            [delivery_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Mark a retryable transport/admission failure without acknowledging it.
    pub fn defer_intake_delivery(
        &mut self,
        delivery_id: &str,
        receipt: &AdmissionReceipt,
        retry_at: DateTime<Utc>,
    ) -> Result<(), ProductError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if scope_enforced_in(&tx)? {
            return Err(ProductError::Invariant("intake service authority required"));
        }
        let exact: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM product_intake_inbox_v1 i JOIN product_orders_v1 o ON o.connector_id=i.connector_id AND o.repository_id=i.repository_id AND o.issue_id=i.issue_id WHERE i.delivery_id=?1 AND i.state IN ('admitted','retryable') AND o.order_id=?2 AND o.lifecycle=?3 AND o.admission_seq=?4 AND o.admission_hash=?5)", params![delivery_id,receipt.order_id,receipt.lifecycle,receipt.sequence,receipt.event_hash], |r| r.get(0))?;
        if !exact {
            return Err(ProductError::Invariant(
                "retry requires exact durable admission receipt",
            ));
        }
        let changed = tx.execute("UPDATE product_intake_inbox_v1 SET state='retryable',retry_at=?1 WHERE delivery_id=?2 AND state IN ('admitted','retryable')", params![retry_at.to_rfc3339(),delivery_id])?;
        if changed != 1 {
            return Err(ProductError::Invariant("delivery cannot be deferred"));
        }
        tx.commit()?;
        Ok(())
    }

    pub fn defer_intake_delivery_for_service(
        &mut self,
        context: &ProductServiceContext,
        delivery_id: &str,
        receipt: &AdmissionReceipt,
        retry_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<(), ProductError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        service::require_service_context_in(
            &tx,
            context,
            ProductServiceAction::AdmitDelivery,
            now,
        )?;
        let exact:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM product_intake_inbox_v1 i JOIN product_orders_v1 o ON o.connector_id=i.connector_id AND o.repository_id=i.repository_id AND o.issue_id=i.issue_id JOIN product_intake_source_bindings_v1 b USING(delivery_id) WHERE i.delivery_id=?1 AND i.connector_id=?6 AND i.repository_id=?7 AND b.source_instance_id=?8 AND b.source_revision=?9 AND b.source_instance_revision=?10 AND b.source_generation=?11 AND i.state IN ('admitted','retryable') AND o.order_id=?2 AND o.lifecycle=?3 AND o.admission_seq=?4 AND o.admission_hash=?5)",params![delivery_id,receipt.order_id,receipt.lifecycle,receipt.sequence,receipt.event_hash,context.connector_id,context.repository_id,context.source_instance_id,context.connector_revision,context.source_instance_revision,context.source_generation],|r|r.get(0))?;
        if !exact {
            return Err(ProductError::Invariant(
                "retry requires exact scoped admission receipt",
            ));
        }
        if tx.execute("UPDATE product_intake_inbox_v1 SET state='retryable',retry_at=?1 WHERE delivery_id=?2 AND state IN ('admitted','retryable')",params![retry_at.to_rfc3339(),delivery_id])?!=1{return Err(ProductError::Invariant("delivery cannot be deferred"));}
        tx.commit()?;
        Ok(())
    }

    pub fn reject_intake_delivery(
        &mut self,
        delivery_id: &str,
        receipt: &AdmissionReceipt,
    ) -> Result<(), ProductError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if scope_enforced_in(&tx)? {
            return Err(ProductError::Invariant("intake service authority required"));
        }
        let exact: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM product_intake_inbox_v1 i JOIN product_orders_v1 o ON o.connector_id=i.connector_id AND o.repository_id=i.repository_id AND o.issue_id=i.issue_id WHERE i.delivery_id=?1 AND i.state IN ('admitted','retryable') AND o.order_id=?2 AND o.lifecycle=?3 AND o.admission_seq=?4 AND o.admission_hash=?5)", params![delivery_id,receipt.order_id,receipt.lifecycle,receipt.sequence,receipt.event_hash], |r| r.get(0))?;
        if !exact {
            return Err(ProductError::Invariant(
                "rejection requires exact durable admission receipt",
            ));
        }
        tx.execute("UPDATE product_intake_inbox_v1 SET state='rejected',retry_at=NULL WHERE delivery_id=?1", [delivery_id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn reject_intake_delivery_for_service(
        &mut self,
        context: &ProductServiceContext,
        delivery_id: &str,
        receipt: &AdmissionReceipt,
        now: DateTime<Utc>,
    ) -> Result<(), ProductError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        service::require_service_context_in(
            &tx,
            context,
            ProductServiceAction::AdmitDelivery,
            now,
        )?;
        let exact:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM product_intake_inbox_v1 i JOIN product_orders_v1 o ON o.connector_id=i.connector_id AND o.repository_id=i.repository_id AND o.issue_id=i.issue_id JOIN product_intake_source_bindings_v1 b USING(delivery_id) WHERE i.delivery_id=?1 AND i.connector_id=?6 AND i.repository_id=?7 AND b.source_instance_id=?8 AND b.source_revision=?9 AND b.source_instance_revision=?10 AND b.source_generation=?11 AND i.state IN ('admitted','retryable') AND o.order_id=?2 AND o.lifecycle=?3 AND o.admission_seq=?4 AND o.admission_hash=?5)",params![delivery_id,receipt.order_id,receipt.lifecycle,receipt.sequence,receipt.event_hash,context.connector_id,context.repository_id,context.source_instance_id,context.connector_revision,context.source_instance_revision,context.source_generation],|r|r.get(0))?;
        if !exact {
            return Err(ProductError::Invariant(
                "rejection requires exact scoped admission receipt",
            ));
        }
        tx.execute("UPDATE product_intake_inbox_v1 SET state='rejected',retry_at=NULL WHERE delivery_id=?1",[delivery_id])?;
        tx.commit()?;
        Ok(())
    }

    /// Claim one item only after an adapter has explicitly marked its durable
    /// backlog row `ready`. Ordinary GitHub issues enter `waiting_unsupported`
    /// until a task grammar/adapter is configured. The claim, epoch, fencing
    /// token and bounded lease are committed under SQLite's write lock.
    pub fn claim_product_work(
        &mut self,
        worker_id: &str,
        now: DateTime<Utc>,
        lease: chrono::Duration,
    ) -> Result<crate::product::ProductWorkClaimResult, ProductError> {
        #[cfg(feature = "a01-harness-support")]
        if self.open_mode.is_a01() {
            return Err(ProductError::Invariant("A01 isolated store work is disabled"));
        }
        #[cfg(feature = "w1-isolated-test-support")]
        if self.open_mode.is_pin() { return Err(ProductError::ContractMismatch); }
        if worker_id.trim().is_empty()
            || worker_id.len() > 128
            || lease <= chrono::Duration::zero()
            || lease > chrono::Duration::hours(1)
        {
            return Err(ProductError::InvalidConfig);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if scope_enforced_in(&tx)? {
            return Err(ProductError::Invariant(
                "product worker service scope is not provisioned",
            ));
        }
        let candidate: Option<(String,i64,String,String,String,i64,i64,String,i64,i64,i64,Option<String>)> = tx.query_row(
            "SELECT o.order_id,o.lifecycle,b.delivery_id,o.task_kind,o.payload_snapshot,o.source_revision,o.admission_seq,o.admission_hash,s.revision,s.enabled,COALESCE(c.epoch,0),c.lease_until FROM product_work_backlog_v1 b JOIN product_orders_v1 o ON o.order_id=b.order_id AND o.lifecycle=b.lifecycle JOIN product_sources_v1 s ON s.connector_id=o.connector_id LEFT JOIN product_work_claims_v1 c ON c.order_id=b.order_id AND c.lifecycle=b.lifecycle WHERE b.state IN ('ready','retryable','claimed') AND b.available_at<=?1 AND (b.state!='claimed' OR c.lease_until IS NULL OR c.lease_until<=?1) ORDER BY b.available_at,o.order_id LIMIT 1",
            [now.to_rfc3339()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get(10)?,r.get(11)?)),
        ).optional()?;
        let Some((
            order_id,
            lifecycle,
            delivery_id,
            task_kind,
            payload,
            source_revision,
            admission_seq,
            admission_hash,
            current_revision,
            enabled,
            old_epoch,
            old_lease,
        )) = candidate
        else {
            let unsupported: Option<(String,String)> = tx.query_row(
                "SELECT o.order_id,o.task_kind FROM product_work_backlog_v1 b JOIN product_orders_v1 o ON o.order_id=b.order_id AND o.lifecycle=b.lifecycle WHERE b.state='waiting_unsupported' ORDER BY b.available_at,o.order_id LIMIT 1",
                [], |r| Ok((r.get(0)?,r.get(1)?)),
            ).optional()?;
            tx.commit()?;
            if let Some((order_id, task_kind)) = unsupported {
                return Ok(crate::product::ProductWorkClaimResult::Unsupported {
                    order_id,
                    task_kind,
                });
            }
            return Ok(crate::product::ProductWorkClaimResult::Empty);
        };
        if enabled != 1 || current_revision != source_revision {
            return Err(ProductError::Invariant(
                "work source is disabled or was rebound",
            ));
        }
        let anchor: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM product_transition_receipts_v1 WHERE seq=?1 AND event_hash=?2 AND order_id=?3 AND lifecycle=?4 AND operation='github-admission')",
            params![admission_seq,admission_hash,order_id,lifecycle], |r| r.get(0),
        )?;
        if !anchor {
            return Err(ProductError::Invariant(
                "work admission anchor is not authoritative",
            ));
        }
        if old_lease.as_deref().is_some_and(|until| {
            DateTime::parse_from_rfc3339(until)
                .map(|v| v.with_timezone(&Utc) > now)
                .unwrap_or(true)
        }) {
            tx.commit()?;
            return Ok(crate::product::ProductWorkClaimResult::Empty);
        }
        let epoch = old_epoch + 1;
        tx.execute(
            "UPDATE product_fence_counter_v1 SET next=next+1 WHERE singleton=1",
            [],
        )?;
        let fence: i64 = tx.query_row(
            "SELECT next FROM product_fence_counter_v1 WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        let payload_digest = payload_digest(&task_kind, &payload);
        let lease_until = now + lease;
        tx.execute("INSERT INTO product_work_claims_v1(order_id,lifecycle,owner_id,epoch,fence,state,lease_until,source_revision,payload_digest) VALUES(?1,?2,?3,?4,?5,'claimed',?6,?7,?8) ON CONFLICT(order_id,lifecycle) DO UPDATE SET owner_id=excluded.owner_id,epoch=excluded.epoch,fence=excluded.fence,state='claimed',lease_until=excluded.lease_until,source_revision=excluded.source_revision,payload_digest=excluded.payload_digest", params![order_id,lifecycle,worker_id,epoch,fence,lease_until.to_rfc3339(),source_revision,payload_digest])?;
        tx.execute("UPDATE product_work_backlog_v1 SET state='claimed',attempts=attempts+1 WHERE order_id=?1 AND lifecycle=?2 AND state IN ('ready','retryable','claimed')", params![order_id,lifecycle])?;
        tx.commit()?;
        Ok(crate::product::ProductWorkClaimResult::Claimed(
            crate::product::ProductWorkClaim {
                order_id,
                lifecycle,
                delivery_id,
                worker_id: worker_id.to_owned(),
                epoch,
                fence,
                source_revision,
                payload_digest,
                service_binding: None,
                lease_until,
                task_kind,
                payload_snapshot: payload,
            },
        ))
    }

    /// Claim only work for a currently authorized source instance. The source
    /// action check and durable owner/fence assignment share this write tx.
    pub fn claim_product_work_for_service(
        &mut self,
        context: &ProductServiceContext,
        worker_id: &str,
        now: DateTime<Utc>,
        lease: chrono::Duration,
    ) -> Result<ProductWorkClaimResult, ProductError> {
        #[cfg(feature = "a01-harness-support")]
        if self.open_mode.is_a01_task() {
            return a01_task::claim(self, context, worker_id, lease, &mut a01_task::TaskHooks::default());
        }
        #[cfg(feature = "a01-harness-support")]
        if self.open_mode.is_a01() {
            return Err(ProductError::Invariant("A01 isolated store work is disabled"));
        }
        #[cfg(feature = "w1-isolated-test-support")]
        if self.open_mode.is_pin() { return Err(ProductError::ContractMismatch); }
        #[cfg(feature = "w1-isolated-test-support")]
        if self.open_mode.is_capture() {
            return w1_capture_claim_private(self, context, worker_id, lease);
        }
        if worker_id.trim().is_empty()
            || worker_id.len() > 128
            || lease <= chrono::Duration::zero()
            || lease > chrono::Duration::hours(1)
        {
            return Err(ProductError::InvalidConfig);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        service::require_service_context_in(&tx, context, ProductServiceAction::DriveReview, now)?;
        let candidate: Option<(String,i64,String,String,String,i64,i64,String,i64,i64,i64,Option<String>)> = tx.query_row(
            "SELECT o.order_id,o.lifecycle,b.delivery_id,o.task_kind,o.payload_snapshot,o.source_revision,o.admission_seq,o.admission_hash,s.revision,s.enabled,COALESCE(c.epoch,0),c.lease_until FROM product_work_backlog_v1 b JOIN product_orders_v1 o ON o.order_id=b.order_id AND o.lifecycle=b.lifecycle JOIN product_sources_v1 s ON s.connector_id=o.connector_id JOIN product_scope_order_provenance_v1 p ON p.order_id=o.order_id AND p.lifecycle=o.lifecycle AND p.verified=1 AND p.source_instance_id=?2 LEFT JOIN product_work_claims_v1 c ON c.order_id=b.order_id AND c.lifecycle=b.lifecycle WHERE o.connector_id=?3 AND o.repository_id=?4 AND (b.state IN ('ready','retryable') OR (b.state IN ('claimed','completed') AND (c.lease_until IS NULL OR c.lease_until<=?1))) AND b.available_at<=?1 ORDER BY b.available_at,o.order_id LIMIT 1",
            params![now.to_rfc3339(),context.source_instance_id,context.connector_id,context.repository_id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get(10)?,r.get(11)?)),
        ).optional()?;
        let Some((
            order_id,
            lifecycle,
            delivery_id,
            task_kind,
            payload,
            source_revision,
            admission_seq,
            admission_hash,
            current_revision,
            enabled,
            old_epoch,
            old_lease,
        )) = candidate
        else {
            tx.commit()?;
            return Ok(ProductWorkClaimResult::Empty);
        };
        if enabled != 1
            || current_revision != source_revision
            || source_revision != context.source_revision
        {
            return Err(ProductError::Invariant(
                "work source was disabled or rebound",
            ));
        }
        let anchor: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM product_transition_receipts_v1 WHERE seq=?1 AND event_hash=?2 AND order_id=?3 AND lifecycle=?4 AND operation='github-admission')",params![admission_seq,admission_hash,order_id,lifecycle],|r|r.get(0))?;
        if !anchor {
            return Err(ProductError::Invariant(
                "work admission anchor is not authoritative",
            ));
        }
        if old_lease.as_deref().is_some_and(|until| {
            DateTime::parse_from_rfc3339(until)
                .map(|v| v.with_timezone(&Utc) > now)
                .unwrap_or(true)
        }) {
            tx.commit()?;
            return Ok(ProductWorkClaimResult::Empty);
        }
        let epoch = old_epoch
            .checked_add(1)
            .ok_or(ProductError::InvalidConfig)?;
        tx.execute(
            "UPDATE product_fence_counter_v1 SET next=next+1 WHERE singleton=1",
            [],
        )?;
        let fence: i64 = tx.query_row(
            "SELECT next FROM product_fence_counter_v1 WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        let digest = payload_digest(&task_kind, &payload);
        let lease_until = now + lease;
        tx.execute("INSERT INTO product_work_claims_v1(order_id,lifecycle,owner_id,epoch,fence,state,lease_until,source_revision,payload_digest) VALUES(?1,?2,?3,?4,?5,'claimed',?6,?7,?8) ON CONFLICT(order_id,lifecycle) DO UPDATE SET owner_id=excluded.owner_id,epoch=excluded.epoch,fence=excluded.fence,state='claimed',lease_until=excluded.lease_until,source_revision=excluded.source_revision,payload_digest=excluded.payload_digest",params![order_id,lifecycle,worker_id,epoch,fence,lease_until.to_rfc3339(),source_revision,digest])?;
        tx.execute("UPDATE product_work_backlog_v1 SET state='claimed',attempts=attempts+1 WHERE order_id=?1 AND lifecycle=?2 AND state IN ('ready','retryable','claimed','completed')",params![order_id,lifecycle])?;
        tx.execute("INSERT INTO product_service_work_bindings_v1(order_id,lifecycle,service_id,service_revision,credential_revision,source_instance_id) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(order_id,lifecycle) DO UPDATE SET service_id=excluded.service_id,service_revision=excluded.service_revision,credential_revision=excluded.credential_revision,source_instance_id=excluded.source_instance_id",params![order_id,lifecycle,context.service_id,context.service_revision,context.credential_revision,context.source_instance_id])?;
        tx.commit()?;
        Ok(ProductWorkClaimResult::Claimed(ProductWorkClaim {
            order_id,
            lifecycle,
            delivery_id,
            worker_id: worker_id.to_owned(),
            epoch,
            fence,
            source_revision,
            payload_digest: digest,
            service_binding: Some(foundry_exec::ReviewServiceBinding {
                service_id: context.service_id.clone(),
                service_revision: context.service_revision,
                credential_revision: context.credential_revision,
                source_instance_id: context.source_instance_id.clone(),
            }),
            lease_until,
            task_kind,
            payload_snapshot: payload,
        }))
    }

    /// Recheck a claim immediately before any adapter side effect. A reclaimed
    /// claim, expired lease, changed source, or altered snapshot fails closed.
    pub fn validate_product_work_claim(
        &self,
        claim: &crate::product::ProductWorkClaim,
        now: DateTime<Utc>,
    ) -> Result<(), ProductError> {
        #[cfg(feature = "a01-harness-support")]
        if self.open_mode.is_a01() {
            return Err(ProductError::Invariant("A01 isolated store work is disabled"));
        }
        #[cfg(feature = "w1-isolated-test-support")]
        if self.open_mode.is_pin() { return Err(ProductError::ContractMismatch); }
        let exact: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM product_work_claims_v1 c JOIN product_work_backlog_v1 b ON b.order_id=c.order_id AND b.lifecycle=c.lifecycle JOIN product_orders_v1 o ON o.order_id=c.order_id AND o.lifecycle=c.lifecycle JOIN product_sources_v1 s ON s.connector_id=o.connector_id JOIN product_transition_receipts_v1 r ON r.seq=o.admission_seq AND r.event_hash=o.admission_hash WHERE c.order_id=?1 AND c.lifecycle=?2 AND c.owner_id=?3 AND c.epoch=?4 AND c.fence=?5 AND c.state='claimed' AND c.lease_until>?6 AND c.source_revision=?7 AND s.revision=?7 AND s.enabled=1 AND b.state='claimed' AND b.delivery_id=?8 AND c.payload_digest=?9 AND o.task_kind=?10 AND o.payload_snapshot=?11 AND r.operation='github-admission')",
            params![claim.order_id,claim.lifecycle,claim.worker_id,claim.epoch,claim.fence,now.to_rfc3339(),claim.source_revision,claim.delivery_id,claim.payload_digest,claim.task_kind,claim.payload_snapshot], |r| r.get(0),
        )?;
        if !exact {
            return Err(ProductError::Invariant(
                "work claim is stale, expired, or no longer authorized",
            ));
        }
        Ok(())
    }

    /// Return the persisted cursor so a restarted poller can resume its
    /// bounded sweep rather than restarting from page one.
    pub fn intake_cursor(
        &self,
        connector: &str,
        fingerprint: &str,
    ) -> Result<Option<crate::product_intake::ProductPollCursor>, ProductError> {
        if self.scope_policy_enforced()? {
            return Err(ProductError::Invariant("intake service authority required"));
        }
        let row: Option<(String,i64,i64,String)> = self.conn.query_row("SELECT sweep_id,next_page,page_ceiling,updated_at FROM product_poll_state_v1 WHERE connector_id=?1 AND query_fingerprint=?2", params![connector,fingerprint], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        Ok(row.map(|(sweep_id, next_page, page_ceiling, updated_at)| {
            crate::product_intake::ProductPollCursor {
                connector_id: connector.to_string(),
                query_fingerprint: fingerprint.to_string(),
                sweep_id,
                next_page: next_page as u32,
                page_ceiling: page_ceiling as u32,
                updated_at,
            }
        }))
    }

    pub fn intake_cursor_for_service(
        &self,
        context: &ProductServiceContext,
        fingerprint: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<crate::product_intake::ProductPollCursor>, ProductError> {
        let tx = self.conn.unchecked_transaction()?;
        service::require_service_context_in(&tx, context, ProductServiceAction::IntakeRead, now)?;
        let bound:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM product_poll_source_bindings_v1 WHERE connector_id=?1 AND query_fingerprint=?2 AND source_instance_id=?3 AND source_revision=?4 AND source_instance_revision=?5 AND source_generation=?6)",params![context.connector_id,fingerprint,context.source_instance_id,context.connector_revision,context.source_instance_revision,context.source_generation],|r|r.get(0))?;
        let row: Option<(String, i64, i64, String)> = if bound {
            tx.query_row("SELECT sweep_id,next_page,page_ceiling,updated_at FROM product_poll_state_v1 WHERE connector_id=?1 AND query_fingerprint=?2",params![context.connector_id,fingerprint],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?
        } else {
            None
        };
        tx.commit()?;
        Ok(row.map(|(sweep_id, next_page, page_ceiling, updated_at)| {
            crate::product_intake::ProductPollCursor {
                connector_id: context.connector_id.clone(),
                query_fingerprint: fingerprint.into(),
                sweep_id,
                next_page: next_page as u32,
                page_ceiling: page_ceiling as u32,
                updated_at,
            }
        }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Fail closed unless the configured, enabled source exactly matches the
    /// polling adapter's origin and stable repository identity.
    pub fn validate_intake_source(
        &self,
        connector: &str,
        origin: &str,
        repository_id: &str,
    ) -> Result<(), ProductError> {
        if self.scope_policy_enforced()? {
            return Err(ProductError::Invariant("intake service authority required"));
        }
        let configured: Option<(String, String, i64)> = self
            .conn
            .query_row(
                "SELECT origin,repository_id,enabled FROM product_sources_v1 WHERE connector_id=?1",
                [connector],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        match configured {
            Some((saved_origin, saved_repo, 1))
                if saved_origin == origin && saved_repo == repository_id =>
            {
                Ok(())
            }
            _ => Err(ProductError::Invariant(
                "connector is disabled, unconfigured, or bound to another source",
            )),
        }
    }

    pub fn validate_intake_source_for_service(
        &self,
        context: &ProductServiceContext,
        connector: &str,
        origin: &str,
        owner: &str,
        repo: &str,
        repository_id: &str,
        now: DateTime<Utc>,
    ) -> Result<(), ProductError> {
        if context.connector_id != connector
            || context.api_origin != origin
            || context.repository_id != repository_id
            || !context.api_owner.eq_ignore_ascii_case(owner)
            || !context.api_repo.eq_ignore_ascii_case(repo)
        {
            return Err(ProductError::Invariant(
                "source does not match service context",
            ));
        }
        self.require_product_service(context, ProductServiceAction::IntakeRead, now)
    }
    pub fn installation_id(&self) -> &str {
        &self.installation_id
    }
    pub fn database_id(&self) -> &str {
        &self.database_id
    }
    /// Current product scope mode, read from the authoritative database.
    /// Product entrypoints use this to refuse an unactivated database rather
    /// than falling back to compatibility authorization.
    pub fn scope_policy_enforced(&self) -> Result<bool, ProductError> {
        let mode: String = self.conn.query_row(
            "SELECT mode FROM product_scope_policy_state_v1 WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        Ok(mode == "enforced")
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ScopedS1ScanStats {
    chain_rows: usize,
    domain_rows: usize,
    peak_retained_rows: usize,
    peak_order_states: usize,
}

fn ledger_row_from_sql(row: &rusqlite::Row<'_>) -> rusqlite::Result<crate::LedgerRow> {
    Ok(crate::LedgerRow {
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
}

/// Performs both authoritative validations without materializing the ledger.
/// The returned counters are private test instrumentation: they count actual
/// cursor rows and retained verifier state during this traversal.
fn verify_scoped_s1(
    tx: &rusqlite::Transaction<'_>,
) -> Result<Option<ScopedS1ScanStats>, ProductError> {
    let mut stats = ScopedS1ScanStats::default();
    let mut chain = crate::sqlite::ChainVerifier::new();
    let mut hash_stmt = tx.prepare(
        "SELECT seq,order_id,from_state,to_state,event,actor,intent,at,prev_hash,hash,event_json
         FROM ledger ORDER BY seq",
    )?;
    let mut hash_rows = hash_stmt.query([])?;
    while let Some(row) = hash_rows.next()? {
        let row = ledger_row_from_sql(row)?;
        stats.peak_retained_rows = stats.peak_retained_rows.max(1);
        if !chain.push(&row) {
            return Ok(None);
        }
        stats.chain_rows = stats.chain_rows.saturating_add(1);
    }
    drop(hash_rows);
    drop(hash_stmt);

    let mut replay = crate::replay::GroupedOrderVerifier::new();
    let mut domain_stmt = tx.prepare(
        "SELECT seq,order_id,from_state,to_state,event,actor,intent,at,prev_hash,hash,event_json
         FROM ledger ORDER BY order_id,seq",
    )?;
    let mut domain_rows = domain_stmt.query([])?;
    while let Some(row) = domain_rows.next()? {
        let row = ledger_row_from_sql(row)?;
        stats.peak_retained_rows = stats.peak_retained_rows.max(1);
        if replay.push(&row).is_err() {
            return Ok(None);
        }
    }
    stats.domain_rows = replay.rows_seen();
    stats.peak_order_states = replay.peak_order_states();
    if stats.domain_rows != stats.chain_rows
        || stats.peak_retained_rows > 1
        || stats.peak_order_states > 1
    {
        return Ok(None);
    }
    Ok(Some(stats))
}

/// Existing order tuples are not sufficient idempotency evidence. Before a
/// service receives a historical receipt, prove the immutable admission is
/// bound to this exact source instance and canonical repository identity.
fn a01_hold_review_in(tx: &rusqlite::Transaction<'_>, order: &str, state: &str) -> Result<bool,ProductError> {
    #[cfg(feature = "a01-harness-support")]
    if state == "review" { return a01_task::canonical_review_in(tx,order); }
    let _ = (tx,order,state); Ok(false)
}

fn verify_order_source_provenance_in(
    tx: &rusqlite::Transaction<'_>,
    order_id: &str,
    lifecycle: i64,
    context: &ProductServiceContext,
) -> Result<(), ProductError> {
    let binding: Option<(String, String, String, String)> = tx
        .query_row(
            "SELECT i.source_instance_id,i.connector_id,r.api_origin,r.stable_repository_id
         FROM product_scope_order_provenance_v1 p
         JOIN product_scope_source_instances_v1 i ON i.source_instance_id=p.source_instance_id
         JOIN product_scope_repositories_v1 r ON r.repository_key=i.repository_key
         WHERE p.order_id=?1 AND p.lifecycle=?2 AND p.verified=1",
            params![order_id, lifecycle],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    match binding {
        Some((source, connector, origin, repository))
            if source == context.source_instance_id
                && connector == context.connector_id
                && origin == context.api_origin
                && repository == context.repository_id =>
        {
            Ok(())
        }
        _ => Err(ProductError::Invariant(
            "existing order provenance conflicts with current source",
        )),
    }
}

fn admit_order_in(
    tx: &rusqlite::Transaction<'_>,
    order_id: &str,
    connector: &str,
    repository_id: &str,
    issue_id: &str,
    task_kind: &str,
    payload: &str,
    at: DateTime<Utc>,
) -> Result<AdmissionReceipt, ProductError> {
    if let Some((existing_order, seq, hash, lifecycle)) = tx.query_row(
            "SELECT order_id, admission_seq, admission_hash, lifecycle FROM product_orders_v1 WHERE connector_id=?1 AND repository_id=?2 AND issue_id=?3",
            params![connector, repository_id, issue_id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?)),
        ).optional()? {
            let requested = payload_digest(task_kind, payload);
            let stored: String = tx.query_row("SELECT payload_digest FROM product_order_payload_digests_v1 WHERE order_id=?1", [&existing_order], |r| r.get(0)).optional()?.unwrap_or_default();
            if stored != requested { return Err(ProductError::Invariant("stable issue identity payload conflict")); }
            return Ok(AdmissionReceipt { order_id: existing_order, lifecycle, sequence: seq, event_hash: hash });
        }
    let (source_repo, source_rev): (String, i64) = tx.query_row(
            "SELECT repository_id,revision FROM product_sources_v1 WHERE connector_id=?1 AND enabled=1", [connector], |r| Ok((r.get(0)?,r.get(1)?)),
        ).optional()?.ok_or(ProductError::Invariant("connector is disabled or unconfigured"))?;
    if source_repo != repository_id {
        return Err(ProductError::Invariant(
            "repository is outside configured source scope",
        ));
    }
    let head: Option<(i64, String)> = tx
        .query_row(
            "SELECT seq,hash FROM ledger ORDER BY seq DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let prev = head.map(|(_, h)| h).unwrap_or_else(|| "GENESIS".into());
    let seq: i64 = tx.query_row("SELECT COALESCE(MAX(seq),0)+1 FROM ledger", [], |r| {
        r.get(0)
    })?;
    let at_s = at.to_rfc3339();
    let event = OrderEvent::Queued;
    let event_s = format!("{event:?}");
    let actor = ActorId {
        kind: ActorKind::Agent,
        source: "github-intake".into(),
        id: connector.into(),
    };
    let actor_s = format!("{:?}/{}", actor.kind, actor.id);
    let json = crate::event_json::event_to_json(&event);
    let v1 = format!("{seq}|{order_id}|received|queued|{event_s}|{actor_s}||{at_s}|{prev}");
    let hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(
            crate::event_json::v2_hash_payload(&v1, &json).as_bytes(),
        ))
    };
    let lifecycle = 1;
    tx.execute("INSERT INTO product_transition_intents_v1(order_id,lifecycle,expected_predecessor,operation) VALUES(?1,?2,0,'github-admission')", params![order_id,lifecycle])?;
    tx.execute("INSERT INTO ledger(order_id,from_state,to_state,event,actor,intent,at,prev_hash,event_json,hash) VALUES(?1,'received','queued',?2,?3,'',?4,?5,?6,?7)", params![order_id,event_s,actor_s,at_s,prev,json,hash])?;
    tx.execute("INSERT INTO product_orders_v1(order_id,lifecycle,connector_id,repository_id,issue_id,task_kind,payload_snapshot,source_revision,admission_seq,admission_hash) VALUES(?1,1,?2,?3,?4,?5,?6,?7,?8,?9)", params![order_id,connector,repository_id,issue_id,task_kind,payload,source_rev,seq,hash])?;
    tx.execute(
        "INSERT INTO product_order_payload_digests_v1(order_id,payload_digest) VALUES(?1,?2)",
        params![order_id, payload_digest(task_kind, payload)],
    )?;
    let scope_enforced = scope_enforced_in(tx)?;
    let scoped_source = tx
        .query_row(
            "SELECT i.source_instance_id,i.enabled,r.stable_repository_id
             FROM product_scope_current_sources_v1 c
             JOIN product_scope_source_instances_v1 i USING(source_instance_id)
             JOIN product_scope_repositories_v1 r USING(repository_key)
             WHERE c.connector_id=?1",
            [connector],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    if let Some((source_instance_id, source_enabled, locator)) = scoped_source {
        if source_enabled != 1 || locator != repository_id {
            return Err(ProductError::Invariant(
                "scoped source is disabled or stable repository identity changed",
            ));
        }
        tx.execute(
            "INSERT INTO product_scope_order_provenance_v1(order_id,lifecycle,source_instance_id,admission_seq,admission_hash,verified) VALUES(?1,?2,?3,?4,?5,1)",
            params![order_id,lifecycle,source_instance_id,seq,hash],
        )?;
    } else if scope_enforced {
        return Err(ProductError::Invariant(
            "scoped source instance is required for product admission",
        ));
    }
    Ok(AdmissionReceipt {
        order_id: order_id.into(),
        lifecycle,
        sequence: seq,
        event_hash: hash,
    })
}

fn canonical_scope_identity(
    identity: &RepositoryIdentity,
) -> Result<RepositoryIdentity, ProductError> {
    RepositoryIdentity::canonicalized(
        &identity.organization,
        &identity.provider,
        &identity.api_origin,
        &identity.stable_repository_id,
    )
    .map_err(|_| ProductError::InvalidConfig)
}

fn validate_scope_source(source: &ScopeSourceManifest) -> Result<(), ProductError> {
    if source.connector_id.trim().is_empty()
        || source.source_instance_id.trim().is_empty()
        || source.generation <= 0
        || source.legacy_repository_locator.trim().is_empty()
        || source.scope_query.trim().is_empty()
    {
        return Err(ProductError::InvalidConfig);
    }
    if canonical_scope_identity(&source.identity)? != source.identity {
        return Err(ProductError::InvalidConfig);
    }
    let (Some(owner), Some(repo)) = (source.api_owner.as_deref(), source.api_repo.as_deref())
    else {
        return Err(ProductError::Invariant(
            "explicit API locator mapping is required; legacy locator data is not inferred",
        ));
    };
    validate_api_locator_parts(owner, repo)?;
    Ok(())
}

fn validate_api_locator_parts(owner: &str, repo: &str) -> Result<(), ProductError> {
    let valid = |value: &str| {
        !value.is_empty()
            && value.len() <= 256
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            && !value.starts_with('.')
            && !value.ends_with('.')
            && value != "."
            && value != ".."
    };
    if !valid(owner) || !valid(repo) {
        return Err(ProductError::InvalidConfig);
    }
    Ok(())
}

fn insert_scope_source(
    tx: &rusqlite::Transaction<'_>,
    source: &ScopeSourceManifest,
    identity: &RepositoryIdentity,
) -> Result<(), ProductError> {
    validate_scope_source(source)?;
    let repository_key = identity.key();
    tx.execute(
        "INSERT INTO product_scope_repositories_v1(repository_key,organization,provider,api_origin,stable_repository_id) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(repository_key) DO NOTHING",
        params![repository_key,identity.organization,identity.provider,identity.api_origin,identity.stable_repository_id],
    )?;
    let stored_identity: (String, String, String, String) = tx.query_row(
        "SELECT organization,provider,api_origin,stable_repository_id FROM product_scope_repositories_v1 WHERE repository_key=?1",
        [&repository_key],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
    )?;
    if stored_identity
        != (
            identity.organization.clone(),
            identity.provider.clone(),
            identity.api_origin.clone(),
            identity.stable_repository_id.clone(),
        )
    {
        return Err(ProductError::Invariant(
            "repository identity hash collision",
        ));
    }
    tx.execute(
        "INSERT INTO product_scope_source_instances_v1(source_instance_id,connector_id,repository_key,generation,revision,legacy_repository_locator,scope_query,enabled,retired_at) VALUES(?1,?2,?3,?4,1,?5,?6,?7,NULL)",
        params![source.source_instance_id,source.connector_id,repository_key,source.generation,source.legacy_repository_locator,source.scope_query,i64::from(source.enabled)],
    )?;
    let (owner, repo) = (
        source
            .api_owner
            .as_deref()
            .ok_or(ProductError::InvalidConfig)?,
        source
            .api_repo
            .as_deref()
            .ok_or(ProductError::InvalidConfig)?,
    );
    tx.execute(
        "INSERT INTO product_scope_api_locators_v1(source_instance_id,api_owner,api_repo) VALUES(?1,?2,?3)",
        params![source.source_instance_id, owner, repo],
    )?;
    Ok(())
}

fn insert_initial_scope_grant(
    tx: &rusqlite::Transaction<'_>,
    actor_id: &str,
    source_instance_id: &str,
    capability: ProductCapability,
    created_by: &str,
    request_key: &str,
) -> Result<(), ProductError> {
    let scope_revision: i64 = tx.query_row(
        "SELECT revision FROM product_scope_subject_revisions_v1 WHERE subject_id=?1 AND source_instance_id=?2",
        params![actor_id, source_instance_id],
        |row| row.get(0),
    ).optional()?.unwrap_or(0) + 1;
    let grant_id = crate::product_scope::framed_hash(
        "foundry-product-scope-grant-v1",
        &[actor_id, source_instance_id, capability.as_str(), "1"],
    );
    let digest = crate::product_scope::framed_hash(
        "foundry-product-scope-migration-grant-v1",
        &[
            actor_id,
            source_instance_id,
            capability.as_str(),
            created_by,
            request_key,
        ],
    );
    tx.execute(
        "INSERT INTO product_scope_subject_revisions_v1(subject_id,source_instance_id,revision) VALUES(?1,?2,?3)
         ON CONFLICT(subject_id,source_instance_id) DO UPDATE SET revision=excluded.revision",
        params![actor_id,source_instance_id,scope_revision],
    )?;
    tx.execute(
        "INSERT INTO product_scope_grants_v1(grant_id,subject_id,source_instance_id,capability,grant_revision,enabled,created_by,request_key) VALUES(?1,?2,?3,?4,1,1,?5,?6)",
        params![grant_id,actor_id,source_instance_id,capability.as_str(),created_by,request_key],
    )?;
    tx.execute(
        "INSERT INTO product_scope_events_v1(actor_id,subject_id,source_instance_id,capability,operation,scope_revision,grant_revision,request_key,request_digest,created_at) VALUES(?1,?2,?3,?4,'grant',?5,1,?6,?7,strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        params![created_by,actor_id,source_instance_id,capability.as_str(),scope_revision,request_key,digest],
    )?;
    Ok(())
}

fn scoped_order_in(
    tx: &rusqlite::Transaction<'_>,
    principal: &ProductPrincipal,
    order_id: &str,
    capability: ProductCapability,
) -> Result<ScopedOrderView, ProductError> {
    if !capability.allowed_for_role(&principal.role) {
        return Err(ProductError::Invariant("role does not permit capability"));
    }
    let binding: Option<(i64,String,i64,String,String,i64)> = tx.query_row(
        "SELECT o.lifecycle,p.source_instance_id,p.admission_seq,p.admission_hash,i.repository_key,i.revision
         FROM product_orders_v1 o
         JOIN product_scope_order_provenance_v1 p ON p.order_id=o.order_id AND p.lifecycle=o.lifecycle AND p.verified=1
         JOIN product_scope_source_instances_v1 i USING(source_instance_id)
         WHERE o.order_id=?1",
        [order_id],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
    ).optional()?;
    let Some((
        lifecycle,
        source_instance_id,
        admission_sequence,
        admission_hash,
        repository_key,
        source_revision,
    )) = binding
    else {
        return Err(ProductError::Invariant(
            "order scope provenance unavailable",
        ));
    };
    let grant_revision: Option<i64> = tx.query_row(
        "SELECT grant_revision FROM product_scope_grants_v1 WHERE subject_id=?1 AND source_instance_id=?2 AND capability=?3 AND enabled=1",
        params![principal.actor_id,source_instance_id,capability.as_str()],
        |r| r.get(0),
    ).optional()?;
    let Some(grant_revision) = grant_revision else {
        return Err(ProductError::Invariant(
            "repository capability grant unavailable",
        ));
    };
    let scope_revision: i64 = tx.query_row(
        "SELECT revision FROM product_scope_subject_revisions_v1 WHERE subject_id=?1 AND source_instance_id=?2",
        params![principal.actor_id,source_instance_id],
        |r| r.get(0),
    ).optional()?.ok_or(ProductError::Invariant("repository scope revision unavailable"))?;
    let stored: (i64, String, i64) = tx.query_row(
        "SELECT lifecycle,admission_hash,admission_seq FROM product_orders_v1 WHERE order_id=?1",
        [order_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if stored.0 != lifecycle || stored.1 != admission_hash || stored.2 != admission_sequence {
        return Err(ProductError::Invariant("order provenance binding changed"));
    }
    let receipt: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM product_transition_receipts_v1 WHERE seq=?1 AND event_hash=?2 AND order_id=?3 AND lifecycle=?4)",
        params![admission_sequence,admission_hash,order_id,lifecycle],
        |r| r.get(0),
    )?;
    if !receipt {
        return Err(ProductError::Invariant(
            "order admission receipt unavailable",
        ));
    }
    let (last_sequence, current_lifecycle, state) = verified_order_state_in(tx, order_id)?;
    if i64::from(current_lifecycle) + 1 != lifecycle || last_sequence < admission_sequence {
        return Err(ProductError::Invariant(
            "order lifecycle verification failed",
        ));
    }
    Ok(ScopedOrderView {
        order_id: order_id.into(),
        lifecycle,
        state,
        source_instance_id,
        repository_key,
        admission_sequence,
        source_revision,
        scope_revision,
        grant_revision,
    })
}

fn scope_enforced_in(tx: &rusqlite::Transaction<'_>) -> Result<bool, ProductError> {
    let mode: String = tx.query_row(
        "SELECT mode FROM product_scope_policy_state_v1 WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    Ok(mode == "enforced")
}

fn create_product_publish_schema(tx: &rusqlite::Transaction<'_>) -> Result<(), ProductError> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS product_publish_outcomes_v1(
            operation_id TEXT PRIMARY KEY REFERENCES product_outcomes_v1(operation_id),
            receipt_json TEXT NOT NULL,
            state TEXT NOT NULL CHECK(state IN ('pending','confirmed')),
            pr_url TEXT,
            started_at INTEGER NOT NULL,
            settled_at INTEGER
         );
         CREATE TRIGGER IF NOT EXISTS product_publish_confirmed_no_update
           BEFORE UPDATE ON product_publish_outcomes_v1
           WHEN OLD.state='confirmed'
           BEGIN SELECT RAISE(ABORT,'confirmed product publish outcome is immutable'); END;
         CREATE TRIGGER IF NOT EXISTS product_publish_confirmed_no_delete
           BEFORE DELETE ON product_publish_outcomes_v1
           WHEN OLD.state='confirmed'
           BEGIN SELECT RAISE(ABORT,'confirmed product publish outcome is immutable'); END;
         CREATE TRIGGER IF NOT EXISTS product_publish_pending_transition_only
           BEFORE UPDATE ON product_publish_outcomes_v1
           WHEN OLD.state!='pending' OR NEW.state!='confirmed'
             OR NEW.operation_id!=OLD.operation_id
             OR NEW.receipt_json!=OLD.receipt_json
             OR NEW.started_at!=OLD.started_at
             OR NEW.pr_url IS NULL OR NEW.settled_at IS NULL
           BEGIN SELECT RAISE(ABORT,'invalid product publish settlement'); END;
         CREATE TRIGGER IF NOT EXISTS product_publish_outcome_no_replace
           BEFORE INSERT ON product_publish_outcomes_v1
           WHEN EXISTS(SELECT 1 FROM product_publish_outcomes_v1 WHERE operation_id=NEW.operation_id)
           BEGIN SELECT RAISE(ABORT,'product publish outcome row already exists'); END;",
    )?;
    Ok(())
}

fn validate_product_publish_receipt_in(
    tx: &rusqlite::Transaction<'_>,
    supplied: &ProductApprovalReceipt,
) -> Result<(), ProductError> {
    let supplied_json = serde_json::to_string(supplied)
        .map_err(|_| ProductError::Invariant("approval receipt serialization failed"))?;
    let stored: String = tx
        .query_row(
            "SELECT receipt_json FROM product_outcomes_v1 WHERE operation_id=?1",
            [&supplied.operation_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(ProductError::Invariant("approval receipt not committed"))?;
    if stored != supplied_json {
        return Err(ProductError::Invariant(
            "approval receipt does not match committed outcome",
        ));
    }
    let (source_enabled, source_revision, lifecycle, admission_seq): (i64, i64, i64, i64) = tx
        .query_row(
            "SELECT s.enabled,s.revision,o.lifecycle,o.admission_seq
             FROM product_orders_v1 o
             JOIN product_sources_v1 s ON s.connector_id=o.connector_id AND s.repository_id=o.repository_id
             WHERE o.order_id=?1",
            [&supplied.order_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?
        .ok_or(ProductError::Invariant("approved source binding unavailable"))?;
    if source_enabled != 1
        || source_revision != supplied.source_revision
        || lifecycle != supplied.lifecycle
    {
        return Err(ProductError::Invariant(
            "approved lifecycle or source binding changed",
        ));
    }
    let (head, replay_lifecycle, replay_state) = verified_order_state_in(tx, &supplied.order_id)?;
    if head < supplied.sequence
        || i64::from(replay_lifecycle + 1) != supplied.lifecycle
        || replay_state != "publishing"
        || supplied.sequence <= admission_seq
    {
        return Err(ProductError::Invariant(
            "approved S1 lifecycle is no longer current",
        ));
    }
    let (stored_tuple_digest,): (String,) = tx
        .query_row(
            "SELECT tuple_digest FROM product_result_bindings_v1
             WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND generation=?4",
            params![
                supplied.order_id,
                supplied.attempt,
                supplied.run_id,
                supplied.generation
            ],
            |row| Ok((row.get(0)?,)),
        )
        .optional()?
        .ok_or(ProductError::Invariant(
            "approved tuple binding unavailable",
        ))?;
    // The binding row is immutable (sealed triggers) and unique on the tuple
    // coordinates; its stored digest was computed from the promoted tuple at
    // bind time, so equality here proves the promoted tuple matches the
    // approval. The artifact digest is already pinned by the exact receipt
    // equality against product_outcomes_v1 above.
    if stored_tuple_digest != supplied.tuple_digest {
        return Err(ProductError::Invariant(
            "current promoted tuple differs from approval",
        ));
    }
    Ok(())
}

fn require_order_capability_in(
    tx: &rusqlite::Transaction<'_>,
    principal: &ProductPrincipal,
    order_id: &str,
    capability: ProductCapability,
    require_current_source: bool,
) -> Result<ScopedOrderView, ProductError> {
    let view = scoped_order_in(tx, principal, order_id, capability)?;
    if require_current_source {
        let current: Option<(String, i64)> = tx
            .query_row(
                "SELECT c.source_instance_id,i.enabled
                 FROM product_scope_current_sources_v1 c
                 JOIN product_scope_source_instances_v1 i USING(source_instance_id)
                 WHERE c.connector_id=(SELECT connector_id FROM product_orders_v1 WHERE order_id=?1)",
                [order_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if !current.is_some_and(|(source_id, enabled)| {
            source_id == view.source_instance_id && enabled == 1
        }) {
            return Err(ProductError::Invariant(
                "current source instance is unavailable",
            ));
        }
    }
    Ok(view)
}

#[cfg(feature = "w1-isolated-test-support")]
fn w1_text(s: &str) -> bool {
    !s.is_empty() && s.len() <= 256 && !s.as_bytes().contains(&0)
}

#[cfg(feature = "w1-isolated-test-support")]
fn w1_authorization_error() -> ProductError {
    ProductError::Invariant("w1 authorization or binding unavailable")
}

#[cfg(feature = "w1-isolated-test-support")]
fn w1_authorize_source(tx: &rusqlite::Transaction<'_>, actor: &str, source: &str) -> Result<(), ProductError> {
    let authorized: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM product_scope_source_instances_v1 i          JOIN product_scope_current_sources_v1 c ON c.source_instance_id=i.source_instance_id AND c.connector_id=i.connector_id          JOIN product_scope_grants_v1 g ON g.source_instance_id=i.source_instance_id AND g.subject_id=?1 AND g.capability=?3 AND g.enabled=1          JOIN product_scope_subject_revisions_v1 r ON r.source_instance_id=i.source_instance_id AND r.subject_id=g.subject_id          WHERE i.source_instance_id=?2 AND i.enabled=1)",
        params![actor, source, ProductCapability::ReadOrder.as_str()], |row| row.get(0),
    )?;
    if authorized { Ok(()) } else { Err(w1_authorization_error()) }
}

#[cfg(feature = "w1-isolated-test-support")]
fn w1_public_receipt(canonical: Vec<u8>) -> Result<W1OperationReceipt, ProductError> {
    let r = w1_owner_codec::decode_receipt(&canonical).map_err(|_| ProductError::ContractMismatch)?;
    let operation = match r.operation.as_str() {
        "approve" => W1Operation::Approve, "revoke" => W1Operation::Revoke, "select" => W1Operation::Select,
        _ => return Err(ProductError::ContractMismatch),
    };
    Ok(W1OperationReceipt { canonical, operation, installation_id: r.installation, database_id: r.database,
        actor_id: r.actor, authority_revision: r.authority, source_instance_id: r.source, digest: r.digest,
        expected_revision: r.expected, result_revision: r.result, request_key: r.key,
        order_id: r.order, lifecycle: r.lifecycle, approval_revision: r.approval })
}

#[cfg(feature = "w1-isolated-test-support")]
fn w1_authorize_order(tx: &rusqlite::Transaction<'_>, principal: &ProductPrincipal, order: &str, lifecycle: i64, source: &str) -> Result<(), ProductError> {
    let view = require_order_capability_in(tx, principal, order, ProductCapability::ReadOrder, true)
        .map_err(|e| match e {
            ProductError::Invariant(_) => w1_authorization_error(),
            ProductError::Database(rusqlite::Error::InvalidColumnType(..) | rusqlite::Error::IntegralValueOutOfRange(..) | rusqlite::Error::FromSqlConversionFailure(..)) => w1_authorization_error(),
            other => other,
        })?;
    if view.lifecycle != lifecycle || view.source_instance_id != source { return Err(w1_authorization_error()); }
    Ok(())
}

#[cfg(feature = "w1-isolated-test-support")]
const W1_COLUMNS: &str = "actor_id,request_key,operation,authority_revision,source_instance_id,digest,expected_revision,approval_result_revision,order_id,lifecycle,selection_result_revision,referenced_approval_revision,referenced_operation,request,receipt";

#[cfg(feature = "w1-isolated-test-support")]
type W1Row = Vec<rusqlite::types::Value>;
#[cfg(feature = "w1-isolated-test-support")]
fn w1_read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<W1Row> {
    (0..15).map(|i| row.get(i)).collect()
}
#[cfg(feature = "w1-isolated-test-support")]
fn w1_str(row: &W1Row, n: usize) -> Option<&str> {
    match &row[n] { rusqlite::types::Value::Text(s) => Some(s), _ => None }
}
#[cfg(feature = "w1-isolated-test-support")]
fn w1_int(row: &W1Row, n: usize) -> Option<i64> {
    match row[n] { rusqlite::types::Value::Integer(v) => Some(v), _ => None }
}
#[cfg(feature = "w1-isolated-test-support")]
fn w1_null(row: &W1Row, n: usize) -> bool { matches!(row[n], rusqlite::types::Value::Null) }

#[cfg(feature = "w1-isolated-test-support")]
fn w1_validate_row(row: &W1Row, installation: &str, database: &str) -> Result<(w1_owner_codec::Request, w1_owner_codec::Receipt, Vec<u8>), ProductError> {
    use rusqlite::types::Value;
    let bad = || ProductError::ContractMismatch;
    let (Value::Blob(q), Value::Blob(r)) = (&row[13], &row[14]) else { return Err(bad()); };
    let request = w1_owner_codec::decode_request(q).map_err(|_| bad())?;
    let receipt = w1_owner_codec::decode_receipt(r).map_err(|_| bad())?;
    let result = request.expected.checked_add(1).ok_or_else(bad)?;
    let selection = request.operation == "select";
    if request.installation != installation || request.database != database
        || w1_str(row,0) != Some(&request.actor) || w1_str(row,1) != Some(&request.key)
        || w1_str(row,2) != Some(&request.operation) || w1_str(row,4) != Some(&request.source)
        || w1_str(row,5) != Some(&request.digest) || w1_int(row,6) != Some(request.expected)
        || w1_int(row,3) != Some(receipt.authority)
        || receipt.installation != request.installation || receipt.database != request.database
        || receipt.operation != request.operation || receipt.actor != request.actor || receipt.source != request.source
        || receipt.digest != request.digest || receipt.expected != request.expected || receipt.result != result
        || receipt.key != request.key || receipt.order != request.order || receipt.lifecycle != request.lifecycle {
        return Err(bad());
    }
    if selection {
        if !w1_null(row,7) || w1_str(row,8) != request.order.as_deref() || w1_int(row,9) != request.lifecycle
            || w1_int(row,10) != Some(result) || w1_int(row,11) != receipt.approval
            || w1_str(row,12) != Some("approve") { return Err(bad()); }
    } else if w1_int(row,7) != Some(result) || !(8..=12).all(|i| w1_null(row,i)) || receipt.approval.is_some() {
        return Err(bad());
    }
    Ok((request, receipt, r.clone()))
}

#[cfg(feature = "w1-isolated-test-support")]
fn w1_replay(tx: &rusqlite::Transaction<'_>, principal: &ProductPrincipal, request: &w1_owner_codec::Request, request_bytes: &[u8]) -> Result<Option<W1OperationReceipt>, ProductError> {
    let sql = format!("SELECT {W1_COLUMNS} FROM product_w1_operations_v1 WHERE actor_id=?1 COLLATE BINARY AND request_key=?2 COLLATE BINARY");
    let row: Option<W1Row> = tx.query_row(&sql, params![principal.actor_id, request.key], w1_read_row).optional().map_err(|e| match e {
        rusqlite::Error::FromSqlConversionFailure(..) | rusqlite::Error::InvalidColumnType(..) | rusqlite::Error::IntegralValueOutOfRange(..) => w1_authorization_error(),
        other => ProductError::Database(other),
    })?;
    let Some(row) = row else { return Ok(None); };
    let source = w1_str(&row,4).filter(|s| w1_text(s)).ok_or_else(w1_authorization_error)?;
    w1_authorize_source(tx, &principal.actor_id, source)?;
    let pure = matches!(w1_str(&row,2), Some("approve" | "revoke")) && (8..=12).all(|i| w1_null(&row,i))
        && w1_str(&row,0).is_some() && w1_str(&row,1).is_some() && w1_int(&row,3).is_some()
        && w1_str(&row,5).is_some() && w1_int(&row,6).is_some() && w1_int(&row,7).is_some()
        && matches!(row[13], rusqlite::types::Value::Blob(_)) && matches!(row[14], rusqlite::types::Value::Blob(_));
    if !pure {
        let order = w1_str(&row,8).filter(|s| w1_text(s)).ok_or_else(w1_authorization_error)?;
        let lifecycle = w1_int(&row,9).filter(|n| *n > 0).ok_or_else(w1_authorization_error)?;
        w1_authorize_order(tx, principal, order, lifecycle, source)?;
    }
    let (stored, _, bytes) = w1_validate_row(&row, &request.installation, &request.database)?;
    w1_validate_histories(tx, &request.installation, &request.database, &stored.source, &stored.digest,
        stored.order.as_deref().zip(stored.lifecycle))?;
    if !matches!(&row[13], rusqlite::types::Value::Blob(b) if b == request_bytes) { return Err(ProductError::Invariant("w1 request key conflict")); }
    Ok(Some(w1_public_receipt(bytes)?))
}

// Validate the direct tuple's selections and their complete order sequences. Approval
// sequences reached from those orders are checked once without expanding them again.
#[cfg(feature = "w1-isolated-test-support")]
fn w1_validate_histories(tx: &rusqlite::Transaction<'_>, installation: &str, database: &str, source: &str, digest: &str, order: Option<(&str, i64)>) -> Result<(i64, bool, i64), ProductError> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut orders = BTreeSet::new();
    if let Some((o,l)) = order { orders.insert((o.to_owned(),l)); }
    let sql = format!("SELECT {W1_COLUMNS} FROM product_w1_operations_v1 WHERE source_instance_id=?1 COLLATE BINARY AND digest=?2 COLLATE BINARY AND (operation='select' OR order_id IS NOT NULL OR lifecycle IS NOT NULL OR selection_result_revision IS NOT NULL OR referenced_approval_revision IS NOT NULL OR referenced_operation IS NOT NULL)");
    let mut stmt = tx.prepare(&sql)?;
    let mut rows = stmt.query(params![source,digest])?;
    while let Some(row) = rows.next()? {
        let raw = w1_read_row(row).map_err(isolated_schema_error)?;
        let (q, _, _) = w1_validate_row(&raw, installation, database)?;
        if q.operation != "select" || q.source != source || q.digest != digest { return Err(ProductError::ContractMismatch); }
        orders.insert((q.order.ok_or(ProductError::ContractMismatch)?,q.lifecycle.ok_or(ProductError::ContractMismatch)?));
    }
    let mut approval_tuples = BTreeSet::new();
    approval_tuples.insert((source.to_owned(),digest.to_owned()));
    let mut references = Vec::new();
    let mut requested_latest = 0;
    for (o,l) in &orders {
        let sql = format!("SELECT {W1_COLUMNS} FROM product_w1_operations_v1 WHERE order_id=?1 COLLATE BINARY AND lifecycle=?2 ORDER BY selection_result_revision");
        let mut stmt = tx.prepare(&sql)?;
        let mut rows = stmt.query(params![o,l])?;
        let mut latest = 0i64;
        let mut bound_source: Option<String> = None;
        while let Some(row) = rows.next()? {
            let raw = w1_read_row(row).map_err(isolated_schema_error)?;
            let (q,r,_) = w1_validate_row(&raw, installation, database)?;
            if q.operation != "select" || q.order.as_deref() != Some(o) || q.lifecycle != Some(*l)
                || q.expected != latest || bound_source.as_ref().is_some_and(|s| s != &q.source) { return Err(ProductError::ContractMismatch); }
            latest = r.result;
            bound_source = Some(q.source.clone());
            approval_tuples.insert((q.source.clone(),q.digest.clone()));
            references.push((q.source,q.digest,r.approval.ok_or(ProductError::ContractMismatch)?));
        }
        if order == Some((o.as_str(),*l)) {
            if bound_source.as_deref().is_some_and(|s| s != source) { return Err(ProductError::ContractMismatch); }
            requested_latest = latest;
        }
    }
    let mut approvals = BTreeMap::new();
    for (s,d) in approval_tuples {
        let sql = format!("SELECT {W1_COLUMNS} FROM product_w1_operations_v1 WHERE source_instance_id=?1 COLLATE BINARY AND digest=?2 COLLATE BINARY AND operation IN ('approve','revoke') ORDER BY approval_result_revision");
        let mut stmt = tx.prepare(&sql)?;
        let mut rows = stmt.query(params![s,d])?;
        let mut latest = 0i64;
        let mut active = false;
        while let Some(row) = rows.next()? {
            let raw = w1_read_row(row).map_err(isolated_schema_error)?;
            let (q,r,_) = w1_validate_row(&raw, installation, database)?;
            if q.source != s || q.digest != d || q.expected != latest || q.operation != if active { "revoke" } else { "approve" } { return Err(ProductError::ContractMismatch); }
            latest = r.result;
            active = !active;
        }
        approvals.insert((s,d),(latest,active));
    }
    for (s,d,revision) in references {
        let Some((latest,_)) = approvals.get(&(s,d)) else { return Err(ProductError::ContractMismatch); };
        if revision <= 0 || revision > *latest || revision % 2 != 1 { return Err(ProductError::ContractMismatch); }
    }
    let (latest,active) = approvals.get(&(source.to_owned(),digest.to_owned())).copied().ok_or(ProductError::ContractMismatch)?;
    Ok((latest,active,requested_latest))
}

#[cfg(feature = "w1-isolated-test-support")]
#[derive(Debug, PartialEq, Eq)]
struct W1LatestSelection {
    order: String,
    lifecycle: i64,
    source: String,
    pack_digest: String,
    selection_revision: i64,
    referenced_approval_revision: i64,
    current_approval_head: i64,
    active: bool,
}

// This read is not authorization. Authenticate before invoking it, and repeat it
// after live authority checks in any future atomic admission transaction.
#[cfg(feature = "w1-isolated-test-support")]
fn w1_latest_selection_in(tx: &rusqlite::Transaction<'_>, installation: &str, database: &str, order: &str, lifecycle: i64, expected_source: &str) -> Result<W1LatestSelection, ProductError> {
    if !w1_text(order) || lifecycle <= 0 || !w1_text(expected_source) {
        return Err(ProductError::InvalidConfig);
    }
    // Every operation must be checked before filtering: any raw identity column
    // could conceal a canonical row. This scans the entire operation table on
    // every call, with no row limit; unrelated corruption makes the read fail.
    // Runtime grows with the table and relevant history; callers must account
    // for this cost. This is consistency checking, not database authenticity.
    {
        let sql = format!("SELECT {W1_COLUMNS} FROM product_w1_operations_v1");
        let mut stmt = tx.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let raw = w1_read_row(row).map_err(isolated_schema_error)?;
            w1_validate_row(&raw, installation, database)?;
        }
    }
    let sql = format!("SELECT {W1_COLUMNS} FROM product_w1_operations_v1 WHERE order_id=?1 COLLATE BINARY AND lifecycle=?2 ORDER BY selection_result_revision");
    let mut stmt = tx.prepare(&sql)?;
    let mut rows = stmt.query(params![order, lifecycle])?;
    let mut latest = None;
    let mut revision = 0;
    while let Some(row) = rows.next()? {
        let raw = w1_read_row(row).map_err(isolated_schema_error)?;
        let (request, receipt, _) = w1_validate_row(&raw, installation, database)?;
        if request.operation != "select" || request.order.as_deref() != Some(order)
            || request.lifecycle != Some(lifecycle) || request.expected != revision {
            return Err(ProductError::ContractMismatch);
        }
        revision = receipt.result;
        latest = Some((request, receipt));
    }
    let (request, receipt) = latest.ok_or(ProductError::Invariant("w1 selection unavailable"))?;
    if request.source != expected_source { return Err(ProductError::Invariant("w1 selection unavailable")); }
    let (head, active, validated_revision) = w1_validate_histories(tx, installation, database, &request.source, &request.digest, Some((order, lifecycle)))?;
    let referenced = receipt.approval.ok_or(ProductError::ContractMismatch)?;
    if validated_revision != revision { return Err(ProductError::ContractMismatch); }
    if !active || head != referenced { return Err(ProductError::Invariant("w1 selection unavailable")); }
    Ok(W1LatestSelection { order: order.to_owned(), lifecycle, source: request.source,
        pack_digest: request.digest, selection_revision: revision,
        referenced_approval_revision: referenced, current_approval_head: head, active })
}

// This is an empty storage container only. Selection and claim reference guards
// are deferred; a populated row does not establish authority.
#[cfg(feature = "w1-isolated-test-support")]
const W1_PIN_MARKER: &str = "CREATE TABLE product_w1_isolated_marker_v1(singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=4))";
#[cfg(feature = "w1-isolated-test-support")]
const W1_PIN_OBJECTS: [(&str, &str, &str); 4] = [
    ("product_w1_lifecycle_pins_v1", "table", "CREATE TABLE product_w1_lifecycle_pins_v1(order_id TEXT NOT NULL, lifecycle INTEGER NOT NULL CHECK(lifecycle>0), installation_id TEXT NOT NULL, database_id TEXT NOT NULL, source_instance_id TEXT NOT NULL, pack_digest TEXT NOT NULL, selection_revision INTEGER NOT NULL CHECK(selection_revision>0), referenced_approval_revision INTEGER NOT NULL CHECK(referenced_approval_revision>0), claim_epoch INTEGER NOT NULL CHECK(claim_epoch>0), claim_fence INTEGER NOT NULL CHECK(claim_fence>0), payload_digest TEXT NOT NULL, admission_seq INTEGER NOT NULL, admission_hash TEXT NOT NULL, PRIMARY KEY(order_id,lifecycle))"),
    ("product_w1_lifecycle_pins_no_update", "trigger", "CREATE TRIGGER product_w1_lifecycle_pins_no_update BEFORE UPDATE ON product_w1_lifecycle_pins_v1 BEGIN SELECT RAISE(ABORT,'immutable lifecycle pin'); END"),
    ("product_w1_lifecycle_pins_no_delete", "trigger", "CREATE TRIGGER product_w1_lifecycle_pins_no_delete BEFORE DELETE ON product_w1_lifecycle_pins_v1 BEGIN SELECT RAISE(ABORT,'immutable lifecycle pin'); END"),
    ("product_w1_lifecycle_pins_no_replace", "trigger", "CREATE TRIGGER product_w1_lifecycle_pins_no_replace BEFORE INSERT ON product_w1_lifecycle_pins_v1 WHEN EXISTS(SELECT 1 FROM product_w1_lifecycle_pins_v1 WHERE order_id=NEW.order_id AND lifecycle=NEW.lifecycle) BEGIN SELECT RAISE(ABORT,'immutable lifecycle pin'); END"),
];

#[cfg(feature = "w1-isolated-test-support")]
fn validate_w1_pin_schema(conn: &Connection) -> Result<(), ProductError> {
    // SQLite lower() folds ASCII; retain original names for exact inventory equality.
    let mut inventory = conn.prepare("SELECT type,name FROM sqlite_schema WHERE lower(name) GLOB 'product_w1_*' OR lower(tbl_name) GLOB 'product_w1_*' ORDER BY type,name").map_err(isolated_schema_error)?;
    let actual: Vec<(String,String)> = inventory.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(isolated_schema_error)?.collect::<rusqlite::Result<_>>().map_err(isolated_schema_error)?;
    let expected = [
        ("index","sqlite_autoindex_product_w1_claim_authority_v1_1"), ("index","sqlite_autoindex_product_w1_claim_authority_v1_2"),
        ("index","sqlite_autoindex_product_w1_lifecycle_pins_v1_1"),
        ("index","sqlite_autoindex_product_w1_operations_v1_1"), ("index","sqlite_autoindex_product_w1_operations_v1_2"), ("index","sqlite_autoindex_product_w1_operations_v1_3"), ("index","sqlite_autoindex_product_w1_operations_v1_4"),
        ("table","product_w1_claim_authority_v1"), ("table","product_w1_isolated_marker_v1"), ("table","product_w1_lifecycle_pins_v1"), ("table","product_w1_operations_v1"),
        ("trigger","product_w1_claim_authority_no_delete"), ("trigger","product_w1_claim_authority_no_replace"), ("trigger","product_w1_claim_authority_no_update"),
        ("trigger","product_w1_lifecycle_pins_no_delete"), ("trigger","product_w1_lifecycle_pins_no_replace"), ("trigger","product_w1_lifecycle_pins_no_update"),
        ("trigger","product_w1_operations_no_delete"), ("trigger","product_w1_operations_no_replace"), ("trigger","product_w1_operations_no_update"),
    ].map(|(t,n)| (t.to_string(), n.to_string()));
    if actual != expected { return Err(ProductError::ContractMismatch); }
    let mut stmt = conn.prepare("SELECT type,name,sql FROM sqlite_schema WHERE tbl_name='product_w1_lifecycle_pins_v1' ORDER BY type,name").map_err(isolated_schema_error)?;
    let actual: Vec<(String,String,Option<String>)> = stmt.query_map([], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(isolated_schema_error)?.collect::<rusqlite::Result<_>>().map_err(isolated_schema_error)?;
    let mut expected: Vec<(String,String,Option<String>)> = W1_PIN_OBJECTS.into_iter().map(|(n,t,s)| (t.into(),n.into(),Some(s.into()))).collect();
    expected.push(("index".into(), "sqlite_autoindex_product_w1_lifecycle_pins_v1_1".into(), None));
    expected.sort();
    if actual != expected { return Err(ProductError::ContractMismatch); }
    let count: i64 = conn.query_row("SELECT count(*) FROM product_w1_lifecycle_pins_v1", [], |r| r.get(0)).map_err(isolated_schema_error)?;
    if count != 0 { return Err(ProductError::ContractMismatch); }
    let mut fk = conn.prepare("PRAGMA foreign_key_list(product_w1_lifecycle_pins_v1)").map_err(isolated_schema_error)?;
    if fk.query([]).map_err(isolated_schema_error)?.next().map_err(isolated_schema_error)?.is_some() { return Err(ProductError::ContractMismatch); }
    Ok(())
}

#[cfg(feature = "w1-isolated-test-support")]
const W1_CAPTURE_MARKER: &str = "CREATE TABLE product_w1_isolated_marker_v1(singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=3))";
#[cfg(feature = "w1-isolated-test-support")]
const W1_CAPTURE_OBJECTS: [(&str, &str, &str); 4] = [
    ("product_w1_claim_authority_v1", "table", r#"CREATE TABLE product_w1_claim_authority_v1(order_id TEXT NOT NULL, lifecycle INTEGER NOT NULL CHECK(lifecycle>0), epoch INTEGER NOT NULL CHECK(epoch>0), fence INTEGER NOT NULL CHECK(fence>0), installation_id TEXT NOT NULL, database_id TEXT NOT NULL, delivery_id TEXT NOT NULL, worker_id TEXT NOT NULL, claimed_at TEXT NOT NULL, claim_lease_until TEXT NOT NULL, payload_digest TEXT NOT NULL, admission_seq INTEGER NOT NULL, admission_hash TEXT NOT NULL, service_id TEXT NOT NULL, service_revision INTEGER NOT NULL, service_lease_until TEXT NOT NULL, credential_revision INTEGER NOT NULL, credential_expires_at TEXT NOT NULL, grant_action TEXT NOT NULL CHECK(grant_action='drive_review'), grant_revision INTEGER NOT NULL, source_instance_id TEXT NOT NULL, source_generation INTEGER NOT NULL, source_instance_revision INTEGER NOT NULL, connector_id TEXT NOT NULL, connector_revision INTEGER NOT NULL, api_origin TEXT NOT NULL, repository_id TEXT NOT NULL, api_owner TEXT NOT NULL, api_repo TEXT NOT NULL, PRIMARY KEY(order_id,lifecycle,epoch), UNIQUE(fence))"#),
    ("product_w1_claim_authority_no_update", "trigger", r#"CREATE TRIGGER product_w1_claim_authority_no_update BEFORE UPDATE ON product_w1_claim_authority_v1 BEGIN SELECT RAISE(ABORT,'immutable claim authority'); END"#),
    ("product_w1_claim_authority_no_delete", "trigger", r#"CREATE TRIGGER product_w1_claim_authority_no_delete BEFORE DELETE ON product_w1_claim_authority_v1 BEGIN SELECT RAISE(ABORT,'immutable claim authority'); END"#),
    ("product_w1_claim_authority_no_replace", "trigger", r#"CREATE TRIGGER product_w1_claim_authority_no_replace BEFORE INSERT ON product_w1_claim_authority_v1 WHEN EXISTS(SELECT 1 FROM product_w1_claim_authority_v1 WHERE (order_id=NEW.order_id AND lifecycle=NEW.lifecycle AND epoch=NEW.epoch) OR fence=NEW.fence) BEGIN SELECT RAISE(ABORT,'immutable claim authority'); END"#),
];

#[cfg(feature = "w1-isolated-test-support")]
fn validate_w1_capture_schema(conn: &Connection) -> Result<(), ProductError> {
    for (name, kind, ddl) in W1_CAPTURE_OBJECTS {
        let object: Option<(String, String, Option<String>)> = conn.query_row(
            "SELECT type,tbl_name,sql FROM sqlite_schema WHERE name=?1", [name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).optional().map_err(isolated_schema_error)?;
        if !matches!(object, Some((ref k, ref owner, Some(ref sql))) if k == kind && owner == "product_w1_claim_authority_v1" && sql == ddl) { return Err(ProductError::ContractMismatch); }
    }
    let mut columns = conn.prepare("PRAGMA table_info(product_w1_claim_authority_v1)").map_err(isolated_schema_error)?;
    let actual: Vec<(String,String,i64,i64)> = columns.query_map([], |r| Ok((r.get(1)?,r.get(2)?,r.get(3)?,r.get(5)?))).map_err(isolated_schema_error)?.collect::<rusqlite::Result<_>>().map_err(isolated_schema_error)?;
    let expected: Vec<(String,String,i64,i64)> = [
        ("order_id","TEXT",1), ("lifecycle","INTEGER",2), ("epoch","INTEGER",3), ("fence","INTEGER",0),
        ("installation_id","TEXT",0), ("database_id","TEXT",0), ("delivery_id","TEXT",0), ("worker_id","TEXT",0),
        ("claimed_at","TEXT",0), ("claim_lease_until","TEXT",0), ("payload_digest","TEXT",0), ("admission_seq","INTEGER",0),
        ("admission_hash","TEXT",0), ("service_id","TEXT",0), ("service_revision","INTEGER",0), ("service_lease_until","TEXT",0),
        ("credential_revision","INTEGER",0), ("credential_expires_at","TEXT",0), ("grant_action","TEXT",0), ("grant_revision","INTEGER",0),
        ("source_instance_id","TEXT",0), ("source_generation","INTEGER",0), ("source_instance_revision","INTEGER",0),
        ("connector_id","TEXT",0), ("connector_revision","INTEGER",0), ("api_origin","TEXT",0), ("repository_id","TEXT",0),
        ("api_owner","TEXT",0), ("api_repo","TEXT",0),
    ].into_iter().map(|(name, ty, pk)| (name.into(),ty.into(),1,pk)).collect();
    if actual != expected { return Err(ProductError::ContractMismatch); }
    let extra: i64 = conn.query_row("SELECT count(*) FROM sqlite_schema WHERE tbl_name='product_w1_claim_authority_v1' AND ((type='trigger' AND name NOT IN ('product_w1_claim_authority_no_update','product_w1_claim_authority_no_delete','product_w1_claim_authority_no_replace')) OR (type='index' AND sql IS NOT NULL))", [], |r| r.get(0)).map_err(isolated_schema_error)?;
    if extra != 0 { return Err(ProductError::ContractMismatch); }
    let mut stmt = conn.prepare("PRAGMA index_list(product_w1_claim_authority_v1)").map_err(isolated_schema_error)?;
    let indexes: Vec<(String,i64,String,i64)> = stmt.query_map([], |r| Ok((r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).map_err(isolated_schema_error)?.collect::<rusqlite::Result<_>>().map_err(isolated_schema_error)?;
    let mut keys = Vec::new();
    for (name, unique, origin, partial) in indexes {
        if unique != 1 || partial != 0 { return Err(ProductError::ContractMismatch); }
        let mut info = conn.prepare("SELECT name,coll FROM pragma_index_xinfo(?1) WHERE key=1 ORDER BY seqno").map_err(isolated_schema_error)?;
        let cols: Vec<(String,String)> = info.query_map([name], |r| Ok((r.get(0)?,r.get(1)?))).map_err(isolated_schema_error)?.collect::<rusqlite::Result<_>>().map_err(isolated_schema_error)?;
        if cols.iter().any(|(_, coll)| coll != "BINARY") { return Err(ProductError::ContractMismatch); }
        keys.push((origin, cols.into_iter().map(|(n,_)| n).collect::<Vec<_>>()));
    }
    keys.sort();
    if keys != vec![("pk".into(), vec!["order_id".into(),"lifecycle".into(),"epoch".into()]), ("u".into(), vec!["fence".into()])] { return Err(ProductError::ContractMismatch); }
    let mut fk = conn.prepare("PRAGMA foreign_key_list(product_w1_claim_authority_v1)").map_err(isolated_schema_error)?;
    if fk.query([]).map_err(isolated_schema_error)?.next().map_err(isolated_schema_error)?.is_some() { return Err(ProductError::ContractMismatch); }
    let mut check = conn.prepare("PRAGMA foreign_key_check").map_err(isolated_schema_error)?;
    if check.query([]).map_err(isolated_schema_error)?.next().map_err(isolated_schema_error)?.is_some() { return Err(ProductError::ContractMismatch); }
    Ok(())
}

#[cfg(feature = "w1-isolated-test-support")]
const W1_OPERATION_TABLE: &str = r#"CREATE TABLE product_w1_operations_v1(
 actor_id TEXT COLLATE BINARY NOT NULL CHECK(typeof(actor_id)='text' AND length(CAST(actor_id AS BLOB)) BETWEEN 1 AND 256 AND instr(actor_id,char(0))=0),
 request_key TEXT COLLATE BINARY NOT NULL CHECK(typeof(request_key)='text' AND length(CAST(request_key AS BLOB)) BETWEEN 1 AND 256 AND instr(request_key,char(0))=0),
 operation TEXT COLLATE BINARY NOT NULL CHECK(operation IN ('approve','revoke','select')),
 authority_revision INTEGER NOT NULL CHECK(typeof(authority_revision)='integer' AND authority_revision>=0),
 source_instance_id TEXT COLLATE BINARY NOT NULL CHECK(typeof(source_instance_id)='text' AND length(CAST(source_instance_id AS BLOB)) BETWEEN 1 AND 256 AND instr(source_instance_id,char(0))=0),
 digest TEXT COLLATE BINARY NOT NULL CHECK(typeof(digest)='text' AND length(CAST(digest AS BLOB))=64 AND digest NOT GLOB '*[^0-9a-f]*'),
 expected_revision INTEGER NOT NULL CHECK(typeof(expected_revision)='integer' AND expected_revision>=0 AND expected_revision<9223372036854775807),
 approval_result_revision INTEGER,
 order_id TEXT COLLATE BINARY,
 lifecycle INTEGER,
 selection_result_revision INTEGER,
 referenced_approval_revision INTEGER,
 referenced_operation TEXT COLLATE BINARY,
 request BLOB NOT NULL CHECK(typeof(request)='blob' AND length(request)<=8192),
 receipt BLOB NOT NULL CHECK(typeof(receipt)='blob' AND length(receipt)<=8192),
 PRIMARY KEY(actor_id,request_key),
 UNIQUE(source_instance_id,digest,approval_result_revision),
 UNIQUE(order_id,lifecycle,selection_result_revision),
 UNIQUE(source_instance_id,digest,approval_result_revision,operation),
 FOREIGN KEY(source_instance_id,digest,referenced_approval_revision,referenced_operation)
   REFERENCES product_w1_operations_v1(source_instance_id,digest,approval_result_revision,operation),
 CHECK((operation IN ('approve','revoke') AND typeof(approval_result_revision)='integer' AND approval_result_revision=expected_revision+1 AND order_id IS NULL AND lifecycle IS NULL AND selection_result_revision IS NULL AND referenced_approval_revision IS NULL AND referenced_operation IS NULL)
    OR (operation='select' AND approval_result_revision IS NULL AND typeof(order_id)='text' AND length(CAST(order_id AS BLOB)) BETWEEN 1 AND 256 AND instr(order_id,char(0))=0 AND typeof(lifecycle)='integer' AND lifecycle>0 AND typeof(selection_result_revision)='integer' AND selection_result_revision=expected_revision+1 AND typeof(referenced_approval_revision)='integer' AND referenced_approval_revision>0 AND referenced_operation IS NOT NULL AND referenced_operation='approve'))
)"#;
#[cfg(feature = "w1-isolated-test-support")]
const W1_OPERATION_UPDATE: &str = r#"CREATE TRIGGER product_w1_operations_no_update BEFORE UPDATE ON product_w1_operations_v1 BEGIN SELECT RAISE(ABORT,'w1 operation immutable'); END"#;
#[cfg(feature = "w1-isolated-test-support")]
const W1_OPERATION_DELETE: &str = r#"CREATE TRIGGER product_w1_operations_no_delete BEFORE DELETE ON product_w1_operations_v1 BEGIN SELECT RAISE(ABORT,'w1 operation immutable'); END"#;
#[cfg(feature = "w1-isolated-test-support")]
const W1_OPERATION_REPLACE: &str = r#"CREATE TRIGGER product_w1_operations_no_replace BEFORE INSERT ON product_w1_operations_v1 WHEN EXISTS(SELECT 1 FROM product_w1_operations_v1 o WHERE (o.actor_id=NEW.actor_id AND o.request_key=NEW.request_key) OR (NEW.approval_result_revision IS NOT NULL AND o.source_instance_id=NEW.source_instance_id AND o.digest=NEW.digest AND o.approval_result_revision=NEW.approval_result_revision) OR (NEW.selection_result_revision IS NOT NULL AND o.order_id=NEW.order_id AND o.lifecycle=NEW.lifecycle AND o.selection_result_revision=NEW.selection_result_revision)) BEGIN SELECT RAISE(ABORT,'w1 operation duplicate'); END"#;

#[cfg(feature = "w1-isolated-test-support")]
fn isolated_schema_error(error: rusqlite::Error) -> ProductError {
    isolated_marker_query_error(error)
}

#[cfg(feature = "w1-isolated-test-support")]
fn validate_isolated_pragmas(conn: &Connection) -> Result<(), ProductError> {
    for pragma in ["PRAGMA foreign_keys", "PRAGMA recursive_triggers"] {
        let value: i64 = conn.query_row(pragma, [], |row| row.get(0)).map_err(isolated_schema_error)?;
        if value != 1 { return Err(ProductError::ContractMismatch); }
    }
    Ok(())
}

#[cfg(feature = "w1-isolated-test-support")]
fn validate_w1_operations(conn: &Connection) -> Result<(), ProductError> {
    for (name, kind, ddl) in [
        ("product_w1_operations_v1", "table", W1_OPERATION_TABLE),
        ("product_w1_operations_no_update", "trigger", W1_OPERATION_UPDATE),
        ("product_w1_operations_no_delete", "trigger", W1_OPERATION_DELETE),
        ("product_w1_operations_no_replace", "trigger", W1_OPERATION_REPLACE),
    ] {
        let object: Option<(String, Option<String>)> = conn.query_row(
            "SELECT type, sql FROM sqlite_schema WHERE name=?1", [name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional().map_err(isolated_schema_error)?;
        if !matches!(object, Some((ref actual_kind, Some(ref actual_ddl))) if actual_kind == kind && actual_ddl == ddl) {
            return Err(ProductError::ContractMismatch);
        }
    }
    let extra: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE tbl_name='product_w1_operations_v1' AND ((type='trigger' AND name NOT IN ('product_w1_operations_no_update','product_w1_operations_no_delete','product_w1_operations_no_replace')) OR (type='index' AND sql IS NOT NULL))",
        [], |row| row.get(0),
    ).map_err(isolated_schema_error)?;
    if extra != 0 { return Err(ProductError::ContractMismatch); }
    let mut indexes = conn.prepare("PRAGMA index_list(product_w1_operations_v1)").map_err(isolated_schema_error)?;
    let index_names: Vec<(String, i64, String, i64)> = indexes.query_map([], |row| Ok((row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))).map_err(isolated_schema_error)?.collect::<rusqlite::Result<_>>().map_err(isolated_schema_error)?;
    let mut keys = Vec::new();
    for (name, unique, origin, partial) in index_names {
        if unique != 1 || partial != 0 || (origin != "pk" && origin != "u") { return Err(ProductError::ContractMismatch); }
        let mut info = conn.prepare("SELECT name,coll FROM pragma_index_xinfo(?1) WHERE key=1 ORDER BY seqno").map_err(isolated_schema_error)?;
        let columns: Vec<(String, String)> = info.query_map([name], |row| Ok((row.get(0)?, row.get(1)?))).map_err(isolated_schema_error)?.collect::<rusqlite::Result<_>>().map_err(isolated_schema_error)?;
        if columns.iter().any(|(_, coll)| coll != "BINARY") { return Err(ProductError::ContractMismatch); }
        keys.push((origin, columns.into_iter().map(|(name, _)| name).collect::<Vec<_>>()));
    }
    keys.sort();
    let mut expected: Vec<(String, Vec<String>)> = [
        ("pk", vec!["actor_id", "request_key"]),
        ("u", vec!["source_instance_id", "digest", "approval_result_revision"]),
        ("u", vec!["order_id", "lifecycle", "selection_result_revision"]),
        ("u", vec!["source_instance_id", "digest", "approval_result_revision", "operation"]),
    ].into_iter().map(|(origin, columns)| (origin.into(), columns.into_iter().map(String::from).collect())).collect();
    expected.sort();
    if keys != expected { return Err(ProductError::ContractMismatch); }
    let mut fk = conn.prepare("PRAGMA foreign_key_list(product_w1_operations_v1)").map_err(isolated_schema_error)?;
    let foreign: Vec<(i64, i64, String, String, String, String, String, String)> = fk.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?))).map_err(isolated_schema_error)?.collect::<rusqlite::Result<_>>().map_err(isolated_schema_error)?;
    let expected_fk: Vec<_> = [("source_instance_id", "source_instance_id"), ("digest", "digest"), ("referenced_approval_revision", "approval_result_revision"), ("referenced_operation", "operation")].into_iter().enumerate().map(|(seq, (from, to))| (0, seq as i64, "product_w1_operations_v1".to_string(), from.to_string(), to.to_string(), "NO ACTION".to_string(), "NO ACTION".to_string(), "NONE".to_string())).collect();
    if foreign != expected_fk { return Err(ProductError::ContractMismatch); }
    let mut check = conn.prepare("PRAGMA foreign_key_check").map_err(isolated_schema_error)?;
    if check.query([]).map_err(isolated_schema_error)?.next().map_err(isolated_schema_error)?.is_some() {
        return Err(ProductError::ContractMismatch);
    }
    Ok(())
}

#[cfg(all(test, feature = "w1-isolated-test-support"))]
fn validate_isolated_marker(conn: &Connection) -> Result<(), ProductError> {
    validate_isolated_marker_version(conn, StoreOpenMode::IsolatedReopen)
}

#[cfg(feature = "w1-isolated-test-support")]
fn validate_isolated_marker_version(conn: &Connection, mode: StoreOpenMode) -> Result<(), ProductError> {
    if mode == StoreOpenMode::Normal { return Err(ProductError::ContractMismatch); }
    const DDL: &str = "CREATE TABLE product_w1_isolated_marker_v1(singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=2))";
    let definition = isolated_marker_definition_result(conn.query_row(
        "SELECT type, sql FROM sqlite_master WHERE name='product_w1_isolated_marker_v1'",
        [], |row| Ok((row.get(0)?, row.get(1)?)),
    ))?;
    let expected = if mode.is_pin() { W1_PIN_MARKER } else if mode.is_capture() { W1_CAPTURE_MARKER } else { DDL };
    if !matches!(definition, Some((ref kind, Some(ref sql))) if kind == "table" && sql == expected) {
        return Err(ProductError::ContractMismatch);
    }
    let rows = (|| -> rusqlite::Result<Vec<(i64, i64)>> {
        let mut stmt = conn.prepare("SELECT singleton, version FROM product_w1_isolated_marker_v1")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect();
        rows
    })().map_err(isolated_marker_query_error)?;
    if rows != [(1, if mode.is_pin() { 4 } else if mode.is_capture() { 3 } else { 2 })] { return Err(ProductError::ContractMismatch); }
    Ok(())
}

#[cfg(feature = "w1-isolated-test-support")]
fn isolated_marker_definition_result(
    result: rusqlite::Result<(String, Option<String>)>,
) -> Result<Option<(String, Option<String>)>, ProductError> {
    result.optional().map_err(isolated_marker_query_error)
}

#[cfg(feature = "w1-isolated-test-support")]
fn isolated_marker_query_error(error: rusqlite::Error) -> ProductError {
    // Only column conversion and column shape errors indicate malformed query results.
    // SQLite failures (including generic SQL errors) retain their database error.
    match error {
        rusqlite::Error::InvalidColumnType(..)
        | rusqlite::Error::InvalidColumnIndex(..)
        | rusqlite::Error::InvalidColumnName(..)
        | rusqlite::Error::IntegralValueOutOfRange(..)
        | rusqlite::Error::FromSqlConversionFailure(..) => ProductError::ContractMismatch,
        other => ProductError::Database(other),
    }
}

#[cfg(all(test, feature = "w1-isolated-test-support"))]
mod isolated_marker_error_tests {
    use super::{ProductError, isolated_marker_definition_result, isolated_marker_query_error, validate_isolated_marker};
    use rusqlite::{Connection, Error, ffi, types::Type};

    #[test]
    fn marker_row_conversion_and_shape_errors_are_contract_mismatches() {
        let errors = [
            Error::InvalidColumnType(1, "version".into(), Type::Text),
            Error::InvalidColumnIndex(1),
            Error::InvalidColumnName("version".into()),
            Error::IntegralValueOutOfRange(1, 256),
            Error::FromSqlConversionFailure(1, Type::Text, Box::new(std::io::Error::other("conversion"))),
        ];
        for error in errors {
            assert!(matches!(isolated_marker_query_error(error), ProductError::ContractMismatch));
        }

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE product_w1_isolated_marker_v1(singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=2)); PRAGMA ignore_check_constraints=ON; INSERT INTO product_w1_isolated_marker_v1 VALUES(1,'invalid');").unwrap();
        assert!(matches!(validate_isolated_marker(&conn), Err(ProductError::ContractMismatch)));
        conn.execute_batch("DELETE FROM product_w1_isolated_marker_v1").unwrap();
        assert!(matches!(validate_isolated_marker(&conn), Err(ProductError::ContractMismatch)));
    }

    #[test]
    fn constructed_marker_sqlite_operational_errors_remain_database_errors() {
        // These classify constructed errors; they do not induce real I/O or locking failures.
        for code in [ffi::SQLITE_IOERR, ffi::SQLITE_BUSY, ffi::SQLITE_LOCKED, ffi::SQLITE_CORRUPT, ffi::SQLITE_NOMEM, ffi::SQLITE_ERROR] {
            let error = Error::SqliteFailure(ffi::Error::new(code), None);
            assert!(matches!(isolated_marker_query_error(error), ProductError::Database(Error::SqliteFailure(inner, _)) if inner.extended_code == code));
        }
    }

    #[test]
    fn marker_definition_result_classifies_shape_and_conversion_errors() {
        // Synthetic results exercise the definition-query mapping; SQLite normally
        // validates sqlite_master before malformed entries can reach this query.
        let errors = [
            Error::InvalidColumnType(0, "type".into(), Type::Integer),
            Error::InvalidColumnIndex(1),
            Error::InvalidColumnName("sql".into()),
            Error::IntegralValueOutOfRange(0, 256),
            Error::FromSqlConversionFailure(1, Type::Text, Box::new(std::io::Error::other("conversion"))),
        ];
        for error in errors {
            assert!(matches!(isolated_marker_definition_result(Err(error)), Err(ProductError::ContractMismatch)));
        }
        assert!(matches!(isolated_marker_definition_result(Err(Error::QueryReturnedNoRows)), Ok(None)));
    }

    #[test]
    fn marker_definition_result_preserves_sqlite_operational_errors() {
        // Constructed errors test classification, not actual I/O or locking failures.
        for code in [ffi::SQLITE_IOERR, ffi::SQLITE_BUSY, ffi::SQLITE_LOCKED, ffi::SQLITE_CORRUPT, ffi::SQLITE_NOMEM, ffi::SQLITE_ERROR] {
            let error = Error::SqliteFailure(ffi::Error::new(code), None);
            assert!(matches!(isolated_marker_definition_result(Err(error)), Err(ProductError::Database(Error::SqliteFailure(inner, _))) if inner.extended_code == code));
        }
    }
}

#[cfg(feature = "w1-isolated-test-support")]
fn isolated_test_path(directory: &Path, name: &str) -> Result<PathBuf, ProductError> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_))) || components.next().is_some() {
        return Err(ProductError::InvalidConfig);
    }
    let metadata = std::fs::symlink_metadata(directory)
        .map_err(|_| ProductError::Invariant("isolated store directory inspection failed"))?;
    if !metadata.file_type().is_dir() { return Err(ProductError::InvalidConfig); }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 { return Err(ProductError::InvalidConfig); }
    }
    #[cfg(not(unix))]
    { return Err(ProductError::InvalidConfig); }
    #[cfg(unix)]
    { Ok(directory.canonicalize().map_err(|_| ProductError::Invariant("isolated store directory canonicalization failed"))?.join(name)) }
}

// Read-only SQLite inspection is limited to the private rollback-journal test
// boundary. It does not establish WAL-safe marker discovery, old-writer fencing,
// power-loss durability, or safety of an operational migration. Pathname replacement
// by a concurrent actor is outside this check's guarantees.
fn reject_isolated_before_path_creation(path: &Path) -> Result<(), ProductError> {
    match std::fs::metadata(path) {
        Ok(_) => {
            let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            let marker: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='product_w1_isolated_marker_v1')",
                [], |r| r.get(0),
            )?;
            if marker { return Err(ProductError::ContractMismatch); }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Creating a missing component before `..` can make an apparently
            // absent path resolve to an existing database. Fail closed there.
            if path.components().any(|part| matches!(part, std::path::Component::ParentDir))
                && path.parent().ok_or(ProductError::InvalidConfig)?.canonicalize().is_err()
            {
                return Err(ProductError::InvalidConfig);
            }
        }
        Err(_) => return Err(ProductError::InvalidConfig),
    }
    Ok(())
}

fn initialize_product_schema_in(
    tx: &rusqlite::Transaction<'_>,
    config: &ProductConfig,
    mode: StoreOpenMode,
) -> Result<(), ProductError> {
    initialize_product_schema_with_a01_checkpoint_in(
        tx, config, mode,
        #[cfg(all(test, feature = "a01-harness-support", unix))]
        None,
    )
}


fn initialize_product_schema_with_a01_checkpoint_in(
    tx: &rusqlite::Transaction<'_>,
    config: &ProductConfig,
    mode: StoreOpenMode,
    #[cfg(all(test, feature = "a01-harness-support", unix))] checkpoint: Option<usize>,
) -> Result<(), ProductError> {
        #[cfg(feature = "w1-isolated-test-support")]
        if mode.is_create() {
            if mode.is_capture() || mode.is_pin() {
                tx.execute_batch(if mode.is_pin() { W1_PIN_MARKER } else { W1_CAPTURE_MARKER })?;
                tx.execute("INSERT INTO product_w1_isolated_marker_v1 VALUES(1,?1)", [if mode.is_pin() { 4 } else { 3 }])?;
                for ddl in W1_CAPTURE_OBJECTS { tx.execute_batch(ddl.2)?; }
                if mode.is_pin() { for ddl in W1_PIN_OBJECTS { tx.execute_batch(ddl.2)?; } }
            } else {
                tx.execute_batch("CREATE TABLE product_w1_isolated_marker_v1(singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=2)); INSERT INTO product_w1_isolated_marker_v1 VALUES(1,2);")?;
            }
            for ddl in [W1_OPERATION_TABLE, W1_OPERATION_UPDATE, W1_OPERATION_DELETE, W1_OPERATION_REPLACE] {
                tx.execute_batch(ddl)?;
            }
        }
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS foundry_contract(singleton INTEGER PRIMARY KEY CHECK(singleton=1), contract TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS product_meta_v1(singleton INTEGER PRIMARY KEY CHECK(singleton=1), installation_id TEXT NOT NULL, database_id TEXT NOT NULL, schema_version INTEGER NOT NULL, recovery_epoch INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE IF NOT EXISTS ledger(seq INTEGER PRIMARY KEY AUTOINCREMENT, order_id TEXT NOT NULL, from_state TEXT NOT NULL, to_state TEXT NOT NULL, event TEXT NOT NULL, actor TEXT NOT NULL, intent TEXT NOT NULL DEFAULT '', at TEXT NOT NULL, prev_hash TEXT NOT NULL, event_json TEXT, hash TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS ledger_order_seq_v1 ON ledger(order_id,seq);
             CREATE TRIGGER IF NOT EXISTS ledger_no_update BEFORE UPDATE ON ledger BEGIN SELECT RAISE(ABORT, 'ledger is append-only'); END;
             CREATE TRIGGER IF NOT EXISTS ledger_no_delete BEFORE DELETE ON ledger BEGIN SELECT RAISE(ABORT, 'ledger is append-only'); END;
             CREATE TABLE IF NOT EXISTS product_transition_receipts_v1(seq INTEGER PRIMARY KEY, event_hash TEXT NOT NULL UNIQUE, order_id TEXT NOT NULL, lifecycle INTEGER NOT NULL, expected_predecessor INTEGER NOT NULL, operation TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS product_transition_intents_v1(order_id TEXT PRIMARY KEY, lifecycle INTEGER NOT NULL, expected_predecessor INTEGER NOT NULL, operation TEXT NOT NULL);
             CREATE TRIGGER IF NOT EXISTS product_ledger_requires_intent BEFORE INSERT ON ledger
               WHEN (SELECT contract FROM foundry_contract WHERE singleton=1)='ProductAtomicV1'
                 AND NOT EXISTS(SELECT 1 FROM product_transition_intents_v1 WHERE order_id=NEW.order_id)
               BEGIN SELECT RAISE(ABORT, 'product transition receipt required'); END;
             CREATE TRIGGER IF NOT EXISTS product_ledger_receipt AFTER INSERT ON ledger
               WHEN (SELECT contract FROM foundry_contract WHERE singleton=1)='ProductAtomicV1'
               BEGIN
                 INSERT INTO product_transition_receipts_v1(seq,event_hash,order_id,lifecycle,expected_predecessor,operation)
                   SELECT NEW.seq,NEW.hash,NEW.order_id,lifecycle,expected_predecessor,operation FROM product_transition_intents_v1 WHERE order_id=NEW.order_id;
                 DELETE FROM product_transition_intents_v1 WHERE order_id=NEW.order_id;
               END;
             CREATE TABLE IF NOT EXISTS product_accounts_v1(actor_id TEXT PRIMARY KEY, role TEXT NOT NULL, enabled INTEGER NOT NULL, revision INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS product_credentials_v1(actor_id TEXT PRIMARY KEY, argon2id_phc TEXT NOT NULL, revision INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS product_sessions_v1(session_digest TEXT PRIMARY KEY, actor_id TEXT NOT NULL, authority_revision INTEGER NOT NULL, expires_at TEXT NOT NULL, revoked INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE IF NOT EXISTS product_login_throttle_v1(actor_id TEXT PRIMARY KEY, window_started_at TEXT NOT NULL, failures INTEGER NOT NULL, blocked_until TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS product_sources_v1(connector_id TEXT PRIMARY KEY, provider TEXT NOT NULL, origin TEXT NOT NULL, repository_id TEXT NOT NULL, scope TEXT NOT NULL, revision INTEGER NOT NULL, enabled INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS product_orders_v1(order_id TEXT PRIMARY KEY, lifecycle INTEGER NOT NULL, connector_id TEXT NOT NULL, repository_id TEXT NOT NULL, issue_id TEXT NOT NULL, task_kind TEXT NOT NULL, payload_snapshot TEXT NOT NULL, source_revision INTEGER NOT NULL, admission_seq INTEGER NOT NULL, admission_hash TEXT NOT NULL);
             CREATE UNIQUE INDEX IF NOT EXISTS product_orders_v1_source_identity ON product_orders_v1(connector_id,repository_id,issue_id);
             CREATE TABLE IF NOT EXISTS product_order_payload_digests_v1(order_id TEXT PRIMARY KEY, payload_digest TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS product_operations_v1(operation_id TEXT PRIMARY KEY, actor_id TEXT NOT NULL, request_key TEXT NOT NULL, request_digest TEXT NOT NULL, order_id TEXT NOT NULL, grant_id TEXT, phase TEXT NOT NULL, owner_id TEXT, epoch INTEGER, fence INTEGER, prepared_at TEXT NOT NULL, expires_at TEXT NOT NULL, scope_revision INTEGER NOT NULL DEFAULT -1, UNIQUE(actor_id, request_key));
             CREATE TABLE IF NOT EXISTS product_grants_v1(grant_id TEXT PRIMARY KEY, token_digest TEXT NOT NULL UNIQUE, actor_id TEXT NOT NULL, order_id TEXT NOT NULL, state TEXT NOT NULL, fence INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS product_fence_counter_v1(singleton INTEGER PRIMARY KEY CHECK(singleton=1), next INTEGER NOT NULL);
             INSERT OR IGNORE INTO product_fence_counter_v1(singleton,next) VALUES(1,0);
             CREATE TABLE IF NOT EXISTS product_outcomes_v1(operation_id TEXT PRIMARY KEY, seq INTEGER NOT NULL UNIQUE, event_hash TEXT NOT NULL UNIQUE, receipt_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS product_token_consumptions_v1(token_digest TEXT PRIMARY KEY, operation_id TEXT NOT NULL UNIQUE, seq INTEGER NOT NULL UNIQUE);
             CREATE TABLE IF NOT EXISTS product_intake_inbox_v1(delivery_id TEXT PRIMARY KEY, connector_id TEXT NOT NULL, repository_id TEXT NOT NULL, issue_id TEXT NOT NULL, version_digest TEXT NOT NULL, payload TEXT NOT NULL, state TEXT NOT NULL, retry_at TEXT);
             CREATE TABLE IF NOT EXISTS product_intake_delivery_meta_v1(delivery_id TEXT PRIMARY KEY REFERENCES product_intake_inbox_v1(delivery_id), observed_updated_at TEXT NOT NULL, task_kind TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS product_intake_source_bindings_v1(delivery_id TEXT PRIMARY KEY,source_instance_id TEXT NOT NULL,source_revision INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS product_poll_source_bindings_v1(connector_id TEXT NOT NULL,query_fingerprint TEXT NOT NULL,source_instance_id TEXT NOT NULL,source_revision INTEGER NOT NULL,PRIMARY KEY(connector_id,query_fingerprint));
             CREATE TABLE IF NOT EXISTS product_intake_rejections_v1(connector_id TEXT NOT NULL, query_fingerprint TEXT NOT NULL, sweep_id TEXT NOT NULL, item_digest TEXT NOT NULL, recorded_at TEXT NOT NULL, PRIMARY KEY(connector_id,query_fingerprint,sweep_id,item_digest));
             CREATE TABLE IF NOT EXISTS product_poll_state_v1(connector_id TEXT NOT NULL, query_fingerprint TEXT NOT NULL, sweep_id TEXT NOT NULL, next_page INTEGER NOT NULL, page_ceiling INTEGER NOT NULL, updated_at TEXT NOT NULL, PRIMARY KEY(connector_id, query_fingerprint));
             CREATE TABLE IF NOT EXISTS product_poll_backoff_v1(connector_id TEXT NOT NULL, query_fingerprint TEXT NOT NULL, failures INTEGER NOT NULL, retry_at TEXT NOT NULL, PRIMARY KEY(connector_id,query_fingerprint));
             CREATE TABLE IF NOT EXISTS product_services_v1(service_id TEXT PRIMARY KEY,revision INTEGER NOT NULL,enabled INTEGER NOT NULL,lease_until TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS product_service_credentials_v1(service_id TEXT PRIMARY KEY,verifier TEXT NOT NULL UNIQUE,credential_revision INTEGER NOT NULL,expires_at TEXT NOT NULL,issued_by TEXT NOT NULL,request_key TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS product_service_grants_v1(service_id TEXT NOT NULL,source_instance_id TEXT NOT NULL,action TEXT NOT NULL,grant_revision INTEGER NOT NULL,enabled INTEGER NOT NULL,PRIMARY KEY(service_id,source_instance_id,action));
             CREATE TABLE IF NOT EXISTS product_service_events_v1(event_seq INTEGER PRIMARY KEY AUTOINCREMENT,service_id TEXT NOT NULL,actor_id TEXT NOT NULL,revision INTEGER NOT NULL,operation TEXT NOT NULL,request_key TEXT NOT NULL,request_digest TEXT NOT NULL,created_at TEXT NOT NULL);
             CREATE UNIQUE INDEX IF NOT EXISTS product_service_credential_issue_once ON product_service_events_v1(service_id,operation,request_key) WHERE operation='credential_issue';
             CREATE TABLE IF NOT EXISTS product_service_operations_v1(actor_id TEXT NOT NULL,request_key TEXT NOT NULL,request_digest TEXT NOT NULL,receipt_json TEXT NOT NULL,PRIMARY KEY(actor_id,request_key));
             CREATE TABLE IF NOT EXISTS product_service_work_bindings_v1(order_id TEXT NOT NULL,lifecycle INTEGER NOT NULL,service_id TEXT NOT NULL,service_revision INTEGER NOT NULL,credential_revision INTEGER NOT NULL,source_instance_id TEXT NOT NULL,PRIMARY KEY(order_id,lifecycle));
             CREATE TABLE IF NOT EXISTS product_work_backlog_v1(order_id TEXT NOT NULL, lifecycle INTEGER NOT NULL, delivery_id TEXT NOT NULL UNIQUE, state TEXT NOT NULL, available_at TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(order_id,lifecycle));
             CREATE TABLE IF NOT EXISTS product_work_claims_v1(order_id TEXT NOT NULL, lifecycle INTEGER NOT NULL, owner_id TEXT, epoch INTEGER NOT NULL, fence INTEGER NOT NULL, state TEXT NOT NULL, lease_until TEXT, source_revision INTEGER, payload_digest TEXT, PRIMARY KEY(order_id,lifecycle));
             CREATE TABLE IF NOT EXISTS product_review_meta_v1(singleton INTEGER PRIMARY KEY CHECK(singleton=1), installation_id TEXT NOT NULL, database_id TEXT NOT NULL, schema_version INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS product_review_holds_v1(order_id TEXT NOT NULL,lifecycle INTEGER NOT NULL,attempt INTEGER NOT NULL,originating_run INTEGER NOT NULL,activation INTEGER NOT NULL,host_epoch INTEGER NOT NULL,hold_instance_id TEXT NOT NULL,hold_revision INTEGER NOT NULL,expected_predecessor INTEGER NOT NULL,snapshot_digest TEXT NOT NULL,snapshot_json TEXT NOT NULL,allowed_targets_json TEXT NOT NULL,deadline TEXT NOT NULL,source_revision INTEGER NOT NULL,admission_seq INTEGER NOT NULL,admission_hash TEXT NOT NULL,state TEXT NOT NULL,PRIMARY KEY(order_id,lifecycle,attempt,originating_run,hold_instance_id));
             CREATE TABLE IF NOT EXISTS product_review_hold_acceptances_v1(operation_id TEXT PRIMARY KEY,actor_id TEXT NOT NULL,request_key TEXT NOT NULL,request_digest TEXT NOT NULL,receipt_json TEXT NOT NULL,order_id TEXT NOT NULL,lifecycle INTEGER NOT NULL,attempt INTEGER NOT NULL,originating_run INTEGER NOT NULL,hold_instance_id TEXT NOT NULL,UNIQUE(actor_id,request_key),UNIQUE(order_id,lifecycle,attempt,originating_run,hold_instance_id));
             CREATE TRIGGER IF NOT EXISTS product_review_acceptance_no_update BEFORE UPDATE ON product_review_hold_acceptances_v1 BEGIN SELECT RAISE(ABORT,'product review acceptance is immutable'); END;
             CREATE TRIGGER IF NOT EXISTS product_review_acceptance_no_delete BEFORE DELETE ON product_review_hold_acceptances_v1 BEGIN SELECT RAISE(ABORT,'product review acceptance is immutable'); END;
             CREATE TRIGGER IF NOT EXISTS product_review_hold_exact_cas BEFORE UPDATE ON product_review_holds_v1
               WHEN OLD.state!='active' OR NEW.state!='accepted' OR NEW.order_id!=OLD.order_id OR NEW.lifecycle!=OLD.lifecycle OR NEW.attempt!=OLD.attempt OR NEW.originating_run!=OLD.originating_run OR NEW.activation!=OLD.activation OR NEW.host_epoch!=OLD.host_epoch OR NEW.hold_instance_id!=OLD.hold_instance_id OR NEW.hold_revision!=OLD.hold_revision OR NEW.expected_predecessor!=OLD.expected_predecessor OR NEW.snapshot_digest!=OLD.snapshot_digest OR NEW.snapshot_json!=OLD.snapshot_json OR NEW.allowed_targets_json!=OLD.allowed_targets_json OR NEW.deadline!=OLD.deadline OR NEW.source_revision!=OLD.source_revision OR NEW.admission_seq!=OLD.admission_seq OR NEW.admission_hash!=OLD.admission_hash
               BEGIN SELECT RAISE(ABORT,'product review hold CAS binding changed'); END;
             CREATE TRIGGER IF NOT EXISTS product_review_hold_no_delete BEFORE DELETE ON product_review_holds_v1 BEGIN SELECT RAISE(ABORT,'product review hold history is immutable'); END;
             CREATE TABLE IF NOT EXISTS product_review_hold_progress_v1(operation_id TEXT PRIMARY KEY,phase TEXT NOT NULL,progress_revision INTEGER NOT NULL,owner_id TEXT NOT NULL,epoch INTEGER NOT NULL,fence INTEGER NOT NULL,reserved_run_id INTEGER NOT NULL,recovery_reason TEXT);
             CREATE TABLE IF NOT EXISTS product_review_run_reservations_v1(reservation_id INTEGER PRIMARY KEY AUTOINCREMENT,operation_id TEXT NOT NULL UNIQUE,order_id TEXT NOT NULL,lifecycle INTEGER NOT NULL,attempt INTEGER NOT NULL,originating_run INTEGER NOT NULL,hold_instance_id TEXT NOT NULL,followup_run_id INTEGER);
             CREATE TABLE IF NOT EXISTS product_review_runs_v1(order_id TEXT NOT NULL,attempt INTEGER NOT NULL,run_id TEXT NOT NULL,base_commit TEXT NOT NULL,result_commit TEXT NOT NULL,pack_digest TEXT NOT NULL,status TEXT NOT NULL,counters_json TEXT NOT NULL,hold_json TEXT,hold_instance_id TEXT,deadline INTEGER,result_json TEXT,provenance_json TEXT,product_lifecycle INTEGER NOT NULL DEFAULT 0,source_revision INTEGER NOT NULL DEFAULT 0,admission_seq INTEGER NOT NULL DEFAULT 0,admission_hash TEXT NOT NULL DEFAULT '',created_at INTEGER NOT NULL,PRIMARY KEY(order_id,attempt,run_id));
             CREATE TABLE IF NOT EXISTS product_review_active_v1(order_id TEXT PRIMARY KEY,key_json TEXT NOT NULL,epoch INTEGER NOT NULL,journal_run TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS product_review_epochs_v1(order_id TEXT PRIMARY KEY,epoch INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS product_review_bindings_v1(key_json TEXT PRIMARY KEY,intent_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS product_review_host_accept_v1(order_id TEXT NOT NULL,attempt INTEGER NOT NULL,run_id INTEGER NOT NULL,activation INTEGER NOT NULL,PRIMARY KEY(order_id,attempt,run_id,activation));
             CREATE TABLE IF NOT EXISTS product_review_host_results_v1(order_id TEXT NOT NULL,attempt INTEGER NOT NULL,run_id INTEGER NOT NULL,activation INTEGER NOT NULL,result_json TEXT NOT NULL,PRIMARY KEY(order_id,attempt,run_id,activation));
             CREATE TABLE IF NOT EXISTS product_review_stages_v1(order_id TEXT NOT NULL,run_id TEXT NOT NULL,sequence INTEGER NOT NULL,stage_id TEXT NOT NULL,status TEXT NOT NULL,output_json TEXT,PRIMARY KEY(order_id,run_id,sequence));
             CREATE TABLE IF NOT EXISTS product_review_sessions_v1(order_id TEXT NOT NULL,run_id TEXT NOT NULL,session_id TEXT NOT NULL,receipt_json TEXT NOT NULL,PRIMARY KEY(order_id,run_id,session_id));
             CREATE TABLE IF NOT EXISTS product_review_budget_v1(order_id TEXT PRIMARY KEY,used INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS product_review_failures_v1(order_id TEXT NOT NULL,run_id TEXT NOT NULL,error TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS product_review_redrive_v1(order_id TEXT NOT NULL,run_id TEXT NOT NULL,payload TEXT NOT NULL,artifact_json TEXT NOT NULL,PRIMARY KEY(order_id,run_id,payload));",
        )?;
        // Run this additive migration only after the intake binding tables
        // exist. Existing rows retain zero defaults and fail closed until an
        // explicit source mapping/rebind supplies their provenance.
        for table in [
            "product_intake_source_bindings_v1",
            "product_poll_source_bindings_v1",
        ] {
            let columns = {
                let mut stmt = tx.prepare(&format!("PRAGMA table_info({table})"))?;
                stmt.query_map([], |r| r.get::<_, String>(1))?
                    .collect::<Result<Vec<_>, _>>()?
            };
            for (name, definition) in [
                ("source_instance_revision", "INTEGER NOT NULL DEFAULT 0"),
                ("source_generation", "INTEGER NOT NULL DEFAULT 0"),
            ] {
                if !columns.iter().any(|column| column == name) {
                    tx.execute(
                        &format!("ALTER TABLE {table} ADD COLUMN {name} {definition}"),
                        [],
                    )?;
                }
            }
        }
        crate::product_scope::create_schema(&tx)?;
        create_product_publish_schema(&tx)?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS product_review_work_bindings_v1(
                order_id TEXT NOT NULL,
                attempt INTEGER NOT NULL,
                run_id TEXT NOT NULL,
                lifecycle INTEGER NOT NULL,
                delivery_id TEXT NOT NULL,
                worker_id TEXT NOT NULL,
                claim_epoch INTEGER NOT NULL,
                claim_fence INTEGER NOT NULL,
                source_revision INTEGER NOT NULL,
                payload_digest TEXT NOT NULL,
                task_kind TEXT NOT NULL,
                payload_snapshot TEXT NOT NULL,
                PRIMARY KEY(order_id,attempt,run_id)
             );",
        )?;
        // Additive upgrade only: do not rewrite or remove any existing work
        // history when opening a ProductAtomicV1 database.
        let work_columns = {
            let mut stmt = tx.prepare("PRAGMA table_info(product_work_claims_v1)")?;
            stmt.query_map([], |r| r.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (name, definition) in [
            ("lease_until", "TEXT"),
            ("source_revision", "INTEGER"),
            ("payload_digest", "TEXT"),
        ] {
            if !work_columns.iter().any(|column| column == name) {
                tx.execute(
                    &format!("ALTER TABLE product_work_claims_v1 ADD COLUMN {name} {definition}"),
                    [],
                )?;
            }
        }
        let reservation_columns = {
            let mut stmt = tx.prepare("PRAGMA table_info(product_review_run_reservations_v1)")?;
            stmt.query_map([], |r| r.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()?
        };
        if !reservation_columns
            .iter()
            .any(|column| column == "followup_run_id")
        {
            tx.execute(
                "ALTER TABLE product_review_run_reservations_v1 ADD COLUMN followup_run_id INTEGER",
                [],
            )?;
            // Preserve coordinates already returned by earlier product
            // receipts. Those historical values are treated as occupied
            // even though the old implementation sourced them from row IDs.
            tx.execute("UPDATE product_review_run_reservations_v1 SET followup_run_id=(SELECT json_extract(a.receipt_json,'$.reserved_run_id') FROM product_review_hold_acceptances_v1 a WHERE a.operation_id=product_review_run_reservations_v1.operation_id) WHERE followup_run_id IS NULL AND EXISTS(SELECT 1 FROM product_review_hold_acceptances_v1 a WHERE a.operation_id=product_review_run_reservations_v1.operation_id)", [])?;
        }
        let product_review_run_columns = {
            let mut stmt = tx.prepare("PRAGMA table_info(product_review_runs_v1)")?;
            stmt.query_map([], |r| r.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (name, definition) in [
            ("product_lifecycle", "INTEGER NOT NULL DEFAULT 0"),
            ("source_revision", "INTEGER NOT NULL DEFAULT 0"),
            ("admission_seq", "INTEGER NOT NULL DEFAULT 0"),
            ("admission_hash", "TEXT NOT NULL DEFAULT ''"),
        ] {
            if !product_review_run_columns
                .iter()
                .any(|column| column == name)
            {
                tx.execute(
                    &format!("ALTER TABLE product_review_runs_v1 ADD COLUMN {name} {definition}"),
                    [],
                )?;
            }
        }
        tx.execute("CREATE UNIQUE INDEX IF NOT EXISTS product_review_run_coordinates_v1 ON product_review_run_reservations_v1(order_id,followup_run_id) WHERE followup_run_id IS NOT NULL", [])?;
        let has_grant_id = {
            let mut stmt = tx.prepare("PRAGMA table_info(product_operations_v1)")?;
            let columns = stmt.query_map([], |r| r.get::<_, String>(1))?;
            columns
                .collect::<Result<Vec<_>, _>>()?
                .iter()
                .any(|c| c == "grant_id")
        };
        if !has_grant_id {
            tx.execute(
                "ALTER TABLE product_operations_v1 ADD COLUMN grant_id TEXT",
                [],
            )?;
        }
        let has_scope_revision = {
            let mut stmt = tx.prepare("PRAGMA table_info(product_operations_v1)")?;
            let columns = stmt.query_map([], |r| r.get::<_, String>(1))?;
            columns
                .collect::<Result<Vec<_>, _>>()?
                .iter()
                .any(|column| column == "scope_revision")
        };
        if !has_scope_revision {
            tx.execute(
                "ALTER TABLE product_operations_v1 ADD COLUMN scope_revision INTEGER NOT NULL DEFAULT -1",
                [],
            )?;
        }
        let contract: Option<String> = tx
            .query_row(
                "SELECT contract FROM foundry_contract WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        match contract.as_deref() {
            Some("ProductAtomicV1") => {}
            Some(_) => return Err(ProductError::ContractMismatch),
            None => {
                tx.execute(
                    "INSERT INTO foundry_contract(singleton,contract) VALUES(1,'ProductAtomicV1')",
                    [],
                )?;
            }
        }
        let meta: Option<(String, String)> = tx
            .query_row(
                "SELECT installation_id,database_id FROM product_meta_v1 WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match meta {
            Some((installation, database))
                if installation == config.installation_id && database == config.database_id => {}
            Some(_) => return Err(ProductError::ContractMismatch),
            None => {
                let version = 3;
                #[cfg(feature = "a01-harness-support")]
                let version = if mode.is_a01() { 0x0A01 } else { version };
                tx.execute("INSERT INTO product_meta_v1(singleton,installation_id,database_id,schema_version) VALUES(1,?1,?2,?3)", params![config.installation_id, config.database_id, version])?;
            }
        }
        let review_meta: Option<(String, String, i64)> = tx
            .query_row(
                "SELECT installation_id,database_id,schema_version FROM product_review_meta_v1 WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        match review_meta {
            Some((installation, database, 1))
                if installation == config.installation_id && database == config.database_id => {}
            Some(_) => return Err(ProductError::ContractMismatch),
            None => {
                tx.execute(
                    "INSERT INTO product_review_meta_v1(singleton,installation_id,database_id,schema_version) VALUES(1,?1,?2,1)",
                    params![config.installation_id, config.database_id],
                )?;
            }
        }
        result::initialize_fresh_v3(&tx, &config.installation_id, &config.database_id)?;
    #[cfg(feature = "a01-harness-support")]
    if mode.is_a01() {
        #[cfg(all(test, feature = "a01-harness-support", unix))]
        a01_initialization_checkpoint(checkpoint, 0)?;
        initialize_a01_binding_foundation_in(
            tx,
            #[cfg(all(test, feature = "a01-harness-support", unix))]
            checkpoint,
        )?;
    }
    #[cfg(feature = "a01-harness-support")]
    if mode.is_a01_task() { a01_task::initialize_in(tx, config, &mut a01_task::TaskHooks::default())?; }
    Ok(())
}

#[cfg(feature = "a01-harness-support")]
#[derive(Clone, Copy, PartialEq, Eq)]
struct A01FileIdentity {
    device: u64,
    inode: u64,
}


#[cfg(feature = "a01-harness-support")]
const A01_BINDING_FOUNDATION_DDL: [&str; 5] = [
    r#"DROP TRIGGER product_result_bindings_no_replace;"#,
    r#"CREATE TRIGGER product_result_bindings_no_replace
BEFORE INSERT ON product_result_bindings_v1
WHEN EXISTS(
  SELECT 1
  FROM product_result_bindings_v1 r
  WHERE r.result_id=NEW.result_id
     OR (
       r.order_id=NEW.order_id
       AND r.lifecycle=NEW.lifecycle
       AND r.attempt=NEW.attempt
       AND r.run_id=NEW.run_id
       AND r.activation=NEW.activation
       AND r.generation=NEW.generation
     )
     OR (
       r.order_id=NEW.order_id
       AND r.attempt=NEW.attempt
       AND r.run_id=NEW.run_id
       AND r.generation=NEW.generation
     )
     OR (
       r.result_id=NEW.result_id
       AND r.binding_digest=NEW.binding_digest
       AND r.tuple_digest=NEW.tuple_digest
     )
)
BEGIN
  SELECT RAISE(ROLLBACK,'product result already exists');
END;"#,
    r#"DROP TRIGGER product_result_bindings_no_legacy_tuple;"#,
    r#"CREATE TRIGGER product_result_bindings_no_legacy_tuple
BEFORE INSERT ON product_result_bindings_v1
WHEN EXISTS(
  SELECT 1
  FROM product_result_legacy_tuples_v1 t
  WHERE t.order_id=NEW.order_id
    AND t.attempt=NEW.attempt
    AND t.run_id=NEW.run_id
    AND t.generation=NEW.generation
)
BEGIN
  SELECT RAISE(ROLLBACK,'legacy tuple cannot be adopted');
END;"#,
    r#"CREATE TRIGGER product_result_bindings_a01_deny_all
BEFORE INSERT ON product_result_bindings_v1
BEGIN
  SELECT RAISE(ROLLBACK,'A01 result binding is disabled');
END;"#,
];

/// Only fresh marked initialization (and its schema template) installs this foundation.
#[cfg(feature = "a01-harness-support")]
fn initialize_a01_binding_foundation_in(
    tx: &rusqlite::Transaction<'_>,
    #[cfg(all(test, feature = "a01-harness-support", unix))] checkpoint: Option<usize>,
) -> Result<(), ProductError> {
    for (_index, sql) in A01_BINDING_FOUNDATION_DDL.iter().enumerate() {
        tx.execute_batch(sql)?;
        #[cfg(all(test, feature = "a01-harness-support", unix))]
        a01_initialization_checkpoint(checkpoint, _index + 1)?;
    }
    Ok(())
}

#[cfg(all(test, feature = "a01-harness-support", unix))]
fn a01_initialization_checkpoint(
    checkpoint: Option<usize>,
    step: usize,
) -> Result<(), ProductError> {
    if checkpoint == Some(step) {
        return Err(ProductError::Invariant("A01 initialization checkpoint"));
    }
    Ok(())
}

#[cfg(feature = "a01-harness-support")]
fn validate_a01_config(config: &ProductConfig) -> Result<(), ProductError> {
    if config.installation_id.trim().is_empty() || config.database_id.trim().is_empty() {
        return Err(ProductError::InvalidConfig);
    }
    Ok(())
}

#[cfg(all(feature = "a01-harness-support", unix))]
fn a01_effective_uid() -> u32 {
    unsafe extern "C" { fn geteuid() -> u32; }
    // geteuid has no arguments, cannot fail, and does not mutate process state.
    unsafe { geteuid() }
}

#[cfg(feature = "a01-harness-support")]
fn a01_test_path(directory: &Path, name: &str) -> Result<PathBuf, ProductError> {
    if !directory.is_absolute() || name.is_empty() || name.contains(['/', '\0'])
        || name == "." || name == ".."
    {
        return Err(ProductError::InvalidConfig);
    }
    let mut parts = Path::new(name).components();
    if !matches!(parts.next(), Some(std::path::Component::Normal(_))) || parts.next().is_some() {
        return Err(ProductError::InvalidConfig);
    }
    let metadata = std::fs::symlink_metadata(directory)
        .map_err(|_| ProductError::Invariant("A01 directory inspection failed"))?;
    if !metadata.file_type().is_dir() {
        return Err(ProductError::InvalidConfig);
    }
    #[cfg(not(unix))]
    { Err(ProductError::InvalidConfig) }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != a01_effective_uid() || metadata.mode() & 0o077 != 0 {
            return Err(ProductError::InvalidConfig);
        }
        let canonical = directory.canonicalize()
            .map_err(|_| ProductError::Invariant("A01 directory inspection failed"))?;
        if canonical.as_os_str() != directory.as_os_str() {
            return Err(ProductError::InvalidConfig);
        }
        Ok(canonical.join(name))
    }
}

#[cfg(feature = "a01-harness-support")]
fn a01_require_absent(path: &Path) -> Result<(), ProductError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Err(ProductError::InvalidConfig),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(ProductError::Invariant("A01 path inspection failed")),
    }
}

#[cfg(feature = "a01-harness-support")]
fn a01_check_sidecars(path: &Path) -> Result<(), ProductError> {
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        a01_require_absent(Path::new(&sidecar))?;
    }
    Ok(())
}

#[cfg(feature = "a01-harness-support")]
fn a01_metadata_identity(metadata: &std::fs::Metadata) -> Result<A01FileIdentity, ProductError> {
    #[cfg(not(unix))]
    { let _ = metadata; Err(ProductError::InvalidConfig) }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if !metadata.file_type().is_file() || metadata.uid() != a01_effective_uid()
            || metadata.nlink() != 1 || metadata.mode() & 0o777 != 0o600
        {
            return Err(ProductError::InvalidConfig);
        }
        Ok(A01FileIdentity { device: metadata.dev(), inode: metadata.ino() })
    }
}

#[cfg(feature = "a01-harness-support")]
fn a01_file_identity(path: &Path) -> Result<A01FileIdentity, ProductError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| ProductError::Invariant("A01 file inspection failed"))?;
    a01_metadata_identity(&metadata)
}

#[cfg(feature = "a01-harness-support")]
fn a01_checked_file(path: &Path, held: &std::fs::File) -> Result<A01FileIdentity, ProductError> {
    let metadata = held.metadata()
        .map_err(|_| ProductError::Invariant("A01 file inspection failed"))?;
    let identity = a01_metadata_identity(&metadata)?;
    if a01_file_identity(path)? != identity {
        return Err(ProductError::Invariant("A01 physical identity changed"));
    }
    Ok(identity)
}

#[cfg(feature = "a01-harness-support")]
fn configure_a01_connection(conn: &Connection) -> Result<(), ProductError> {
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    conn.pragma_update(None, "foreign_keys", 1)?;
    conn.pragma_update(None, "recursive_triggers", 1)?;
    conn.pragma_update(None, "ignore_check_constraints", 0)?;
    conn.pragma_update(None, "journal_mode", "delete")?;
    validate_a01_pragmas(conn)
}

#[cfg(feature = "a01-harness-support")]
fn validate_a01_pragmas(conn: &Connection) -> Result<(), ProductError> {
    for (pragma, expected) in [
        ("foreign_keys", 1), ("recursive_triggers", 1),
        ("ignore_check_constraints", 0), ("busy_timeout", 5000),
    ] {
        let actual: i64 = conn.pragma_query_value(None, pragma, |row| row.get(0))?;
        if actual != expected { return Err(ProductError::ContractMismatch); }
    }
    let journal: String = conn.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    if journal != "delete" { return Err(ProductError::ContractMismatch); }
    Ok(())
}

#[cfg(feature = "a01-harness-support")]
fn a01_read_header(file: &std::fs::File) -> Result<[u8; 20], ProductError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;

        let mut header = [0_u8; 20];
        file.read_exact_at(&mut header, 0).map_err(|error| {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                ProductError::ContractMismatch
            } else {
                ProductError::Invariant("A01 header inspection failed")
            }
        })?;
        Ok(header)
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Err(ProductError::InvalidConfig)
    }
}

#[cfg(feature = "a01-harness-support")]
fn a01_contract_error(error: rusqlite::Error) -> ProductError {
    match error {
        rusqlite::Error::InvalidColumnType(..)
        | rusqlite::Error::InvalidColumnIndex(..)
        | rusqlite::Error::InvalidColumnName(..)
        | rusqlite::Error::IntegralValueOutOfRange(..)
        | rusqlite::Error::FromSqlConversionFailure(..)
        | rusqlite::Error::QueryReturnedNoRows => ProductError::ContractMismatch,
        // Busy, I/O, storage and other operational errors keep their cause.
        other => ProductError::Database(other),
    }
}

#[cfg(feature = "a01-harness-support")]
fn a01_schema_definitions(conn: &Connection) -> Result<Vec<(String, String, String, Option<String>)>, ProductError> {
    let mut statement = conn.prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name")?;
    let rows = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    rows.collect::<rusqlite::Result<_>>().map_err(a01_contract_error)
}

#[cfg(feature = "a01-harness-support")]
fn validate_a01_binding_history(conn: &Connection) -> Result<(), ProductError> {
    let has_task: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='product_a01_profile_v1' AND type='table')",[],|r|r.get(0))?;
    if !has_task {
        let bindings:i64=conn.query_row("SELECT count(*) FROM product_result_bindings_v1",[],|r|r.get(0))?;
        if bindings!=0 { return Err(ProductError::ContractMismatch); }
    }

    Ok(())
}

#[cfg(feature = "a01-harness-support")]
fn validate_a01_contract(conn: &Connection, config: &ProductConfig) -> Result<(), ProductError> {
    validate_a01_profile_contract(conn, config, StoreOpenMode::A01Create)
}

#[cfg(feature = "a01-harness-support")]
fn validate_a01_profile_contract(conn: &Connection, config: &ProductConfig, profile: StoreOpenMode) -> Result<(), ProductError> {
    // Rebuild only an in-memory reference using the same trusted initializers.
    // Compare every definition, including trigger bodies and alternate indexes;
    // presence of a familiar object name is insufficient. Never repair disk.
    let mut reference = Connection::open_in_memory()?;
    reference.pragma_update(None, "foreign_keys", 1)?;
    reference.pragma_update(None, "recursive_triggers", 1)?;
    let tx = reference.transaction_with_behavior(TransactionBehavior::Immediate)?;
    initialize_product_schema_in(&tx, config, profile)?;
    if a01_schema_definitions(conn)? != a01_schema_definitions(&tx)? {
        return Err(ProductError::ContractMismatch);
    }
    // Validate the entire row set before any singleton-filtered value checks.
    // CHECK constraints may have been disabled by a previous connection.
    for table in [
        "foundry_contract",
        "product_meta_v1",
        "product_review_meta_v1",
        "product_result_meta_v1",
    ] {
        let rows: (i64, i64) = conn.query_row(
            &format!("SELECT count(*),count(CASE WHEN singleton=1 THEN 1 END) FROM {table}"),
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).map_err(a01_contract_error)?;
        if rows != (1, 1) {
            return Err(ProductError::ContractMismatch);
        }
    }
    let contract: (i64, String) = conn.query_row(
        "SELECT count(*),contract FROM foundry_contract", [], |r| Ok((r.get(0)?, r.get(1)?)),
    ).map_err(a01_contract_error)?;
    if contract != (1, "ProductAtomicV1".into()) { return Err(ProductError::ContractMismatch); }
    for (table, version) in [("product_meta_v1", 0x0A01), ("product_review_meta_v1", 1)] {
        let meta: (i64, String, String, i64) = conn.query_row(
            &format!("SELECT count(*),installation_id,database_id,schema_version FROM {table} WHERE singleton=1"),
            [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        ).map_err(a01_contract_error)?;
        if meta != (1, config.installation_id.clone(), config.database_id.clone(), version) {
            return Err(ProductError::ContractMismatch);
        }
    }
    result::validate_v3_connection(conn, &config.installation_id, &config.database_id)
        .map_err(|error| match error {
            ProductError::Database(error) => a01_contract_error(error), other => other,
        })?;
    validate_a01_binding_history(conn)?;
    let mut fk = conn.prepare("PRAGMA foreign_key_check")?;
    if fk.query([])?.next()?.is_some() { return Err(ProductError::ContractMismatch); }
    Ok(())
}

#[cfg(feature = "a01-harness-support")]
fn validate_a01_connection(conn: &Connection, config: &ProductConfig) -> Result<(), ProductError> {
    validate_a01_pragmas(conn)?;
    validate_a01_contract(conn, config)
}

fn normalized_path(path: &Path) -> Result<PathBuf, ProductError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| ProductError::InvalidConfig)?
            .join(path)
    };
    let parent = absolute.parent().ok_or(ProductError::InvalidConfig)?;
    std::fs::create_dir_all(parent).map_err(|_| ProductError::InvalidConfig)?;
    let parent = parent
        .canonicalize()
        .map_err(|_| ProductError::InvalidConfig)?;
    Ok(parent.join(absolute.file_name().ok_or(ProductError::InvalidConfig)?))
}

fn resolve_session_in(
    tx: &rusqlite::Transaction<'_>,
    session_digest: &str,
    now: DateTime<Utc>,
) -> Result<ProductPrincipal, ProductError> {
    if !is_sha256_hex(session_digest) {
        return Err(ProductError::Invariant("session invalid"));
    }
    let row: Option<(String,String,i64,i64,String,i64)> = tx.query_row(
        "SELECT s.actor_id,a.role,s.authority_revision,a.revision,s.expires_at,s.revoked FROM product_sessions_v1 s JOIN product_accounts_v1 a ON a.actor_id=s.actor_id WHERE s.session_digest=?1 AND a.enabled=1",
        [session_digest], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
    ).optional()?;
    let Some((actor_id, role, session_revision, authority_revision, expires, revoked)) = row else {
        return Err(ProductError::Invariant("session invalid"));
    };
    let expires = DateTime::parse_from_rfc3339(&expires)
        .map_err(|_| ProductError::Invariant("session expiry invalid"))?
        .with_timezone(&Utc);
    if revoked != 0 || session_revision != authority_revision || now >= expires {
        return Err(ProductError::Invariant(
            "session expired, revoked, or stale",
        ));
    }
    Ok(ProductPrincipal {
        actor_id,
        role,
        authority_revision,
    })
}

fn verified_order_state_in(
    tx: &rusqlite::Transaction<'_>,
    order_id: &str,
) -> Result<(i64, u32, String), ProductError> {
    let mut statement = tx.prepare(
        "SELECT seq,order_id,from_state,to_state,event,actor,intent,at,prev_hash,hash,event_json FROM ledger ORDER BY seq",
    )?;
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
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let lifecycles = crate::replay::replay(&rows, crate::sqlite::SqliteLedger::verify_rows)
        .map_err(|_| ProductError::Invariant("S1 chain or lifecycle verification failed"))?;
    let current = lifecycles
        .iter()
        .rev()
        .find(|order| order.order_id == order_id)
        .ok_or(ProductError::Invariant(
            "order has no verified S1 lifecycle",
        ))?;
    Ok((
        current.last_seq,
        current.lifecycle,
        current.order.state().to_string(),
    ))
}

fn operation_id(installation: &str, actor: &str, request_key: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    for value in [
        "foundry-product-operation-v1",
        installation,
        actor,
        request_key,
    ] {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    hex::encode(hash.finalize())
}

fn product_hold_operation_id(installation: &str, actor: &str, request_key: &str) -> String {
    framed_sha256(&[
        "foundry-product-hold-operation-v1".into(),
        installation.into(),
        actor.into(),
        request_key.into(),
    ])
}

#[allow(clippy::too_many_arguments)]
fn product_hold_request_digest(
    installation: &str,
    actor: &str,
    authority_revision: i64,
    request_key: &str,
    order_id: &str,
    lifecycle: i64,
    hold_instance_id: &str,
    hold_revision: i64,
    attempt: i64,
    originating_run: i64,
    activation: i64,
    host_epoch: i64,
    source_revision: i64,
    predecessor_seq: i64,
    snapshot_digest: &str,
    decision: &ProductHoldDecision,
    source_instance_id: &str,
    scope_revision: i64,
    grant_revision: i64,
) -> String {
    let encoded_decision = serde_json::to_string(decision).unwrap_or_default();
    framed_sha256(&[
        "foundry-product-review-request-v1".into(),
        installation.into(),
        actor.into(),
        request_key.into(),
        order_id.into(),
        authority_revision.to_string(),
        lifecycle.to_string(),
        hold_instance_id.into(),
        hold_revision.to_string(),
        attempt.to_string(),
        originating_run.to_string(),
        activation.to_string(),
        host_epoch.to_string(),
        source_revision.to_string(),
        predecessor_seq.to_string(),
        snapshot_digest.into(),
        encoded_decision,
        source_instance_id.into(),
        scope_revision.to_string(),
        grant_revision.to_string(),
    ])
}

fn product_review_snapshot_digest(snapshot_json: &str) -> String {
    framed_sha256(&[
        "foundry-product-review-snapshot-v1".into(),
        snapshot_json.into(),
    ])
}

fn framed_sha256(fields: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    for field in fields {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    hex::encode(hash.finalize())
}

fn approval_digest(installation: &str, actor: &str, request: &ProductApprovalIntent) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    let values = vec![
        "foundry-product-approval-v1".to_string(),
        installation.to_string(),
        actor.to_string(),
        request.order_id.clone(),
        request.request_key.clone(),
        request.grant_id.clone(),
        request.token_digest.clone(),
        request.expected_source_revision.to_string(),
        request.expected_attempt.to_string(),
        request.run_id.to_string(),
        request.generation.to_string(),
        request.artifact_digest.clone(),
        request.intent.clone(),
        request.tuple_digest.clone(),
    ];
    for value in values {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    hex::encode(hash.finalize())
}

fn product_tuple_digest(
    order: &str,
    attempt: i64,
    run: i64,
    generation: i64,
    commit: &str,
    location: &str,
    generations_json: &str,
) -> Result<String, ProductError> {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"foundry-artifact-tuple-v1");
    for bytes in [
        order.as_bytes(),
        &attempt.to_be_bytes(),
        &run.to_be_bytes(),
        &generation.to_be_bytes(),
        commit.as_bytes(),
        location.as_bytes(),
    ] {
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    // Validate JSON before digesting, then re-encode canonically as the exec
    // engine's serde representation does for its generations vector.
    let generations: serde_json::Value = serde_json::from_str(generations_json)
        .map_err(|_| ProductError::Invariant("promoted tuple generations invalid"))?;
    hash.update(
        serde_json::to_vec(&generations)
            .map_err(|_| ProductError::Invariant("promoted tuple generations invalid"))?,
    );
    Ok(hex::encode(hash.finalize()))
}

fn payload_digest(kind: &str, payload: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"foundry-product-intake-v1\0");
    h.update((kind.len() as u64).to_be_bytes());
    h.update(kind.as_bytes());
    h.update((payload.len() as u64).to_be_bytes());
    h.update(payload.as_bytes());
    hex::encode(h.finalize())
}

/// Version digest includes the observed remote update marker and canonical
/// execution snapshot. It is separate from the stable repository/issue key.
pub fn delivery_version_digest(
    issue_id: &str,
    observed_updated_at: &str,
    task_kind: &str,
    payload: &str,
) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"foundry-github-delivery-version-v1\0");
    for value in [issue_id, observed_updated_at, task_kind, payload] {
        h.update((value.len() as u64).to_be_bytes());
        h.update(value.as_bytes());
    }
    hex::encode(h.finalize())
}

/// Opaque, installation and origin scoped delivery key. Repository and issue
/// names/numbers are intentionally absent; stable API IDs identify them.
pub fn github_delivery_id(
    installation: &str,
    connector: &str,
    origin: &str,
    repository_id: &str,
    issue_id: &str,
    version_digest: &str,
) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"foundry-github-delivery-id-v1\0");
    for value in [
        installation,
        connector,
        origin,
        repository_id,
        issue_id,
        version_digest,
    ] {
        h.update((value.len() as u64).to_be_bytes());
        h.update(value.as_bytes());
    }
    hex::encode(h.finalize())
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn sha256_hex(value: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(value))
}

#[cfg(test)]
fn product_insert_killpoint(boundary: &str) {
    if std::env::var("B1_PRODUCT_KILL_BOUNDARY").as_deref() != Ok(boundary) {
        return;
    }
    use std::io::{Read, Write};
    println!("B1_BOUNDARY:{boundary}");
    let _ = std::io::stdout().flush();
    let mut byte = [0_u8; 1];
    let _ = std::io::stdin().read(&mut byte);
}

#[cfg(test)]
fn product_hold_insert_killpoint(boundary: &str) {
    product_insert_killpoint(boundary);
}

#[cfg(test)]
mod insert_kill_tests {
    use super::*;
    use std::io::BufRead;

    #[test]
    fn child_pauses_at_selected_real_product_hold_boundary() {
        let (Ok(path), Ok(session)) = (
            std::env::var("B1_PRODUCT_KILL_DB"),
            std::env::var("B1_PRODUCT_KILL_SESSION"),
        ) else {
            return;
        };
        let mut store = ProductStore::open(Path::new(&path), config()).unwrap();
        let expected = {
            let conn = Connection::open(&path).unwrap();
            conn.query_row("SELECT lifecycle,attempt,originating_run,activation,host_epoch,hold_revision,source_revision,expected_predecessor,snapshot_digest FROM product_review_holds_v1 WHERE hold_instance_id='kill-hold'", [], |r| Ok(ProductHoldExpectation { lifecycle:r.get(0)?,attempt:r.get(1)?,originating_run:r.get(2)?,activation:r.get(3)?,host_epoch:r.get(4)?,hold_revision:r.get(5)?,source_revision:r.get(6)?,predecessor_seq:r.get(7)?,snapshot_digest:r.get(8)? })).unwrap()
        };
        store
            .accept_product_review_hold(
                &session,
                "insert-kill-order",
                "kill-hold",
                &expected,
                "kill-hold-key",
                ProductHoldDecision::CommitPass,
                Utc::now(),
            )
            .unwrap();
    }

    #[test]
    fn child_competes_for_product_hold_acceptance() {
        let (Ok(path), Ok(session), Ok(gate), Ok(ready), Ok(request_key)) = (
            std::env::var("B1_HOLD_RACE_DB"),
            std::env::var("B1_HOLD_RACE_SESSION"),
            std::env::var("B1_HOLD_RACE_GATE"),
            std::env::var("B1_HOLD_RACE_READY"),
            std::env::var("B1_HOLD_RACE_KEY"),
        ) else {
            return;
        };
        let mut store = ProductStore::open(Path::new(&path), config()).unwrap();
        let expected = {
            let conn = Connection::open(&path).unwrap();
            conn.query_row("SELECT lifecycle,attempt,originating_run,activation,host_epoch,hold_revision,source_revision,expected_predecessor,snapshot_digest FROM product_review_holds_v1 WHERE hold_instance_id='kill-hold'",[],|r|Ok(ProductHoldExpectation{lifecycle:r.get(0)?,attempt:r.get(1)?,originating_run:r.get(2)?,activation:r.get(3)?,host_epoch:r.get(4)?,hold_revision:r.get(5)?,source_revision:r.get(6)?,predecessor_seq:r.get(7)?,snapshot_digest:r.get(8)?})).unwrap()
        };
        std::fs::write(ready, b"ready").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !Path::new(&gate).exists() {
            assert!(std::time::Instant::now() < deadline, "race gate timeout");
            std::thread::yield_now();
        }
        match store.accept_product_review_hold(
            &session,
            "insert-kill-order",
            "kill-hold",
            &expected,
            &request_key,
            ProductHoldDecision::CommitPass,
            Utc::now(),
        ) {
            Ok(receipt) => println!("HOLD_RESULT:accepted:{}", receipt.operation_id),
            Err(_) => println!("HOLD_RESULT:denied"),
        }
    }

    fn config() -> ProductConfig {
        ProductConfig {
            installation_id: "b1-insert-kill".into(),
            database_id: "test-db".into(),
        }
    }
    fn append_state(conn: &Connection, from: &str, to: &str, event: OrderEvent) {
        use sha2::Digest as _;
        let seq: i64 = conn
            .query_row("SELECT COALESCE(MAX(seq),0)+1 FROM ledger", [], |r| {
                r.get(0)
            })
            .unwrap();
        let prev: String = conn
            .query_row(
                "SELECT hash FROM ledger ORDER BY seq DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let predecessor: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(seq),0) FROM ledger WHERE order_id='insert-kill-order'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute("INSERT INTO product_transition_intents_v1(order_id,lifecycle,expected_predecessor,operation) VALUES('insert-kill-order',1,?1,'test-lifecycle')", [predecessor]).unwrap();
        let event_text = format!("{event:?}");
        let event_json = crate::event_json::event_to_json(&event);
        let at = Utc::now().to_rfc3339();
        let v1 = format!(
            "{seq}|insert-kill-order|{from}|{to}|{event_text}|Agent/github-intake||{at}|{prev}"
        );
        let hash = hex::encode(sha2::Sha256::digest(
            crate::event_json::v2_hash_payload(&v1, &event_json).as_bytes(),
        ));
        conn.execute("INSERT INTO ledger(order_id,from_state,to_state,event,actor,intent,at,prev_hash,event_json,hash) VALUES('insert-kill-order',?1,?2,?3,'Agent/github-intake','',?4,?5,?6,?7)", params![from,to,event_text,at,prev,event_json,hash]).unwrap();
    }

    fn setup_hold_only(path: &Path) -> String {
        let mut store = ProductStore::open(path, config()).unwrap();
        store
            .bind_source(
                "gh",
                "github",
                "https://api.github.com",
                "repo:42",
                "repo:42",
            )
            .unwrap();
        store
            .admit_order(
                "insert-kill-order",
                "gh",
                "repo:42",
                "issue:7",
                "dep-bump",
                "snapshot",
                Utc::now(),
            )
            .unwrap();
        let session = "a".repeat(64);
        let conn = Connection::open(path).unwrap();
        conn.execute("INSERT INTO product_accounts_v1(actor_id,role,enabled,revision) VALUES('owner','owner',1,1)", []).unwrap();
        conn.execute("INSERT INTO product_sessions_v1(session_digest,actor_id,authority_revision,expires_at,revoked) VALUES(?1,'owner',1,?2,0)", params![session,(Utc::now()+chrono::Duration::hours(1)).to_rfc3339()]).unwrap();
        append_state(&conn, "queued", "working", OrderEvent::DieStarted);
        append_state(
            &conn,
            "working",
            "inspecting",
            OrderEvent::DieSucceeded { attempt: 1 },
        );
        append_state(&conn, "inspecting", "waiting", OrderEvent::InspectionPassed);
        drop(conn);
        session
    }

    #[test]
    fn sigkill_at_each_real_hold_acceptance_statement_is_all_or_none() {
        use std::process::{Command, Stdio};
        use std::sync::mpsc;
        use std::time::{Duration, SystemTime, UNIX_EPOCH};
        let root = std::env::temp_dir().join(format!(
            "b1-hold-kill-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        for boundary in [
            "after_hold_cas",
            "after_run_reservation",
            "after_acceptance_insert",
            "after_progress_insert",
            "after_acceptance_commit",
        ] {
            let path = root.join(format!("{boundary}.db"));
            let session = setup_hold_only(&path);
            let conn = Connection::open(&path).unwrap();
            let (admission_seq,admission_hash):(i64,String)=conn.query_row("SELECT admission_seq,admission_hash FROM product_orders_v1 WHERE order_id='insert-kill-order'",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
            let predecessor: i64 = conn
                .query_row(
                    "SELECT MAX(seq) FROM ledger WHERE order_id='insert-kill-order'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let snapshot = "{\"review\":\"frozen\"}";
            let snapshot_digest = product_review_snapshot_digest(snapshot);
            conn.execute("INSERT INTO product_review_holds_v1(order_id,lifecycle,attempt,originating_run,activation,host_epoch,hold_instance_id,hold_revision,expected_predecessor,snapshot_digest,snapshot_json,allowed_targets_json,deadline,source_revision,admission_seq,admission_hash,state) VALUES('insert-kill-order',1,1,1,1,7,'kill-hold',3,?1,?2,?3,'[]',?4,1,?5,?6,'active')",params![predecessor,snapshot_digest,snapshot,(Utc::now()+chrono::Duration::hours(1)).to_rfc3339(),admission_seq,admission_hash]).unwrap();
            drop(conn);
            let mut child=Command::new(std::env::current_exe().unwrap()).args(["--exact","product::insert_kill_tests::child_pauses_at_selected_real_product_hold_boundary","--nocapture"]).env("B1_PRODUCT_KILL_DB",&path).env("B1_PRODUCT_KILL_SESSION",&session).env("B1_PRODUCT_KILL_BOUNDARY",boundary).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().unwrap();
            let stdout = child.stdout.take().unwrap();
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(stdout).lines() {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
            let marker = format!("B1_BOUNDARY:{boundary}");
            let reached = loop {
                match rx.recv_timeout(Duration::from_secs(10)) {
                    Ok(Ok(line)) if line.trim() == marker => break true,
                    Ok(Ok(_)) => continue,
                    Ok(Err(_))
                    | Err(mpsc::RecvTimeoutError::Disconnected)
                    | Err(mpsc::RecvTimeoutError::Timeout) => break false,
                }
            };
            if !reached {
                let _ = child.kill();
                let _ = child.wait();
                panic!("child failed to reach hold boundary {boundary}");
            }
            child.kill().unwrap();
            let status = child.wait().unwrap();
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(9));
            let conn = Connection::open(&path).unwrap();
            let rows:(String,i64,i64,i64,i64)=conn.query_row("SELECT (SELECT state FROM product_review_holds_v1 WHERE hold_instance_id='kill-hold'),(SELECT count(*) FROM product_review_run_reservations_v1),(SELECT count(*) FROM product_review_hold_acceptances_v1),(SELECT count(*) FROM product_review_hold_progress_v1),(SELECT count(*) FROM product_review_hold_acceptances_v1 WHERE request_key='kill-hold-key')",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap();
            if boundary == "after_acceptance_commit" {
                assert_eq!(rows, ("accepted".into(), 1, 1, 1, 1));
            } else {
                assert_eq!(
                    rows,
                    ("active".into(), 0, 0, 0, 0),
                    "partial acceptance survived {boundary}"
                );
            }
            let mut reopened = ProductStore::open(&path, config()).unwrap();
            reopened
                .admit_order(
                    &format!("writable-{boundary}"),
                    "gh",
                    "repo:42",
                    "issue:99",
                    "dep-bump",
                    "writable",
                    Utc::now(),
                )
                .unwrap();
            drop(reopened);
            let _ = session;
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn separate_process_hold_acceptance_same_and_different_keys() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, SystemTime, UNIX_EPOCH};
        let root = std::env::temp_dir().join(format!(
            "b1-hold-race-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        for same_key in [false, true] {
            let path = root.join(format!("race-{same_key}.db"));
            let session = setup_hold_only(&path);
            let conn = Connection::open(&path).unwrap();
            let (seq,hash):(i64,String)=conn.query_row("SELECT admission_seq,admission_hash FROM product_orders_v1 WHERE order_id='insert-kill-order'",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
            let predecessor: i64 = conn
                .query_row(
                    "SELECT MAX(seq) FROM ledger WHERE order_id='insert-kill-order'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let snapshot = "{\"race\":true}";
            let digest = product_review_snapshot_digest(snapshot);
            conn.execute("INSERT INTO product_review_holds_v1(order_id,lifecycle,attempt,originating_run,activation,host_epoch,hold_instance_id,hold_revision,expected_predecessor,snapshot_digest,snapshot_json,allowed_targets_json,deadline,source_revision,admission_seq,admission_hash,state) VALUES('insert-kill-order',1,1,1,1,8,'kill-hold',1,?1,?2,?3,'[]',?4,1,?5,?6,'active')",params![predecessor,digest,snapshot,(Utc::now()+chrono::Duration::hours(1)).to_rfc3339(),seq,hash]).unwrap();
            drop(conn);
            let gate = root.join(format!("gate-{same_key}"));
            let ready_a = root.join(format!("a-{same_key}"));
            let ready_b = root.join(format!("b-{same_key}"));
            let exe = std::env::current_exe().unwrap();
            let spawn = |ready: &Path, key: &str| {
                Command::new(&exe)
                    .args([
                        "--exact",
                        "product::insert_kill_tests::child_competes_for_product_hold_acceptance",
                        "--nocapture",
                    ])
                    .env("B1_HOLD_RACE_DB", &path)
                    .env("B1_HOLD_RACE_SESSION", &session)
                    .env("B1_HOLD_RACE_GATE", &gate)
                    .env("B1_HOLD_RACE_READY", ready)
                    .env("B1_HOLD_RACE_KEY", key)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .unwrap()
            };
            let a = spawn(&ready_a, "same-hold-key");
            let b = spawn(
                &ready_b,
                if same_key {
                    "same-hold-key"
                } else {
                    "different-hold-key"
                },
            );
            let until = std::time::Instant::now() + Duration::from_secs(10);
            while !ready_a.exists() || !ready_b.exists() {
                assert!(
                    std::time::Instant::now() < until,
                    "hold contender did not reach barrier"
                );
                std::thread::yield_now();
            }
            std::fs::write(&gate, b"go").unwrap();
            let out_a = a.wait_with_output().unwrap();
            let out_b = b.wait_with_output().unwrap();
            assert!(
                out_a.status.success() && out_b.status.success(),
                "a contender test process failed"
            );
            let text_a = String::from_utf8_lossy(&out_a.stdout);
            let text_b = String::from_utf8_lossy(&out_b.stdout);
            assert!(text_a.contains("HOLD_RESULT:"));
            assert!(text_b.contains("HOLD_RESULT:"));
            let accepted_a = text_a
                .lines()
                .find(|l| l.starts_with("HOLD_RESULT:"))
                .unwrap();
            let accepted_b = text_b
                .lines()
                .find(|l| l.starts_with("HOLD_RESULT:"))
                .unwrap();
            if same_key {
                assert_eq!(accepted_a, accepted_b);
                assert!(accepted_a.starts_with("HOLD_RESULT:accepted:"));
            } else {
                assert_ne!(
                    accepted_a.starts_with("HOLD_RESULT:accepted:"),
                    accepted_b.starts_with("HOLD_RESULT:accepted:")
                );
            }
            let conn = Connection::open(&path).unwrap();
            let count: i64 = conn
                .query_row(
                    "SELECT count(*) FROM product_review_hold_acceptances_v1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1);
            drop(conn);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod scoped_stream_tests {
    use super::{ScopedS1ScanStats, verify_scoped_s1};
    use crate::{LedgerRow, event_json, sqlite};
    use rusqlite::{Connection, params};

    fn ledger_fixture(count: usize, illegal_at: Option<usize>) -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE ledger(seq INTEGER PRIMARY KEY, order_id TEXT NOT NULL, from_state TEXT NOT NULL,
             to_state TEXT NOT NULL, event TEXT NOT NULL, actor TEXT NOT NULL, intent TEXT NOT NULL,
             at TEXT NOT NULL, prev_hash TEXT NOT NULL, hash TEXT NOT NULL, event_json TEXT);
             CREATE INDEX ledger_order_seq_v1 ON ledger(order_id,seq);",
        ).unwrap();
        let tx = conn.transaction().unwrap();
        let mut previous = "GENESIS".to_owned();
        for index in 1..=count {
            let event = if illegal_at == Some(index) {
                "NotAnOrderEvent"
            } else {
                "Queued"
            };
            let event_json = (index % 2 == 0)
                .then(|| event_json::event_to_json(&foundry_domain::OrderEvent::Queued));
            let mut row = LedgerRow {
                seq: index as i64,
                order_id: format!("foreign-{index:05}"),
                from_state: "received".into(),
                to_state: "queued".into(),
                event: event.into(),
                actor: "synthetic".into(),
                intent: String::new(),
                at: "2026-10-18T00:00:00Z".into(),
                prev_hash: previous,
                hash: String::new(),
                event_json,
            };
            row.hash = sqlite::expected_row_hash(&row, &row.prev_hash);
            tx.execute(
                "INSERT INTO ledger(seq,order_id,from_state,to_state,event,actor,intent,at,prev_hash,hash,event_json)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![row.seq,row.order_id,row.from_state,row.to_state,row.event,row.actor,row.intent,
                    row.at,row.prev_hash,row.hash,row.event_json],
            ).unwrap();
            previous = row.hash;
        }
        tx.commit().unwrap();
        conn
    }

    fn scan(conn: &Connection) -> Result<Option<ScopedS1ScanStats>, super::ProductError> {
        let tx = conn.unchecked_transaction()?;
        let stats = verify_scoped_s1(&tx)?;
        tx.commit()?;
        Ok(stats)
    }

    #[test]
    fn foreign_order_growth_keeps_verifier_rows_and_states_constant() {
        let conn = ledger_fixture(1_000, None);
        let stats = scan(&conn).unwrap().unwrap();
        assert_eq!((stats.chain_rows, stats.domain_rows), (1_000, 1_000));
        assert_eq!(stats.peak_retained_rows, 1);
        assert_eq!(stats.peak_order_states, 1);
    }

    #[test]
    fn full_stream_rejects_hash_tampering_sequence_gaps_and_illegal_domain_events() {
        let conn = ledger_fixture(3, None);
        conn.execute("UPDATE ledger SET actor='tampered' WHERE seq=2", [])
            .unwrap();
        assert!(scan(&conn).unwrap().is_none());

        let gap = ledger_fixture(3, None);
        gap.execute("DELETE FROM ledger WHERE seq=2", []).unwrap();
        assert_eq!(
            gap.query_row("SELECT COUNT(*) FROM ledger WHERE seq=2", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(scan(&gap).unwrap().is_none());

        let illegal = ledger_fixture(3, Some(2));
        assert!(scan(&illegal).unwrap().is_none());
    }
}

#[cfg(all(test, feature = "w1-isolated-test-support", unix))]
mod isolated_operation_validation_tests {
    use super::*;
    use rusqlite::{ffi, types::Type};
    use std::os::unix::fs::DirBuilderExt;

    #[test]
    fn operation_schema_error_classifies_synthetic_errors() {
        // Constructed errors only: no actual I/O or locking failure is induced.
        for code in [ffi::SQLITE_ERROR, ffi::SQLITE_IOERR, ffi::SQLITE_BUSY] {
            let error = rusqlite::Error::SqliteFailure(ffi::Error::new(code), None);
            assert!(matches!(isolated_schema_error(error), ProductError::Database(rusqlite::Error::SqliteFailure(inner, _)) if inner.extended_code == code));
        }
        let error = rusqlite::Error::InvalidColumnType(0, "value".into(), Type::Text);
        assert!(matches!(isolated_schema_error(error), ProductError::ContractMismatch));
    }

    #[test]
    fn actual_isolated_store_connection_pragmas_on_create_and_reopen() {
        let root = std::env::temp_dir().join(format!("foundry-w1-connection-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let config = || ProductConfig { installation_id: "w1-install".into(), database_id: "w1-db".into() };
        for reopen in [false, true] {
            let store = if reopen { ProductStore::reopen_isolated_test_store(&root, "isolated.db", config()).unwrap() } else { ProductStore::create_isolated_test_store(&root, "isolated.db", config()).unwrap() };
            for pragma in ["foreign_keys", "recursive_triggers"] {
                assert_eq!(store.conn.query_row(&format!("PRAGMA {pragma}"), [], |row| row.get::<_, i64>(0)).unwrap(), 1);
            }
            assert!(validate_isolated_pragmas(&store.conn).is_ok());
            for pragma in ["foreign_keys", "recursive_triggers"] {
                store.conn.pragma_update(None, pragma, "OFF").unwrap();
                assert_eq!(store.conn.query_row(&format!("PRAGMA {pragma}"), [], |row| row.get::<_, i64>(0)).unwrap(), 0);
                assert!(matches!(validate_isolated_pragmas(&store.conn), Err(ProductError::ContractMismatch)));
                store.conn.pragma_update(None, pragma, "ON").unwrap();
                assert!(validate_isolated_pragmas(&store.conn).is_ok());
            }
            drop(store);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}


#[cfg(feature = "w1-isolated-test-support")]
#[allow(dead_code)]
mod w1_owner_codec {
    const REQUEST: &[u8] = b"foundry-w1-owner-operation-v2\0";
    const RECEIPT: &[u8] = b"foundry-w1-owner-receipt-v2\0";
    const CAP: usize = 8192;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) struct Invalid;
    type Result<T> = std::result::Result<T, Invalid>;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) struct Request {
        pub installation: String,
        pub database: String,
        pub operation: String,
        pub actor: String,
        pub source: String,
        pub digest: String,
        pub expected: i64,
        pub key: String,
        pub order: Option<String>,
        pub lifecycle: Option<i64>,
    }
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) struct Receipt {
        pub installation: String,
        pub database: String,
        pub operation: String,
        pub actor: String,
        pub authority: i64,
        pub source: String,
        pub digest: String,
        pub expected: i64,
        pub result: i64,
        pub key: String,
        pub order: Option<String>,
        pub lifecycle: Option<i64>,
        pub approval: Option<i64>,
    }
    pub(super) fn within_cap(n: usize) -> bool {
        n <= CAP
    }
    fn text(s: &str) -> bool {
        !s.is_empty() && s.len() <= 256 && !s.as_bytes().contains(&0)
    }
    fn common(
        i: &str,
        d: &str,
        op: &str,
        a: &str,
        s: &str,
        digest: &str,
        expected: i64,
        key: &str,
        order: Option<&str>,
        lifecycle: Option<i64>,
    ) -> Result<()> {
        if ![i, d, a, s, key].iter().all(|v| text(v))
            || expected < 0
            || expected == i64::MAX
            || digest.len() != 64
            || !digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Invalid);
        }
        match op {
            "select" if order.is_some_and(text) && lifecycle.is_some_and(|n| n > 0) => Ok(()),
            "approve" | "revoke" if order.is_none() && lifecycle.is_none() => Ok(()),
            _ => Err(Invalid),
        }
    }
    fn frame(domain: &[u8], fields: &[Option<&str>]) -> Result<Vec<u8>> {
        let mut size = domain.len();
        for f in fields {
            size = size.checked_add(1).ok_or(Invalid)?;
            if let Some(s) = f {
                size = size
                    .checked_add(8)
                    .and_then(|n| n.checked_add(s.len()))
                    .ok_or(Invalid)?;
            }
        }
        if !within_cap(size) {
            return Err(Invalid);
        }
        let mut out = Vec::with_capacity(size);
        out.extend_from_slice(domain);
        for f in fields {
            match f {
                None => out.push(0),
                Some(s) => {
                    out.push(1);
                    out.extend_from_slice(
                        &u64::try_from(s.len()).map_err(|_| Invalid)?.to_be_bytes(),
                    );
                    out.extend_from_slice(s.as_bytes());
                }
            }
        }
        Ok(out)
    }
    pub(super) fn encode_request(r: &Request) -> Result<Vec<u8>> {
        common(
            &r.installation,
            &r.database,
            &r.operation,
            &r.actor,
            &r.source,
            &r.digest,
            r.expected,
            &r.key,
            r.order.as_deref(),
            r.lifecycle,
        )?;
        let expected = r.expected.to_string();
        let lifecycle = r.lifecycle.map(|n| n.to_string());
        frame(
            REQUEST,
            &[
                Some(&r.installation),
                Some(&r.database),
                Some(&r.operation),
                Some(&r.actor),
                Some(&r.source),
                Some(&r.digest),
                Some(&expected),
                Some(&r.key),
                r.order.as_deref(),
                lifecycle.as_deref(),
            ],
        )
    }
    pub(super) fn encode_receipt(r: &Receipt) -> Result<Vec<u8>> {
        common(
            &r.installation,
            &r.database,
            &r.operation,
            &r.actor,
            &r.source,
            &r.digest,
            r.expected,
            &r.key,
            r.order.as_deref(),
            r.lifecycle,
        )?;
        if r.authority < 0
            || r.expected.checked_add(1) != Some(r.result)
            || r.result <= 0
            || (r.operation == "select" && !r.approval.is_some_and(|n| n > 0))
            || (r.operation != "select" && r.approval.is_some())
        {
            return Err(Invalid);
        }
        let authority = r.authority.to_string();
        let expected = r.expected.to_string();
        let result = r.result.to_string();
        let lifecycle = r.lifecycle.map(|n| n.to_string());
        let approval = r.approval.map(|n| n.to_string());
        frame(
            RECEIPT,
            &[
                Some(&r.installation),
                Some(&r.database),
                Some(&r.operation),
                Some(&r.actor),
                Some(&authority),
                Some(&r.source),
                Some(&r.digest),
                Some(&expected),
                Some(&result),
                Some(&r.key),
                r.order.as_deref(),
                lifecycle.as_deref(),
                approval.as_deref(),
            ],
        )
    }
    fn parse<'a>(input: &'a [u8], domain: &[u8], count: usize) -> Result<Vec<Option<&'a str>>> {
        if !within_cap(input.len()) || !input.starts_with(domain) {
            return Err(Invalid);
        }
        let mut at = domain.len();
        let mut fields = Vec::with_capacity(count);
        for _ in 0..count {
            let tag = *input.get(at).ok_or(Invalid)?;
            at = at.checked_add(1).ok_or(Invalid)?;
            match tag {
                0 => fields.push(None),
                1 => {
                    let end = at.checked_add(8).ok_or(Invalid)?;
                    let prefix: [u8; 8] = input
                        .get(at..end)
                        .ok_or(Invalid)?
                        .try_into()
                        .map_err(|_| Invalid)?;
                    let len = usize::try_from(u64::from_be_bytes(prefix)).map_err(|_| Invalid)?;
                    let end_payload = end.checked_add(len).ok_or(Invalid)?;
                    if !within_cap(end_payload) {
                        return Err(Invalid);
                    }
                    let bytes = input.get(end..end_payload).ok_or(Invalid)?;
                    fields.push(Some(std::str::from_utf8(bytes).map_err(|_| Invalid)?));
                    at = end_payload;
                }
                _ => return Err(Invalid),
            }
        }
        if at != input.len() {
            return Err(Invalid);
        }
        Ok(fields)
    }
    fn required(f: Option<&str>) -> Result<String> {
        Ok(f.ok_or(Invalid)?.to_owned())
    }
    fn number(f: Option<&str>) -> Result<i64> {
        let s = f.ok_or(Invalid)?;
        if s.is_empty()
            || (s.len() > 1 && s.starts_with('0'))
            || !s.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(Invalid);
        }
        s.parse().map_err(|_| Invalid)
    }
    fn optional_number(f: Option<&str>) -> Result<Option<i64>> {
        f.map(|s| number(Some(s))).transpose()
    }
    pub(super) fn decode_request(input: &[u8]) -> Result<Request> {
        let f = parse(input, REQUEST, 10)?;
        let r = Request {
            installation: required(f[0])?,
            database: required(f[1])?,
            operation: required(f[2])?,
            actor: required(f[3])?,
            source: required(f[4])?,
            digest: required(f[5])?,
            expected: number(f[6])?,
            key: required(f[7])?,
            order: f[8].map(str::to_owned),
            lifecycle: optional_number(f[9])?,
        };
        if encode_request(&r)? != input {
            return Err(Invalid);
        }
        Ok(r)
    }
    pub(super) fn decode_receipt(input: &[u8]) -> Result<Receipt> {
        let f = parse(input, RECEIPT, 13)?;
        let r = Receipt {
            installation: required(f[0])?,
            database: required(f[1])?,
            operation: required(f[2])?,
            actor: required(f[3])?,
            authority: number(f[4])?,
            source: required(f[5])?,
            digest: required(f[6])?,
            expected: number(f[7])?,
            result: number(f[8])?,
            key: required(f[9])?,
            order: f[10].map(str::to_owned),
            lifecycle: optional_number(f[11])?,
            approval: optional_number(f[12])?,
        };
        if encode_receipt(&r)? != input {
            return Err(Invalid);
        }
        Ok(r)
    }
}

#[cfg(all(test, feature = "w1-isolated-test-support"))]
mod w1_owner_encoding_tests {
    use super::w1_owner_codec::*;
    const DR: &str = "666f756e6472792d77312d6f776e65722d6f7065726174696f6e2d763200";
    const DC: &str = "666f756e6472792d77312d6f776e65722d726563656970742d763200";
    const I: &str = "01000000000000000149";
    const D: &str = "01000000000000000144";
    const ACT: &str = "010000000000000002c3a9";
    const S: &str = "01000000000000000153";
    const Z: &str = "01000000000000000130";
    const ONE: &str = "01000000000000000131";
    const K: &str = "0100000000000000014b";
    const O: &str = "0100000000000000014f";
    fn request(op: &str) -> Request {
        Request {
            installation: "I".into(),
            database: "D".into(),
            operation: op.into(),
            actor: "é".into(),
            source: "S".into(),
            digest: "a".repeat(64),
            expected: 0,
            key: "K".into(),
            order: (op == "select").then(|| "O".into()),
            lifecycle: (op == "select").then_some(1),
        }
    }
    fn receipt(op: &str) -> Receipt {
        let r = request(op);
        Receipt {
            installation: r.installation,
            database: r.database,
            operation: r.operation,
            actor: r.actor,
            authority: 7,
            source: r.source,
            digest: r.digest,
            expected: r.expected,
            result: 1,
            key: r.key,
            order: r.order,
            lifecycle: r.lifecycle,
            approval: (op == "select").then_some(1),
        }
    }
    fn unhex(s: &str) -> Vec<u8> {
        s.as_bytes()
            .chunks_exact(2)
            .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
            .collect()
    }
    fn golden(op: &str, rec: bool) -> Vec<u8> {
        let operation = match op {
            "approve" => "010000000000000007617070726f7665",
            "revoke" => "0100000000000000067265766f6b65",
            _ => "01000000000000000673656c656374",
        };
        let digest = format!("010000000000000040{}", "61".repeat(64));
        let mut parts = vec![if rec { DC } else { DR }, I, D, operation, ACT];
        if rec {
            parts.push("01000000000000000137");
        }
        parts.extend([S, &digest, Z]);
        if rec {
            parts.push(ONE);
        }
        parts.push(K);
        if op == "select" {
            parts.extend([O, ONE]);
            if rec {
                parts.push(ONE);
            }
        } else {
            parts.extend(["00", "00"]);
            if rec {
                parts.push("00");
            }
        }
        unhex(&parts.concat())
    }
    // Test-only framing permits malformed individual fields while retaining all other controls.
    fn raw(rec: bool, fields: &[Option<&[u8]>]) -> Vec<u8> {
        let mut b = unhex(if rec { DC } else { DR });
        for f in fields {
            if let Some(v) = f {
                b.push(1);
                b.extend_from_slice(&(v.len() as u64).to_be_bytes());
                b.extend_from_slice(v);
            } else {
                b.push(0);
            }
        }
        b
    }
    fn fields(rec: bool) -> Vec<Option<Vec<u8>>> {
        let mut f: Vec<Option<Vec<u8>>> = [
            "I",
            "D",
            "select",
            "é",
            "S",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "0",
            "K",
            "O",
            "1",
        ]
        .iter()
        .map(|s| Some(s.as_bytes().to_vec()))
        .collect();
        if rec {
            f.insert(4, Some(b"7".to_vec()));
            f.insert(8, Some(b"1".to_vec()));
            f.push(Some(b"1".to_vec()));
        }
        f
    }
    fn wire(rec: bool, f: &[Option<Vec<u8>>]) -> Vec<u8> {
        raw(rec, &f.iter().map(|v| v.as_deref()).collect::<Vec<_>>())
    }
    fn valid(rec: bool, b: &[u8]) -> bool {
        if rec {
            decode_receipt(b).is_ok()
        } else {
            decode_request(b).is_ok()
        }
    }
    fn typed_valid(rec: bool, f: &[Option<Vec<u8>>]) -> bool {
        let get = |i: usize| f[i].as_ref().map(|v| String::from_utf8(v.clone()).unwrap());
        let num = |i: usize| get(i).map(|s| s.parse::<i64>().unwrap());
        if rec {
            encode_receipt(&Receipt {
                installation: get(0).unwrap(),
                database: get(1).unwrap(),
                operation: get(2).unwrap(),
                actor: get(3).unwrap(),
                authority: num(4).unwrap(),
                source: get(5).unwrap(),
                digest: get(6).unwrap(),
                expected: num(7).unwrap(),
                result: num(8).unwrap(),
                key: get(9).unwrap(),
                order: get(10),
                lifecycle: num(11),
                approval: num(12),
            })
            .is_ok()
        } else {
            encode_request(&Request {
                installation: get(0).unwrap(),
                database: get(1).unwrap(),
                operation: get(2).unwrap(),
                actor: get(3).unwrap(),
                source: get(4).unwrap(),
                digest: get(5).unwrap(),
                expected: num(6).unwrap(),
                key: get(7).unwrap(),
                order: get(8),
                lifecycle: num(9),
            })
            .is_ok()
        }
    }
    #[test]
    fn w1_codec_golden_approve_revoke_select() {
        for (op, lens) in [
            ("approve", (182, 201)),
            ("revoke", (181, 200)),
            ("select", (199, 227)),
        ] {
            let q = golden(op, false);
            let c = golden(op, true);
            assert_eq!((q.len(), c.len()), lens);
            assert_eq!(encode_request(&request(op)).unwrap(), q);
            assert_eq!(decode_request(&q).unwrap(), request(op));
            assert_eq!(encode_receipt(&receipt(op)).unwrap(), c);
            assert_eq!(decode_receipt(&c).unwrap(), receipt(op));
            assert_eq!(encode_request(&decode_request(&q).unwrap()).unwrap(), q);
            assert_eq!(encode_receipt(&decode_receipt(&c).unwrap()).unwrap(), c);
        }
    }
    #[test]
    fn w1_codec_text_and_null_boundaries() {
        for rec in [false, true] {
            let ids: &[usize] = if rec {
                &[0, 1, 3, 5, 9, 10]
            } else {
                &[0, 1, 3, 4, 7, 8]
            };
            for &idx in ids {
                for (s, ok) in [
                    ("x".into(), true),
                    ("x".repeat(256), true),
                    (String::new(), false),
                    ("x".repeat(257), false),
                    ("a\0b".into(), false),
                    ("é".repeat(128), true),
                    ("é".repeat(129), false),
                    (format!("{}x", "é".repeat(128)), false),
                ] {
                    let mut f = fields(rec);
                    f[idx] = Some(s.into_bytes());
                    assert_eq!(valid(rec, &wire(rec, &f)), ok, "{rec} {idx}");
                    assert_eq!(typed_valid(rec, &f), ok);
                }
                let mut f = fields(rec);
                f[idx] = Some(vec![0xff]);
                assert!(!valid(rec, &wire(rec, &f)));
            }
            let n = if rec { 10 } else { 8 };
            for idx in 0..n {
                let mut f = fields(rec);
                f[idx] = None;
                assert!(!valid(rec, &wire(rec, &f)), "{rec} {idx}");
            }
            let mut a = fields(rec);
            a[0] = Some("é".as_bytes().to_vec());
            let mut b = a.clone();
            b[0] = Some("e\u{301}".as_bytes().to_vec());
            assert!(valid(rec, &wire(rec, &a)));
            assert!(valid(rec, &wire(rec, &b)));
            assert_ne!(wire(rec, &a), wire(rec, &b));
            let mut f = fields(rec);
            f[0] = Some(vec![]);
            assert!(!valid(rec, &wire(rec, &f)));
            let mut f = fields(rec);
            f[2] = Some(b"approve".to_vec());
            let tail = if rec { 10 } else { 8 };
            for x in &mut f[tail..] {
                *x = None;
            }
            assert!(valid(rec, &wire(rec, &f)));
            f[tail] = Some(vec![]);
            assert!(!valid(rec, &wire(rec, &f)));
        }
        let mut r = request("select");
        for s in ["", &"x".repeat(257), "x\0y"] {
            r.order = Some(s.into());
            assert!(encode_request(&r).is_err());
        }
    }
    #[test]
    fn w1_codec_domains_and_structure() {
        for rec in [false, true] {
            let f = fields(rec);
            let b = wire(rec, &f);
            assert!(valid(rec, &b));
            let domain = if rec { unhex(DC) } else { unhex(DR) };
            let count = if rec { 13 } else { 10 };
            assert_eq!(f.len(), count, "C03 {rec}: select frame count");
            assert_eq!(b, golden("select", rec), "C03 {rec}: valid baseline framing");
            for omitted in 0..count {
                let mut remaining = f.clone();
                remaining.remove(omitted);
                assert_eq!(remaining.len(), count - 1, "C03 {rec}: one frame removed at {omitted}");
                assert!(!valid(rec, &wire(rec, &remaining)), "C03 {rec}: omitted frame {omitted} must fail");
            }
            assert!(!valid(!rec, &b));
            let mut bad = b.clone();
            bad[0] ^= 1;
            assert!(!valid(rec, &bad));
            for n in 0..domain.len() {
                assert!(!valid(rec, &b[..n]));
            }
            let mut bad = b.clone();
            bad[domain.len() - 1] = 1;
            assert!(!valid(rec, &bad));
            let mut bad = b.clone();
            bad.remove(domain.len() - 1);
            assert!(!valid(rec, &bad));
            for tag in [2, 255] {
                let mut bad = b.clone();
                bad[domain.len()] = tag;
                assert!(!valid(rec, &bad));
            }
            for n in domain.len()..b.len() {
                assert!(!valid(rec, &b[..n]), "{rec} {n}");
            }
            let mut bad = b.clone();
            bad.push(0);
            assert!(!valid(rec, &bad));
            let mut bad = b.clone();
            bad.push(0xff);
            assert!(!valid(rec, &bad));
            let mut swapped = f.clone();
            swapped.swap(0, 1);
            let b = wire(rec, &swapped);
            assert!(valid(rec, &b));
            if rec {
                let v = decode_receipt(&b).unwrap();
                assert_eq!((v.installation, v.database), ("D".into(), "I".into()));
            } else {
                let v = decode_request(&b).unwrap();
                assert_eq!((v.installation, v.database), ("D".into(), "I".into()));
            }
        }
    }
    fn maxima() -> (Request, Receipt) {
        let mut q = request("select");
        q.installation = "i".repeat(256);
        q.database = "d".repeat(256);
        q.actor = "a".repeat(256);
        q.source = "s".repeat(256);
        q.key = "k".repeat(256);
        q.order = Some("o".repeat(256));
        q.expected = i64::MAX - 1;
        q.lifecycle = Some(i64::MAX);
        let mut r = receipt("select");
        r.installation = q.installation.clone();
        r.database = q.database.clone();
        r.actor = q.actor.clone();
        r.source = q.source.clone();
        r.key = q.key.clone();
        r.order = q.order.clone();
        r.expected = q.expected;
        r.lifecycle = q.lifecycle;
        r.authority = i64::MAX;
        r.result = i64::MAX;
        r.approval = Some(i64::MAX);
        (q, r)
    }
    #[test]
    fn w1_codec_bounded_lengths_and_maxima() {
        let (q, r) = maxima();
        assert_eq!(encode_request(&q).unwrap().len(), 1764);
        assert_eq!(encode_receipt(&r).unwrap().len(), 1846);
        assert_eq!(decode_request(&encode_request(&q).unwrap()).unwrap(), q);
        assert_eq!(decode_receipt(&encode_receipt(&r).unwrap()).unwrap(), r);
        if usize::BITS < 64 {
            assert!(usize::try_from(u64::MAX).is_err());
        }
        assert!(within_cap(8191));
        assert!(within_cap(8192));
        assert!(!within_cap(8193));
        for rec in [false, true] {
            assert!(!valid(rec, &vec![0; 8192]));
            assert!(!valid(rec, &vec![0; 8193]));
            let b = wire(rec, &fields(rec));
            let offset = if rec {
                unhex(DC).len()
            } else {
                unhex(DR).len()
            };
            for len in [257u64, 8193, u64::MAX] {
                let mut x = b.clone();
                x[offset + 1..offset + 9].copy_from_slice(&len.to_be_bytes());
                assert!(!valid(rec, &x));
            }
        }
    }
    #[test]
    fn w1_codec_operation_digest_and_nullability() {
        for rec in [false, true] {
            for op in ["approve", "revoke", "select"] {
                assert!(valid(rec, &golden(op, rec)));
                let digest = "0123456789abcdef".repeat(4);
                if rec {
                    let mut value = receipt(op);
                    value.digest = digest.clone();
                    let encoded = encode_receipt(&value).expect("C05: valid receipt digest encodes");
                    assert_eq!(decode_receipt(&encoded).expect("C05: receipt digest decodes").digest, digest, "C05 receipt {op}: exact digest");
                } else {
                    let mut value = request(op);
                    value.digest = digest.clone();
                    let encoded = encode_request(&value).expect("C05: valid request digest encodes");
                    assert_eq!(decode_request(&encoded).expect("C05: request digest decodes").digest, digest, "C05 request {op}: exact digest");
                }
                if rec {
                    assert!(encode_receipt(&receipt(op)).is_ok());
                } else {
                    assert!(encode_request(&request(op)).is_ok());
                }
            }
            for op in [b"".as_slice(), b"SELECT", b"other", b"a\0b", &[0xff]] {
                let mut f = fields(rec);
                f[2] = Some(op.to_vec());
                assert!(!valid(rec, &wire(rec, &f)));
                if std::str::from_utf8(op).is_ok() {
                    assert!(!typed_valid(rec, &f));
                }
            }
            let idx = if rec { 6 } else { 5 };
            for d in [
                "a".repeat(63),
                "a".repeat(64),
                "a".repeat(65),
                "A".repeat(64),
                "g".repeat(64),
                format!("{}\0", "a".repeat(63)),
            ] {
                let mut f = fields(rec);
                f[idx] = Some(d.as_bytes().to_vec());
                assert_eq!(valid(rec, &wire(rec, &f)), d == "a".repeat(64));
                assert_eq!(typed_valid(rec, &f), d == "a".repeat(64));
            }
            let mut f = fields(rec);
            f[idx] = Some(vec![0xff; 64]);
            assert!(!valid(rec, &wire(rec, &f)));
            for op in ["approve", "revoke", "select"] {
                let start = if rec { 10 } else { 8 };
                let count = if rec { 3 } else { 2 };
                for mask in 0..(1 << count) {
                    let mut f = fields(rec);
                    f[2] = Some(op.as_bytes().to_vec());
                    for j in 0..count {
                        if mask & (1 << j) == 0 {
                            f[start + j] = None;
                        }
                    }
                    let ok = if op == "select" {
                        mask == (1 << count) - 1
                    } else {
                        mask == 0
                    };
                    assert_eq!(valid(rec, &wire(rec, &f)), ok, "{rec} {op} {mask}");
                    assert_eq!(typed_valid(rec, &f), ok);
                }
            }
        }
    }
    #[test]
    fn w1_codec_decimal_grammar() {
        for rec in [false, true] {
            let indexes: &[usize] = if rec { &[4, 7, 8, 11, 12] } else { &[6, 9] };
            for &idx in indexes {
                for v in [
                    "+1",
                    "-1",
                    "01",
                    "00",
                    " 1",
                    "1 ",
                    "",
                    "x",
                    "١",
                    "1\0",
                    "9223372036854775808",
                    "999999999999999999999999999",
                ] {
                    let mut f = fields(rec);
                    f[idx] = Some(v.as_bytes().to_vec());
                    assert!(!valid(rec, &wire(rec, &f)), "{rec} {idx} {v:?}");
                }
                let mut f = fields(rec);
                f[idx] = Some(vec![0xff]);
                assert!(!valid(rec, &wire(rec, &f)));
                for n in [0, 1, 9, 10] {
                    let mut f = fields(rec);
                    f[idx] = Some(n.to_string().into_bytes());
                    if rec && idx == 7 {
                        f[8] = Some((n + 1).to_string().into_bytes());
                    }
                    if rec && idx == 8 && n > 0 {
                        f[7] = Some((n - 1).to_string().into_bytes());
                    }
                    let positive = if rec {
                        idx == 8 || idx == 11 || idx == 12
                    } else {
                        idx == 9
                    };
                    assert_eq!(valid(rec, &wire(rec, &f)), !positive || n > 0);
                    assert_eq!(typed_valid(rec, &f), !positive || n > 0);
                }
            }
        }
    }
    #[test]
    fn w1_codec_numeric_ranges_and_increment() {
        let mut q_control = request("select");
        let mut r_control = receipt("select");
        assert!(encode_request(&q_control).is_ok(), "C07: valid request control");
        assert!(encode_receipt(&r_control).is_ok(), "C07: valid receipt control");
        q_control.expected = -1;
        assert!(encode_request(&q_control).is_err(), "C07 request: negative expected");
        q_control = request("select");
        q_control.lifecycle = Some(-1);
        assert!(encode_request(&q_control).is_err(), "C07 request: negative lifecycle");
        r_control.expected = -1;
        assert!(encode_receipt(&r_control).is_err(), "C07 receipt: negative expected");
        r_control = receipt("select");
        r_control.lifecycle = Some(-1);
        assert!(encode_receipt(&r_control).is_err(), "C07 receipt: negative lifecycle");
        r_control = receipt("select");
        r_control.result = -1;
        assert!(encode_receipt(&r_control).is_err(), "C07 receipt: negative result");
        r_control = receipt("select");
        r_control.authority = -1;
        assert!(encode_receipt(&r_control).is_err(), "C07 receipt: negative authority");
        r_control = receipt("select");
        r_control.approval = Some(-1);
        assert!(encode_receipt(&r_control).is_err(), "C07 receipt: negative approval");
        let (mut q, mut r) = maxima();
        assert!(encode_request(&q).is_ok());
        assert!(decode_request(&encode_request(&q).unwrap()).is_ok());
        assert!(encode_receipt(&r).is_ok());
        assert_eq!(decode_receipt(&encode_receipt(&r).unwrap()).unwrap(), r);
        q.expected = i64::MAX;
        assert!(encode_request(&q).is_err());
        r.expected = i64::MAX;
        assert!(encode_receipt(&r).is_err());
        for rec in [false, true] {
            let mut f = fields(rec);
            let ei = if rec { 7 } else { 6 };
            f[ei] = Some(i64::MAX.to_string().into_bytes());
            assert!(!valid(rec, &wire(rec, &f)));
        }
        let mut r = receipt("select");
        for n in [0, i64::MAX] {
            r.authority = n;
            assert!(encode_receipt(&r).is_ok());
            assert_eq!(
                decode_receipt(&encode_receipt(&r).unwrap())
                    .unwrap()
                    .authority,
                n
            );
        }
        r.authority = -1;
        assert!(encode_receipt(&r).is_err());
        r.authority = 0;
        for n in [0, 1, i64::MAX] {
            let mut q = request("select");
            q.lifecycle = Some(n);
            assert_eq!(encode_request(&q).is_ok(), n > 0);
            let mut c = receipt("select");
            c.lifecycle = Some(n);
            assert_eq!(encode_receipt(&c).is_ok(), n > 0);
            c.lifecycle = Some(1);
            c.approval = Some(n);
            assert_eq!(encode_receipt(&c).is_ok(), n > 0);
            for (rec, idx) in [(false, 9), (true, 11), (true, 12)] {
                let mut f = fields(rec);
                f[idx] = Some(n.to_string().into_bytes());
                assert_eq!(valid(rec, &wire(rec, &f)), n > 0);
            }
        }
        for (expected, result, ok) in [
            (0, 0, false),
            (0, 1, true),
            (1, 1, false),
            (i64::MAX - 1, i64::MAX, true),
            (i64::MAX, i64::MAX, false),
        ] {
            let mut c = receipt("select");
            c.expected = expected;
            c.result = result;
            assert_eq!(encode_receipt(&c).is_ok(), ok);
            let mut f = fields(true);
            f[7] = Some(expected.to_string().into_bytes());
            f[8] = Some(result.to_string().into_bytes());
            assert_eq!(valid(true, &wire(true, &f)), ok);
        }
        for n in [0, i64::MAX] {
            let mut f = fields(true);
            f[4] = Some(n.to_string().into_bytes());
            assert!(valid(true, &wire(true, &f)));
        }
    }
    #[test]
    fn w1_codec_canonical_reencoding() {
        for op in ["approve", "revoke", "select"] {
            let q = golden(op, false);
            let r = golden(op, true);
            assert_eq!(encode_request(&decode_request(&q).unwrap()).unwrap(), q);
            assert_eq!(encode_receipt(&decode_receipt(&r).unwrap()).unwrap(), r);
            assert_eq!(encode_request(&request(op)), encode_request(&request(op)));
            let first = encode_receipt(&receipt(op)).expect("C08: first receipt encoding succeeds");
            let second = encode_receipt(&receipt(op)).expect("C08: repeated receipt encoding succeeds");
            assert_eq!(first, second, "C08 receipt {op}: repeated encoding differs");
        }
        let (q, r) = maxima();
        let qb = encode_request(&q).unwrap();
        let rb = encode_receipt(&r).unwrap();
        assert_eq!(encode_request(&decode_request(&qb).unwrap()).unwrap(), qb);
        assert_eq!(encode_receipt(&decode_receipt(&rb).unwrap()).unwrap(), rb);
        for rec in [false, true] {
            let mut f = fields(rec);
            f[if rec { 7 } else { 6 }] = Some(b"00".to_vec());
            assert!(!valid(rec, &wire(rec, &f)));
            let mut b = wire(rec, &fields(rec));
            b.push(0);
            assert!(!valid(rec, &b));
        }
        let mut a = request("select");
        a.installation = "é".into();
        let mut b = a.clone();
        b.installation = "e\u{301}".into();
        assert_ne!(encode_request(&a).unwrap(), encode_request(&b).unwrap());
    }
    #[test]
    fn w1_codec_error_context_boundary() {
        let mut q = request("approve");
        q.key.clear();
        assert_eq!(encode_request(&q), Err(Invalid));
        assert_eq!(decode_request(b"malformed"), Err(Invalid));
        let mut r = receipt("select");
        r.approval = None;
        assert_eq!(encode_receipt(&r), Err(Invalid));
        assert_eq!(decode_receipt(b"malformed"), Err(Invalid));
    }
}

#[cfg(all(test, feature = "w1-isolated-test-support"))]
mod w1_stage2_reader_tests {
    use super::*;

    fn context() -> ProductServiceContext {
        ProductServiceContext { service_id: "svc".into(), service_revision: 2, credential_revision: 3, source_instance_id: "inst".into(), source_generation: 4, source_instance_revision: 5, connector_revision: 6, source_revision: 6, connector_id: "conn".into(), api_origin: "origin".into(), repository_id: "repo".into(), api_owner: "owner".into(), api_repo: "name".into() }
    }
    fn time() -> DateTime<Utc> { w1_canonical_time("2026-01-01T00:00:00+00:00").unwrap() }
    fn database() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE product_services_v1(service_id TEXT,revision INTEGER,enabled INTEGER,lease_until TEXT);
        CREATE TABLE product_service_credentials_v1(service_id TEXT,credential_revision INTEGER,expires_at TEXT);
        CREATE TABLE product_service_grants_v1(service_id TEXT,source_instance_id TEXT,action TEXT,enabled INTEGER,grant_revision INTEGER);
        CREATE TABLE product_scope_source_instances_v1(source_instance_id TEXT,connector_id TEXT,repository_key TEXT,enabled INTEGER,generation INTEGER,revision INTEGER);
        CREATE TABLE product_scope_current_sources_v1(connector_id TEXT,source_instance_id TEXT);
        CREATE TABLE product_scope_repositories_v1(repository_key TEXT,api_origin TEXT,stable_repository_id TEXT);
        CREATE TABLE product_scope_api_locators_v1(source_instance_id TEXT,api_owner TEXT,api_repo TEXT);
        CREATE TABLE product_sources_v1(connector_id TEXT,revision INTEGER,enabled INTEGER,origin TEXT);
        CREATE TABLE product_work_backlog_v1(order_id TEXT,lifecycle INTEGER,delivery_id TEXT,state TEXT,available_at TEXT,attempts INTEGER);
        CREATE TABLE product_orders_v1(order_id TEXT,lifecycle INTEGER,connector_id TEXT,repository_id TEXT,task_kind TEXT,payload_snapshot TEXT,source_revision INTEGER,admission_seq INTEGER,admission_hash TEXT);
        CREATE TABLE product_scope_order_provenance_v1(order_id TEXT,lifecycle INTEGER,verified INTEGER,source_instance_id TEXT,admission_seq INTEGER,admission_hash TEXT);
        CREATE TABLE product_work_claims_v1(order_id TEXT,lifecycle INTEGER,epoch INTEGER,lease_until TEXT);
        CREATE TABLE product_transition_receipts_v1(seq INTEGER,event_hash TEXT,order_id TEXT,lifecycle INTEGER,operation TEXT);
        INSERT INTO product_services_v1 VALUES('svc',2,1,'2026-01-01T03:00:00+00:00');
        INSERT INTO product_service_credentials_v1 VALUES('svc',3,'2026-01-01T02:00:00+00:00');
        INSERT INTO product_service_grants_v1 VALUES('svc','inst','drive_review',1,17);
        INSERT INTO product_scope_source_instances_v1 VALUES('inst','conn','key',1,4,5);
        INSERT INTO product_scope_current_sources_v1 VALUES('conn','inst');
        INSERT INTO product_scope_repositories_v1 VALUES('key','origin','repo');
        INSERT INTO product_scope_api_locators_v1 VALUES('inst','OWNER','NAME');
        INSERT INTO product_sources_v1 VALUES('conn',6,1,'origin');
        INSERT INTO product_work_backlog_v1 VALUES('order',1,'delivery','ready','2026-01-01T00:00:00+00:00',0);
        INSERT INTO product_orders_v1 VALUES('order',1,'conn','repo','task','payload',6,8,'hash');
        INSERT INTO product_scope_order_provenance_v1 VALUES('order',1,1,'inst',8,'hash');
        INSERT INTO product_transition_receipts_v1 VALUES(8,'hash','order',1,'github-admission');").unwrap();
        c
    }
    // Synthetic in-memory fixture; these snapshots cover every fixture table with SQLite types.
    fn snapshot(c: &Connection) -> Vec<(String, Vec<Vec<rusqlite::types::Value>>)> {
        let mut names = c.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap();
        let names: Vec<String> = names.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
        names.into_iter().map(|name| {
            let quoted = name.replace('"', "\"\"");
            let mut stmt = c.prepare(&format!("SELECT * FROM \"{quoted}\"")).unwrap();
            let count = stmt.column_count();
            let mut rows: Vec<Vec<rusqlite::types::Value>> = stmt.query_map([], |r| (0..count).map(|i| r.get(i)).collect()).unwrap().map(Result::unwrap).collect();
            rows.sort_by_key(|row| format!("{row:?}"));
            (name, rows)
        }).collect()
    }
    #[test]
    fn candidate_reader_provenance_binding_and_numeric_boundaries() {
        let mut c = database();
        let before = snapshot(&c);
        { let tx = c.transaction().unwrap(); let candidate = w1_claim_candidate_in(&tx, &context(), time()).unwrap().unwrap(); assert_eq!(candidate.old_epoch, None); assert_eq!(candidate.attempts, 0); }
        assert_eq!(snapshot(&c), before);
        // Both receipts are valid for the order, but provenance names the other one.
        c.execute("INSERT INTO product_transition_receipts_v1 VALUES(9,'other','order',1,'github-admission')", []).unwrap();
        c.execute("UPDATE product_scope_order_provenance_v1 SET admission_seq=9,admission_hash='other'", []).unwrap();
        let before = snapshot(&c);
        { let tx = c.transaction().unwrap(); assert!(matches!(w1_claim_candidate_in(&tx, &context(), time()), Err(ProductError::Invariant("order provenance binding changed")))); }
        assert_eq!(snapshot(&c), before);
        c.execute("UPDATE product_scope_order_provenance_v1 SET admission_seq=8,admission_hash='hash'", []).unwrap();
        c.execute("INSERT INTO product_work_claims_v1 VALUES('order',1,-1,NULL)", []).unwrap();
        let before = snapshot(&c);
        { let tx = c.transaction().unwrap(); assert!(matches!(w1_claim_candidate_in(&tx, &context(), time()), Err(ProductError::ContractMismatch))); }
        assert_eq!(snapshot(&c), before);
        c.execute("UPDATE product_work_claims_v1 SET epoch=?1", [i64::MAX]).unwrap();
        c.execute("UPDATE product_work_backlog_v1 SET attempts=?1", [i64::MAX]).unwrap();
        let before = snapshot(&c);
        { let tx = c.transaction().unwrap(); let candidate = w1_claim_candidate_in(&tx, &context(), time()).unwrap().unwrap(); assert_eq!(candidate.old_epoch, Some(i64::MAX)); assert_eq!(candidate.attempts, i64::MAX); assert!(candidate.old_epoch.unwrap().checked_add(1).is_none()); assert!(candidate.attempts.checked_add(1).is_none()); }
        assert_eq!(snapshot(&c), before);
        c.execute("UPDATE product_work_backlog_v1 SET attempts=-1", []).unwrap();
        let before = snapshot(&c);
        { let tx = c.transaction().unwrap(); assert!(matches!(w1_claim_candidate_in(&tx, &context(), time()), Err(ProductError::ContractMismatch))); }
        assert_eq!(snapshot(&c), before);
    }
    #[test]
    fn candidate_reader_ordering_states_and_nonselected_corruption() {
        let mut c = database();
        c.execute_batch("INSERT INTO product_work_backlog_v1 VALUES('z',1,'delivery-z','retryable','2026-01-01T00:00:00+00:00',0);
            INSERT INTO product_orders_v1 VALUES('z',1,'conn','repo','task','payload',6,9,'hash-z');
            INSERT INTO product_scope_order_provenance_v1 VALUES('z',1,1,'inst',9,'hash-z');
            INSERT INTO product_transition_receipts_v1 VALUES(9,'hash-z','z',1,'github-admission');").unwrap();
        let before = snapshot(&c);
        { let tx = c.transaction().unwrap(); assert_eq!(w1_claim_candidate_in(&tx, &context(), time()).unwrap().unwrap().order_id, "order"); }
        assert_eq!(snapshot(&c), before);
        c.execute("UPDATE product_work_backlog_v1 SET available_at='bad' WHERE order_id='z'", []).unwrap();
        let before = snapshot(&c);
        { let tx = c.transaction().unwrap(); assert!(matches!(w1_claim_candidate_in(&tx, &context(), time()), Err(ProductError::ContractMismatch))); }
        assert_eq!(snapshot(&c), before);
        c.execute("UPDATE product_work_backlog_v1 SET available_at='2026-01-01T00:00:00+00:00' WHERE order_id='z'", []).unwrap();
        c.execute("INSERT INTO product_work_claims_v1 VALUES('order',1,1,'2026-01-01T00:00:01+00:00')", []).unwrap();
        for state in ["claimed", "completed"] {
            c.execute("UPDATE product_work_backlog_v1 SET state=?1 WHERE order_id='order'", [state]).unwrap();
            let before = snapshot(&c);
            { let tx = c.transaction().unwrap(); assert_eq!(w1_claim_candidate_in(&tx, &context(), time()).unwrap().unwrap().order_id, "z"); assert_eq!(w1_claim_candidate_in(&tx, &context(), time() + chrono::Duration::seconds(1)).unwrap().unwrap().order_id, "order"); }
            assert_eq!(snapshot(&c), before);
        }
    }
    #[test]
    fn authority_reader_validates_time_and_grant_without_writes() {
        let mut c = database();
        let before = c.total_changes();
        let tx = c.transaction().unwrap();
        let authority = w1_service_authority_in(&tx, &context(), time()).unwrap();
        assert_eq!(authority.grant_revision, 17);
        assert_eq!(authority.grant_action, "drive_review");
        assert_eq!(authority.service_lease_until, "2026-01-01T03:00:00+00:00");
        assert_eq!(authority.credential_expires_at, "2026-01-01T02:00:00+00:00");
        assert!(matches!(w1_service_authority_in(&tx, &context(), time() + chrono::Duration::hours(2)), Err(ProductError::Invariant("service authority is absent, expired, or revoked"))));
        let mut denied = context(); denied.source_generation += 1;
        assert!(matches!(w1_service_authority_in(&tx, &denied, time()), Err(ProductError::Invariant(_))));
        drop(tx);
        assert_eq!(c.total_changes(), before);
        for bad in ["bad", "2026-01-01T03:00:00Z", "2026-01-01T04:00:00+01:00", "2026-01-01T03:00:00.000+00:00"] {
            c.execute("UPDATE product_services_v1 SET lease_until=?1", [bad]).unwrap();
            let before = c.total_changes();
            let tx = c.transaction().unwrap();
            assert!(matches!(w1_service_authority_in(&tx, &context(), time()), Err(ProductError::ContractMismatch)));
            drop(tx); assert_eq!(c.total_changes(), before);
        }
        c.execute("UPDATE product_services_v1 SET lease_until='2026-01-01T03:00:00+00:00'", []).unwrap();
        c.execute("UPDATE product_service_credentials_v1 SET expires_at='2026-01-01T02:00:00Z'", []).unwrap();
        let tx = c.transaction().unwrap();
        assert!(matches!(w1_service_authority_in(&tx, &context(), time()), Err(ProductError::ContractMismatch)));
    }
    #[test]
    fn candidate_reader_validates_all_times_and_preserves_selection() {
        let mut c = database();
        let before = c.total_changes();
        let tx = c.transaction().unwrap();
        assert_eq!(w1_claim_candidate_in(&tx, &context(), time()).unwrap().unwrap().order_id, "order");
        assert!(w1_claim_candidate_in(&tx, &context(), time() - chrono::Duration::seconds(1)).unwrap().is_none());
        drop(tx); assert_eq!(c.total_changes(), before);
        for bad in ["bad", "2026-01-01T00:00:00Z", "2026-01-01T01:00:00+01:00", "2026-01-01T00:00:00.000+00:00"] {
            c.execute("UPDATE product_work_backlog_v1 SET available_at=?1", [bad]).unwrap();
            let before = c.total_changes(); let tx = c.transaction().unwrap();
            assert!(matches!(w1_claim_candidate_in(&tx, &context(), time()), Err(ProductError::ContractMismatch)));
            drop(tx); assert_eq!(c.total_changes(), before);
        }
        c.execute("UPDATE product_work_backlog_v1 SET available_at='2026-01-01T00:00:00+00:00'", []).unwrap();
        c.execute("INSERT INTO product_work_claims_v1 VALUES('order',1,1,'bad')", []).unwrap();
        let before = c.total_changes(); let tx = c.transaction().unwrap();
        assert!(matches!(w1_claim_candidate_in(&tx, &context(), time()), Err(ProductError::ContractMismatch)));
        drop(tx); assert_eq!(c.total_changes(), before);
        c.execute("UPDATE product_work_claims_v1 SET lease_until='2026-01-01T00:00:01+00:00'", []).unwrap();
        let tx = c.transaction().unwrap();
        assert!(w1_claim_candidate_in(&tx, &context(), time()).unwrap().is_none());
        assert!(w1_claim_candidate_in(&tx, &context(), time() + chrono::Duration::seconds(1)).unwrap().is_some());
    }
}

#[cfg(all(test, feature = "w1-isolated-test-support", unix))]
mod w1_latest_selection_reader_tests {
    use super::*;
    use crate::product_identity::ProductPasswordKdf;
    use std::os::unix::fs::DirBuilderExt;

    struct Dir(PathBuf);
    impl Drop for Dir { fn drop(&mut self) { std::fs::remove_dir_all(&self.0).unwrap(); } }

    fn fixture() -> (Dir, ProductStore, String, DateTime<Utc>) {
        let path = PathBuf::from("/home/cosmo/.hermes/cache/scratch").join(format!("w1-reader-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        let dir = Dir(path);
        let mut store = ProductStore::create_isolated_test_store(&dir.0, "db", ProductConfig { installation_id: "w1-install".into(), database_id: "w1-db".into() }).unwrap();
        let source = ScopeSourceManifest { connector_id: "gh".into(), source_instance_id: "source-instance-1".into(), generation: 1, identity: RepositoryIdentity::canonicalized("acme", "GitHub", "https://api.github.com", "42").unwrap(), api_owner: Some("acme".into()), api_repo: Some("proj".into()), legacy_repository_locator: "acme/proj".into(), scope_query: "open".into(), enabled: true };
        let grant = ScopeGrantManifest { actor_id: "owner".into(), source_instance_id: source.source_instance_id.clone(), capabilities: vec![ProductCapability::ReadOrder] };
        store.bootstrap_owner_scoped("owner", &ProductPasswordKdf::hash("test-password").unwrap(), &[source], &[grant]).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z").unwrap().with_timezone(&Utc);
        let session = store.login("owner", "test-password", now).unwrap().digest().to_owned();
        store.admit_order("order-1", "gh", "42", "issue:1", "dep-bump", "immutable task", now).unwrap();
        (dir, store, session, now)
    }

    fn snapshot_conn(conn: &Connection) -> std::collections::BTreeMap<String, Vec<Vec<rusqlite::types::Value>>> {
        let mut stmt = conn.prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name").unwrap();
        let names: Vec<String> = stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
        let mut result: std::collections::BTreeMap<_, _> = names.into_iter().map(|name| {
            let mut stmt = conn.prepare(&format!("SELECT * FROM \"{}\"", name.replace('"', "\"\""))).unwrap();
            let n = stmt.column_count();
            let mut rows: Vec<Vec<rusqlite::types::Value>> = stmt.query_map([], |r| (0..n).map(|i| r.get(i)).collect()).unwrap().map(Result::unwrap).collect();
            rows.sort_by_cached_key(|r| format!("{r:?}"));
            (name, rows)
        }).collect();
        let mut stmt = conn.prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name").unwrap();
        let rows = stmt.query_map([], |r| (0..4).map(|i| r.get(i)).collect()).unwrap().map(Result::unwrap).collect();
        result.insert("__schema__".into(), rows);
        result
    }

    fn snapshot(path: &Path) -> std::collections::BTreeMap<String, Vec<Vec<rusqlite::types::Value>>> {
        snapshot_conn(&Connection::open(path).unwrap())
    }

    fn read(store: &mut ProductStore, order: &str, lifecycle: i64, source: &str) -> Result<W1LatestSelection, ProductError> {
        let tx = store.conn.transaction().unwrap();
        let result = w1_latest_selection_in(&tx, "w1-install", "w1-db", order, lifecycle, source);
        tx.commit().unwrap();
        result
    }

    #[test]
    fn latest_selection_validates_history_and_is_read_only() {
        let (dir, mut store, session, now) = fixture();
        let source = "source-instance-1"; let a = "a".repeat(64); let b = "b".repeat(64);
        store.w1_change_pack_approval(&session, source, &a, 0, true, "a", now).unwrap();
        store.w1_select_pack(&session, "order-1", 1, source, &a, 0, "s1", now).unwrap();
        store.w1_change_pack_approval(&session, source, &b, 0, true, "b", now).unwrap();
        store.w1_select_pack(&session, "order-1", 1, source, &b, 1, "s2", now).unwrap();
        let before = snapshot(&dir.0.join("db"));
        assert_eq!(read(&mut store, "order-1", 1, source).unwrap(), W1LatestSelection { order: "order-1".into(), lifecycle: 1, source: source.into(), pack_digest: b, selection_revision: 2, referenced_approval_revision: 1, current_approval_head: 1, active: true });
        assert!(matches!(read(&mut store, "order-1", 1, "wrong-source"), Err(ProductError::Invariant("w1 selection unavailable"))));
        assert!(matches!(read(&mut store, "wrong-order", 1, source), Err(ProductError::Invariant("w1 selection unavailable"))));
        assert!(matches!(read(&mut store, "order-1", 2, source), Err(ProductError::Invariant("w1 selection unavailable"))));
        assert_eq!(snapshot(&dir.0.join("db")), before);
        // Tampering is confined to a rolled-back transaction; production triggers stay intact.
        let tx = store.conn.transaction().unwrap();
        tx.execute_batch("DROP TRIGGER product_w1_operations_no_update; UPDATE product_w1_operations_v1 SET receipt=x'00' WHERE request_key='s1';").unwrap();
        let corrupted = snapshot_conn(&tx);
        assert!(matches!(w1_latest_selection_in(&tx, "w1-install", "w1-db", "order-1", 1, source), Err(ProductError::ContractMismatch)));
        assert_eq!(snapshot_conn(&tx), corrupted);
        tx.rollback().unwrap();
        assert_eq!(snapshot(&dir.0.join("db")), before);
        let tx = store.conn.transaction().unwrap();
        tx.execute_batch("DROP TABLE product_w1_operations_v1").unwrap();
        let corrupted = snapshot_conn(&tx);
        assert!(matches!(w1_latest_selection_in(&tx, "w1-install", "w1-db", "order-1", 1, source), Err(ProductError::Database(_))));
        assert_eq!(snapshot_conn(&tx), corrupted);
        tx.rollback().unwrap();
        assert_eq!(snapshot(&dir.0.join("db")), before);
    }

    // Adversarial SQL simulates already-corrupted denormalized columns; it is
    // not a normal lifecycle operation. Each mutation is rolled back.
    #[test]
    fn concealed_latest_and_revoke_fail_closed_without_writes() {
        let (_dir, mut store, session, now) = fixture();
        let source = "source-instance-1"; let a = "a".repeat(64); let b = "b".repeat(64);
        store.w1_change_pack_approval(&session, source, &a, 0, true, "a", now).unwrap();
        store.w1_select_pack(&session, "order-1", 1, source, &a, 0, "s1", now).unwrap();
        store.w1_change_pack_approval(&session, source, &b, 0, true, "b", now).unwrap();
        store.w1_select_pack(&session, "order-1", 1, source, &b, 1, "s2", now).unwrap();
        assert_eq!(read(&mut store, "order-1", 1, source).unwrap().pack_digest, b);
        for sql in [
            "UPDATE product_w1_operations_v1 SET order_id='other-order' WHERE request_key='s2'",
            "UPDATE product_w1_operations_v1 SET lifecycle=2 WHERE request_key='s2'",
            "UPDATE product_w1_operations_v1 SET source_instance_id='other-source' WHERE request_key='s2'",
            "UPDATE product_w1_operations_v1 SET digest='cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc' WHERE request_key='s2'",
        ] {
            store.conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
            let tx = store.conn.transaction().unwrap();
            tx.execute_batch("DROP TRIGGER product_w1_operations_no_update").unwrap();
            tx.execute_batch(sql).unwrap();
            let before = snapshot_conn(&tx);
            assert!(matches!(w1_latest_selection_in(&tx, "w1-install", "w1-db", "order-1", 1, source), Err(ProductError::ContractMismatch)), "{sql}");
            assert_eq!(snapshot_conn(&tx), before, "{sql}");
            tx.rollback().unwrap();
            store.conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        }
        store.w1_change_pack_approval(&session, source, &b, 1, false, "revoke", now).unwrap();
        assert!(matches!(read(&mut store, "order-1", 1, source), Err(ProductError::Invariant("w1 selection unavailable"))));
        for sql in [
            "UPDATE product_w1_operations_v1 SET source_instance_id='other-source' WHERE request_key='revoke'",
            "UPDATE product_w1_operations_v1 SET digest='cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc' WHERE request_key='revoke'",
        ] {
            store.conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
            let tx = store.conn.transaction().unwrap();
            tx.execute_batch("DROP TRIGGER product_w1_operations_no_update").unwrap();
            tx.execute_batch(sql).unwrap();
            let before = snapshot_conn(&tx);
            assert!(matches!(w1_latest_selection_in(&tx, "w1-install", "w1-db", "order-1", 1, source), Err(ProductError::ContractMismatch)), "{sql}");
            assert_eq!(snapshot_conn(&tx), before, "{sql}");
            tx.rollback().unwrap();
            store.conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        }
    }

    #[test]
    fn reapproval_requires_fresh_selection_and_preserves_replay() {
        let (dir, mut store, session, now) = fixture();
        let source = "source-instance-1"; let digest = "a".repeat(64);
        store.w1_change_pack_approval(&session, source, &digest, 0, true, "a1", now).unwrap();
        let old = store.w1_select_pack(&session, "order-1", 1, source, &digest, 0, "s1", now).unwrap();
        let path = dir.0.join("db");
        let before = snapshot(&path); assert_eq!(read(&mut store, "order-1", 1, source).unwrap().current_approval_head, 1); assert_eq!(snapshot(&path), before);
        store.w1_change_pack_approval(&session, source, &digest, 1, false, "r1", now).unwrap();
        let before = snapshot(&path); assert!(matches!(read(&mut store, "order-1", 1, source), Err(ProductError::Invariant("w1 selection unavailable")))); assert_eq!(snapshot(&path), before);
        store.w1_change_pack_approval(&session, source, &digest, 2, true, "a2", now).unwrap();
        let before = snapshot(&path); assert!(matches!(read(&mut store, "order-1", 1, source), Err(ProductError::Invariant("w1 selection unavailable")))); assert_eq!(snapshot(&path), before);
        assert_eq!(store.w1_select_pack(&session, "order-1", 1, source, &digest, 0, "s1", now).unwrap(), old);
        assert_eq!(snapshot(&path), before);
        store.w1_select_pack(&session, "order-1", 1, source, &digest, 1, "s2", now).unwrap();
        let before = snapshot(&path);
        let selected = read(&mut store, "order-1", 1, source).unwrap();
        assert_eq!((selected.pack_digest, selected.selection_revision, selected.referenced_approval_revision, selected.current_approval_head, selected.active), (digest, 2, 3, 3, true));
        assert_eq!(snapshot(&path), before);
    }
}

#[cfg(all(test, feature = "w1-isolated-test-support", unix))]
mod w1_atomic_private_tests {
    use super::*;
    use crate::{ProductPollCursor, ProductServiceGrant};
    use crate::product_identity::ProductPasswordKdf;
    use std::os::unix::fs::DirBuilderExt;
    use std::sync::Arc;

    fn fixture() -> (ProductStore, ProductServiceContext, DateTime<Utc>) {
        let dir = std::env::temp_dir().join(format!("w1-private-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let config = ProductConfig { installation_id: "w1-install".into(), database_id: "w1-db".into() };
        let mut store = ProductStore::create_w1_capture_test_store(&dir, "v3.db", config, Arc::new(SystemProductClock)).unwrap();
    let source = ScopeSourceManifest { connector_id: "gh".into(), source_instance_id: "source-instance-1".into(), generation: 1, identity: RepositoryIdentity::canonicalized("acme", "GitHub", "https://api.github.com", "42").unwrap(), api_owner: Some("acme".into()), api_repo: Some("proj".into()), legacy_repository_locator: "acme/proj".into(), scope_query: "open".into(), enabled: true };
    let grant = ScopeGrantManifest { actor_id: "owner".into(), source_instance_id: source.source_instance_id.clone(), capabilities: vec![ProductCapability::ReadOrder] };
    store.bootstrap_owner_scoped("owner", &ProductPasswordKdf::hash("test-password").unwrap(), &[source], &[grant]).unwrap();
    let now = SystemProductClock.now();
    let session = store.login("owner", "test-password", now).unwrap().digest().to_owned();
    let grants: Vec<_> = [ProductServiceAction::IntakeRead, ProductServiceAction::AdmitDelivery, ProductServiceAction::AckDelivery, ProductServiceAction::DriveReview].into_iter().map(|action| ProductServiceGrant { source_instance_id: "source-instance-1".into(), action }).collect();
    store.manage_product_service(&session, "service", &grants, true, now + chrono::Duration::hours(3), 0, "configure", now).unwrap();
    let token = store.issue_product_service_credential(&session, "service", 1, now + chrono::Duration::hours(2), "credential", now).unwrap().expose_once_to_file_writer().to_owned();
    let service = store.authenticate_product_service_for_action(&token, "gh", "https://api.github.com", "42", ProductServiceAction::DriveReview, now).unwrap();
    let issue = "issue:service-w1";
    let payload = "immutable local adapter task";
    let updated = now.to_rfc3339();
    let version = crate::product::delivery_version_digest(issue, &updated, "dep-bump", payload);
    let delivery = crate::ProductDelivery { delivery_id: crate::product::github_delivery_id("w1-install", "gh", "https://api.github.com", "42", issue, &version), connector_id: "gh".into(), repository_id: "42".into(), issue_id: issue.into(), observed_updated_at: updated.clone(), version_digest: version, task_kind: "dep-bump".into(), payload: payload.into() };
    let cursor = ProductPollCursor { connector_id: "gh".into(), query_fingerprint: "v3-query".into(), sweep_id: "v3-sweep".into(), next_page: 1, page_ceiling: 1, updated_at: updated };
    store.persist_intake_page_for_service(&service, &cursor, &[delivery.clone()], &[], now).unwrap();
    let receipt = store.admit_intake_delivery_for_service(&service, &delivery.delivery_id, "service-order", now).unwrap();
    store.mark_intake_acked_for_service(&service, &delivery.delivery_id, &receipt, now).unwrap();
    assert_eq!(store.read_scoped_order(&session, &receipt.order_id, now).unwrap().admission_sequence, receipt.sequence);

        // Test-only adapter readiness setup after genuine intake and admission.
        assert_eq!(store.conn.execute("UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id=?1 AND lifecycle=?2 AND delivery_id=?3 AND state='waiting_unsupported'", params![receipt.order_id,receipt.lifecycle,delivery.delivery_id]).unwrap(), 1);
        (store, service, now)
    }
    fn snapshot(c: &Connection) -> std::collections::BTreeMap<String, Vec<Vec<rusqlite::types::Value>>> {
        let mut names = c.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap();
        let names: Vec<String> = names.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
        names.into_iter().map(|name| {
            let quoted = name.replace('"', "\"\"");
            let mut stmt = c.prepare(&format!("SELECT * FROM \"{quoted}\"")).unwrap();
            let count = stmt.column_count();
            let mut rows: Vec<Vec<rusqlite::types::Value>> = stmt.query_map([], |r| (0..count).map(|i| r.get(i)).collect()).unwrap().map(Result::unwrap).collect();
            rows.sort_by_key(|row| format!("{row:?}"));
            (name, rows)
        }).collect()
    }
    #[test]
    fn w1_private_atomic_rollback_and_claim() {
        use rusqlite::types::Value::{Integer, Text};
        let (mut store, context, _) = fixture();
        let before = snapshot(&store.conn);
        store.w1_capture_fail_after_legacy_writes_once().unwrap();
        assert!(matches!(w1_capture_claim_private(&mut store, &context, "", chrono::Duration::minutes(5)), Err(ProductError::InvalidConfig)));
        assert_eq!(snapshot(&store.conn), before);
        assert!(matches!(w1_capture_claim_private(&mut store, &context, "worker", chrono::Duration::minutes(5)), Err(ProductError::Invariant("w1 capture injected failure"))));
        assert_eq!(snapshot(&store.conn), before);
        let claim = match w1_capture_claim_private(&mut store, &context, "worker", chrono::Duration::minutes(5)).unwrap() { ProductWorkClaimResult::Claimed(c) => c, other => panic!("{other:?}") };
        let mut expected = before;
        expected.get_mut("product_fence_counter_v1").unwrap()[0][1] = Integer(claim.fence);
        let backlog = &mut expected.get_mut("product_work_backlog_v1").unwrap()[0];
        backlog[3] = Text("claimed".into()); backlog[5] = Integer(1);
        expected.get_mut("product_work_claims_v1").unwrap().push(vec![Text(claim.order_id.clone()),Integer(claim.lifecycle),Text("worker".into()),Integer(1),Integer(1),Text("claimed".into()),Text(claim.lease_until.to_rfc3339()),Integer(claim.source_revision),Text(claim.payload_digest.clone())]);
        expected.get_mut("product_service_work_bindings_v1").unwrap().push(vec![Text(claim.order_id.clone()),Integer(claim.lifecycle),Text(context.service_id.clone()),Integer(context.service_revision),Integer(context.credential_revision),Text(context.source_instance_id.clone())]);
        let authority = w1_service_authority_in(&store.conn.unchecked_transaction().unwrap(), &context, claim.lease_until - chrono::Duration::minutes(5)).unwrap();
        expected.get_mut("product_w1_claim_authority_v1").unwrap().push(vec![Text(claim.order_id.clone()),Integer(claim.lifecycle),Integer(1),Integer(1),Text("w1-install".into()),Text("w1-db".into()),Text(claim.delivery_id.clone()),Text("worker".into()),Text((claim.lease_until - chrono::Duration::minutes(5)).to_rfc3339()),Text(claim.lease_until.to_rfc3339()),Text(claim.payload_digest.clone()),Integer(store.conn.query_row("SELECT admission_seq FROM product_orders_v1 WHERE order_id=?1", [&claim.order_id], |r| r.get(0)).unwrap()),Text(store.conn.query_row("SELECT admission_hash FROM product_orders_v1 WHERE order_id=?1", [&claim.order_id], |r| r.get(0)).unwrap()),Text(context.service_id.clone()),Integer(context.service_revision),Text(authority.service_lease_until),Integer(context.credential_revision),Text(authority.credential_expires_at),Text(authority.grant_action),Integer(authority.grant_revision),Text(context.source_instance_id.clone()),Integer(context.source_generation),Integer(context.source_instance_revision),Text(context.connector_id.clone()),Integer(context.connector_revision),Text(context.api_origin.clone()),Text(context.repository_id.clone()),Text(context.api_owner.clone()),Text(context.api_repo.clone())]);
        assert_eq!(snapshot(&store.conn), expected);
    }
    #[test]
    fn w1_private_atomic_boundaries_and_missing_claim() {
        let (mut store, context, _) = fixture();
        for column in ["attempts", "fence"] {
            if column == "attempts" { store.conn.execute("UPDATE product_work_backlog_v1 SET attempts=?1", [i64::MAX]).unwrap(); }
            else { store.conn.execute("UPDATE product_fence_counter_v1 SET next=?1 WHERE singleton=1", [i64::MAX]).unwrap(); }
            let before = snapshot(&store.conn);
            assert!(matches!(w1_capture_claim_private(&mut store, &context, "worker", chrono::Duration::minutes(5)), Err(ProductError::InvalidConfig)));
            assert_eq!(snapshot(&store.conn), before);
            if column == "attempts" { store.conn.execute("UPDATE product_work_backlog_v1 SET attempts=0", []).unwrap(); }
            else { store.conn.execute("UPDATE product_fence_counter_v1 SET next=0 WHERE singleton=1", []).unwrap(); }
        }
        for state in ["claimed", "completed"] {
            store.conn.execute("UPDATE product_work_backlog_v1 SET state=?1", [state]).unwrap();
            let before = snapshot(&store.conn);
            assert!(matches!(w1_capture_claim_private(&mut store, &context, "worker", chrono::Duration::minutes(5)), Err(ProductError::ContractMismatch)));
            assert_eq!(snapshot(&store.conn), before);
        }
        store.conn.execute("INSERT INTO product_work_claims_v1(order_id,lifecycle,epoch,fence,state) SELECT order_id,lifecycle,?1,1,'completed' FROM product_work_backlog_v1", [i64::MAX]).unwrap();
        let before = snapshot(&store.conn);
        assert!(matches!(w1_capture_claim_private(&mut store, &context, "worker", chrono::Duration::minutes(5)), Err(ProductError::InvalidConfig)));
        assert_eq!(snapshot(&store.conn), before);
    }
}


#[cfg(all(test, feature = "a01-harness-support", unix))]
mod a01_connection_tests {
    use super::*;
    use std::os::unix::fs::DirBuilderExt;

    fn require<T, E>(result: Result<T, E>) -> T {
        match result { Ok(value) => value, Err(_) => panic!("A01 connection test operation failed") }
    }
    struct PrivateDir(PathBuf);
    impl PrivateDir {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let stamp = require(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)).as_nanos();
            let path = std::env::temp_dir().join(format!("foundry-a01-connection-{}-{stamp}-{serial}", std::process::id()));
            require(std::fs::DirBuilder::new().mode(0o700).create(&path));
            Self(require(path.canonicalize()))
        }
    }
    impl Drop for PrivateDir {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }
    fn config() -> ProductConfig {
        ProductConfig { installation_id: "a01-connection-installation".into(), database_id: "a01-connection-database".into() }
    }

    #[test]
    fn a01_returned_connections_apply_required_pragmas() {
        let dir = PrivateDir::new();
        let mut store = require(ProductStore::create_a01_isolated_test_store(&dir.0, "store.db", config(), std::sync::Arc::new(SystemProductClock)));
        for reopening in [false, true] {
            if reopening {
                drop(store);
                store = require(ProductStore::reopen_a01_isolated_test_store(&dir.0, "store.db", config(), std::sync::Arc::new(SystemProductClock)));
            }
            // Read the actual returned connection, never a stand-in connection.
            require(validate_a01_pragmas(&store.conn));
            require(validate_a01_contract(&store.conn, &config()));
            assert!(store.a01_identity == Some(require(a01_file_identity(&dir.0.join("store.db")))), "A01 held physical identity mismatch");
            assert!(matches!(store.claim_product_work("", Utc::now(), chrono::Duration::zero()), Err(ProductError::Invariant("A01 isolated store work is disabled"))), "A01 early work guard mismatch");
            // Discriminate every readback check independently on this handle.
            for (pragma, wrong, correct) in [
                ("foreign_keys", 0, 1), ("recursive_triggers", 0, 1),
                ("ignore_check_constraints", 1, 0), ("busy_timeout", 1, 5000),
            ] {
                require(store.conn.pragma_update(None, pragma, wrong));
                assert!(matches!(validate_a01_pragmas(&store.conn), Err(ProductError::ContractMismatch)), "A01 pragma check missing");
                require(store.conn.pragma_update(None, pragma, correct));
                require(validate_a01_pragmas(&store.conn));
            }
            require(store.conn.pragma_update(None, "journal_mode", "persist"));
            assert!(matches!(validate_a01_pragmas(&store.conn), Err(ProductError::ContractMismatch)), "A01 journal check missing");
            require(store.conn.pragma_update(None, "journal_mode", "delete"));
            require(validate_a01_pragmas(&store.conn));
        }
        // A byte-identical copy with matching logical identities still fails
        // the held-file comparison. Assembly must retain this expected inode
        // across its own independently opened participants.
        let original = dir.0.join("store.db");
        let held = require(std::fs::File::open(&original));
        let copied = dir.0.join("copied.db");
        require(std::fs::copy(&original, &copied));
        assert!(matches!(a01_checked_file(&copied, &held), Err(ProductError::Invariant("A01 physical identity changed"))), "A01 copied file accepted as held identity");
    }

    #[test]
    fn a01_operational_errors_remain_database_errors() {
        for code in [rusqlite::ffi::SQLITE_BUSY, rusqlite::ffi::SQLITE_LOCKED,
            rusqlite::ffi::SQLITE_IOERR, rusqlite::ffi::SQLITE_FULL,
            rusqlite::ffi::SQLITE_CANTOPEN, rusqlite::ffi::SQLITE_READONLY]
        {
            let error = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None);
            assert!(matches!(a01_contract_error(error), ProductError::Database(_)), "A01 operational error reclassified");
        }
        let dir = PrivateDir::new();
        let store = require(ProductStore::create_a01_isolated_test_store(&dir.0, "store.db", config(), std::sync::Arc::new(SystemProductClock)));
        require(validate_a01_contract(&store.conn, &config()));
        let writer = require(Connection::open(dir.0.join("store.db")));
        require(writer.execute_batch("BEGIN EXCLUSIVE"));
        require(store.conn.busy_timeout(std::time::Duration::ZERO));
        let result = validate_a01_contract(&store.conn, &config());
        assert!(matches!(result, Err(ProductError::Database(rusqlite::Error::SqliteFailure(ref error, _))) if error.code == rusqlite::ErrorCode::DatabaseBusy), "A01 real busy error reclassified");
        require(writer.execute_batch("ROLLBACK"));
        require(store.conn.busy_timeout(std::time::Duration::from_millis(5000)));
        require(validate_a01_connection(&store.conn, &config()));
    }

    #[test]
    fn a01_header_inspection_distinguishes_truncation_from_read_io() {
        let dir = PrivateDir::new();
        drop(require(ProductStore::create_a01_isolated_test_store(
            &dir.0, "store.db", config(), std::sync::Arc::new(SystemProductClock),
        )));
        let path = dir.0.join("store.db");
        require(require(std::fs::OpenOptions::new().write(true).open(&path)).set_len(19));
        let before = require(std::fs::read(&path));
        let identity = require(a01_file_identity(&path));
        assert!(matches!(
            ProductStore::reopen_a01_isolated_test_store(
                &dir.0, "store.db", config(), std::sync::Arc::new(SystemProductClock),
            ),
            Err(ProductError::ContractMismatch)
        ), "A01 truncated header classification changed");
        assert!(before == require(std::fs::read(&path)), "A01 truncated header was mutated");
        assert!(identity == require(a01_file_identity(&path)), "A01 truncated file was replaced");
        for suffix in ["-journal", "-wal", "-shm"] {
            assert!(!dir.0.join(format!("store.db{suffix}")).exists(),
                "A01 truncated header created a sidecar");
        }

        // A directory descriptor gives a real operational read failure, without
        // fault injection, process-global hooks, or access outside this fixture.
        let directory = require(std::fs::File::open(&dir.0));
        assert!(matches!(
            a01_read_header(&directory),
            Err(ProductError::Invariant("A01 header inspection failed"))
        ), "A01 operational header read was classified as malformed content");
    }
}

#[cfg(all(test, feature = "a01-harness-support", unix))]
mod a01_binding_tests {
    // Parent execution requires newly owned private TMPDIR and separate private build targets
    // for feature configurations, with both B0 evidence activations absent.
    use super::*;
    use chrono::{DateTime, Utc};
    use rusqlite::{params_from_iter, types::Value};
    use std::collections::BTreeMap;
    use std::os::unix::fs::DirBuilderExt;
    use std::path::PathBuf;
    use std::sync::Arc;

    #[track_caller]
    fn require<T, E>(result: Result<T, E>) -> T {
        match result {
            Ok(value) => value,
            Err(_) => panic!("A01 foundation test operation failed"),
        }
    }
    struct PrivateDir(PathBuf);
impl PrivateDir {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let stamp = require(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)).as_nanos();
        let path = std::env::temp_dir().join(format!("foundry-a01-{}-{stamp}-{serial}", std::process::id()));
        require(std::fs::DirBuilder::new().mode(0o700).create(&path));
        Self(require(path.canonicalize()))
    }
}
    fn now() -> DateTime<Utc> {
    require(DateTime::parse_from_rfc3339("2026-01-01T00:00:00+00:00")).with_timezone(&Utc)
}
    fn config() -> ProductConfig {
    ProductConfig { installation_id: "a01-test-installation".into(), database_id: "a01-test-database".into() }
}
    struct FoundationClock;
    impl ProductClock for FoundationClock {
        fn now(&self) -> DateTime<Utc> { now() }
    }
    fn clock() -> Arc<dyn ProductClock> { Arc::new(FoundationClock) }

    // This prepares committed intake/admission only. It never makes backlog executable.
    fn prepare_admission(store: &mut ProductStore) {
        
    require(store.bind_source("gh", "github", "https://api.github.com", "42", "42"));
    let updated = now().to_rfc3339();
    let version = crate::product::delivery_version_digest("issue:a01", &updated, "dep-bump", "immutable task");
    let delivery = crate::ProductDelivery {
        delivery_id: crate::product::github_delivery_id(&config().installation_id, "gh", "https://api.github.com", "42", "issue:a01", &version),
        connector_id: "gh".into(), repository_id: "42".into(), issue_id: "issue:a01".into(),
        observed_updated_at: updated.clone(), version_digest: version, task_kind: "dep-bump".into(), payload: "immutable task".into(),
    };
    let cursor = crate::ProductPollCursor { connector_id: "gh".into(), query_fingerprint: "a01-query".into(), sweep_id: "a01-sweep".into(), next_page: 1, page_ceiling: 1, updated_at: updated };
    require(store.persist_intake_page(&cursor, &[delivery.clone()]));
    let receipt = require(store.admit_intake_delivery(&delivery.delivery_id, "a01-order", now()));
        require(store.mark_intake_acked(&delivery.delivery_id, &receipt));
    }

    fn marked(reopened: bool) -> (PrivateDir, ProductStore) {
        let dir = PrivateDir::new();
        let mut store = require(ProductStore::create_a01_isolated_test_store(
            &dir.0, "foundation.sqlite", config(), clock(),
        ));
        prepare_admission(&mut store);
        if reopened {
            drop(store);
            store = require(ProductStore::reopen_a01_isolated_test_store(
                &dir.0, "foundation.sqlite", config(), clock(),
            ));
        }
        require(validate_a01_pragmas(&store.conn));
        (dir, store)
    }

    type State = BTreeMap<String, Vec<Vec<Value>>>;
    fn state(conn: &Connection) -> State {
        let mut schema = require(conn.prepare(
            "SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name"
        ));
        let names = require(require(schema.query_map([], |row| row.get::<_, String>(0)))
            .collect::<rusqlite::Result<Vec<_>>>());
        let mut all = BTreeMap::new();
        for name in names {
            let escaped = name.replace('"', "\"\"");
            let query = require(conn.prepare(&format!("SELECT * FROM \"{escaped}\"")));
            let width = query.column_count();
            let order = (1..=width).map(|i| i.to_string()).collect::<Vec<_>>().join(",");
            let mut query = require(conn.prepare(&format!("SELECT * FROM \"{escaped}\" ORDER BY {order}")));
            let rows = require(require(query.query_map([], |row| {
                (0..width).map(|i| row.get::<_, Value>(i)).collect::<rusqlite::Result<Vec<_>>>()
            })).collect::<rusqlite::Result<Vec<_>>>());
            all.insert(name, rows);
        }
        all
    }

    const BINDING_COLUMNS: &[(&str, bool)] = &[("result_id", false), ("order_id", false), ("lifecycle", true), ("attempt", true), ("run_id", true), ("activation", true), ("generation", true), ("host_result_digest", false), ("tuple_digest", false), ("binding_digest", false), ("payload_json", false), ("receipt_json", false)];
    const TUPLE_COLUMNS: &[(&str, bool)] = &[("order_id", false), ("attempt", true), ("run_id", true), ("generation", true), ("immutable_commit", false), ("artifact_location", false), ("generations_json", false)];
    const LEGACY_COLUMNS: &[(&str,bool)] = &[("order_id",false),("attempt",true),("run_id",true),("generation",true)];
    const TUPLE_TABLE: &str = "artifact_tuples";
    const LEGACY_TABLE: &str = "product_result_legacy_tuples_v1";
    const GUARDS: [&str; 3] = [
        r#"CREATE TRIGGER product_result_bindings_no_replace
BEFORE INSERT ON product_result_bindings_v1
WHEN EXISTS(
  SELECT 1
  FROM product_result_bindings_v1 r
  WHERE r.result_id=NEW.result_id
     OR (
       r.order_id=NEW.order_id
       AND r.lifecycle=NEW.lifecycle
       AND r.attempt=NEW.attempt
       AND r.run_id=NEW.run_id
       AND r.activation=NEW.activation
       AND r.generation=NEW.generation
     )
     OR (
       r.order_id=NEW.order_id
       AND r.attempt=NEW.attempt
       AND r.run_id=NEW.run_id
       AND r.generation=NEW.generation
     )
     OR (
       r.result_id=NEW.result_id
       AND r.binding_digest=NEW.binding_digest
       AND r.tuple_digest=NEW.tuple_digest
     )
)
BEGIN
  SELECT RAISE(ROLLBACK,'product result already exists');
END;"#,
r#"CREATE TRIGGER product_result_bindings_no_legacy_tuple
BEFORE INSERT ON product_result_bindings_v1
WHEN EXISTS(
  SELECT 1
  FROM product_result_legacy_tuples_v1 t
  WHERE t.order_id=NEW.order_id
    AND t.attempt=NEW.attempt
    AND t.run_id=NEW.run_id
    AND t.generation=NEW.generation
)
BEGIN
  SELECT RAISE(ROLLBACK,'legacy tuple cannot be adopted');
END;"#,
r#"CREATE TRIGGER product_result_bindings_a01_deny_all
BEFORE INSERT ON product_result_bindings_v1
BEGIN
  SELECT RAISE(ROLLBACK,'A01 result binding is disabled');
END;"#
    ];
    const GUARD_NAMES: [&str; 3] = [
        "product_result_bindings_no_replace", "product_result_bindings_no_legacy_tuple", "product_result_bindings_a01_deny_all"
    ];
    const MESSAGES: &[&str] = &[
        "product result already exists", "legacy tuple cannot be adopted", "A01 result binding is disabled"
    ];
    const COLLISION_COLUMNS: [&[&str]; 4] = [
        &["result_id"],
&["order_id", "lifecycle", "attempt", "run_id", "activation", "generation"],
&["order_id", "attempt", "run_id", "generation"],
&["result_id", "binding_digest", "tuple_digest"]
    ];
    const CONFLICT_TARGETS: [&[&str]; 4] = [
        &["result_id"],
&["order_id", "lifecycle", "attempt", "run_id", "activation", "generation"],
&["order_id", "attempt", "run_id", "generation"],
&["result_id", "binding_digest", "tuple_digest"]
    ];
    const EPOCH_WRITE: &str = "UPDATE product_meta_v1 SET recovery_epoch = recovery_epoch + 1";

    fn structural_values(columns: &[(&str, bool)], seed: i64) -> Vec<Value> {
        columns.iter().map(|(column, integer)| {
            if *integer { Value::Integer(seed) }
            else { Value::Text(format!("structural-{seed}-{column}")) }
        }).collect()
    }

    fn placeholders(len: usize) -> String {
        (1..=len).map(|i| format!("?{i}")).collect::<Vec<_>>().join(",")
    }

    fn insertion(table: &str, columns: &[(&str, bool)]) -> String {
        let names = columns.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(",");
        format!("INSERT INTO {table} ({names}) VALUES ({})", placeholders(columns.len()))
    }

    fn tuple_values(binding: &[Value], seed: i64) -> Vec<Value> {
        let mut values = structural_values(TUPLE_COLUMNS, seed);
        for (i, (name, _)) in TUPLE_COLUMNS.iter().enumerate() {
            if let Some(j) = BINDING_COLUMNS.iter().position(|(candidate, _)| candidate == name) {
                values[i] = binding[j].clone();
            }
        }
        values
    }

    fn earlier_writes(conn: &Connection, binding: &[Value]) {
        assert!(require(conn.execute(EPOCH_WRITE, [])) == 1, "A01 independent write missing");
        let tuple = tuple_values(binding, 99);
        let legacy_exists: bool = require(conn.query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM {TUPLE_TABLE}
                 WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND generation=?4)"
            ),
            rusqlite::params![tuple[0], tuple[1], tuple[2], tuple[3]],
            |row| row.get(0),
        ));
        if !legacy_exists {
            assert!(require(conn.execute(
                &insertion(TUPLE_TABLE, TUPLE_COLUMNS),
                params_from_iter(tuple_values(binding, 99).iter()),
            )) == 1, "A01 structural tuple missing");
        }
    }

    fn trigger_denial(error: &rusqlite::Error) {
        assert!(matches!(error, rusqlite::Error::SqliteFailure(code, Some(message))
            if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER
            && MESSAGES.contains(&message.as_str())), "A01 trigger denial missing");
    }

    fn ended_transaction(conn: &Connection) {
        assert!(conn.is_autocommit(), "A01 denied transaction remained active");
        let commit = conn.execute_batch("COMMIT");
        assert!(matches!(commit, Err(rusqlite::Error::SqliteFailure(_, Some(ref message)))
            if message == "cannot commit - no transaction is active"),
            "A01 commit unexpectedly survived denial");
    }

    fn raw_probe(conn: &Connection, sql: &str, values: &[Value], binding: &[Value]) {
        let before = state(conn);
        require(conn.execute_batch("BEGIN IMMEDIATE"));
        earlier_writes(conn, binding);
        let result = conn.execute(sql, params_from_iter(values.iter()));
        let error = match result { Err(error) => error, Ok(_) => panic!("A01 binding unexpectedly inserted") };
        trigger_denial(&error);
        ended_transaction(conn);
        assert!(state(conn) == before, "A01 denial changed committed baseline");
        require(conn.execute_batch("BEGIN IMMEDIATE; COMMIT"));
    }

    fn independently_preserved(dir: &PrivateDir, expected: &State) {
        let observer = require(Connection::open_with_flags(
            dir.0.join("foundation.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ));
        assert!(state(&observer) == *expected, "A01 durable baseline changed");
        drop(observer);
        let reopened = require(ProductStore::reopen_a01_isolated_test_store(
            &dir.0, "foundation.sqlite", config(), clock(),
        ));
        require(validate_a01_pragmas(&reopened.conn));
        assert!(state(&reopened.conn) == *expected, "A01 checked reopen changed baseline");
    }

    #[test]
    fn a01_binding_raw_catch_and_commit_rolls_back_transaction() {
        for reopened in [false, true] {
            let (dir, store) = marked(reopened);
            let before = state(&store.conn);
            let binding = structural_values(BINDING_COLUMNS, 17);
            raw_probe(&store.conn, &insertion("product_result_bindings_v1", BINDING_COLUMNS), &binding, &binding);
            drop(store);
            independently_preserved(&dir, &before);
        }
    }

    // This proves finalizer-style error propagation at the SQL boundary only.
    fn typed_insert(
        tx: &rusqlite::Transaction<'_>,
        binding: &[Value],
        observed: &mut Option<rusqlite::Error>,
    ) -> Result<(), result::ProductResultError> {
        tx.execute(&insertion("product_result_bindings_v1", BINDING_COLUMNS), params_from_iter(binding.iter()))
            .map_err(|error| {
                *observed = Some(error);
                result::ProductResultError::Conflict
            })?;
        Ok(())
    }

    #[test]
    fn a01_binding_typed_error_rolls_back_before_wrapper_drop() {
        for reopened in [false, true] {
            let (dir, mut store) = marked(reopened);
            let before = state(&store.conn);
            let binding = structural_values(BINDING_COLUMNS, 23);
            let tx = require(store.conn.transaction_with_behavior(TransactionBehavior::Immediate));
            earlier_writes(&tx, &binding);
            let mut observed = None;
            assert!(matches!(typed_insert(&tx, &binding, &mut observed),
                Err(result::ProductResultError::Conflict)), "A01 typed denial missing");
            let error = match observed { Some(error) => error, None => panic!("A01 SQL denial missing") };
            trigger_denial(&error);
            // Observe while the wrapper is still alive; its destructor is not the rollback witness.
            assert!(tx.is_autocommit(), "A01 typed transaction remained active");
            assert!(matches!(tx.commit(), Err(rusqlite::Error::SqliteFailure(_, Some(ref message)))
                if message == "cannot commit - no transaction is active"),
                "A01 typed commit unexpectedly survived denial");
            assert!(state(&store.conn) == before, "A01 typed denial changed baseline");
            require(store.conn.execute_batch("BEGIN IMMEDIATE; COMMIT"));
            drop(store);
            independently_preserved(&dir, &before);
        }
    }

    fn forms() -> Vec<String> {
        let base = insertion("product_result_bindings_v1", BINDING_COLUMNS);
        let mut forms = vec![base.clone()];
        for action in ["IGNORE", "REPLACE", "ABORT", "FAIL", "ROLLBACK"] {
            forms.push(base.replacen("INSERT", &format!("INSERT OR {action}"), 1));
        }
        forms.push(format!("{base} ON CONFLICT DO NOTHING"));
        forms.push(format!("{base} ON CONFLICT DO UPDATE SET result_id=excluded.result_id"));
        for target in CONFLICT_TARGETS {
            for action in ["DO NOTHING", "DO UPDATE SET result_id=excluded.result_id"] {
                forms.push(format!("{base} ON CONFLICT ({}) {action}", target.join(",")));
            }
        }
        forms
    }

    #[test]
    fn a01_binding_input_forms_and_nulls_deny_on_fresh_and_reopen() {
        for reopened in [false, true] {
            let (dir, store) = marked(reopened);
            let before = state(&store.conn);
            let binding = structural_values(BINDING_COLUMNS, 31);
            for sql in forms() { raw_probe(&store.conn, &sql, &binding, &binding); }
            let sql = insertion("product_result_bindings_v1", BINDING_COLUMNS);
            for i in 0..binding.len() {
                let mut values = binding.clone();
                values[i] = Value::Null;
                raw_probe(&store.conn, &sql, &values, &binding);
            }
            raw_probe(&store.conn, &sql, &vec![Value::Null; binding.len()], &binding);
            raw_probe(&store.conn, "INSERT INTO product_result_bindings_v1 DEFAULT VALUES", &[], &binding);
            drop(store);
            independently_preserved(&dir, &before);
        }
    }

    fn multirow_forms() -> [String; 2] {
        let base = insertion("product_result_bindings_v1", BINDING_COLUMNS);
        let names = BINDING_COLUMNS.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(",");
        let slots = placeholders(BINDING_COLUMNS.len());
        [
            format!("{base}, ({slots})"),
            format!("INSERT INTO product_result_bindings_v1 ({names}) SELECT {slots} UNION ALL SELECT {slots}"),
        ]
    }

    #[test]
    fn a01_binding_multirow_denial_preserves_committed_evidence() {
        for reopened in [false, true] {
            let (dir, store) = marked(reopened);
            let before = state(&store.conn);
            let binding = structural_values(BINDING_COLUMNS, 37);
            for sql in multirow_forms() { raw_probe(&store.conn, &sql, &binding, &binding); }
            drop(store);
            independently_preserved(&dir, &before);
        }
    }

    // Deliberately synthetic ordinary-schema laboratory. Never returned by a product opener.
    fn laboratory(case: usize, order: &[usize], rollback: bool) -> (Connection, Vec<Value>) {
        let mut conn = require(Connection::open_in_memory());
        let tx = require(conn.transaction_with_behavior(TransactionBehavior::Immediate));
        require(initialize_product_schema_in(&tx, &config(), StoreOpenMode::Normal));
        require(tx.commit());
        require(conn.pragma_update(None, "foreign_keys", false));

        // Temporarily remove insertion guards solely for synthetic laboratory seeding.
        let guards: Vec<(String, String)> = {
            let mut statement = require(conn.prepare(
                "SELECT name,sql FROM sqlite_schema WHERE type='trigger' AND (tbl_name=?1 OR tbl_name=?2) AND sql LIKE '%BEFORE INSERT%'"
            ));
            require(require(statement.query_map(
                rusqlite::params!["product_result_bindings_v1", LEGACY_TABLE],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )).collect::<rusqlite::Result<Vec<_>>>())
        };
        for (name, _) in &guards { require(conn.execute_batch(&format!("DROP TRIGGER {name}"))); }
        let seed = structural_values(BINDING_COLUMNS, 51);
        let mut candidate = structural_values(BINDING_COLUMNS, 61);
        if case < 4 || case == 5 {
            let matching = if case == 5 { COLLISION_COLUMNS[0] } else { COLLISION_COLUMNS[case] };
            for name in matching {
                let i = match BINDING_COLUMNS.iter().position(|(column, _)| column == name) {
                    Some(i) => i, None => panic!("A01 structural collision key missing"),
                };
                candidate[i] = seed[i].clone();
            }
            assert!(require(conn.execute(&insertion("product_result_bindings_v1", BINDING_COLUMNS),
                params_from_iter(seed.iter()))) == 1, "A01 laboratory seed missing");
        }
        if case == 4 || case == 5 {
            let mut values = structural_values(LEGACY_COLUMNS, 71);
            for (i, (name, _)) in LEGACY_COLUMNS.iter().enumerate() {
                if let Some(j) = BINDING_COLUMNS.iter().position(|(column, _)| column == name) {
                    values[i] = candidate[j].clone();
                }
            }
            assert!(require(conn.execute(&insertion(LEGACY_TABLE, LEGACY_COLUMNS),
                params_from_iter(values.iter()))) == 1, "A01 laboratory legacy seed missing");
        }
        // Restore the original census and other insertion guards before installing the tested delta.
        for (_, sql) in &guards { require(conn.execute_batch(sql)); }
        require(conn.pragma_update(None, "foreign_keys", true));
        require(conn.pragma_update(None, "recursive_triggers", true));
        for name in &GUARD_NAMES[..2] { require(conn.execute_batch(&format!("DROP TRIGGER {name}"))); }
        for index in order {
            let sql = if rollback { GUARDS[*index].to_owned() }
                else { require(guards.iter().find(|(name, _)| name.as_str() == GUARD_NAMES[*index])
                    .map(|(_, sql)| sql.clone()).ok_or(())) };
            require(conn.execute_batch(&sql));
        }
        (conn, candidate)
    }

    #[test]
    fn a01_binding_structural_conflict_matrix_rolls_back() {
        // Six-column overlap also hits the four-column key; digest overlap also hits the primary key.
        for case in 0..7 {
            let (conn, binding) = laboratory(case, &[0, 1, 2], true);
            for sql in forms().into_iter().chain(multirow_forms()) {
                raw_probe(&conn, &sql, &binding, &binding);
            }
        }
    }

    #[test]
    fn a01_binding_structural_trigger_orders_are_independent() {
        for order in [[0,1,2], [0,2,1], [1,0,2], [1,2,0], [2,0,1], [2,1,0]] {
            for case in 0..7 {
                let (conn, binding) = laboratory(case, &order, true);
                raw_probe(&conn, &insertion("product_result_bindings_v1", BINDING_COLUMNS), &binding, &binding);
            }
        }
        // Incomplete enforcement exists only in the laboratory, to discriminate each replacement.
        for case in 0..6 {
            let guard_name = if case == 4 {
                "product_result_bindings_no_legacy_tuple"
            } else {
                "product_result_bindings_no_replace"
            };
            let guard = require(GUARD_NAMES.iter().position(|name| *name == guard_name).ok_or(()));
            let (conn, binding) = laboratory(case, &[guard], true);
            for sql in forms().into_iter().chain(multirow_forms()) {
                raw_probe(&conn, &sql, &binding, &binding);
            }
        }
    }

    #[test]
    fn a01_binding_probe_distinguishes_abort_from_rollback() {
        let (conn, binding) = laboratory(0, &[0], false);
        let before = state(&conn);
        let baseline_epoch: i64 = require(conn.query_row(
            "SELECT recovery_epoch FROM product_meta_v1", [], |row| row.get(0),
        ));
        require(conn.execute_batch("BEGIN IMMEDIATE"));
        earlier_writes(&conn, &binding);
        let error = match conn.execute(&insertion("product_result_bindings_v1", BINDING_COLUMNS),
            params_from_iter(binding.iter())) {
            Err(error) => error, Ok(_) => panic!("A01 ordinary duplicate guard missing"),
        };
        trigger_denial(&error);
        assert!(!conn.is_autocommit(), "A01 ABORT calibration lost transaction");
        require(conn.execute_batch("COMMIT"));
        assert!(state(&conn) != before, "A01 ABORT calibration lost earlier writes");
        // Prove both independent effects survived the explicit commit.
        let prior_epoch: i64 = require(conn.query_row(
            "SELECT recovery_epoch FROM product_meta_v1", [], |row| row.get(0),
        ));
        assert!(prior_epoch == baseline_epoch + 1, "A01 ABORT calibration epoch missing");
        let tuples: i64 = require(conn.query_row(
            &format!("SELECT count(*) FROM {TUPLE_TABLE}"), [], |row| row.get(0),
        ));
        assert!(tuples == 1, "A01 ABORT calibration tuple missing");
    }

    #[test]
    fn a01_binding_structural_carrier_multirow_rolls_back_earlier_rows() {
        let (conn, binding) = laboratory(6, &[0, 1, 2], true);
        require(conn.execute_batch("CREATE TABLE foundation_carrier(position INTEGER PRIMARY KEY)"));
        let literals = binding.iter().map(|value| match value {
            Value::Integer(value) => value.to_string(),
            Value::Text(value) => format!("'{}'", value.replace('\'', "''")),
            _ => panic!("A01 carrier structural value unsupported"),
        }).collect::<Vec<_>>().join(",");
        let names = BINDING_COLUMNS.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(",");
        require(conn.execute_batch(&format!(
            "CREATE TRIGGER foundation_carrier_second AFTER INSERT ON foundation_carrier
             WHEN NEW.position=2 BEGIN INSERT INTO product_result_bindings_v1 ({names}) VALUES ({literals}); END"
        )));
        let before = state(&conn);
        require(conn.execute_batch("BEGIN IMMEDIATE"));
        earlier_writes(&conn, &binding);
        let error = match conn.execute("INSERT INTO foundation_carrier VALUES (1),(2)", []) {
            Err(error) => error, Ok(_) => panic!("A01 carrier denial missing"),
        };
        trigger_denial(&error);
        ended_transaction(&conn);
        assert!(state(&conn) == before, "A01 carrier earlier row survived rollback");
        require(conn.execute_batch("BEGIN IMMEDIATE; COMMIT"));
    }

    #[test]
    fn a01_binding_initialization_is_atomic() {
        for step in 0..8 {
            let dir = PrivateDir::new();
            let result = ProductStore::create_a01_isolated_test_store_with_checkpoint(
                &dir.0, "foundation.sqlite", config(), clock(), Some(step),
            );
            assert!(matches!(result, Err(ProductError::Invariant("A01 initialization checkpoint"))),
                "A01 initialization checkpoint was not reached");
            let observer = require(Connection::open_with_flags(
                dir.0.join("foundation.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            ));
            let objects: i64 = require(observer.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'", [], |row| row.get(0),
            ));
            let discriminator: i64 = require(observer.pragma_query_value(None, "user_version", |row| row.get(0)));
            assert!(objects == 0 && discriminator == 0, "A01 failed initialization left schema or marker");
        }
    }
    #[test]
    fn a01_binding_foundation_history_census() {
        // Only the census helper sees these synthetic ordinary-schema laboratory rows.
        let (empty, binding) = laboratory(6, &[0, 1, 2], true);
        require(validate_a01_binding_history(&empty));
        require(empty.execute(&insertion(TUPLE_TABLE, TUPLE_COLUMNS),
            params_from_iter(tuple_values(&binding, 97).iter())));
        require(validate_a01_binding_history(&empty));
        let (populated, _) = laboratory(0, &[0, 1, 2], true);
        assert!(matches!(validate_a01_binding_history(&populated),
            Err(ProductError::ContractMismatch)), "A01 binding history was adopted");
    }
    #[test]
    fn a01_binding_exact_schema_delta_is_marked_only() {
        let (_dir, store) = marked(false);
        let actual = require(a01_schema_definitions(&store.conn));
        let mut ordinary = require(Connection::open_in_memory());
        let tx = require(ordinary.transaction_with_behavior(TransactionBehavior::Immediate));
        require(initialize_product_schema_in(&tx, &config(), StoreOpenMode::Normal));
        require(tx.commit());
        let mut expected = require(a01_schema_definitions(&ordinary));
        let tokens = |sql: &str| sql.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        for index in 0..3 {
            let row = (
                "trigger".to_owned(), GUARD_NAMES[index].to_owned(),
                "product_result_bindings_v1".to_owned(),
                Some(GUARDS[index].trim_end_matches(';').to_owned()),
            );
            if index < 2 {
                let original = require(expected.iter_mut().find(|item| item.1 == GUARD_NAMES[index]).ok_or(()));
                let sql = require(original.3.as_ref().ok_or(()));
                assert!(sql.contains("RAISE(ABORT"), "A01 ordinary guard action changed");
                assert!(tokens(sql) == tokens(&GUARDS[index].trim_end_matches(';').replace("RAISE(ROLLBACK", "RAISE(ABORT")),
                    "A01 replacement changed ordinary predicate");
                *original = row;
            } else {
                assert!(!expected.iter().any(|item| item.1 == GUARD_NAMES[index]),
                    "A01 denial leaked into ordinary schema");
                expected.push(row);
            }
        }
        expected.sort();
        assert!(actual == expected, "A01 schema changed beyond the exact foundation delta");
    }

    #[test]
    fn a01_binding_foundation_tuple_only_reopens() {
        let (dir, store) = marked(false);
        let binding = structural_values(BINDING_COLUMNS, 83);
        assert!(require(store.conn.execute(&insertion(TUPLE_TABLE, TUPLE_COLUMNS),
            params_from_iter(tuple_values(&binding, 83).iter()))) == 1,
            "A01 tuple-only write missing");
        let before = state(&store.conn);
        drop(store);
        independently_preserved(&dir, &before);
    }

    #[test]
    fn a01_binding_foundation_connection_observations() {
        fn observe(
            store: &ProductStore,
        ) -> Result<(i64, i64, i64, i64, String, i32, bool), ProductError> {
            let foreign_keys = store.conn
                .pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
            let recursive_triggers = store.conn
                .pragma_query_value(None, "recursive_triggers", |row| row.get(0))?;
            let ignore_check_constraints = store.conn
                .pragma_query_value(None, "ignore_check_constraints", |row| row.get(0))?;
            let busy_timeout = store.conn
                .pragma_query_value(None, "busy_timeout", |row| row.get(0))?;
            let journal_mode = store.conn
                .pragma_query_value(None, "journal_mode", |row| row.get(0))?;
            let tx = store.conn.unchecked_transaction()?;
            let denial = tx.execute(
                "INSERT INTO product_result_bindings_v1 (
                    result_id, order_id, lifecycle, attempt, run_id, activation, generation,
                    host_result_digest, tuple_digest, binding_digest, payload_json, receipt_json
                 ) VALUES (?1, ?1, 1, 1, ?1, 1, 1, ?1, ?1, ?1, '{}', '{}')",
                ["foundation-observation"],
            ).err().ok_or_else(|| ProductError::Invariant(
                "A01 foundation observation unexpectedly admitted a binding".into(),
            ))?;
            let ended_transaction = tx.is_autocommit();
            let extended_code = match denial {
                rusqlite::Error::SqliteFailure(error, _) => error.extended_code,
                error => return Err(ProductError::Database(error)),
            };
            drop(tx);
            Ok((
                foreign_keys,
                recursive_triggers,
                ignore_check_constraints,
                busy_timeout,
                journal_mode,
                extended_code,
                ended_transaction,
            ))
        }
        for reopened in [false, true] {
            let (_dir, store) = marked(reopened);
            let before: i64 = require(store.conn.query_row(
                "SELECT count(*) FROM product_result_bindings_v1", [], |row| row.get(0),
            ));
            assert_eq!(before, 0, "A01 foundation binding census mismatch");
            let (foreign_keys, recursive_triggers, ignore_check_constraints, busy_timeout,
                journal_mode, denial_code, ended_transaction) = require(observe(&store));
            assert_eq!(foreign_keys, 1);
            assert_eq!(recursive_triggers, 1);
            assert_eq!(ignore_check_constraints, 0);
            assert!((0..=5000).contains(&busy_timeout));
            assert_eq!(journal_mode.to_ascii_lowercase(), "delete");
            assert_eq!(denial_code, rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER);
            assert!(ended_transaction, "A01 denial must roll back the transaction");
            let after: i64 = require(store.conn.query_row(
                "SELECT count(*) FROM product_result_bindings_v1", [], |row| row.get(0),
            ));
            assert_eq!(after, 0, "A01 foundation binding census mismatch");
        }
    }

}


// Bounded experimental task profile. No producer or result authority is granted.
#[cfg(feature = "a01-harness-support")]
mod a01_task {
    use super::*;
    use serde::{Deserialize, Serialize};
    const PROFILE: &str = "task-claim-v1";
    const TASK_KIND: &str = "a01-fixed-v1";
    const TASK_BYTES: &[u8] = b"foundry-a01-fixed-task-v1\n";
    const PERMISSION_GUARD_DDL: &str = r#"CREATE TRIGGER product_result_bindings_a01_permission_guard
BEFORE INSERT ON product_result_bindings_v1
BEGIN
  SELECT CASE
    WHEN t.order_id IS NULL
      OR p.seq IS NULL
      OR json_type(p.j,'$.kind') IS NULL
      OR json_type(t.j,'$.bridge_permission') IS NOT 'object'
      OR json_type(t.j,'$.bridge_permission.grant_id') IS NOT 'text'
      OR json_type(t.j,'$.bridge_permission.grant_revision') IS NOT 'integer'
      OR json_extract(t.j,'$.bridge_permission.grant_revision') <= 0
      OR json_type(t.j,'$.bridge_permission.subject_id') IS NOT 'text'
      OR json_type(t.j,'$.bridge_permission.source_instance_id') IS NOT 'text'
    THEN RAISE(ROLLBACK,'A01 bridge permission not bound')

    WHEN json_extract(t.j,'$.bridge_permission.subject_id') IS NOT t.actor_id
      OR json_extract(t.j,'$.bridge_permission.source_instance_id')
           IS NOT t.source_instance_id
      OR json_extract(t.j,'$.admission.store.installation_id')
           IS NOT m.installation_id
      OR json_extract(t.j,'$.admission.store.database_id')
           IS NOT m.database_id
      OR json_extract(p.j,'$.kind') IS NOT 'a01-passed-bridge-v1'
      OR json_extract(p.j,'$.task') IS NOT t.j
      OR json_extract(p.j,'$.claim') IS NOT json_object(
           'store',json_object(
             'installation_id',m.installation_id,
             'database_id',m.database_id),
           'order_id',c.order_id,
           'lifecycle',c.lifecycle,
           'task_revision',c.task_revision,
           'delivery_id',c.delivery_id,
           'worker_id',c.worker_id,
           'epoch',c.epoch,
           'fence',c.fence,
           'claimed_at_ns',c.claimed_at_ns,
           'lease_until_ns',c.lease_until_ns,
           'lease_until_text',c.lease_until_text,
           'source_revision',c.source_revision,
           'payload_digest',c.payload_digest,
           'service_id',c.service_id,
           'service_revision',c.service_revision,
           'credential_revision',c.credential_revision,
           'source_instance_id',c.source_instance_id)
      OR p.order_id IS NOT NEW.order_id
      OR p.hash IS NOT json_extract(n.j,'$.s1_event_hash')
      OR p.event IS NOT 'InspectionPassed'
      OR p.to_state IS NOT 'waiting'
      OR r.order_id IS NOT NEW.order_id
      OR r.lifecycle IS NOT NEW.lifecycle
      OR COALESCE(r.operation IN (
           'a01-passed-bridge-v1','review-hold-commit-pass'),0) <> 1
      OR r.expected_predecessor
           IS NOT json_extract(p.j,'$.predecessor_seq')
      OR json_extract(p.j,'$.host_key.order') IS NOT NEW.order_id
      OR json_extract(p.j,'$.host_key.attempt') IS NOT NEW.attempt
      OR json_extract(p.j,'$.host_key.run') IS NOT NEW.run_id
      OR json_extract(p.j,'$.host_key.activation') IS NOT NEW.activation
      OR json_extract(p.j,'$.host_result_digest')
           IS NOT NEW.host_result_digest
      OR json_extract(n.j,'$.work.delivery_id') IS NOT c.delivery_id
      OR json_extract(n.j,'$.work.worker_id') IS NOT c.worker_id
      OR json_extract(n.j,'$.work.claim_epoch') IS NOT c.epoch
      OR json_extract(n.j,'$.work.claim_fence') IS NOT c.fence
      OR json_extract(n.j,'$.work.source_revision') IS NOT c.source_revision
      OR json_extract(n.j,'$.work.payload_digest') IS NOT c.payload_digest
      OR json_extract(n.j,'$.work.authority.service_id') IS NOT t.service_id
      OR json_extract(n.j,'$.work.authority.service_revision')
           IS NOT t.service_revision
      OR json_extract(n.j,'$.work.authority.credential_revision')
           IS NOT t.credential_revision
      OR json_extract(n.j,'$.work.authority.drive_review_grant_revision')
           IS NOT t.drive_review_grant_revision
      OR json_extract(n.j,'$.work.authority.source_instance_id')
           IS NOT t.source_instance_id
      OR json_extract(n.j,'$.work.authority.source_generation')
           IS NOT t.source_generation
      OR json_extract(n.j,'$.work.authority.source_instance_revision')
           IS NOT t.source_instance_revision
      OR json_extract(n.j,'$.work.authority.connector_revision')
           IS NOT t.connector_revision
      OR o.hash IS NOT json_extract(p.j,'$.output_event_hash')
      OR o.order_id IS NOT NEW.order_id
      OR ro.operation IS NOT 'a01-producer-output-v1'
      OR ro.lifecycle IS NOT NEW.lifecycle
      OR json_extract(o.j,'$.task') IS NOT t.j
      OR json_extract(o.j,'$.claim') IS NOT json_extract(p.j,'$.claim')
      OR json_extract(o.j,'$.base_commit')
           IS NOT json_extract(n.j,'$.host_result.base_commit')
      OR json_extract(o.j,'$.result_commit')
           IS NOT json_extract(n.j,'$.host_result.generations[0].commit')
    THEN RAISE(ROLLBACK,'A01 bridge permission identity mismatch')

    WHEN t.operation IS NOT 'authorize'
      OR t.revision IS NOT (
           SELECT MAX(x.revision)
           FROM product_a01_task_revisions_v1 x
           WHERE x.order_id=NEW.order_id AND x.lifecycle=NEW.lifecycle)
      OR g.enabled IS NOT 1
      OR g.grant_id IS NOT json_extract(t.j,'$.bridge_permission.grant_id')
      OR g.grant_revision
           IS NOT json_extract(t.j,'$.bridge_permission.grant_revision')
      OR a.enabled IS NOT 1
      OR a.role IS NOT 'owner'
      OR a.revision IS NOT t.actor_account_revision
      OR sr.revision IS NOT t.actor_scope_revision
      OR rg.enabled IS NOT 1
      OR rg.grant_revision IS NOT t.read_order_grant_revision
    THEN RAISE(ROLLBACK,'A01 bridge permission revoked')
    ELSE NULL
  END
  FROM (
    SELECT CASE WHEN json_valid(NEW.payload_json)
      THEN NEW.payload_json ELSE '{}' END AS j
  ) n
  LEFT JOIN product_meta_v1 m ON m.singleton=1
  LEFT JOIN product_a01_claim_snapshots_v1 c
    ON c.order_id=NEW.order_id AND c.lifecycle=NEW.lifecycle
  LEFT JOIN (
    SELECT *,CASE WHEN json_valid(CAST(receipt_bytes AS TEXT))
      THEN CAST(receipt_bytes AS TEXT) ELSE '{}' END AS j
    FROM product_a01_task_revisions_v1
  ) t ON t.order_id=c.order_id AND t.lifecycle=c.lifecycle
       AND t.revision=c.task_revision
  LEFT JOIN (
    SELECT *,CASE WHEN json_valid(intent)
      THEN intent ELSE '{}' END AS j FROM ledger
  ) p ON p.seq=json_extract(n.j,'$.s1_sequence')
  LEFT JOIN product_transition_receipts_v1 r
    ON r.seq=p.seq AND r.event_hash=p.hash
  LEFT JOIN (
    SELECT *,CASE WHEN json_valid(intent)
      THEN intent ELSE '{}' END AS j FROM ledger
  ) o ON o.seq=json_extract(p.j,'$.output_sequence')
  LEFT JOIN product_transition_receipts_v1 ro
    ON ro.seq=o.seq AND ro.event_hash=o.hash
  LEFT JOIN product_scope_grants_v1 g
    ON g.subject_id=t.actor_id
       AND g.source_instance_id=t.source_instance_id
       AND g.capability='a01_bridge_passed'
  LEFT JOIN product_accounts_v1 a ON a.actor_id=t.actor_id
  LEFT JOIN product_scope_subject_revisions_v1 sr
    ON sr.subject_id=t.actor_id
       AND sr.source_instance_id=t.source_instance_id
  LEFT JOIN product_scope_grants_v1 rg
    ON rg.subject_id=t.actor_id
       AND rg.source_instance_id=t.source_instance_id
       AND rg.capability='read_order';
END;"#;

    const DDL: &[&str] = &[
r#"CREATE TABLE product_a01_profile_v1 (
    singleton INTEGER NOT NULL PRIMARY KEY CHECK(singleton = 1),
    version INTEGER NOT NULL CHECK(version = 1),
    profile TEXT NOT NULL CHECK(profile = 'task-claim-v1'),
    installation_id TEXT NOT NULL
        CHECK(length(CAST(installation_id AS BLOB)) BETWEEN 1 AND 256),
    database_id TEXT NOT NULL
        CHECK(length(CAST(database_id AS BLOB)) BETWEEN 1 AND 256)
) STRICT, WITHOUT ROWID;"#,
r#"CREATE TABLE product_a01_task_revisions_v1 (
    profile_singleton INTEGER NOT NULL CHECK(profile_singleton = 1),
    order_id TEXT NOT NULL
        CHECK(length(CAST(order_id AS BLOB)) BETWEEN 1 AND 256),
    lifecycle INTEGER NOT NULL,
    revision INTEGER NOT NULL,
    expected_revision INTEGER NOT NULL
        CHECK(expected_revision >= 0
          AND expected_revision < 9223372036854775807),
    operation TEXT NOT NULL CHECK(operation IN ('authorize', 'revoke')),
    actor_id TEXT NOT NULL
        CHECK(length(CAST(actor_id AS BLOB)) BETWEEN 1 AND 256),
    actor_account_revision INTEGER NOT NULL,
    actor_scope_revision INTEGER NOT NULL,
    read_order_grant_revision INTEGER NOT NULL,
    request_key TEXT NOT NULL
        CHECK(length(CAST(request_key AS BLOB)) BETWEEN 1 AND 128),

    delivery_id TEXT NOT NULL
        CHECK(length(CAST(delivery_id AS BLOB)) BETWEEN 1 AND 256),
    admission_seq INTEGER NOT NULL,
    admission_hash TEXT NOT NULL
        CHECK(length(CAST(admission_hash AS BLOB)) = 64
          AND instr(admission_hash, char(0)) = 0
          AND admission_hash NOT GLOB '*[^0-9a-f]*'),
    raw_kind TEXT NOT NULL
        CHECK(length(CAST(raw_kind AS BLOB)) BETWEEN 1 AND 128),
    raw_payload_digest TEXT NOT NULL
        CHECK(length(CAST(raw_payload_digest AS BLOB)) = 64
          AND instr(raw_payload_digest, char(0)) = 0
          AND raw_payload_digest NOT GLOB '*[^0-9a-f]*'),

    source_instance_id TEXT NOT NULL
        CHECK(length(CAST(source_instance_id AS BLOB)) BETWEEN 1 AND 256),
    source_generation INTEGER NOT NULL,
    source_instance_revision INTEGER NOT NULL,
    connector_id TEXT NOT NULL
        CHECK(length(CAST(connector_id AS BLOB)) BETWEEN 1 AND 256),
    connector_revision INTEGER NOT NULL,
    repository_key TEXT NOT NULL
        CHECK(length(CAST(repository_key AS BLOB)) BETWEEN 1 AND 256),
    repository_provider TEXT NOT NULL
        CHECK(length(CAST(repository_provider AS BLOB)) BETWEEN 1 AND 128),
    api_origin TEXT NOT NULL
        CHECK(length(CAST(api_origin AS BLOB)) BETWEEN 1 AND 2048),
    stable_repository_id TEXT NOT NULL
        CHECK(length(CAST(stable_repository_id AS BLOB)) BETWEEN 1 AND 256),
    api_owner TEXT NOT NULL
        CHECK(length(CAST(api_owner AS BLOB)) BETWEEN 1 AND 256),
    api_repo TEXT NOT NULL
        CHECK(length(CAST(api_repo AS BLOB)) BETWEEN 1 AND 256),

    service_id TEXT NOT NULL
        CHECK(length(CAST(service_id AS BLOB)) BETWEEN 1 AND 256),
    service_revision INTEGER NOT NULL,
    credential_revision INTEGER NOT NULL,
    drive_review_grant_revision INTEGER NOT NULL,
    service_lease_until_ns INTEGER NOT NULL
        CHECK(service_lease_until_ns >= 0),
    credential_expires_at_ns INTEGER NOT NULL
        CHECK(credential_expires_at_ns >= 0),

    task_kind TEXT NOT NULL CHECK(task_kind = 'a01-fixed-v1'),
    task_bytes BLOB NOT NULL
        CHECK(task_bytes =
              CAST('foundry-a01-fixed-task-v1' || char(10) AS BLOB)),
    task_digest TEXT NOT NULL
        CHECK(length(CAST(task_digest AS BLOB)) = 64
          AND instr(task_digest, char(0)) = 0
          AND task_digest NOT GLOB '*[^0-9a-f]*'),

    authorized_at_ns INTEGER NOT NULL CHECK(authorized_at_ns >= 0),
    expires_at_ns INTEGER NOT NULL,
    checked_at_ns INTEGER NOT NULL,
    request_bytes BLOB NOT NULL
        CHECK(length(request_bytes) BETWEEN 1 AND 65536),
    receipt_bytes BLOB NOT NULL
        CHECK(length(receipt_bytes) BETWEEN 1 AND 65536),

    PRIMARY KEY(order_id, lifecycle, revision),
    UNIQUE(actor_id, request_key),

    CHECK(min(lifecycle, revision, actor_account_revision,
              actor_scope_revision, read_order_grant_revision,
              admission_seq, source_generation, source_instance_revision,
              connector_revision, service_revision, credential_revision,
              drive_review_grant_revision) > 0),
    CHECK(revision = expected_revision + 1),
    CHECK(expires_at_ns > authorized_at_ns),
    CHECK(checked_at_ns >= authorized_at_ns),
    CHECK(operation <> 'authorize'
          OR checked_at_ns = authorized_at_ns),

    FOREIGN KEY(profile_singleton)
        REFERENCES product_a01_profile_v1(singleton),
    FOREIGN KEY(order_id)
        REFERENCES product_orders_v1(order_id),
    FOREIGN KEY(order_id, lifecycle)
        REFERENCES product_work_backlog_v1(order_id, lifecycle),
    FOREIGN KEY(delivery_id)
        REFERENCES product_intake_inbox_v1(delivery_id),
    FOREIGN KEY(admission_seq)
        REFERENCES product_transition_receipts_v1(seq),
    FOREIGN KEY(actor_id)
        REFERENCES product_accounts_v1(actor_id),
    FOREIGN KEY(source_instance_id)
        REFERENCES product_scope_source_instances_v1(source_instance_id),
    FOREIGN KEY(repository_key)
        REFERENCES product_scope_repositories_v1(repository_key),
    FOREIGN KEY(service_id)
        REFERENCES product_services_v1(service_id)
) STRICT, WITHOUT ROWID;"#,
r#"CREATE TABLE product_a01_claim_snapshots_v1 (
    order_id TEXT NOT NULL
        CHECK(length(CAST(order_id AS BLOB)) BETWEEN 1 AND 256),
    lifecycle INTEGER NOT NULL CHECK(lifecycle > 0),
    task_revision INTEGER NOT NULL CHECK(task_revision > 0),
    delivery_id TEXT NOT NULL
        CHECK(length(CAST(delivery_id AS BLOB)) BETWEEN 1 AND 256),
    worker_id TEXT NOT NULL
        CHECK(length(CAST(worker_id AS BLOB)) BETWEEN 1 AND 128),
    epoch INTEGER NOT NULL CHECK(epoch = 1),
    fence INTEGER NOT NULL CHECK(fence > 0),
    claimed_at_ns INTEGER NOT NULL CHECK(claimed_at_ns >= 0),
    lease_until_ns INTEGER NOT NULL CHECK(lease_until_ns > claimed_at_ns),
    lease_until_text TEXT NOT NULL
        CHECK(length(CAST(lease_until_text AS BLOB)) = 30),
    source_revision INTEGER NOT NULL CHECK(source_revision > 0),
    payload_digest TEXT NOT NULL
        CHECK(length(CAST(payload_digest AS BLOB)) = 64
          AND instr(payload_digest, char(0)) = 0
          AND payload_digest NOT GLOB '*[^0-9a-f]*'),
    service_id TEXT NOT NULL
        CHECK(length(CAST(service_id AS BLOB)) BETWEEN 1 AND 256),
    service_revision INTEGER NOT NULL CHECK(service_revision > 0),
    credential_revision INTEGER NOT NULL CHECK(credential_revision > 0),
    source_instance_id TEXT NOT NULL
        CHECK(length(CAST(source_instance_id AS BLOB)) BETWEEN 1 AND 256),

    PRIMARY KEY(order_id, lifecycle),
    UNIQUE(fence),

    FOREIGN KEY(order_id, lifecycle, task_revision)
        REFERENCES product_a01_task_revisions_v1
                   (order_id, lifecycle, revision),
    FOREIGN KEY(order_id, lifecycle)
        REFERENCES product_work_claims_v1(order_id, lifecycle),
    FOREIGN KEY(order_id, lifecycle)
        REFERENCES product_service_work_bindings_v1(order_id, lifecycle),
    FOREIGN KEY(delivery_id)
        REFERENCES product_intake_inbox_v1(delivery_id)
) STRICT, WITHOUT ROWID;"#,
r#"CREATE TRIGGER product_a01_profile_insert_guard
BEFORE INSERT ON product_a01_profile_v1
BEGIN
    SELECT CASE WHEN
        EXISTS(SELECT 1 FROM product_a01_profile_v1)
        OR NOT EXISTS(
            SELECT 1 FROM product_meta_v1
            WHERE singleton = 1
              AND installation_id = NEW.installation_id
              AND database_id = NEW.database_id
              AND schema_version = 2561
        )
    THEN RAISE(ROLLBACK, 'A01 profile insert denied') END;
END;"#,
r#"CREATE TRIGGER product_a01_profile_no_update
BEFORE UPDATE ON product_a01_profile_v1
BEGIN
    SELECT RAISE(ROLLBACK, 'A01 profile is immutable');
END;"#,
r#"CREATE TRIGGER product_a01_profile_no_delete
BEFORE DELETE ON product_a01_profile_v1
BEGIN
    SELECT RAISE(ROLLBACK, 'A01 profile is immutable');
END;"#,
r#"CREATE TRIGGER product_a01_task_insert_guard
BEFORE INSERT ON product_a01_task_revisions_v1
BEGIN
    SELECT CASE WHEN EXISTS(
        SELECT 1 FROM product_a01_task_revisions_v1
        WHERE (order_id = NEW.order_id
               AND lifecycle = NEW.lifecycle
               AND revision = NEW.revision)
           OR (actor_id = NEW.actor_id AND request_key = NEW.request_key)
    ) THEN RAISE(ROLLBACK, 'A01 task identity already exists') END;

    SELECT CASE WHEN NEW.expected_revision IS NOT COALESCE((
        SELECT max(revision) FROM product_a01_task_revisions_v1
        WHERE order_id = NEW.order_id AND lifecycle = NEW.lifecycle
    ), 0)
    THEN RAISE(ROLLBACK, 'A01 task revision conflict') END;

    SELECT CASE WHEN COALESCE((
        SELECT operation FROM product_a01_task_revisions_v1
        WHERE order_id = NEW.order_id
          AND lifecycle = NEW.lifecycle
          AND revision = NEW.expected_revision
    ), 'none') <> CASE
        WHEN NEW.operation = 'authorize' AND NEW.expected_revision = 0
            THEN 'none'
        WHEN NEW.operation = 'authorize' THEN 'revoke'
        ELSE 'authorize'
    END
    THEN RAISE(ROLLBACK, 'A01 task operation conflict') END;

    SELECT CASE WHEN NEW.operation = 'authorize' AND (
        EXISTS(
            SELECT 1 FROM product_work_claims_v1
            WHERE order_id = NEW.order_id AND lifecycle = NEW.lifecycle
        )
        OR EXISTS(
            SELECT 1 FROM product_a01_claim_snapshots_v1
            WHERE order_id = NEW.order_id AND lifecycle = NEW.lifecycle
        )
    ) THEN RAISE(ROLLBACK, 'A01 task already claimed') END;
END;"#,
r#"CREATE TRIGGER product_a01_task_no_update
BEFORE UPDATE ON product_a01_task_revisions_v1
BEGIN
    SELECT RAISE(ROLLBACK, 'A01 task history is immutable');
END;"#,
r#"CREATE TRIGGER product_a01_task_no_delete
BEFORE DELETE ON product_a01_task_revisions_v1
BEGIN
    SELECT RAISE(ROLLBACK, 'A01 task history is immutable');
END;"#,
r#"CREATE TRIGGER product_a01_claim_insert_guard
BEFORE INSERT ON product_a01_claim_snapshots_v1
BEGIN
    SELECT CASE WHEN EXISTS(
        SELECT 1 FROM product_a01_claim_snapshots_v1
        WHERE (order_id = NEW.order_id AND lifecycle = NEW.lifecycle)
           OR fence = NEW.fence
    ) THEN RAISE(ROLLBACK, 'A01 claim identity already exists') END;

    SELECT CASE WHEN NOT EXISTS(
        SELECT 1
        FROM product_a01_task_revisions_v1 r
        JOIN product_work_claims_v1 c
          ON c.order_id = r.order_id AND c.lifecycle = r.lifecycle
        JOIN product_work_backlog_v1 b
          ON b.order_id = r.order_id AND b.lifecycle = r.lifecycle
        JOIN product_service_work_bindings_v1 w
          ON w.order_id = r.order_id AND w.lifecycle = r.lifecycle
        WHERE r.order_id = NEW.order_id
          AND r.lifecycle = NEW.lifecycle
          AND r.revision = NEW.task_revision
          AND r.operation = 'authorize'
          AND r.revision = (
              SELECT max(x.revision)
              FROM product_a01_task_revisions_v1 x
              WHERE x.order_id = r.order_id AND x.lifecycle = r.lifecycle
          )
          AND r.delivery_id = NEW.delivery_id
          AND r.connector_revision = NEW.source_revision
          AND r.raw_payload_digest = NEW.payload_digest
          AND r.service_id = NEW.service_id
          AND r.service_revision = NEW.service_revision
          AND r.credential_revision = NEW.credential_revision
          AND r.source_instance_id = NEW.source_instance_id
          AND NEW.claimed_at_ns >= r.authorized_at_ns
          AND NEW.claimed_at_ns < r.expires_at_ns
          AND NEW.lease_until_ns <= min(
              r.expires_at_ns,
              r.service_lease_until_ns,
              r.credential_expires_at_ns
          )
          AND c.owner_id = NEW.worker_id
          AND c.epoch = NEW.epoch
          AND c.fence = NEW.fence
          AND c.state = 'claimed'
          AND c.lease_until = NEW.lease_until_text
          AND c.source_revision = NEW.source_revision
          AND c.payload_digest = NEW.payload_digest
          AND b.delivery_id = NEW.delivery_id
          AND b.state = 'claimed'
          AND b.attempts = 1
          AND w.service_id = NEW.service_id
          AND w.service_revision = NEW.service_revision
          AND w.credential_revision = NEW.credential_revision
          AND w.source_instance_id = NEW.source_instance_id
    ) THEN RAISE(ROLLBACK, 'A01 claim evidence mismatch') END;
END;"#,
r#"CREATE TRIGGER product_a01_claim_no_update
BEFORE UPDATE ON product_a01_claim_snapshots_v1
BEGIN
    SELECT RAISE(ROLLBACK, 'A01 claim snapshot is immutable');
END;"#,
r#"CREATE TRIGGER product_a01_claim_no_delete
BEFORE DELETE ON product_a01_claim_snapshots_v1
BEGIN
    SELECT RAISE(ROLLBACK, 'A01 claim snapshot is immutable');
END;"#,
    PERMISSION_GUARD_DDL,
];

    #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct A01TaskStoreRef {
        pub installation_id:String,
        pub database_id:String,
    }

    #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct A01TaskAdmissionExpectation {
        pub store:A01TaskStoreRef,
        pub order_id:String,
        pub lifecycle:i64,
        pub delivery_id:String,
        pub admission_seq:i64,
        pub admission_hash:String,
        pub raw_kind:String,
        pub raw_payload_digest:String,
        pub source_instance_id:String,
        pub source_generation:i64,
        pub source_instance_revision:i64,
        pub connector_id:String,
        pub connector_revision:i64,
        pub repository_key:String,
        pub repository_provider:String,
        pub api_origin:String,
        pub stable_repository_id:String,
        pub api_owner:String,
        pub api_repo:String,
    }

    #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct A01TaskOwnerExpectation {
        pub account_revision:i64,
        pub scope_revision:i64,
        pub read_order_grant_revision:i64,
    }

    #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct A01TaskServiceExpectation {
        pub service_id:String,
        pub service_revision:i64,
        pub credential_revision:i64,
        pub drive_review_grant_revision:i64,
        pub service_lease_until_ns:i64,
        pub credential_expires_at_ns:i64,
    }

    #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct A01BridgePermissionRef {
        pub grant_id: String,
        pub grant_revision: i64,
        pub subject_id: String,
        pub source_instance_id: String,
    }

    #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct A01AuthorizeTaskRequest {
        pub bridge_permission: A01BridgePermissionRef,
        pub request_key:String,
        pub expected_revision:i64,
        pub admission:A01TaskAdmissionExpectation,
        pub owner:A01TaskOwnerExpectation,
        pub service:A01TaskServiceExpectation,
        pub expires_at_ns:i64,
    }

    #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct A01RevokeTaskRequest {
        pub request_key:String,
        pub expected_revision:i64,
        pub admission:A01TaskAdmissionExpectation,
        pub owner:A01TaskOwnerExpectation,
    }

    #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct A01TaskReceipt {
        pub bridge_permission: A01BridgePermissionRef,
        pub version:u32,
        pub profile:String,
        pub operation:A01TaskOperation,
        pub actor_id:String,
        pub request_key:String,
        pub expected_revision:i64,
        pub revision:i64,
        pub admission:A01TaskAdmissionExpectation,
        pub owner:A01TaskOwnerExpectation,
        pub service:A01TaskServiceExpectation,
        pub task_kind:String,
        pub task_bytes:Vec<u8>,
        pub task_digest:String,
        pub authorized_at_ns:i64,
        pub expires_at_ns:i64,
        pub checked_at_ns:i64,
    }

    #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct A01TaskClaimView {
        pub store:A01TaskStoreRef,
        pub order_id:String,
        pub lifecycle:i64,
        pub task_revision:i64,
        pub delivery_id:String,
        pub worker_id:String,
        pub epoch:i64,
        pub fence:i64,
        pub claimed_at_ns:i64,
        pub lease_until_ns:i64,
        pub lease_until_text:String,
        pub source_revision:i64,
        pub payload_digest:String,
        pub service_id:String,
        pub service_revision:i64,
        pub credential_revision:i64,
        pub source_instance_id:String,
    }

    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum A01TaskOperation { Authorize, Revoke }

    /// Retained by the creator; intentionally has no constructor, Debug or serde.
    pub struct A01TaskStoreIdentity {
        installation_id: String,
        database_id: String,
        file: A01FileIdentity,
    }

    impl ProductStore {
    #[cfg(feature = "a01-harness-support")]

pub fn create_a01_task_claim_test_store(
        directory: &Path,
        name: &str,
        config: ProductConfig,
        clock: std::sync::Arc<dyn ProductClock>,

    ) -> Result<(ProductStore, A01TaskStoreIdentity), ProductError> {
        validate_a01_config(&config)?;
        let path = a01_test_path(directory, name)?;
        a01_require_absent(&path)?;
        a01_check_sidecars(&path)?;
        #[cfg(not(unix))]
        { let _ = clock; Err(ProductError::InvalidConfig) }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let held = std::fs::OpenOptions::new().read(true).write(true)
                .create_new(true).mode(0o600).open(&path)
                .map_err(|_| ProductError::Invariant("A01 exclusive file creation failed"))?;
            let identity = a01_checked_file(&path, &held)?;
            // The fresh schema and discriminator share open_internal's one
            // IMMEDIATE initialization transaction. No ordinary v3 is committed.
            let mut store = Self::open_internal_with_checkpoint(
            &path, config, clock, StoreOpenMode::A01TaskCreate,
            #[cfg(all(test, feature = "a01-harness-support", unix))]
            None,
        )?;
            if a01_checked_file(&path, &held)? != identity {
                return Err(ProductError::Invariant("A01 physical identity changed"));
            }
            a01_check_sidecars(&path)?;
            store.a01_identity = Some(identity);
            let retained = A01TaskStoreIdentity {
                installation_id: store.installation_id.clone(),
                database_id: store.database_id.clone(), file: identity,
            };
            Ok((store, retained))
        }
    }

    #[cfg(feature = "a01-harness-support")]
    pub fn reopen_a01_task_claim_test_store(
        directory: &Path,
        name: &str,
        config: ProductConfig,
        clock: std::sync::Arc<dyn ProductClock>,
        retained: &A01TaskStoreIdentity,
    ) -> Result<ProductStore, ProductError> {
        validate_a01_config(&config)?;
        if retained.installation_id != config.installation_id || retained.database_id != config.database_id {
            return Err(ProductError::ContractMismatch);
        }
        let path = a01_test_path(directory, name)?;
        a01_check_sidecars(&path)?;
        // A foreign regular file fails the retained identity contract before
        // checking the permissions required of this store's own file.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|_| ProductError::Invariant("A01 file inspection failed"))?;
            if metadata.file_type().is_file()
                && (metadata.dev() != retained.file.device || metadata.ino() != retained.file.inode)
            {
                return Err(ProductError::ContractMismatch);
            }
        }
        a01_file_identity(&path)?;
        let held = std::fs::File::open(&path)
            .map_err(|_| ProductError::Invariant("A01 file inspection failed"))?;
        let identity = a01_checked_file(&path, &held)?;
        if identity != retained.file { return Err(ProductError::ContractMismatch); }
        // Reject WAL format before SQLite inspection can create WAL/SHM files.
        #[cfg(unix)]
        {
            let header = a01_read_header(&held)?;
            if &header[..16] != b"SQLite format 3\0" || header[18..20] != [1, 1] {
                return Err(ProductError::ContractMismatch);
            }
        }
        {
            let inspection = Connection::open_with_flags(
                &path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            inspection.busy_timeout(std::time::Duration::from_secs(5))?;
            let tx = inspection.unchecked_transaction()?;
            let journal: String = tx.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
            if journal != "delete" { return Err(ProductError::ContractMismatch); }
            validate_a01_task_contract(&tx, &config)?;
            tx.commit()?;
        }
        if a01_checked_file(&path, &held)? != identity {
            return Err(ProductError::Invariant("A01 physical identity changed"));
        }
        a01_check_sidecars(&path)?;
        let conn = Connection::open_with_flags(
            &path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        )?;
        configure_a01_connection(&conn)?;
        {
            let tx = conn.unchecked_transaction()?;
            validate_a01_task_connection(&tx, &config)?;
            tx.commit()?;
        }
        if a01_checked_file(&path, &held)? != identity {
            return Err(ProductError::Invariant("A01 physical identity changed"));
        }
        a01_check_sidecars(&path)?;
        Ok(Self {
            conn,
            open_mode: StoreOpenMode::A01TaskReopen,
            a01_identity: Some(identity),
            path,
            installation_id: config.installation_id,
            database_id: config.database_id,
            clock,
            #[cfg(feature = "w1-isolated-test-support")]
            w1_capture_fail_after_legacy_writes: false,
            #[cfg(feature = "w1-isolated-test-support")]
            w1_capture_busy_observer: None,
            review_config: None,
            artifact_root: None,
            artifact_root_handle: None,
            active_review_claim: None,
            active_review_claim_lock: None,
            #[cfg(any(test, feature = "p2-test-support"))]
            charge_budget_observer: None,
        })
    }

    }

    #[derive(Default)]
    pub(super) struct TaskHooks<'a> {
        #[cfg(all(test, unix))]
        callback: Option<&'a mut dyn FnMut(&str) -> Result<(), ProductError>>,
        lifetime: std::marker::PhantomData<&'a ()>,
    }
    impl<'a> TaskHooks<'a> {
        #[cfg(all(test, unix))]
        pub(super) fn with_callback(callback: &'a mut dyn FnMut(&str) -> Result<(),ProductError>) -> Self {
            Self{callback:Some(callback),lifetime:std::marker::PhantomData}
        }
        fn hit(&mut self, name: &str) -> Result<(), ProductError> {
            #[cfg(all(test, unix))]
            if let Some(callback) = self.callback.as_mut() { callback(name)?; }
            let _ = name;
            Ok(())
        }
    }
    fn evidence(reason: &'static str) -> ProductError { ProductError::Invariant(reason) }
    fn unavailable() -> ProductError { ProductError::Invariant("A01 authorization unavailable") }
    fn conflict() -> ProductError { ProductError::Invariant("A01 revision conflict") }
    fn ineligible() -> ProductError { ProductError::Invariant("A01 task not eligible") }
    fn auth_error(e: ProductError) -> ProductError {
        match e { ProductError::Invariant(_) => unavailable(), ProductError::Database(e)=>stored_error(e), other => other }
    }
    fn scope_error(e: ProductError) -> ProductError {
        match e {
            ProductError::Invariant("role does not permit capability" | "order scope provenance unavailable" | "repository capability grant unavailable" | "current source instance is unavailable")=>unavailable(),
            ProductError::Invariant(_)=>evidence("A01 scope invariant invalid"),
            ProductError::Database(e)=>stored_error(e), other=>other,
        }
    }
    fn stored_error(e: rusqlite::Error) -> ProductError {
        match e {
            rusqlite::Error::InvalidColumnType(..) | rusqlite::Error::IntegralValueOutOfRange(..)
            | rusqlite::Error::FromSqlConversionFailure(..) | rusqlite::Error::QueryReturnedNoRows => evidence("A01 stored row missing or malformed"),
            other => ProductError::Database(other),
        }
    }
    fn one(n: usize) -> Result<(), ProductError> { if n == 1 { Ok(()) } else { Err(conflict()) } }
    pub(super) fn finish<T>(tx: rusqlite::Transaction<'_>, result: Result<T, ProductError>, hooks: &mut TaskHooks<'_>) -> Result<T, ProductError> {
        match result.and_then(|v| { hooks.hit("precommit")?; Ok(v) }) {
            Ok(v) => { tx.commit()?; Ok(v) }
            Err(e) => { if !tx.is_autocommit() { tx.rollback()?; } Err(e) }
        }
    }
    pub(super) fn initialize_in(tx: &rusqlite::Transaction<'_>, config: &ProductConfig, hooks: &mut TaskHooks<'_>) -> Result<(), ProductError> {
        tx.execute_batch("DROP TRIGGER IF EXISTS product_result_bindings_a01_deny_all")?;
        for (i, ddl) in DDL.iter().enumerate() {
            tx.execute_batch(ddl)?;
            hooks.hit(&format!("schema-{i}"))?;
        }
        one(tx.execute("INSERT INTO product_a01_profile_v1 VALUES(1,1,?1,?2,?3)", params![PROFILE,config.installation_id,config.database_id])?)?;
        hooks.hit("profile")
    }
    // One strict conversion path for every participating time. Chrono alone
    // accepts leap seconds and silently consumes fractional digits beyond nine.
    pub(super) fn time_ns(value: &str) -> Result<i64, ProductError> {
        let bytes = value.as_bytes();
        if bytes.len() < 20 || bytes.get(10) != Some(&b'T') || bytes.get(16) != Some(&b':')
            || !bytes.get(17..19).is_some_and(|v| v.iter().all(u8::is_ascii_digit) && v[0] < b'6') {
            return Err(evidence("A01 timestamp layout invalid"));
        }
        let mut end = 19;
        if bytes.get(end) == Some(&b'.') {
            end += 1;
            let begin = end;
            while bytes.get(end).is_some_and(u8::is_ascii_digit) { end += 1; }
            if end == begin { return Err(evidence("A01 timestamp fraction precision invalid: end")); }
        if end - begin > 9 { return Err(evidence("A01 timestamp fraction precision invalid: end check 2")); }
        }
        if !(bytes.get(end..) == Some(&b"Z"[..]) || (bytes.len() == end + 6
            && matches!(bytes[end], b'+' | b'-') && bytes[end+3] == b':'
            && bytes[end+1..end+3].iter().all(u8::is_ascii_digit)
            && bytes[end+4..end+6].iter().all(u8::is_ascii_digit))) { return Err(evidence("A01 timestamp timezone invalid")); }
        let time = DateTime::parse_from_rfc3339(value).map_err(|_| evidence("A01 RFC3339 timestamp invalid"))?.with_timezone(&Utc);
        clock_ns(time)
    }
    fn clock_ns(time: DateTime<Utc>) -> Result<i64, ProductError> {
        if time.timestamp_subsec_nanos() >= 1_000_000_000 { return Err(evidence("A01 clock leap second unsupported")); }
        time.timestamp_nanos_opt().filter(|n| *n >= 0).ok_or_else(|| evidence("A01 clock nanoseconds out of range"))
    }
    fn datetime(ns: i64) -> Result<DateTime<Utc>, ProductError> {
        if ns < 0 { return Err(evidence("A01 datetime nanoseconds negative")); }
        DateTime::from_timestamp(ns / 1_000_000_000, (ns % 1_000_000_000) as u32).ok_or_else(|| evidence("A01 datetime nanoseconds out of range"))
    }
    fn canonical<T: Serialize>(value: &T) -> Result<Vec<u8>, ProductError> {
        let bytes = serde_json::to_vec(value).map_err(|_| evidence("A01 canonical JSON serialization failed"))?;
        if bytes.is_empty() || bytes.len() > 65536 { return Err(ProductError::InvalidConfig); }
        Ok(bytes)
    }
    fn decode<T: Serialize + serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, ProductError> {
        if bytes.is_empty() { return Err(evidence("A01 canonical JSON byte length invalid: bytes.is_empty")); }
        if bytes.len() > 65536 { return Err(evidence("A01 canonical JSON byte length invalid: bytes.len")); }
        let value: T = serde_json::from_slice(bytes).map_err(|_| evidence("A01 canonical JSON deserialization failed"))?;
        if canonical(&value).map_err(|_| evidence("A01 canonical JSON reserialization failed"))? != bytes { return Err(evidence("A01 canonical JSON round-trip mismatch")); }
        Ok(value)
    }
    fn text(value: &str, max: usize) -> bool {
        !value.trim().is_empty() && value.len() <= max && !value.as_bytes().contains(&0)
    }
    fn digest(value: &str) -> bool {
        value.len() == 64 && value.bytes().all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
    }
    fn validate_admission(a: &A01TaskAdmissionExpectation) -> Result<(), ProductError> {
        if [a.lifecycle,a.admission_seq,a.source_generation,a.source_instance_revision,a.connector_revision].iter().any(|v| *v <= 0)
            || [&a.store.installation_id,&a.store.database_id,&a.order_id,&a.delivery_id,&a.source_instance_id,&a.connector_id,&a.repository_key,&a.stable_repository_id,&a.api_owner,&a.api_repo].iter().any(|v| !text(v,256))
            || !text(&a.raw_kind,128) || !text(&a.repository_provider,128) || !text(&a.api_origin,2048)
            || !digest(&a.admission_hash) || !digest(&a.raw_payload_digest) { return Err(ProductError::InvalidConfig); }
        Ok(())
    }
    fn validate_owner(o: &A01TaskOwnerExpectation) -> Result<(), ProductError> {
        if [o.account_revision,o.scope_revision,o.read_order_grant_revision].iter().any(|v| *v <= 0) { return Err(ProductError::InvalidConfig); }
        Ok(())
    }
    fn validate_service(v: &A01TaskServiceExpectation) -> Result<(), ProductError> {
        if !text(&v.service_id,256) || [v.service_revision,v.credential_revision,v.drive_review_grant_revision].iter().any(|n| *n <= 0)
            || v.service_lease_until_ns < 0 || v.credential_expires_at_ns < 0 { return Err(ProductError::InvalidConfig); }
        Ok(())
    }
    fn validate_key(key: &str, revision: i64) -> Result<(), ProductError> {
        if !text(key,128) || !(0..i64::MAX).contains(&revision) { return Err(ProductError::InvalidConfig); }
        Ok(())
    }
    fn task_digest() -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"foundry-product-task-v1\0");
        for value in [TASK_KIND.as_bytes(), TASK_BYTES] { h.update((value.len() as u64).to_be_bytes()); h.update(value); }
        hex::encode(h.finalize())
    }
    #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Context {
        service_id: String, service_revision: i64, credential_revision: i64,
        source_instance_id: String, source_generation: i64, source_instance_revision: i64,
        connector_revision: i64, source_revision: i64, connector_id: String,
        api_origin: String, repository_id: String, api_owner: String, api_repo: String,
    }
    impl From<&ProductServiceContext> for Context {
        fn from(c: &ProductServiceContext) -> Self {
            Self { service_id:c.service_id.clone(),service_revision:c.service_revision,credential_revision:c.credential_revision,
                source_instance_id:c.source_instance_id.clone(),source_generation:c.source_generation,source_instance_revision:c.source_instance_revision,
                connector_revision:c.connector_revision,source_revision:c.source_revision,connector_id:c.connector_id.clone(),
                api_origin:c.api_origin.clone(),repository_id:c.repository_id.clone(),api_owner:c.api_owner.clone(),api_repo:c.api_repo.clone() }
        }
    }
    #[derive(Clone, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct AuthorizeEnvelope {
        version: u32, profile: String, operation: A01TaskOperation, actor_id: String,
        request: A01AuthorizeTaskRequest, context: Context, task_kind: String, task_bytes: Vec<u8>,
    }
    #[derive(Clone, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct RevokeEnvelope {
        version: u32, profile: String, operation: A01TaskOperation, actor_id: String, request: A01RevokeTaskRequest,
    }
    fn session(tx: &rusqlite::Transaction<'_>, key: &str, now: i64) -> Result<ProductPrincipal, ProductError> {
        // Explicit strict wrapper: the ordinary resolver intentionally retains its
        // original RFC3339 behavior outside this profile.
        let expiry: Option<String> = tx.query_row("SELECT expires_at FROM product_sessions_v1 WHERE session_digest=?1", [key], |r| r.get(0)).optional().map_err(stored_error)?;
        let expiry = time_ns(&expiry.ok_or_else(unavailable)?)?;
        let principal = resolve_session_in(tx,key,datetime(now)?).map_err(auth_error)?;
        if principal.role != "owner" || expiry <= now { return Err(unavailable()); }
        Ok(principal)
    }
    fn visible(tx: &rusqlite::Transaction<'_>, principal: &ProductPrincipal, a: &A01TaskAdmissionExpectation, current: bool) -> Result<ScopedOrderView,ProductError> {
        let v = require_order_capability_in(tx,principal,&a.order_id,ProductCapability::ReadOrder,current).map_err(scope_error)?;
        if v.lifecycle != a.lifecycle || v.source_instance_id != a.source_instance_id || v.repository_key != a.repository_key { return Err(unavailable()); }
        Ok(v)
    }
    fn owner(tx: &rusqlite::Transaction<'_>, principal: &ProductPrincipal, a: &A01TaskAdmissionExpectation, expected: &A01TaskOwnerExpectation, current: bool) -> Result<(),ProductError> {
        let v = visible(tx,principal,a,current)?;
        if principal.authority_revision != expected.account_revision || v.scope_revision != expected.scope_revision || v.grant_revision != expected.read_order_grant_revision { return Err(unavailable()); }
        Ok(())
    }
    fn issuing_owner(tx: &rusqlite::Transaction<'_>, receipt: &A01TaskReceipt) -> Result<(),ProductError> {
        let row: Option<(String,i64)> = tx.query_row("SELECT role,revision FROM product_accounts_v1 WHERE actor_id=?1 AND enabled=1", [&receipt.actor_id], |r| Ok((r.get(0)?,r.get(1)?))).optional().map_err(stored_error)?;
        let (role,revision) = row.ok_or_else(unavailable)?;
        if role != "owner" { return Err(unavailable()); }
        owner(tx,&ProductPrincipal{actor_id:receipt.actor_id.clone(),role,authority_revision:revision},&receipt.admission,&receipt.owner,true)
    }
    // Reads deadlines without SQL text comparisons and checks independent
    // connector, instance, credential and action grant revisions.
    fn authority(tx: &rusqlite::Transaction<'_>, c: &ProductServiceContext, now: i64) -> Result<A01TaskServiceExpectation,ProductError> {
        let row: Option<(String,String,i64)> = tx.query_row(
            "SELECT s.lease_until,k.expires_at,g.grant_revision FROM product_services_v1 s JOIN product_service_credentials_v1 k USING(service_id) JOIN product_service_grants_v1 g ON g.service_id=s.service_id JOIN product_scope_source_instances_v1 i ON i.source_instance_id=g.source_instance_id JOIN product_scope_current_sources_v1 cs ON cs.connector_id=i.connector_id AND cs.source_instance_id=i.source_instance_id JOIN product_sources_v1 ps ON ps.connector_id=i.connector_id JOIN product_scope_repositories_v1 r ON r.repository_key=i.repository_key JOIN product_scope_api_locators_v1 l USING(source_instance_id) WHERE s.service_id=?1 AND s.enabled=1 AND s.revision=?2 AND k.credential_revision=?3 AND g.source_instance_id=?4 AND g.action='drive_review' AND g.enabled=1 AND i.enabled=1 AND i.generation=?5 AND i.revision=?6 AND ps.revision=?7 AND ps.enabled=1 AND i.connector_id=?8 AND r.api_origin=?9 AND r.stable_repository_id=?10 AND ps.origin=?9 AND ps.repository_id=?10 AND ps.provider=r.provider AND l.api_owner=?11 COLLATE NOCASE AND l.api_repo=?12 COLLATE NOCASE",
            params![c.service_id,c.service_revision,c.credential_revision,c.source_instance_id,c.source_generation,c.source_instance_revision,c.connector_revision,c.connector_id,c.api_origin,c.repository_id,c.api_owner,c.api_repo],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(stored_error)?;
        let (lease,expiry,grant) = row.ok_or_else(unavailable)?;
        let service_lease_until_ns=time_ns(&lease)?; let credential_expires_at_ns=time_ns(&expiry)?;
        if c.source_revision != c.connector_revision || grant <= 0 || service_lease_until_ns <= now || credential_expires_at_ns <= now { return Err(unavailable()); }
        Ok(A01TaskServiceExpectation { service_id:c.service_id.clone(),service_revision:c.service_revision,credential_revision:c.credential_revision,drive_review_grant_revision:grant,service_lease_until_ns,credential_expires_at_ns })
    }
    fn context_for(r: &A01TaskReceipt) -> ProductServiceContext {
        let a=&r.admission; let v=&r.service;
        ProductServiceContext { service_id:v.service_id.clone(),service_revision:v.service_revision,credential_revision:v.credential_revision,
            source_instance_id:a.source_instance_id.clone(),source_generation:a.source_generation,source_instance_revision:a.source_instance_revision,
            connector_revision:a.connector_revision,source_revision:a.connector_revision,connector_id:a.connector_id.clone(),
            api_origin:a.api_origin.clone(),repository_id:a.stable_repository_id.clone(),api_owner:a.api_owner.clone(),api_repo:a.api_repo.clone() }
    }
    fn context_matches(c: &Context, a: &A01TaskAdmissionExpectation, v: &A01TaskServiceExpectation) -> bool {
        c.service_id==v.service_id && c.service_revision==v.service_revision && c.credential_revision==v.credential_revision
            && c.source_instance_id==a.source_instance_id && c.source_generation==a.source_generation && c.source_instance_revision==a.source_instance_revision
            && c.connector_revision==a.connector_revision && c.source_revision==a.connector_revision && c.connector_id==a.connector_id
            && c.api_origin==a.api_origin && c.repository_id==a.stable_repository_id && c.api_owner.eq_ignore_ascii_case(&a.api_owner) && c.api_repo.eq_ignore_ascii_case(&a.api_repo)
    }
    fn admission(tx: &rusqlite::Transaction<'_>, a: &A01TaskAdmissionExpectation) -> Result<(String,String,i64),ProductError> {
        let store: (String,String) = tx.query_row("SELECT installation_id,database_id FROM product_meta_v1 WHERE singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?))).map_err(stored_error)?;
        if store != (a.store.installation_id.clone(),a.store.database_id.clone()) { return Err(evidence("A01 admission store identity mismatch")); }
        let raw: Option<(String,String,String,String,String,String,i64,String)> = tx.query_row(
            "SELECT o.task_kind,o.payload_snapshot,i.issue_id,i.version_digest,m.observed_updated_at,b.state,b.attempts,b.available_at FROM product_work_backlog_v1 b JOIN product_orders_v1 o ON o.order_id=b.order_id AND o.lifecycle=b.lifecycle JOIN product_intake_inbox_v1 i ON i.delivery_id=b.delivery_id JOIN product_intake_delivery_meta_v1 m USING(delivery_id) JOIN product_order_payload_digests_v1 d ON d.order_id=o.order_id JOIN product_scope_order_provenance_v1 p ON p.order_id=o.order_id AND p.lifecycle=o.lifecycle JOIN product_intake_source_bindings_v1 ib ON ib.delivery_id=i.delivery_id JOIN product_scope_source_instances_v1 si ON si.source_instance_id=p.source_instance_id JOIN product_scope_repositories_v1 sr ON sr.repository_key=si.repository_key JOIN product_transition_receipts_v1 tr ON tr.seq=o.admission_seq WHERE o.order_id=?1 AND o.lifecycle=?2 AND b.delivery_id=?3 AND i.state='acked' AND o.connector_id=?4 AND i.connector_id=o.connector_id AND o.repository_id=?5 AND i.repository_id=o.repository_id AND i.issue_id=o.issue_id AND o.source_revision=?6 AND o.admission_seq=?7 AND o.admission_hash=?8 AND o.task_kind=?9 AND m.task_kind=o.task_kind AND i.payload=o.payload_snapshot AND d.payload_digest=?10 AND p.verified=1 AND p.source_instance_id=?11 AND p.admission_seq=o.admission_seq AND p.admission_hash=o.admission_hash AND ib.source_instance_id=p.source_instance_id AND ib.source_revision=o.source_revision AND ib.source_generation=?12 AND ib.source_instance_revision=?13 AND si.connector_id=o.connector_id AND si.repository_key=?14 AND si.generation=ib.source_generation AND sr.provider=?15 AND sr.api_origin=?16 AND sr.stable_repository_id=o.repository_id AND tr.order_id=o.order_id AND tr.lifecycle=o.lifecycle AND tr.event_hash=o.admission_hash AND tr.operation='github-admission'",
            params![a.order_id,a.lifecycle,a.delivery_id,a.connector_id,a.stable_repository_id,a.connector_revision,a.admission_seq,a.admission_hash,a.raw_kind,a.raw_payload_digest,a.source_instance_id,a.source_generation,a.source_instance_revision,a.repository_key,a.repository_provider,a.api_origin],
            |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).optional().map_err(stored_error)?;
        let (kind,payload,issue,version,updated,state,attempts,available)=raw.ok_or_else(|| evidence("A01 admission delivery missing"))?;
        if payload_digest(&kind,&payload)!=a.raw_payload_digest || delivery_version_digest(&issue,&updated,&kind,&payload)!=version
            || github_delivery_id(&a.store.installation_id,&a.connector_id,&a.api_origin,&a.stable_repository_id,&issue,&version)!=a.delivery_id { return Err(evidence("A01 admission delivery identity mismatch")); }
        let (seq,lifecycle,_verified_state)=verified_order_state_in(tx,&a.order_id).map_err(|e| match e {ProductError::Invariant(_)=>evidence("A01 admission ledger state invalid"),other=>other})?;
        if seq < a.admission_seq { return Err(evidence("A01 admission sequence or lifecycle mismatch: seq")); }
        if i64::from(lifecycle).checked_add(1)!=Some(a.lifecycle) { return Err(evidence("A01 admission sequence or lifecycle mismatch: from")); }
        time_ns(&available)?;
        Ok((payload,state,attempts))
    }
    fn no_execution(tx: &rusqlite::Transaction<'_>, a: &A01TaskAdmissionExpectation) -> Result<(),ProductError> {
        for table in ["product_work_claims_v1","product_a01_claim_snapshots_v1","product_service_work_bindings_v1",
            "product_review_work_bindings_v1","product_review_runs_v1","product_review_sessions_v1","product_review_host_accept_v1",
            "product_review_holds_v1","product_review_hold_acceptances_v1","product_review_run_reservations_v1","product_review_active_v1",
            "product_review_epochs_v1","product_review_host_results_v1","product_review_stages_v1","product_review_budget_v1",
            "product_review_failures_v1","product_review_redrive_v1","product_artifacts_v1","product_result_bindings_v1"] {
            let exists: bool=tx.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE order_id=?1)"),[&a.order_id],|r|r.get(0))?;
            if exists { return Err(ineligible()); }
        }
        Ok(())
    }

    const COLUMNS: &str = "profile_singleton,order_id,lifecycle,revision,expected_revision,operation,actor_id,actor_account_revision,actor_scope_revision,read_order_grant_revision,request_key,delivery_id,admission_seq,admission_hash,raw_kind,raw_payload_digest,source_instance_id,source_generation,source_instance_revision,connector_id,connector_revision,repository_key,repository_provider,api_origin,stable_repository_id,api_owner,api_repo,service_id,service_revision,credential_revision,drive_review_grant_revision,service_lease_until_ns,credential_expires_at_ns,task_kind,task_bytes,task_digest,authorized_at_ns,expires_at_ns,checked_at_ns,request_bytes,receipt_bytes";
    fn operation(op: A01TaskOperation) -> &'static str { match op { A01TaskOperation::Authorize=>"authorize",A01TaskOperation::Revoke=>"revoke" } }
    fn values(r: &A01TaskReceipt, request: &[u8]) -> Result<Vec<rusqlite::types::Value>, ProductError> {
        Ok(vec![
            rusqlite::types::Value::from(1_i64),
            rusqlite::types::Value::from(r.admission.order_id.clone()),
            rusqlite::types::Value::from(r.admission.lifecycle),
            rusqlite::types::Value::from(r.revision),
            rusqlite::types::Value::from(r.expected_revision),
            rusqlite::types::Value::from(operation(r.operation).to_owned()),
            rusqlite::types::Value::from(r.actor_id.clone()),
            rusqlite::types::Value::from(r.owner.account_revision),
            rusqlite::types::Value::from(r.owner.scope_revision),
            rusqlite::types::Value::from(r.owner.read_order_grant_revision),
            rusqlite::types::Value::from(r.request_key.clone()),
            rusqlite::types::Value::from(r.admission.delivery_id.clone()),
            rusqlite::types::Value::from(r.admission.admission_seq),
            rusqlite::types::Value::from(r.admission.admission_hash.clone()),
            rusqlite::types::Value::from(r.admission.raw_kind.clone()),
            rusqlite::types::Value::from(r.admission.raw_payload_digest.clone()),
            rusqlite::types::Value::from(r.admission.source_instance_id.clone()),
            rusqlite::types::Value::from(r.admission.source_generation),
            rusqlite::types::Value::from(r.admission.source_instance_revision),
            rusqlite::types::Value::from(r.admission.connector_id.clone()),
            rusqlite::types::Value::from(r.admission.connector_revision),
            rusqlite::types::Value::from(r.admission.repository_key.clone()),
            rusqlite::types::Value::from(r.admission.repository_provider.clone()),
            rusqlite::types::Value::from(r.admission.api_origin.clone()),
            rusqlite::types::Value::from(r.admission.stable_repository_id.clone()),
            rusqlite::types::Value::from(r.admission.api_owner.clone()),
            rusqlite::types::Value::from(r.admission.api_repo.clone()),
            rusqlite::types::Value::from(r.service.service_id.clone()),
            rusqlite::types::Value::from(r.service.service_revision),
            rusqlite::types::Value::from(r.service.credential_revision),
            rusqlite::types::Value::from(r.service.drive_review_grant_revision),
            rusqlite::types::Value::from(r.service.service_lease_until_ns),
            rusqlite::types::Value::from(r.service.credential_expires_at_ns),
            rusqlite::types::Value::from(r.task_kind.clone()),
            rusqlite::types::Value::from(r.task_bytes.clone()),
            rusqlite::types::Value::from(r.task_digest.clone()),
            rusqlite::types::Value::from(r.authorized_at_ns),
            rusqlite::types::Value::from(r.expires_at_ns),
            rusqlite::types::Value::from(r.checked_at_ns),
            rusqlite::types::Value::from(request.to_vec()),
            rusqlite::types::Value::from(canonical(r)?),
        ])
    }
    struct TaskRow { receipt: A01TaskReceipt, request: Vec<u8> }
    fn read_row(row: &rusqlite::Row<'_>) -> Result<TaskRow,ProductError> {
        let request: Vec<u8> = row.get(39).map_err(stored_error)?;
        let bytes: Vec<u8> = row.get(40).map_err(stored_error)?;
        let receipt: A01TaskReceipt=decode(&bytes)?;
        validate_receipt(&receipt)?;
        for (i,expected) in values(&receipt,&request)?.iter().enumerate() {
            let actual: rusqlite::types::Value=row.get(i).map_err(stored_error)?;
            if &actual!=expected { return Err(evidence("A01 task row columns differ from receipt")); }
        }
        validate_envelope(&receipt,&request)?;
        Ok(TaskRow{receipt,request})
    }
    fn validate_receipt(r: &A01TaskReceipt) -> Result<(),ProductError> {
        validate_admission(&r.admission).map_err(|_|evidence("A01 task receipt admission invalid"))?;
        if !text(&r.bridge_permission.grant_id,256) || r.bridge_permission.grant_revision <= 0
            || !text(&r.bridge_permission.subject_id,256) || !text(&r.bridge_permission.source_instance_id,256)
            || (r.operation == A01TaskOperation::Authorize && r.bridge_permission.subject_id != r.actor_id)
            || r.bridge_permission.source_instance_id != r.admission.source_instance_id { return Err(evidence("A01 task receipt bridge permission invalid")); }
        validate_owner(&r.owner).map_err(|_|evidence("A01 task receipt owner invalid"))?;
        validate_service(&r.service).map_err(|_|evidence("A01 task receipt service invalid"))?;
        validate_key(&r.request_key,r.expected_revision).map_err(|_|evidence("A01 task receipt request key invalid"))?;
        if r.version!=1 || r.profile!=PROFILE || !text(&r.actor_id,256) || r.revision!=r.expected_revision.checked_add(1).ok_or_else(|| evidence("A01 task receipt revision exhausted"))?
            || r.task_kind!=TASK_KIND || r.task_bytes!=TASK_BYTES || r.task_digest!=task_digest()
            || r.authorized_at_ns<0 || r.expires_at_ns<=r.authorized_at_ns || r.checked_at_ns<r.authorized_at_ns
            || (r.operation==A01TaskOperation::Authorize && r.checked_at_ns!=r.authorized_at_ns) { return Err(evidence("A01 task receipt fields invalid")); }
        Ok(())
    }
    fn validate_envelope(r: &A01TaskReceipt, bytes: &[u8]) -> Result<(),ProductError> {
        match r.operation {
            A01TaskOperation::Authorize=> {
                let e: AuthorizeEnvelope=decode(bytes)?;
                if e.version!=1 || e.profile!=PROFILE || e.operation!=r.operation || e.actor_id!=r.actor_id
                    || e.request.request_key!=r.request_key || e.request.expected_revision!=r.expected_revision || e.request.admission!=r.admission
                    || e.request.bridge_permission!=r.bridge_permission || e.request.owner!=r.owner || e.request.service!=r.service || e.request.expires_at_ns!=r.expires_at_ns
                    || e.task_kind!=TASK_KIND || e.task_bytes!=TASK_BYTES || !context_matches(&e.context,&r.admission,&r.service) { return Err(evidence("A01 task envelope binding mismatch")); }
            }
            A01TaskOperation::Revoke=> {
                let e: RevokeEnvelope=decode(bytes)?;
                if e.version!=1 || e.profile!=PROFILE || e.operation!=r.operation || e.actor_id!=r.actor_id
                    || e.request.request_key!=r.request_key || e.request.expected_revision!=r.expected_revision
                    || e.request.admission!=r.admission || e.request.owner!=r.owner { return Err(evidence("A01 task envelope request mismatch")); }
            }
        }
        Ok(())
    }
    fn latest(tx: &Connection, a: &A01TaskAdmissionExpectation) -> Result<Option<TaskRow>,ProductError> {
        let mut stmt=tx.prepare(&format!("SELECT {COLUMNS} FROM product_a01_task_revisions_v1 WHERE order_id=?1 AND lifecycle=?2 ORDER BY revision DESC LIMIT 1"))?;
        let mut rows=stmt.query(params![a.order_id,a.lifecycle])?;
        rows.next()?.map(read_row).transpose()
    }
    fn local(tx: &Connection, actor: &str, key: &str) -> Result<Option<TaskRow>,ProductError> {
        let mut stmt=tx.prepare(&format!("SELECT {COLUMNS} FROM product_a01_task_revisions_v1 WHERE actor_id=?1 AND request_key=?2"))?;
        let mut rows=stmt.query(params![actor,key])?;
        rows.next()?.map(read_row).transpose()
    }
    fn replay(tx: &rusqlite::Transaction<'_>, principal: &ProductPrincipal, key: &str, request: &[u8]) -> Result<Option<A01TaskReceipt>,ProductError> {
        let Some(row)=local(tx,&principal.actor_id,key)? else { return Ok(None); };
        visible(tx,principal,&row.receipt.admission,false)?;
        if row.request!=request { return Err(ProductError::Invariant("A01 request conflict")); }
        Ok(Some(row.receipt))
    }
    fn insert(tx: &rusqlite::Transaction<'_>, receipt: &A01TaskReceipt, request: &[u8]) -> Result<(),ProductError> {
        validate_receipt(receipt)?; validate_envelope(receipt,request)?;
        let values=values(receipt,request)?;
        let placeholders=vec!["?";values.len()].join(",");
        one(tx.execute(&format!("INSERT INTO product_a01_task_revisions_v1({COLUMNS}) VALUES({placeholders})"),rusqlite::params_from_iter(values.iter()))?)
    }
    impl ProductStore {
        pub fn authorize_a01_task(&mut self, session_digest: &str, context: &ProductServiceContext, request: &A01AuthorizeTaskRequest) -> Result<A01TaskReceipt,ProductError> {
            authorize(self,session_digest,context,request,&mut TaskHooks::default())
        }
        /// Read-only construction from a unique, acked, durable intake receipt.
        /// Bridge permission must already have been issued independently.
        pub fn prepare_a01_authorization_request(
            &mut self, session_digest: &str, context: &ProductServiceContext,
            receipts: &[AdmissionReceipt], stable_issue_id: &str,
            bridge_permission: &A01BridgePermissionRef, request_key: &str,
            expires_at_ns: i64,
        ) -> Result<A01AuthorizeTaskRequest,ProductError> {
            if !self.open_mode.is_a01_task() { return Err(ProductError::ContractMismatch); }
            validate_key(request_key,0)?;
            if !text(stable_issue_id,256) { return Err(ProductError::InvalidConfig); }
            let tx = self.conn.unchecked_transaction()?;
            let now = clock_ns(self.clock.now())?;
            let principal = session(&tx,session_digest,now)?;
            let service = authority(&tx,context,now)?;
            permission(&tx,bridge_permission,&principal.actor_id,&context.source_instance_id)?;
            let mut selected: Option<AdmissionReceipt> = None;
            for receipt in receipts {
                let row: Option<(String,String,String,String)> = tx.query_row(
                    "SELECT i.issue_id,i.repository_id,p.source_instance_id,i.connector_id FROM product_orders_v1 o JOIN product_work_backlog_v1 b ON b.order_id=o.order_id AND b.lifecycle=o.lifecycle JOIN product_intake_inbox_v1 i USING(delivery_id) JOIN product_scope_order_provenance_v1 p ON p.order_id=o.order_id AND p.lifecycle=o.lifecycle WHERE o.order_id=?1 AND o.lifecycle=?2 AND o.admission_seq=?3 AND o.admission_hash=?4 AND i.state='acked' AND p.verified=1 AND p.admission_seq=o.admission_seq AND p.admission_hash=o.admission_hash",
                    params![receipt.order_id,receipt.lifecycle,receipt.sequence,receipt.event_hash],
                    |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(stored_error)?;
                let (issue,repo,source,connector) = row.ok_or_else(||evidence("A01 selection foreign receipt"))?;
                if issue==stable_issue_id && repo==context.repository_id && source==context.source_instance_id && connector==context.connector_id {
                    if selected.as_ref().is_some_and(|prior|prior!=receipt) { return Err(evidence("A01 selection ambiguous")); }
                    selected=Some(receipt.clone());
                }
            }
            let receipt=selected.ok_or_else(||evidence("A01 selection missing"))?;
            let view=require_order_capability_in(&tx,&principal,&receipt.order_id,ProductCapability::ReadOrder,true).map_err(scope_error)?;
            let (delivery_id,raw_kind,raw_payload_digest):(String,String,String)=tx.query_row(
                "SELECT b.delivery_id,o.task_kind,d.payload_digest FROM product_orders_v1 o JOIN product_work_backlog_v1 b ON b.order_id=o.order_id AND b.lifecycle=o.lifecycle JOIN product_order_payload_digests_v1 d USING(order_id) WHERE o.order_id=?1 AND o.lifecycle=?2",
                params![receipt.order_id,receipt.lifecycle],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(stored_error)?;
            let provider:String=tx.query_row("SELECT provider FROM product_scope_repositories_v1 WHERE repository_key=?1",[&view.repository_key],|r|r.get(0)).map_err(stored_error)?;
            let admission=A01TaskAdmissionExpectation {
                store:A01TaskStoreRef{installation_id:self.installation_id.clone(),database_id:self.database_id.clone()},
                order_id:receipt.order_id,lifecycle:receipt.lifecycle,delivery_id,admission_seq:receipt.sequence,admission_hash:receipt.event_hash,
                raw_kind,raw_payload_digest,source_instance_id:context.source_instance_id.clone(),source_generation:context.source_generation,
                source_instance_revision:context.source_instance_revision,connector_id:context.connector_id.clone(),connector_revision:context.connector_revision,
                repository_key:view.repository_key,repository_provider:provider,api_origin:context.api_origin.clone(),stable_repository_id:context.repository_id.clone(),
                api_owner:context.api_owner.clone(),api_repo:context.api_repo.clone(),
            };
            validate_admission(&admission)?;
            admission_check_for_request(&tx,&admission)?;
            let request=A01AuthorizeTaskRequest{bridge_permission:bridge_permission.clone(),request_key:request_key.into(),expected_revision:0,
                admission,owner:A01TaskOwnerExpectation{account_revision:principal.authority_revision,scope_revision:view.scope_revision,read_order_grant_revision:view.grant_revision},
                service,expires_at_ns};
            if expires_at_ns<=now { return Err(ProductError::InvalidConfig); }
            tx.commit()?;
            Ok(request)
        }
        pub fn revoke_a01_task(&mut self, session_digest: &str, request: &A01RevokeTaskRequest) -> Result<A01TaskReceipt,ProductError> {
            revoke(self,session_digest,request,&mut TaskHooks::default())
        }
        pub fn read_a01_task_receipt(&mut self, session_digest: &str, request_key: &str) -> Result<Option<A01TaskReceipt>,ProductError> {
            if !self.open_mode.is_a01_task() { return Err(ProductError::ContractMismatch); }
            validate_key(request_key,0)?;
            let tx=self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let result=(|| {
                let now=clock_ns(self.clock.now())?; let p=session(&tx,session_digest,now)?;
                let Some(row)=local(&tx,&p.actor_id,request_key)? else {return Ok(None);};
                match visible(&tx,&p,&row.receipt.admission,false) {
                    Ok(_)=>Ok(Some(row.receipt)), Err(ProductError::Invariant("A01 authorization unavailable"))=>Ok(None), Err(e)=>Err(e),
                }
            })();
            finish(tx,result,&mut TaskHooks::default())
        }
        pub fn read_a01_task_claim(&mut self, session_digest: &str, order_id: &str, lifecycle: i64) -> Result<Option<A01TaskClaimView>,ProductError> {
            if !self.open_mode.is_a01_task() { return Err(ProductError::ContractMismatch); }
            if !text(order_id,256) || lifecycle<=0 { return Err(ProductError::InvalidConfig); }
            let tx=self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let result=(|| {
                let now=clock_ns(self.clock.now())?; let p=session(&tx,session_digest,now)?;
                let v=require_order_capability_in(&tx,&p,order_id,ProductCapability::ReadOrder,false).map_err(scope_error)?;
                if v.lifecycle!=lifecycle { return Err(unavailable()); }
                snapshot(&tx,&A01TaskStoreRef{installation_id:self.installation_id.clone(),database_id:self.database_id.clone()},order_id,lifecycle)
            })();
            finish(tx,result,&mut TaskHooks::default())
        }
    }
    fn admission_check_for_request(tx: &rusqlite::Transaction<'_>, a: &A01TaskAdmissionExpectation) -> Result<(),ProductError> {
        let (_,state,attempts)=admission(tx,a)?;
        if state!="waiting_unsupported" || attempts!=0 { return Err(ineligible()); }
        queued(tx,&a.order_id)?;
        no_execution(tx,a)
    }
    pub(super) fn authorize(store: &mut ProductStore, key: &str, context: &ProductServiceContext, request: &A01AuthorizeTaskRequest, hooks: &mut TaskHooks<'_>) -> Result<A01TaskReceipt,ProductError> {
        if !store.open_mode.is_a01_task() { return Err(ProductError::ContractMismatch); }
        validate_admission(&request.admission)?; validate_owner(&request.owner)?; validate_service(&request.service)?; validate_key(&request.request_key,request.expected_revision)?;
        if request.expires_at_ns<0 { return Err(ProductError::InvalidConfig); }
        hooks.hit("before-lock")?;
        let tx=store.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result=(|| {
            let now=clock_ns(store.clock.now())?; hooks.hit("after-lock")?;
            let p=session(&tx,key,now)?;
            let envelope=AuthorizeEnvelope{version:1,profile:PROFILE.into(),operation:A01TaskOperation::Authorize,actor_id:p.actor_id.clone(),request:request.clone(),context:Context::from(context),task_kind:TASK_KIND.into(),task_bytes:TASK_BYTES.to_vec()};
            let bytes=canonical(&envelope)?;
            if let Some(receipt)=replay(&tx,&p,&request.request_key,&bytes)? {return Ok(receipt);}
            owner(&tx,&p,&request.admission,&request.owner,true)?;
            permission(&tx, &request.bridge_permission, &p.actor_id, &request.admission.source_instance_id)?;
            if request.expires_at_ns<=now { return Err(ProductError::InvalidConfig); }
            let previous=latest(&tx,&request.admission)?;
            if previous.as_ref().map_or(0,|v|v.receipt.revision)!=request.expected_revision {return Err(conflict());}
            if previous.as_ref().is_some_and(|v| v.receipt.operation!=A01TaskOperation::Revoke) {return Err(ineligible());}
            if previous.as_ref().is_some_and(|v| v.receipt.admission!=request.admission) {return Err(evidence("A01 authorization admission changed"));}
            if !context_matches(&envelope.context,&request.admission,&request.service) || authority(&tx,context,now)?!=request.service {return Err(unavailable());}
            let (_,state,attempts)=admission(&tx,&request.admission)?;
            if state!="waiting_unsupported" || attempts!=0 {return Err(ineligible());}
            queued(&tx, &request.admission.order_id)?;
            no_execution(&tx,&request.admission)?;
            let r=A01TaskReceipt{bridge_permission:request.bridge_permission.clone(),version:1,profile:PROFILE.into(),operation:A01TaskOperation::Authorize,actor_id:p.actor_id,request_key:request.request_key.clone(),expected_revision:request.expected_revision,revision:request.expected_revision.checked_add(1).ok_or(ProductError::InvalidConfig)?,admission:request.admission.clone(),owner:request.owner.clone(),service:request.service.clone(),task_kind:TASK_KIND.into(),task_bytes:TASK_BYTES.to_vec(),task_digest:task_digest(),authorized_at_ns:now,expires_at_ns:request.expires_at_ns,checked_at_ns:now};
            insert(&tx,&r,&bytes)?; hooks.hit("authorize-insert")?;
            one(tx.execute("UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id=?1 AND lifecycle=?2 AND delivery_id=?3 AND state='waiting_unsupported' AND attempts=0",params![r.admission.order_id,r.admission.lifecycle,r.admission.delivery_id])?)?;
            hooks.hit("authorize-ready")?; Ok(r)
        })();
        finish(tx,result,hooks)
    }
    pub(super) fn revoke(store: &mut ProductStore, key: &str, request: &A01RevokeTaskRequest, hooks: &mut TaskHooks<'_>) -> Result<A01TaskReceipt,ProductError> {
        if !store.open_mode.is_a01_task() { return Err(ProductError::ContractMismatch); }
        validate_admission(&request.admission)?; validate_owner(&request.owner)?; validate_key(&request.request_key,request.expected_revision)?;
        hooks.hit("before-lock")?;
        let tx=store.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result=(|| {
            let now=clock_ns(store.clock.now())?; hooks.hit("after-lock")?;
            let p=session(&tx,key,now)?;
            let bytes=canonical(&RevokeEnvelope{version:1,profile:PROFILE.into(),operation:A01TaskOperation::Revoke,actor_id:p.actor_id.clone(),request:request.clone()})?;
            if let Some(receipt)=replay(&tx,&p,&request.request_key,&bytes)? {return Ok(receipt);}
            owner(&tx,&p,&request.admission,&request.owner,false)?;
            let previous=latest(&tx,&request.admission)?.ok_or_else(conflict)?.receipt;
            if previous.revision!=request.expected_revision {return Err(conflict());}
            if previous.operation!=A01TaskOperation::Authorize {return Err(ineligible());}
            if previous.admission!=request.admission {return Err(evidence("A01 revocation admission changed"));}
            let mut r=previous;
            r.operation=A01TaskOperation::Revoke; r.actor_id=p.actor_id; r.request_key=request.request_key.clone(); r.expected_revision=request.expected_revision;
            r.revision=request.expected_revision.checked_add(1).ok_or(ProductError::InvalidConfig)?; r.owner=request.owner.clone(); r.checked_at_ns=now;
            let (_,state,attempts)=admission(&tx,&r.admission)?;
            if state=="ready" { no_execution(&tx,&r.admission)?; }
            else if state=="claimed" && attempts==1 {
                snapshot(&tx,&r.admission.store,&r.admission.order_id,r.admission.lifecycle)?.ok_or_else(|| evidence("A01 revocation claim snapshot missing"))?;
            } else {return Err(ineligible());}
            insert(&tx,&r,&bytes)?; hooks.hit("revoke-insert")?;
            if state=="ready" {
                one(tx.execute("UPDATE product_work_backlog_v1 SET state='waiting_unsupported' WHERE order_id=?1 AND lifecycle=?2 AND state='ready' AND attempts=0",params![r.admission.order_id,r.admission.lifecycle])?)?;
            } else if state!="claimed" {return Err(ineligible());}
            hooks.hit("revoke-backlog")?; Ok(r)
        })();
        finish(tx,result,hooks)
    }

    const SNAPSHOT_COLUMNS: &str = "order_id,lifecycle,task_revision,delivery_id,worker_id,epoch,fence,claimed_at_ns,lease_until_ns,lease_until_text,source_revision,payload_digest,service_id,service_revision,credential_revision,source_instance_id";
    fn snapshot(tx: &rusqlite::Transaction<'_>, store: &A01TaskStoreRef, order: &str, lifecycle: i64) -> Result<Option<A01TaskClaimView>,ProductError> {
        let v=tx.query_row(&format!("SELECT {SNAPSHOT_COLUMNS} FROM product_a01_claim_snapshots_v1 WHERE order_id=?1 AND lifecycle=?2"),params![order,lifecycle],|r|Ok(A01TaskClaimView{
            store:store.clone(),order_id:r.get(0)?,lifecycle:r.get(1)?,task_revision:r.get(2)?,delivery_id:r.get(3)?,worker_id:r.get(4)?,epoch:r.get(5)?,fence:r.get(6)?,claimed_at_ns:r.get(7)?,lease_until_ns:r.get(8)?,lease_until_text:r.get(9)?,source_revision:r.get(10)?,payload_digest:r.get(11)?,service_id:r.get(12)?,service_revision:r.get(13)?,credential_revision:r.get(14)?,source_instance_id:r.get(15)?
        })).optional().map_err(stored_error)?;
        if let Some(v)=&v { validate_snapshot(tx,v)?; }
        Ok(v)
    }
    fn validate_snapshot(tx: &rusqlite::Transaction<'_>, v: &A01TaskClaimView) -> Result<(),ProductError> {
        if !text(&v.worker_id,128) || v.epoch!=1 || v.fence<=0 || v.claimed_at_ns<0 || v.lease_until_ns<=v.claimed_at_ns
            || time_ns(&v.lease_until_text)?!=v.lease_until_ns || datetime(v.lease_until_ns)?.to_rfc3339_opts(chrono::SecondsFormat::Nanos,true)!=v.lease_until_text { return Err(evidence("A01 claim snapshot fields or lease text invalid")); }
        let mut stmt=tx.prepare(&format!("SELECT {COLUMNS} FROM product_a01_task_revisions_v1 WHERE order_id=?1 AND lifecycle=?2 AND revision=?3"))?;
        let mut rows=stmt.query(params![v.order_id,v.lifecycle,v.task_revision])?;
        let r=read_row(rows.next()?.ok_or_else(|| evidence("A01 claim snapshot task receipt missing"))?)?.receipt;
        if r.operation!=A01TaskOperation::Authorize || v.store!=r.admission.store || v.delivery_id!=r.admission.delivery_id
            || v.source_revision!=r.admission.connector_revision || v.payload_digest!=r.admission.raw_payload_digest
            || v.service_id!=r.service.service_id || v.service_revision!=r.service.service_revision || v.credential_revision!=r.service.credential_revision
            || v.source_instance_id!=r.admission.source_instance_id || v.claimed_at_ns<r.authorized_at_ns || v.claimed_at_ns>=r.expires_at_ns
            || v.lease_until_ns>r.expires_at_ns.min(r.service.service_lease_until_ns).min(r.service.credential_expires_at_ns) { return Err(evidence("A01 claim snapshot task binding or lease cap invalid")); }
        // Historical equalities deliberately do not require latest authorization,
        // live deadlines or mutable account/source/service revisions.
        let equal: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM product_work_claims_v1 c JOIN product_work_backlog_v1 b USING(order_id,lifecycle) JOIN product_service_work_bindings_v1 w USING(order_id,lifecycle) WHERE c.order_id=?1 AND c.lifecycle=?2 AND c.owner_id=?3 AND c.epoch=?4 AND c.fence=?5 AND ((c.state='claimed' AND b.state='claimed' AND c.lease_until=?6) OR (c.state='completed' AND b.state='completed' AND EXISTS(SELECT 1 FROM product_review_runs_v1 rr JOIN product_review_work_bindings_v1 rw ON rw.order_id=rr.order_id AND rw.attempt=rr.attempt AND rw.run_id=rr.run_id JOIN product_review_host_results_v1 hr ON hr.order_id=rr.order_id AND hr.attempt=rr.attempt AND rr.run_id=hr.order_id||':'||hr.attempt||':'||hr.run_id||':'||hr.activation WHERE rr.order_id=c.order_id AND rr.product_lifecycle=c.lifecycle AND rr.status IN ('passed','failed','resolved') AND hr.result_json=rr.result_json AND rw.worker_id=c.owner_id AND rw.claim_epoch=c.epoch AND rw.claim_fence=c.fence AND rw.delivery_id=b.delivery_id AND rw.source_revision=c.source_revision AND rw.payload_digest=c.payload_digest))) AND c.source_revision=?7 AND c.payload_digest=?8 AND b.delivery_id=?9 AND b.attempts=1 AND w.service_id=?10 AND w.service_revision=?11 AND w.credential_revision=?12 AND w.source_instance_id=?13)",params![v.order_id,v.lifecycle,v.worker_id,v.epoch,v.fence,v.lease_until_text,v.source_revision,v.payload_digest,v.delivery_id,v.service_id,v.service_revision,v.credential_revision,v.source_instance_id],|r|r.get(0))?;
        if !equal {return Err(evidence("A01 claim snapshot current row mismatch"));}
        let next: i64=tx.query_row("SELECT next FROM product_fence_counter_v1 WHERE singleton=1",[],|r|r.get(0)).map_err(stored_error)?;
        if next<v.fence {return Err(evidence("A01 claim snapshot fence counter invalid"));}
        Ok(())
    }
    pub(super) fn claim(store: &mut ProductStore, context: &ProductServiceContext, worker: &str, lease: chrono::Duration, hooks: &mut TaskHooks<'_>) -> Result<ProductWorkClaimResult,ProductError> {
        if !store.open_mode.is_a01_task() {return Err(ProductError::ContractMismatch);}
        if !text(worker,128) || lease<=chrono::Duration::zero() || lease>chrono::Duration::hours(1) {return Err(ProductError::InvalidConfig);}
        let duration=lease.num_nanoseconds().ok_or(ProductError::InvalidConfig)?;
        hooks.hit("before-lock")?;
        let tx=store.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result=(|| {
            let now=clock_ns(store.clock.now())?; hooks.hit("after-lock")?;
            let live=authority(&tx,context,now)?;
            let mut stmt=tx.prepare(&format!("SELECT {cols} FROM product_a01_task_revisions_v1 r JOIN product_work_backlog_v1 b ON b.order_id=r.order_id AND b.lifecycle=r.lifecycle WHERE b.state='ready' AND r.operation='authorize' AND r.service_id=?1 AND r.source_instance_id=?2 AND r.revision=(SELECT max(revision) FROM product_a01_task_revisions_v1 x WHERE x.order_id=r.order_id AND x.lifecycle=r.lifecycle) AND NOT EXISTS(SELECT 1 FROM product_work_claims_v1 c WHERE c.order_id=r.order_id AND c.lifecycle=r.lifecycle) AND NOT EXISTS(SELECT 1 FROM product_a01_claim_snapshots_v1 c WHERE c.order_id=r.order_id AND c.lifecycle=r.lifecycle)",cols=COLUMNS.split(',').map(|c|format!("r.{c}")).collect::<Vec<_>>().join(",")))?;
            let mut rows=stmt.query(params![context.service_id,context.source_instance_id])?;
            let mut candidates=Vec::new();
            while let Some(row)=rows.next()? {
                let row=read_row(row)?;
                let a=&row.receipt.admission;
                let available: String=tx.query_row("SELECT available_at FROM product_work_backlog_v1 WHERE order_id=?1 AND lifecycle=?2",params![a.order_id,a.lifecycle],|r|r.get(0)).map_err(stored_error)?;
                let available=time_ns(&available)?;
                if available<=now && row.receipt.authorized_at_ns<=now && now<row.receipt.expires_at_ns {candidates.push((available,row.receipt));}
            }
            drop(rows); drop(stmt);
            candidates.sort_by(|a,b|a.0.cmp(&b.0).then_with(||a.1.admission.order_id.cmp(&b.1.admission.order_id)));
            let Some((_,r))=candidates.into_iter().next() else {return Ok(ProductWorkClaimResult::Empty);};
            if live!=r.service || !context_matches(&Context::from(context),&r.admission,&r.service) {return Err(unavailable());}
            issuing_owner(&tx,&r)?;
            permission(&tx,&r.bridge_permission,&r.actor_id,&r.admission.source_instance_id)?;
            let (payload,state,attempts)=admission(&tx,&r.admission)?;
            if state!="ready" || attempts!=0 {return Err(ineligible());}
            queued(&tx, &r.admission.order_id)?;
            no_execution(&tx,&r.admission)?;
            let attempts=attempts.checked_add(1).ok_or_else(|| evidence("A01 claim attempt counter exhausted"))?;
            let old:i64=tx.query_row("SELECT next FROM product_fence_counter_v1 WHERE singleton=1",[],|r|r.get(0)).map_err(stored_error)?;
            if !(0..i64::MAX).contains(&old) {return Err(evidence("A01 claim fence counter out of range"));}
            let fence=old.checked_add(1).ok_or_else(|| evidence("A01 claim fence counter exhausted"))?;
            let end=now.checked_add(duration).ok_or(ProductError::InvalidConfig)?.min(r.expires_at_ns).min(live.service_lease_until_ns).min(live.credential_expires_at_ns);
            if end<=now {return Err(unavailable());}
            let until=datetime(end)?;
            let until_text=until.to_rfc3339_opts(chrono::SecondsFormat::Nanos,true);
            if time_ns(&until_text)?!=end {return Err(evidence("A01 claim lease timestamp round-trip mismatch"));}
            let a=&r.admission;
            one(tx.execute("UPDATE product_fence_counter_v1 SET next=?1 WHERE singleton=1 AND next=?2",params![fence,old])?)?; hooks.hit("fence")?;
            one(tx.execute("INSERT INTO product_work_claims_v1(order_id,lifecycle,owner_id,epoch,fence,state,lease_until,source_revision,payload_digest) VALUES(?1,?2,?3,1,?4,'claimed',?5,?6,?7)",params![a.order_id,a.lifecycle,worker,fence,until_text,a.connector_revision,a.raw_payload_digest])?)?; hooks.hit("claim")?;
            one(tx.execute("INSERT INTO product_service_work_bindings_v1(order_id,lifecycle,service_id,service_revision,credential_revision,source_instance_id) VALUES(?1,?2,?3,?4,?5,?6)",params![a.order_id,a.lifecycle,live.service_id,live.service_revision,live.credential_revision,a.source_instance_id])?)?; hooks.hit("binding")?;
            one(tx.execute("UPDATE product_work_backlog_v1 SET state='claimed',attempts=?1 WHERE order_id=?2 AND lifecycle=?3 AND delivery_id=?4 AND state='ready' AND attempts=0",params![attempts,a.order_id,a.lifecycle,a.delivery_id])?)?; hooks.hit("backlog")?;
            one(tx.execute(&format!("INSERT INTO product_a01_claim_snapshots_v1({SNAPSHOT_COLUMNS}) VALUES(?1,?2,?3,?4,?5,1,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)"),params![a.order_id,a.lifecycle,r.revision,a.delivery_id,worker,fence,now,end,until_text,a.connector_revision,a.raw_payload_digest,live.service_id,live.service_revision,live.credential_revision,a.source_instance_id])?)?; hooks.hit("snapshot")?;
            Ok(ProductWorkClaimResult::Claimed(ProductWorkClaim{order_id:a.order_id.clone(),lifecycle:a.lifecycle,delivery_id:a.delivery_id.clone(),worker_id:worker.into(),epoch:1,fence,source_revision:a.connector_revision,payload_digest:a.raw_payload_digest.clone(),service_binding:Some(foundry_exec::ReviewServiceBinding{service_id:live.service_id,service_revision:live.service_revision,credential_revision:live.credential_revision,source_instance_id:a.source_instance_id.clone()}),lease_until:until,task_kind:a.raw_kind.clone(),payload_snapshot:payload}))
        })();
        finish(tx,result,hooks)
    }
    pub(super) fn validate_a01_task_connection(tx: &rusqlite::Transaction<'_>, config: &ProductConfig) -> Result<(),ProductError> {
        validate_a01_pragmas(tx)?;
        validate_a01_task_contract(tx,config)
    }
    fn validate_a01_task_contract(tx: &rusqlite::Transaction<'_>, config: &ProductConfig) -> Result<(),ProductError> {
        validate_a01_profile_contract(tx,config,StoreOpenMode::A01TaskCreate)?;
        let profile:(i64,i64,i64,String,String,String)=tx.query_row("SELECT count(*),singleton,version,profile,installation_id,database_id FROM product_a01_profile_v1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).map_err(a01_contract_error)?;
        if profile!=(1,1,1,PROFILE.into(),config.installation_id.clone(),config.database_id.clone()) {return Err(ProductError::ContractMismatch);}
        let result=(|| {
            let mut statement=tx.prepare(&format!("SELECT {COLUMNS} FROM product_a01_task_revisions_v1 ORDER BY order_id,lifecycle,revision"))?;
            let mut rows=statement.query([])?;
            let mut previous:Option<A01TaskReceipt>=None;
            while let Some(row)=rows.next()? {
                let r=read_row(row)?.receipt;
                admission(tx,&r.admission)?;
                let prior=previous.as_ref().filter(|p|p.admission.order_id==r.admission.order_id && p.admission.lifecycle==r.admission.lifecycle);
                if r.expected_revision!=prior.map_or(0,|p|p.revision) { return Err(evidence("A01 task revision history discontinuity: r.expected_revision")); }
        if r.operation==prior.map_or(A01TaskOperation::Revoke,|p|p.operation) { return Err(evidence("A01 task revision history discontinuity: r.operation")); }
                if let Some(p)=prior {
                    if p.admission!=r.admission {return Err(evidence("A01 task history admission changed"));}
                    if r.operation==A01TaskOperation::Revoke && (r.service!=p.service || r.task_digest!=p.task_digest || r.authorized_at_ns!=p.authorized_at_ns || r.expires_at_ns!=p.expires_at_ns) {return Err(evidence("A01 task revocation history changed authorization"));}
                }
                previous=Some(r);
            }
            let mut stmt=tx.prepare("SELECT order_id,lifecycle FROM product_a01_claim_snapshots_v1")?;
            let keys=stmt.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let store=A01TaskStoreRef{installation_id:config.installation_id.clone(),database_id:config.database_id.clone()};
            for (order,lifecycle) in keys {snapshot(tx,&store,&order,lifecycle)?.ok_or_else(|| evidence("A01 claim census snapshot missing"))?;}
            let orphan:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM product_work_claims_v1 c WHERE NOT EXISTS(SELECT 1 FROM product_a01_claim_snapshots_v1 s WHERE s.order_id=c.order_id AND s.lifecycle=c.lifecycle)) OR EXISTS(SELECT 1 FROM product_service_work_bindings_v1 c WHERE NOT EXISTS(SELECT 1 FROM product_a01_claim_snapshots_v1 s WHERE s.order_id=c.order_id AND s.lifecycle=c.lifecycle))",[],|r|r.get(0))?;
            if orphan {return Err(evidence("A01 claim snapshot orphaned"));}
            validate_binding_history_in(tx)?;
            // Every ready/claimed backlog must be accounted for by this profile;
            // history is inspected without asking mutable authority to be live.
            let mut stmt=tx.prepare("SELECT order_id,lifecycle,state,attempts FROM product_work_backlog_v1")?;
            let backlogs=stmt.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            for (order,lifecycle,state,attempts) in backlogs {
                let mut stmt=tx.prepare(&format!("SELECT {COLUMNS} FROM product_a01_task_revisions_v1 WHERE order_id=?1 AND lifecycle=?2 ORDER BY revision DESC LIMIT 1"))?;
                let mut rows=stmt.query(params![order,lifecycle])?;
                let latest=rows.next()?.map(read_row).transpose()?;
                let snap=snapshot(tx,&store,&order,lifecycle)?;
                match (latest,snap) {
                    (None,None) if state=="waiting_unsupported" && attempts==0=>{},
                    (Some(row),None) if attempts==0 && state==if row.receipt.operation==A01TaskOperation::Authorize {"ready"} else {"waiting_unsupported"}=>{},
                    (Some(row),Some(snap)) if matches!(state.as_str(),"claimed"|"completed") && attempts==1 && (row.receipt.revision==snap.task_revision || (row.receipt.operation==A01TaskOperation::Revoke && snap.task_revision.checked_add(1)==Some(row.receipt.revision)))=>{},
                    _=>return Err(evidence("A01 task backlog and claim census mismatch")),
                }
            }
            Ok(())
        })();
        result.map_err(|e|match e {ProductError::Invariant(_)=>ProductError::ContractMismatch,other=>other})
    }


    #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct A01ProducerReservation {
        pub task: A01TaskReceipt,
        pub claim: A01TaskClaimView,
        pub request_key: String,
        pub attempt: u64,
        pub run: u64,
        pub activation: u64,
        pub predecessor_seq: i64,
        pub sequence: i64,
        pub event_hash: String,
    }

    const REAL_PREFIX: &str = "foundry-a01-real-output/v1\n";
    #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct A01RealManifest {
        pub order_id: String,
        pub attempt: u32,
        pub task: String,
        pub profile: String,
        pub base: String,
        pub fix_context: bool,
        pub context: Vec<String>,
    }
    #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct A01RealEvidence {
        pub manifest_sha256: String,
        pub die_output_sha256: String,
        pub process_log_sha256: String,
        pub session_record_sha256: String,
        pub exit_code: i32,
    }
    #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct A01RealOutput {
        pub version: u32,
        pub kind: String,
        pub task_digest: String,
        pub task_receipt_sha256: String,
        pub reservation_sha256: String,
        pub base_commit: String,
        pub result_commit: String,
        pub manifest: A01RealManifest,
        pub evidence: A01RealEvidence,
    }
    pub fn a01_real_reservation_sha256(r: &A01ProducerReservation) -> Result<String, ProductError> {
        bound_digest("foundry-a01-real-reservation-v1", r)
    }
    pub fn a01_real_task_receipt_sha256(r: &A01TaskReceipt) -> Result<String, ProductError> {
        bound_digest("foundry-a01-real-task-receipt-v1", r)
    }
    fn bound_digest<T: Serialize>(domain: &str, value: &T) -> Result<String, ProductError> {
        let bytes = canonical(value)?;
        let json = std::str::from_utf8(&bytes).map_err(|_| evidence("A01 real JSON invalid"))?;
        Ok(crate::product_scope::framed_hash(domain, &[json]))
    }
    pub fn a01_real_inputs(r: &A01ProducerReservation) -> Result<std::collections::BTreeMap<String,String>, ProductError> {
        if r.task.task_kind != TASK_KIND || r.task.task_bytes != TASK_BYTES || r.task.task_digest != task_digest() {
            return Err(evidence("A01 real fixed task mismatch"));
        }
        Ok([
            ("action", "create-a01-proof-v1".to_owned()),
            ("proof_path", ".foundry/a01-proof.txt".to_owned()),
            ("task_kind", r.task.task_kind.clone()),
            ("task_utf8", String::from_utf8(r.task.task_bytes.clone()).map_err(|_| evidence("A01 real task UTF-8 invalid"))?),
            ("task_digest", r.task.task_digest.clone()),
            ("reservation_sha256", a01_real_reservation_sha256(r)?),
        ].into_iter().map(|(k,v)|(k.to_owned(),v)).collect())
    }
    pub fn a01_real_manifest(r: &A01ProducerReservation, base: &str) -> Result<A01RealManifest, ProductError> {
        if !git_oid(base) { return Err(evidence("A01 real base invalid")); }
        Ok(A01RealManifest {
            order_id: format!("a01p-{}", a01_real_reservation_sha256(r)?), attempt: 0,
            task: "producer:a01-fixed-v1".into(), profile: "coding".into(), base: base.into(), fix_context: false,
            context: a01_real_inputs(r)?.iter().map(|(k,v)|format!("{k}={v}")).collect(),
        })
    }
    fn git_oid(value: &str) -> bool {
        matches!(value.len(),40|64) && value.bytes().all(|b|b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }
    fn validate_real(r: &A01ProducerReservation, o: &A01RealOutput) -> Result<(),ProductError> {
        if o.version != 1 || o.kind != "real-git" || o.task_digest != r.task.task_digest
            || o.task_receipt_sha256 != a01_real_task_receipt_sha256(&r.task)?
            || o.reservation_sha256 != a01_real_reservation_sha256(r)?
            || !git_oid(&o.base_commit) || !git_oid(&o.result_commit)
            || o.base_commit.len() != o.result_commit.len() || o.base_commit == o.result_commit
            || o.manifest != a01_real_manifest(r, &o.base_commit)? || o.evidence.exit_code != 0
            || [&o.evidence.manifest_sha256, &o.evidence.die_output_sha256, &o.evidence.process_log_sha256,
                &o.evidence.session_record_sha256].iter().any(|s| !digest(s)) {
            return Err(evidence("A01 real output binding invalid"));
        }
        Ok(())
    }
    pub fn encode_a01_real_output(r: &A01ProducerReservation, o: &A01RealOutput) -> Result<String,ProductError> {
        validate_real(r,o)?;
        let bytes = canonical(o)?;
        let json = std::str::from_utf8(&bytes).map_err(|_| evidence("A01 real output UTF-8 invalid"))?;
        let output = format!("{REAL_PREFIX}{json}");
        output_bounds(&output)?;
        Ok(output)
    }
    fn output_bounds(output: &str) -> Result<(),ProductError> {
        if output.is_empty() || output.len()>4096 || output.as_bytes().contains(&0) { return Err(ProductError::InvalidConfig); }
        Ok(())
    }
    /// Validates canonical bytes and reservation bindings, not physical execution.
    pub fn decode_a01_real_output(r: &A01ProducerReservation, output: &str) -> Result<A01RealOutput,ProductError> {
        output_bounds(output)?;
        let json = output.strip_prefix(REAL_PREFIX).ok_or_else(||evidence("A01 real output framing invalid"))?;
        let value: A01RealOutput = decode(json.as_bytes())?;
        validate_real(r,&value)?;
        Ok(value)
    }
    fn output_commits(r: &A01ProducerReservation, output: &str) -> Result<(String,String),ProductError> {
        output_bounds(output)?;
        if output.starts_with("foundry-a01-real-output/") {
            let value = decode_a01_real_output(r,output)?;
            Ok((value.base_commit,value.result_commit))
        } else { Ok(commits(r,output)) }
    }
    #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct A01AcceptedProducerOutput {
        pub reservation: A01ProducerReservation,
        pub output: String,
        pub base_commit: String,
        pub result_commit: String,
        pub intent: foundry_exec::ExecutionIntent,
        pub sequence: i64,
        pub event_hash: String,
    }
    #[derive(Debug, thiserror::Error)]
    pub enum A01ProducerError {
        #[error("A01 producer operation rejected")]
        Product(#[from] ProductError),
        #[error("A01 producer commit uncertain")]
        CommitUncertain,
    }
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ProducerStart {
        kind: String, task: A01TaskReceipt, claim: A01TaskClaimView,
        request_key: String, attempt: u64, run: u64, activation: u64, predecessor_seq: i64,
    }
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ProducerOutput {
        kind: String, task: A01TaskReceipt, claim: A01TaskClaimView,
        reservation: A01ProducerReservation, output: String,
        base_commit: String, result_commit: String, intent: foundry_exec::ExecutionIntent,
    }
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PassedBridge {
        kind: String, task: A01TaskReceipt, claim: A01TaskClaimView,
        output_sequence: i64, output_event_hash: String,
        host_key: foundry_exec::HostKey, host_result_digest: String, predecessor_seq: i64,
    }
    fn queued(tx: &rusqlite::Transaction<'_>, order: &str) -> Result<(),ProductError> {
        if verified_order_state_in(tx,order)?.2 != "queued" { return Err(ineligible()); }
        Ok(())
    }
    fn permission(tx: &rusqlite::Transaction<'_>, p: &A01BridgePermissionRef, actor: &str, source: &str) -> Result<(),ProductError> {
        if p.subject_id != actor || p.source_instance_id != source || p.grant_revision <= 0 { return Err(unavailable()); }
        let exact: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM product_scope_grants_v1 WHERE subject_id=?1 AND source_instance_id=?2 AND capability='a01_bridge_passed' AND enabled=1 AND grant_id=?3 AND grant_revision=?4)",params![actor,source,p.grant_id,p.grant_revision],|r|r.get(0))?;
        if !exact { return Err(unavailable()); } Ok(())
    }
    impl ProductStore {
        pub fn read_a01_bridge_permission(&mut self, session_digest: &str, source_instance_id: &str) -> Result<Option<A01BridgePermissionRef>,ProductError> {
            if !self.open_mode.is_a01_task() { return Err(ProductError::ContractMismatch); }
            if !text(source_instance_id,256) { return Err(ProductError::InvalidConfig); }
            let tx=self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let p=session(&tx,session_digest,clock_ns(self.clock.now())?)?;
            let value=tx.query_row("SELECT grant_id,grant_revision,subject_id,source_instance_id FROM product_scope_grants_v1 WHERE subject_id=?1 AND source_instance_id=?2 AND capability='a01_bridge_passed' AND enabled=1",params![p.actor_id,source_instance_id],|r|Ok(A01BridgePermissionRef{grant_id:r.get(0)?,grant_revision:r.get(1)?,subject_id:r.get(2)?,source_instance_id:r.get(3)?})).optional().map_err(stored_error)?;
            tx.commit()?; Ok(value)
        }
        pub fn reserve_a01_producer(&mut self, service: &ProductServiceContext, claim: &ProductWorkClaim, task_revision: i64, request_key: &str) -> Result<A01ProducerReservation,A01ProducerError> {
            if !self.open_mode.is_a01_task() { return Err(ProductError::ContractMismatch.into()); }
            validate_key(request_key,0)?;
            let tx=self.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(ProductError::from)?;
            let now=clock_ns(self.clock.now())?;
            let (task,snap)=task_claim(&tx,&claim.order_id,claim.lifecycle)?;
            if task.revision!=task_revision { return Err(conflict().into()); }
            live(&tx,&task,&snap,service,claim,now)?;
            let current=replayed(&tx,&claim.order_id)?;
            if current.order.state().to_string()!="queued" { return Err(ineligible().into()); }
            let attempt=u64::from(current.order.attempt().max(1));
            let start=ProducerStart{kind:"a01-producer-start-v1".into(),task:task.clone(),claim:snap.clone(),request_key:request_key.into(),attempt,run:1,activation:1,predecessor_seq:current.last_seq};
            let (sequence,event_hash,actual_attempt)=append(&tx,&claim.order_id,claim.lifecycle,OrderEvent::DieStarted,"a01-producer-start-v1",&canonical(&start)?,datetime(now)?)?;
            if actual_attempt!=attempt {return Err(evidence("A01 producer reservation attempt mismatch").into());}
            let r=A01ProducerReservation{task,claim:snap,request_key:request_key.into(),attempt,run:1,activation:1,predecessor_seq:current.last_seq,sequence,event_hash};
            tx.commit().map_err(|_|A01ProducerError::CommitUncertain)?; Ok(r)
        }
        pub fn accept_a01_producer_output(&mut self, service: &ProductServiceContext, claim: &ProductWorkClaim, reservation: &A01ProducerReservation, output: &str) -> Result<A01AcceptedProducerOutput,A01ProducerError> {
            if !self.open_mode.is_a01_task() { return Err(ProductError::ContractMismatch.into()); }
            if output.is_empty() || output.len()>4096 || output.as_bytes().contains(&0) {return Err(ProductError::InvalidConfig.into());}
            let tx=self.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(ProductError::from)?;
            let now=clock_ns(self.clock.now())?;
            validate_reservation(&tx,reservation)?;
            live(&tx,&reservation.task,&reservation.claim,service,claim,now)?;
            let current=replayed(&tx,&claim.order_id)?;
            if current.last_seq!=reservation.sequence || current.order.state().to_string()!="working" {return Err(ineligible().into());}
            let (base_commit,result_commit)=output_commits(reservation,output)?;
            let intent=foundry_exec::ExecutionIntent{order:foundry_exec::OrderId(claim.order_id.clone()),attempt:foundry_exec::Attempt(reservation.attempt),run:foundry_exec::RunId(reservation.run),activation:reservation.activation,generation:foundry_exec::Generation(0),epoch:foundry_exec::Epoch(reservation.claim.epoch as u64),external_ref:format!("review:{}:{}:{}..{}",claim.order_id,reservation.attempt,base_commit,result_commit),recorded_at:datetime(now)?};
            let original=ProducerOutput{kind:"a01-producer-output-v1".into(),task:reservation.task.clone(),claim:reservation.claim.clone(),reservation:reservation.clone(),output:output.into(),base_commit:base_commit.clone(),result_commit:result_commit.clone(),intent:intent.clone()};
            let (sequence,event_hash,_)=append(&tx,&claim.order_id,claim.lifecycle,OrderEvent::DieSucceeded{attempt:reservation.attempt.try_into().map_err(|_|evidence("A01 producer output attempt out of range"))?},"a01-producer-output-v1",&canonical(&original)?,datetime(now)?)?;
            let accepted=A01AcceptedProducerOutput{reservation:reservation.clone(),output:output.into(),base_commit,result_commit,intent,sequence,event_hash};
            one(tx.execute("INSERT INTO product_review_redrive_v1(order_id,run_id,payload,artifact_json) VALUES(?1,?2,'a01-producer-output-v1',?3)",params![claim.order_id,journal(reservation),String::from_utf8(canonical(&accepted)?).map_err(|_|evidence("A01 producer output receipt UTF-8 invalid"))?]).map_err(ProductError::from)?)?;
            tx.commit().map_err(|_|A01ProducerError::CommitUncertain)?; Ok(accepted)
        }
        pub fn read_a01_producer_output(&mut self, service: &ProductServiceContext, claim: &ProductWorkClaim, reservation: &A01ProducerReservation) -> Result<Option<A01AcceptedProducerOutput>,A01ProducerError> {
            if !self.open_mode.is_a01_task() {return Err(ProductError::ContractMismatch.into());}
            let tx=self.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(ProductError::from)?;
            let now=clock_ns(self.clock.now())?;
            validate_reservation(&tx,reservation)?;
            live(&tx,&reservation.task,&reservation.claim,service,claim,now)?;
            let accepted=output_for(&tx,&claim.order_id)?;
            if accepted.as_ref().is_some_and(|a| a.reservation!=*reservation) {return Err(evidence("A01 producer output reservation mismatch").into());}
            tx.commit().map_err(|_|A01ProducerError::CommitUncertain)?; Ok(accepted)
        }
    }
    fn task_claim(tx: &rusqlite::Transaction<'_>, order: &str, lifecycle: i64) -> Result<(A01TaskReceipt,A01TaskClaimView),ProductError> {
        let store=tx.query_row("SELECT installation_id,database_id FROM product_meta_v1 WHERE singleton=1",[],|r|Ok(A01TaskStoreRef{installation_id:r.get(0)?,database_id:r.get(1)?}))?;
        let snap=snapshot(tx,&store,order,lifecycle)?.ok_or_else(|| evidence("A01 producer claim snapshot missing"))?;
        let mut st=tx.prepare(&format!("SELECT {COLUMNS} FROM product_a01_task_revisions_v1 WHERE order_id=?1 AND lifecycle=?2 AND revision=?3"))?;
        let mut rows=st.query(params![order,lifecycle,snap.task_revision])?;
        let task=read_row(rows.next()?.ok_or_else(|| evidence("A01 producer task receipt missing"))?)?.receipt;
        admission(tx,&task.admission)?;
        Ok((task,snap))
    }
    fn live(tx: &rusqlite::Transaction<'_>, task: &A01TaskReceipt, snap: &A01TaskClaimView, service: &ProductServiceContext, claim: &ProductWorkClaim, now: i64) -> Result<(),ProductError> {
        validate_respective_claim(task,snap,claim)?;
        let latest=latest(tx,&task.admission)?.ok_or_else(|| evidence("A01 producer latest task receipt missing"))?.receipt;
        if latest!=*task || now<task.authorized_at_ns || now>=task.expires_at_ns || now>=snap.lease_until_ns {return Err(unavailable());}
        issuing_owner(tx,task)?;
        permission(tx,&task.bridge_permission,&task.actor_id,&task.admission.source_instance_id)?;
        if !context_matches(&Context::from(service),&task.admission,&task.service) || authority(tx,service,now)?!=task.service {return Err(unavailable());}
        let active:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM product_work_claims_v1 WHERE order_id=?1 AND lifecycle=?2 AND owner_id=?3 AND epoch=?4 AND fence=?5 AND state='claimed' AND lease_until=?6)",params![snap.order_id,snap.lifecycle,snap.worker_id,snap.epoch,snap.fence,snap.lease_until_text],|r|r.get(0))?;
        if !active {return Err(unavailable());} Ok(())
    }
    fn validate_respective_claim(task: &A01TaskReceipt, s: &A01TaskClaimView, c: &ProductWorkClaim) -> Result<(),ProductError> {
        if c.order_id!=s.order_id || c.lifecycle!=s.lifecycle || c.delivery_id!=s.delivery_id || c.worker_id!=s.worker_id
            || c.epoch!=s.epoch || c.fence!=s.fence || c.source_revision!=s.source_revision || c.payload_digest!=s.payload_digest
            || clock_ns(c.lease_until)?!=s.lease_until_ns || c.task_kind!=task.admission.raw_kind
            || payload_digest(&c.task_kind,&c.payload_snapshot)!=s.payload_digest
            || !c.service_binding.as_ref().is_some_and(|b|b.service_id==s.service_id && b.service_revision==s.service_revision && b.credential_revision==s.credential_revision && b.source_instance_id==s.source_instance_id) {return Err(evidence("A01 producer work claim binding mismatch"));} Ok(())
    }
    fn journal(r: &A01ProducerReservation) -> String {
        format!("{}:{}:{}:{}",r.claim.order_id,r.attempt,r.run,r.activation)
    }
    fn commits(r: &A01ProducerReservation, output: &str) -> (String,String) {
        let a=&r.task.admission; let c=&r.claim;
        let lifecycle=a.lifecycle.to_string(); let revision=r.task.revision.to_string(); let epoch=c.epoch.to_string(); let fence=c.fence.to_string(); let seq=r.sequence.to_string();
        let inputs=[a.store.installation_id.as_str(),a.store.database_id.as_str(),a.order_id.as_str(),&lifecycle,&revision,&r.task.task_digest,&c.worker_id,&epoch,&fence,&seq,&r.event_hash,output];
        (crate::product_scope::framed_hash("foundry-a01-producer-base-v1",&inputs),crate::product_scope::framed_hash("foundry-a01-producer-result-v1",&inputs))
    }
    fn replayed(tx:&rusqlite::Transaction<'_>, order:&str) -> Result<crate::replay::ReplayedOrder,ProductError> {
        replayed_through(tx,order,i64::MAX)
    }
    fn replayed_through(tx: &rusqlite::Transaction<'_>, order:&str, sequence:i64) -> Result<crate::replay::ReplayedOrder,ProductError> {
        let mut st=tx.prepare("SELECT seq,order_id,from_state,to_state,event,actor,intent,at,prev_hash,hash,event_json FROM ledger WHERE seq<=?1 ORDER BY seq")?;
        let rows=st.query_map([sequence],|r|Ok(crate::sqlite::LedgerRow{seq:r.get(0)?,order_id:r.get(1)?,from_state:r.get(2)?,to_state:r.get(3)?,event:r.get(4)?,actor:r.get(5)?,intent:r.get(6)?,at:r.get(7)?,prev_hash:r.get(8)?,hash:r.get(9)?,event_json:r.get(10)?}))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let orders=crate::replay::replay(&rows,crate::sqlite::SqliteLedger::verify_rows).map_err(|_|evidence("A01 producer ledger replay or chain invalid"))?;
        orders.into_iter().rev().find(|r|r.order_id==order).ok_or_else(|| evidence("A01 producer replayed order missing"))
    }
    fn append(tx: &rusqlite::Transaction<'_>, order: &str, lifecycle: i64, event: OrderEvent, operation: &str, bytes: &[u8], now: DateTime<Utc>) -> Result<(i64,String,u64),ProductError> {
        use sha2::Digest;
        let mut current=replayed(tx,order)?;
        if i64::from(current.lifecycle)+1!=lifecycle {return Err(evidence("A01 producer transition lifecycle mismatch"));}
        let predecessor=current.last_seq;
        let attempt=current.order.attempt().max(1);
        let from=current.order.state().to_string();
        let to=current.order.transition(event.clone(),attempt).map_err(|_|evidence("A01 producer order transition invalid"))?.to_string();
        let seq:i64=tx.query_row("SELECT COALESCE(MAX(seq),0)+1 FROM ledger",[],|r|r.get(0))?;
        let previous:String=tx.query_row("SELECT hash FROM ledger ORDER BY seq DESC LIMIT 1",[],|r|r.get(0))?;
        let event_text=format!("{event:?}"); let event_json=crate::event_json::event_to_json(&event);
        let actor="Service/a01-producer"; let intent=std::str::from_utf8(bytes).map_err(|_|evidence("A01 producer transition intent UTF-8 invalid"))?; let at=now.to_rfc3339();
        let v1=format!("{seq}|{order}|{from}|{to}|{event_text}|{actor}|{intent}|{at}|{previous}");
        let hash=hex::encode(sha2::Sha256::digest(crate::event_json::v2_hash_payload(&v1,&event_json).as_bytes()));
        one(tx.execute("INSERT INTO product_transition_intents_v1(order_id,lifecycle,expected_predecessor,operation) VALUES(?1,?2,?3,?4)",params![order,lifecycle,predecessor,operation])?)?;
        one(tx.execute("INSERT INTO ledger(order_id,from_state,to_state,event,actor,intent,at,prev_hash,event_json,hash) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![order,from,to,event_text,actor,intent,at,previous,event_json,hash])?)?;
        receipt(tx,seq,&hash,order,lifecycle,operation,predecessor)?;
        Ok((seq,hash,u64::from(current.order.attempt())))
    }
    fn receipt(tx: &rusqlite::Transaction<'_>, seq: i64, hash: &str, order: &str, lifecycle: i64, op: &str, predecessor: i64) -> Result<(),ProductError> {
        let exact:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM product_transition_receipts_v1 WHERE seq=?1 AND event_hash=?2 AND order_id=?3 AND lifecycle=?4 AND operation=?5 AND expected_predecessor=?6) AND NOT EXISTS(SELECT 1 FROM product_transition_intents_v1 WHERE order_id=?3)",params![seq,hash,order,lifecycle,op,predecessor],|r|r.get(0))?;
        if !exact {return Err(evidence("A01 producer transition receipt mismatch"));} Ok(())
    }
    fn validate_reservation(tx: &rusqlite::Transaction<'_>, r: &A01ProducerReservation) -> Result<(),ProductError> {
        let (task,snap)=task_claim(tx,&r.claim.order_id,r.claim.lifecycle)?;
        if r.task!=task { return Err(evidence("A01 producer reservation task or execution coordinates mismatch: r.task")); }
        if r.claim!=snap { return Err(evidence("A01 producer reservation task or execution coordinates mismatch: r.claim")); }
        if r.attempt==0 { return Err(evidence("A01 producer reservation task or execution coordinates mismatch: r.attempt")); }
        if r.run!=1 { return Err(evidence("A01 producer reservation task or execution coordinates mismatch: r.run")); }
        if r.activation!=1 { return Err(evidence("A01 producer reservation task or execution coordinates mismatch: r.activation")); }
        validate_key(&r.request_key,0).map_err(|_|evidence("A01 producer reservation request key invalid"))?;
        let (intent,hash,event,from,to):(String,String,String,String,String)=tx.query_row("SELECT intent,hash,event,from_state,to_state FROM ledger WHERE seq=?1 AND order_id=?2",params![r.sequence,r.claim.order_id],|v|Ok((v.get(0)?,v.get(1)?,v.get(2)?,v.get(3)?,v.get(4)?))).map_err(stored_error)?;
        let start:ProducerStart=decode(intent.as_bytes())?;
        if start.kind!="a01-producer-start-v1" { return Err(evidence("A01 producer reservation ledger provenance mismatch: start.kind")); }
        if start.task!=r.task { return Err(evidence("A01 producer reservation ledger provenance mismatch: start.task")); }
        if start.claim!=r.claim { return Err(evidence("A01 producer reservation ledger provenance mismatch: start.claim")); }
        if start.request_key!=r.request_key { return Err(evidence("A01 producer reservation ledger provenance mismatch: start.request_key")); }
        if start.attempt!=r.attempt { return Err(evidence("A01 producer reservation ledger provenance mismatch: start.attempt")); }
        if start.run!=r.run { return Err(evidence("A01 producer reservation ledger provenance mismatch: start.run")); }
        if start.activation!=r.activation { return Err(evidence("A01 producer reservation ledger provenance mismatch: start.activation")); }
        if start.predecessor_seq!=r.predecessor_seq { return Err(evidence("A01 producer reservation ledger provenance mismatch: start.predecessor_seq")); }
        if hash!=r.event_hash { return Err(evidence("A01 producer reservation ledger provenance mismatch: hash")); }
        if event!="DieStarted" { return Err(evidence("A01 producer reservation ledger provenance mismatch: event")); }
        if from!="queued" { return Err(evidence("A01 producer reservation ledger provenance mismatch: from")); }
        if to!="working" { return Err(evidence("A01 producer reservation ledger provenance mismatch: to")); }
        receipt(tx,r.sequence,&r.event_hash,&r.claim.order_id,r.claim.lifecycle,"a01-producer-start-v1",r.predecessor_seq)?;
        replayed(tx,&r.claim.order_id)?; Ok(())
    }
    fn output_for(tx: &rusqlite::Transaction<'_>, order: &str) -> Result<Option<A01AcceptedProducerOutput>,ProductError> {
        let mut st=tx.prepare("SELECT artifact_json,run_id FROM product_review_redrive_v1 WHERE order_id=?1 AND payload='a01-producer-output-v1'")?;
        let rows=st.query_map([order],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let original_count:i64=tx.query_row("SELECT count(*) FROM ledger l JOIN product_transition_receipts_v1 r ON r.seq=l.seq AND r.event_hash=l.hash WHERE l.order_id=?1 AND r.operation='a01-producer-output-v1'",[order],|r|r.get(0))?;
        if rows.is_empty() && original_count==0 {return Ok(None);}
        if rows.len()!=1 { return Err(evidence("A01 producer output mirror or ledger count invalid: rows.len")); }
        if original_count!=1 { return Err(evidence("A01 producer output mirror or ledger count invalid: original_count")); }
        let a:A01AcceptedProducerOutput=decode(rows[0].0.as_bytes())?;
        let r=&a.reservation;
        validate_reservation(tx,r)?;
        if r.claim.order_id!=order { return Err(evidence("A01 producer output mirror identity or bytes invalid: r.claim.order_id")); }
        if rows[0].1!=journal(r) { return Err(evidence("A01 producer output mirror identity or bytes invalid: rows")); }
        if a.output.is_empty() { return Err(evidence("A01 producer output mirror identity or bytes invalid: a.output.is_empty")); }
        if a.output.len()>4096 { return Err(evidence("A01 producer output mirror identity or bytes invalid: a.output.len")); }
        if a.output.as_bytes().contains(&0) { return Err(evidence("A01 producer output mirror identity or bytes invalid: a.output.as_bytes")); }
        let (intent,hash,at,event,from,to):(String,String,String,String,String,String)=tx.query_row("SELECT intent,hash,at,event,from_state,to_state FROM ledger WHERE seq=?1 AND order_id=?2",params![a.sequence,order],|v|Ok((v.get(0)?,v.get(1)?,v.get(2)?,v.get(3)?,v.get(4)?,v.get(5)?))).map_err(stored_error)?;
        let original:ProducerOutput=decode(intent.as_bytes())?;
        let (base,result)=output_commits(r,&a.output)?;
        if original.kind!="a01-producer-output-v1" { return Err(evidence("A01 producer output immutable provenance mismatch: original.kind")); }
        if original.task!=r.task { return Err(evidence("A01 producer output immutable provenance mismatch: original.task")); }
        if original.claim!=r.claim { return Err(evidence("A01 producer output immutable provenance mismatch: original.claim")); }
        if original.reservation!=*r { return Err(evidence("A01 producer output immutable provenance mismatch: original.reservation")); }
        if original.output!=a.output { return Err(evidence("A01 producer output immutable provenance mismatch: original.output")); }
        if original.base_commit!=a.base_commit { return Err(evidence("A01 producer output immutable provenance mismatch: original.base_commit")); }
        if original.result_commit!=a.result_commit { return Err(evidence("A01 producer output immutable provenance mismatch: original.result_commit")); }
        if original.intent!=a.intent { return Err(evidence("A01 producer output immutable provenance mismatch: original.intent")); }
        if hash!=a.event_hash { return Err(evidence("A01 producer output immutable provenance mismatch: hash")); }
        if a.base_commit!=base { return Err(evidence("A01 producer output immutable provenance mismatch: a.base_commit")); }
        if a.result_commit!=result { return Err(evidence("A01 producer output immutable provenance mismatch: a.result_commit")); }
        if a.intent.order.0!=order { return Err(evidence("A01 producer output immutable provenance mismatch: a.intent.order.0")); }
        if a.intent.attempt.0!=r.attempt { return Err(evidence("A01 producer output immutable provenance mismatch: a.intent.attempt.0")); }
        if a.intent.run.0!=r.run { return Err(evidence("A01 producer output immutable provenance mismatch: a.intent.run.0")); }
        if a.intent.activation!=r.activation { return Err(evidence("A01 producer output immutable provenance mismatch: a.intent.activation")); }
        if a.intent.epoch.0!=r.claim.epoch as u64 { return Err(evidence("A01 producer output immutable provenance mismatch: a.intent.epoch.0")); }
        if a.intent.generation.0!=0 { return Err(evidence("A01 producer output immutable provenance mismatch: a.intent.generation.0")); }
        if a.intent.external_ref!=format!("review:{order}:{}:{base}..{result}",r.attempt) { return Err(evidence("A01 producer output immutable provenance mismatch: a.intent.external_ref")); }
        if clock_ns(a.intent.recorded_at)?!=time_ns(&at)? { return Err(evidence("A01 producer output immutable provenance mismatch: clock_ns")); }
        if event!=format!("{:?}",OrderEvent::DieSucceeded{attempt:r.attempt.try_into().map_err(|_|evidence("A01 producer output evidence attempt out of range"))?}) { return Err(evidence("A01 producer output immutable provenance mismatch: event")); }
        if from!="working" { return Err(evidence("A01 producer output immutable provenance mismatch: from")); }

        let mut predecessor = replayed_through(tx,order,r.sequence)?;
        let attempt = r.attempt.try_into().map_err(|_|evidence("A01 producer output evidence attempt out of range"))?;
        let expected_to = predecessor.order.transition(OrderEvent::DieSucceeded{attempt},attempt)
            .map_err(|_|evidence("A01 producer output domain transition invalid"))?.to_string();
        if to!=expected_to { return Err(evidence("A01 producer output immutable provenance mismatch: to")); }
        receipt(tx,a.sequence,&a.event_hash,order,r.claim.lifecycle,"a01-producer-output-v1",r.sequence)?;
        Ok(Some(a))
    }
    pub(super) fn canonical_review_in(tx: &rusqlite::Transaction<'_>, order: &str) -> Result<bool,ProductError> {
        let marked:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='product_a01_profile_v1')",[],|r|r.get(0))?;
        if !marked {return Ok(false);}
        Ok(output_for(tx,order)?.is_some())
    }
    pub(super) fn validate_host_intent_in(tx: &rusqlite::Transaction<'_>, intent: &foundry_exec::ExecutionIntent) -> Result<(),ProductError> {
        if canonical_review_in(tx,&intent.order.0)? {
            let output=output_for(tx,&intent.order.0)?.ok_or_else(|| evidence("A01 host intent producer output missing"))?;
            if output.intent!=*intent {return Err(evidence("A01 host intent producer binding mismatch"));}
        }
        Ok(())
    }
    pub(super) fn passed_provenance_in(tx: &rusqlite::Transaction<'_>, intent: &foundry_exec::ExecutionIntent, result: &foundry_exec::HostResult, now: DateTime<Utc>) -> Result<Option<Vec<u8>>,ProductError> {
        if !canonical_review_in(tx,&intent.order.0)? {return Ok(None);}
        let a=output_for(tx,&intent.order.0)?.ok_or_else(|| evidence("A01 bridge producer output missing"))?;
        let r=&a.reservation; let task=&r.task; let snap=&r.claim;
        if intent.order!=a.intent.order { return Err(evidence("A01 bridge host result lineage mismatch: intent.order")); }
        if intent.attempt!=a.intent.attempt { return Err(evidence("A01 bridge host result lineage mismatch: intent.attempt")); }
        if intent.activation!=a.intent.activation { return Err(evidence("A01 bridge host result lineage mismatch: intent.activation")); }
        if result.base_commit!=a.base_commit { return Err(evidence("A01 bridge host result lineage mismatch: result.base_commit")); }
        if !result.generations.first().is_some_and(|g|g.generation==0 && g.commit==a.result_commit) { return Err(evidence("A01 bridge host result lineage mismatch: result.generations.first")); }
        // Original immutable evidence is checked above. Only ordinary loss of
        // current authority takes the evidence-only completion branch.
        let payload=admission(tx,&task.admission)?.0;
        let actual=ProductWorkClaim{order_id:snap.order_id.clone(),lifecycle:snap.lifecycle,delivery_id:snap.delivery_id.clone(),worker_id:snap.worker_id.clone(),epoch:snap.epoch,fence:snap.fence,source_revision:snap.source_revision,payload_digest:snap.payload_digest.clone(),service_binding:Some(foundry_exec::ReviewServiceBinding{service_id:snap.service_id.clone(),service_revision:snap.service_revision,credential_revision:snap.credential_revision,source_instance_id:snap.source_instance_id.clone()}),lease_until:datetime(snap.lease_until_ns)?,task_kind:task.admission.raw_kind.clone(),payload_snapshot:payload};
        match live(tx,task,snap,&context_for(task),&actual,clock_ns(now)?) {
            Ok(())=>{}, Err(ProductError::Invariant("A01 authorization unavailable"))=>return Ok(None), Err(e)=>return Err(e),
        }
        let predecessor=replayed(tx,&intent.order.0)?.last_seq;
        canonical(&PassedBridge{kind:"a01-passed-bridge-v1".into(),task:task.clone(),claim:snap.clone(),output_sequence:a.sequence,output_event_hash:a.event_hash,host_key:foundry_exec::HostKey{order:intent.order.0.clone(),attempt:intent.attempt.0,run:intent.run.0,activation:intent.activation},host_result_digest:product_host_result_digest(result).map_err(|_|evidence("A01 bridge host result digest invalid"))?,predecessor_seq:predecessor}).map(Some)
    }
    pub(super) fn complete_bridge_in(tx: &rusqlite::Transaction<'_>, intent: &foundry_exec::ExecutionIntent, result: &foundry_exec::HostResult, now: DateTime<Utc>) -> Result<(),ProductError> {
        if let Some(bytes)=passed_provenance_in(tx,intent,result,now)? {
            let lifecycle=output_for(tx,&intent.order.0)?.ok_or_else(|| evidence("A01 bridge completion producer output missing"))?.reservation.claim.lifecycle;
            append(tx,&intent.order.0,lifecycle,OrderEvent::InspectionPassed,"a01-passed-bridge-v1",&bytes,now)?;
        }
        Ok(())
    }

    fn validate_binding_history_in(tx: &rusqlite::Transaction<'_>) -> Result<(),ProductError> {
        let mut st=tx.prepare("SELECT order_id,lifecycle,attempt,run_id,activation,generation,tuple_digest,payload_json FROM product_result_bindings_v1")?;
        let rows=st.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?,r.get::<_,i64>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        // Reuse the frozen predicate for historical identity, never current grants.
        let guard = PERMISSION_GUARD_DDL;
        let query=guard.split_once("  SELECT CASE").ok_or_else(|| evidence("A01 binding history guard query marker missing"))?.1;
        let before_revocation=query.split_once("    WHEN t.operation IS NOT 'authorize'").ok_or_else(|| evidence("A01 binding history revocation marker missing"))?.0;
        let from=query.split_once("  FROM (").ok_or_else(|| evidence("A01 binding history FROM marker missing"))?.1;
        let mut query=format!("SELECT CASE{before_revocation} ELSE 0 END FROM ({from}");
        query=query.trim_end().strip_suffix("END;").ok_or_else(|| evidence("A01 binding history query terminator missing"))?.trim_end().to_owned();
        query=query.replace("RAISE(ROLLBACK,'A01 bridge permission not bound')","1")
            .replace("RAISE(ROLLBACK,'A01 bridge permission identity mismatch')","1");
        for name in ["order_id","lifecycle","attempt","run_id","activation","host_result_digest","payload_json"] {
            query=query.replace(&format!("NEW.{name}"),&format!(":{name}"));
        }
        for (order,lifecycle,attempt,run,activation,generation,tuple,payload) in rows {
            let value:serde_json::Value=serde_json::from_str(&payload).map_err(|_|evidence("A01 binding history payload JSON invalid"))?;
            let host:foundry_exec::HostResult=serde_json::from_value(value["host_result"].clone()).map_err(|_|evidence("A01 binding history host result invalid"))?;
            let digest=product_host_result_digest(&host).map_err(|_|evidence("A01 binding history host result digest invalid"))?;
            let rejected:i64=tx.query_row(&query,rusqlite::named_params!{":order_id":order,":lifecycle":lifecycle,":attempt":attempt,":run_id":run,":activation":activation,":host_result_digest":digest,":payload_json":payload},|r|r.get(0))?;
            if rejected!=0 {return Err(evidence("A01 binding history provenance guard rejected"));}
            let bridge_seq=value["s1_sequence"].as_i64().ok_or_else(|| evidence("A01 binding history bridge sequence missing"))?;
            let bridge_json:String=tx.query_row("SELECT intent FROM ledger WHERE seq=?1",[bridge_seq],|r|r.get(0))?;
            let bridge:PassedBridge=decode(bridge_json.as_bytes())?;
            let output=output_for(tx,&order)?.ok_or_else(|| evidence("A01 binding history producer output missing"))?;
            if bridge.task!=output.reservation.task { return Err(evidence("A01 binding history producer provenance mismatch: bridge.task")); }
        if bridge.claim!=output.reservation.claim { return Err(evidence("A01 binding history producer provenance mismatch: bridge.claim")); }
        if bridge.output_sequence!=output.sequence { return Err(evidence("A01 binding history producer provenance mismatch: bridge.output_sequence")); }
        if bridge.output_event_hash!=output.event_hash { return Err(evidence("A01 binding history producer provenance mismatch: bridge.output_event_hash")); }
            super::result::approval_result_in(tx,&order,lifecycle,attempt,run,generation,&tuple,&host.result_commit).map_err(|_|evidence("A01 binding history approval lineage invalid"))?;
            replayed(tx,&order)?;
        }
        Ok(())
    }
    // Test-only corruption carrier: keep the typed envelopes and flattened
    // columns consistent so the history census, rather than the codec, rejects.
    #[cfg(all(test, unix))]
    pub(super) fn forge_history_for_test(conn: &Connection, key: &str, case: &str) -> Result<(),ProductError> {
        let tx=conn.unchecked_transaction()?;
        let mut row=local(&tx,"owner",key)?.ok_or_else(|| evidence("A01 test history task missing"))?;
        match case {
            "gap"=>{row.receipt.expected_revision=2;row.receipt.revision=3;},
            "initial-revoke"=>row.receipt.operation=A01TaskOperation::Revoke,
            "repeated-authorize"=>row.receipt.operation=A01TaskOperation::Authorize,
            _=>return Err(evidence("A01 test history mutation unknown")),
        }
        let r=&row.receipt;
        row.request=match r.operation {
            A01TaskOperation::Authorize=>canonical(&AuthorizeEnvelope{version:1,profile:PROFILE.into(),operation:r.operation,actor_id:r.actor_id.clone(),request:A01AuthorizeTaskRequest{bridge_permission:r.bridge_permission.clone(),request_key:r.request_key.clone(),expected_revision:r.expected_revision,admission:r.admission.clone(),owner:r.owner.clone(),service:r.service.clone(),expires_at_ns:r.expires_at_ns},context:Context::from(&context_for(r)),task_kind:TASK_KIND.into(),task_bytes:TASK_BYTES.to_vec()})?,
            A01TaskOperation::Revoke=>canonical(&RevokeEnvelope{version:1,profile:PROFILE.into(),operation:r.operation,actor_id:r.actor_id.clone(),request:A01RevokeTaskRequest{request_key:r.request_key.clone(),expected_revision:r.expected_revision,admission:r.admission.clone(),owner:r.owner.clone()}})?,
        };
        let triggers={
            let mut st=tx.prepare("SELECT name,sql FROM sqlite_schema WHERE type='trigger' AND tbl_name='product_a01_task_revisions_v1'")?;
            let rows=st.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (name,_) in &triggers {tx.execute_batch(&format!("DROP TRIGGER \"{name}\""))?;}
        one(tx.execute("DELETE FROM product_a01_task_revisions_v1 WHERE actor_id='owner' AND request_key=?1",[key])?)?;
        insert(&tx,&row.receipt,&row.request)?;
        for (_,ddl) in &triggers {tx.execute_batch(ddl)?;}
        tx.commit()?;
        Ok(())
    }

}

#[cfg(feature = "a01-harness-support")]
pub use a01_task::{A01RealManifest,A01RealEvidence,A01RealOutput,a01_real_inputs,a01_real_manifest,a01_real_reservation_sha256,a01_real_task_receipt_sha256,encode_a01_real_output,decode_a01_real_output,A01BridgePermissionRef,A01ProducerReservation,A01AcceptedProducerOutput,A01ProducerError,A01TaskStoreRef,A01TaskAdmissionExpectation,A01TaskOwnerExpectation,A01TaskServiceExpectation,A01AuthorizeTaskRequest,A01RevokeTaskRequest,A01TaskReceipt,A01TaskClaimView,A01TaskOperation,A01TaskStoreIdentity};


#[cfg(all(test, feature = "a01-harness-support", unix))]
mod a01_task_tests {
    use super::a01_task;
use chrono::{DateTime, Duration, Utc};
use crate::{ProductClock, ProductConfig, ProductError, ProductStore};
use rusqlite::{Connection, params, types::Value};
use std::collections::BTreeMap;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

// All test failures use fixed diagnostics, including fallible fixture setup.
#[track_caller]
fn require<T, E>(result: Result<T, E>) -> T {
    match result { Ok(value) => value, Err(_) => panic!("A01 test operation failed") }
}

#[track_caller]
fn require_fixture<T>(
    result: rusqlite::Result<T>,
    case: &'static str,
    stage: &'static str,
) -> T {
    match result {
        Ok(value) => value,
        Err(rusqlite::Error::SqliteFailure(code, _)) => panic!(
            "A01 fixture operation failed: case={case} stage={stage} sqlite_code={}",
            code.extended_code
        ),
        Err(_) => panic!(
            "A01 fixture operation failed: case={case} stage={stage} sqlite_code=none"
        ),
    }
}

struct PrivateDir(PathBuf);
impl PrivateDir {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let stamp = require(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)).as_nanos();
        let path = std::env::temp_dir().join(format!("foundry-a01-{}-{stamp}-{serial}", std::process::id()));
        require(std::fs::DirBuilder::new().mode(0o700).create(&path));
        Self(require(path.canonicalize()))
    }
}
impl Drop for PrivateDir {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

struct FixedClock;
impl ProductClock for FixedClock { fn now(&self) -> DateTime<Utc> { now() } }
fn now() -> DateTime<Utc> {
    require(DateTime::parse_from_rfc3339("2026-01-01T00:00:00+00:00")).with_timezone(&Utc)
}
fn clock() -> Arc<dyn ProductClock> { Arc::new(FixedClock) }
fn config() -> ProductConfig {
    ProductConfig { installation_id: "a01-test-installation".into(), database_id: "a01-test-database".into() }
}
fn create(dir: &PrivateDir) -> ProductStore {
    require(ProductStore::create_a01_isolated_test_store(&dir.0, "store.db", config(), clock()))
}
fn reopen(dir: &PrivateDir) -> ProductStore {
    require(ProductStore::reopen_a01_isolated_test_store(&dir.0, "store.db", config(), clock()))
}
fn read_db(path: &Path) -> Connection {
    require(Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY))
}
fn count(path: &Path, table: &str) -> i64 {
    require(read_db(path).query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)))
}

type Files = BTreeMap<PathBuf, (u32, u32, u64, Vec<u8>)>;
fn files(dir: &Path, database_bytes: bool) -> Files {
    fn visit(root: &Path, dir: &Path, database_bytes: bool, out: &mut Files) {
        for entry in require(std::fs::read_dir(dir)) {
            let entry = require(entry);
            let path = entry.path();
            let meta = require(std::fs::symlink_metadata(&path));
            let relative = require(path.strip_prefix(root)).to_path_buf();
            let bytes = if meta.file_type().is_symlink() {
                use std::os::unix::ffi::OsStrExt;
                require(std::fs::read_link(&path)).as_os_str().as_bytes().to_vec()
            } else if meta.is_file() && (database_bytes || relative != Path::new("store.db")) {
                require(std::fs::read(&path))
            } else { Vec::new() };
            out.insert(relative, (meta.mode(), meta.uid(), meta.nlink(), bytes));
            if meta.is_dir() { visit(root, &path, database_bytes, out); }
        }
    }
    let mut out = BTreeMap::new();
    visit(dir, dir, database_bytes, &mut out);
    out
}

// Keep rows and schema private. Never use assert_eq! on these observations.
fn logical(path: &Path) -> BTreeMap<String, Vec<Vec<Value>>> {
    let conn = read_db(path);
    let mut names = require(conn.prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name"));
    let names: Vec<String> = require(require(names.query_map([], |r| r.get(0))).collect());
    let mut out = BTreeMap::new();
    for name in names {
        let quoted = name.replace('"', "\"\"");
        let mut st = require(conn.prepare(&format!("SELECT * FROM \"{quoted}\"")));
        let columns = st.column_count();
        let mut rows: Vec<Vec<Value>> = require(require(st.query_map([], |r| {
            (0..columns).map(|i| r.get(i)).collect::<rusqlite::Result<_>>()
        })).collect());
        rows.sort_by_cached_key(|row| format!("{row:?}"));
        out.insert(name, rows);
    }
    let mut st = require(conn.prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name"));
    out.insert("schema definitions".into(), require(require(st.query_map([], |r| {
        (0..4).map(|i| r.get(i)).collect::<rusqlite::Result<_>>()
    })).collect()));
    out
}
fn preserved(dir: &PrivateDir) -> (BTreeMap<String, Vec<Vec<Value>>>, Files) {
    (logical(&dir.0.join("store.db")), files(&dir.0, false))
}
use crate::product::{A01AuthorizeTaskRequest, A01RevokeTaskRequest, A01TaskAdmissionExpectation,
    A01TaskOwnerExpectation, A01TaskServiceExpectation, A01TaskStoreRef, A01TaskStoreIdentity};

struct TaskClock(std::sync::Mutex<DateTime<Utc>>);
impl ProductClock for TaskClock { fn now(&self) -> DateTime<Utc> { *require(self.0.lock()) } }
impl TaskClock { fn set(&self, value: DateTime<Utc>) { *require(self.0.lock())=value; } }

// Transport is simulated. Admission, authentication and every positive write
// use the real ledger APIs. This fixture never seeds ready or claim evidence.
struct TaskClaimFixture {
    dir: PrivateDir,
    store: ProductStore,
    identity: A01TaskStoreIdentity,
    clock: Arc<TaskClock>,
    session: String,
    service: crate::ProductServiceContext,
    authorization: A01AuthorizeTaskRequest,
}
impl TaskClaimFixture {
    fn new() -> Self {
        use crate::{ProductCapability, ProductServiceAction, ProductServiceGrant, ScopeGrantManifest, ScopeSourceManifest, RepositoryIdentity};
        use crate::product_identity::ProductPasswordKdf;
        let dir=PrivateDir::new();
        let clock=Arc::new(TaskClock(std::sync::Mutex::new(now())));
        let (mut store,identity)=require(ProductStore::create_a01_task_claim_test_store(&dir.0,"store.db",config(),clock.clone()));
        let source=ScopeSourceManifest{connector_id:"gh".into(),source_instance_id:"task-source".into(),generation:1,
            identity:require(RepositoryIdentity::canonicalized("acme","GitHub","https://api.github.com","42")),
            api_owner:Some("acme".into()),api_repo:Some("project".into()),legacy_repository_locator:"acme/project".into(),scope_query:"open".into(),enabled:true};
        let grants=[ScopeGrantManifest{actor_id:"owner".into(),source_instance_id:"task-source".into(),capabilities:vec![ProductCapability::ReadOrder]}];
        require(store.bootstrap_owner_scoped("owner",&require(ProductPasswordKdf::hash("task-profile-fixture-password")),&[source],&grants));
        let session=require(store.login("owner","task-profile-fixture-password",now())).digest().to_owned();
        require(store.create_account(&session,"bridge-admin","owner",&require(ProductPasswordKdf::hash("task-admin-fixture-password")),now()));
        let admin=require(store.login("bridge-admin","task-admin-fixture-password",now())).digest().to_owned();
        let db=read_db(store.path());
        let scope_revision:i64=require(db.query_row("SELECT revision FROM product_scope_subject_revisions_v1 WHERE subject_id='owner' AND source_instance_id='task-source'",[],|r|r.get(0)));
        drop(db);
        require(store.change_product_scope(&admin,"owner","task-source",ProductCapability::A01BridgePassed,crate::ScopeMutation::Grant,scope_revision,"bridge-grant",now()));
        let bridge_permission=require(require(store.read_a01_bridge_permission(&session,"task-source")).ok_or(()));
        let grants:Vec<_>=[ProductServiceAction::IntakeRead,ProductServiceAction::AdmitDelivery,ProductServiceAction::AckDelivery,ProductServiceAction::DriveReview]
            .into_iter().map(|action|ProductServiceGrant{source_instance_id:"task-source".into(),action}).collect();
        require(store.manage_product_service(&session,"task-service",&grants,true,now()+Duration::hours(4),0,"service",now()));
        let credential=require(store.issue_product_service_credential(&session,"task-service",1,now()+Duration::hours(3),"credential",now()));
        let service=require(store.authenticate_product_service_for_action(credential.expose_once_to_file_writer(),"gh","https://api.github.com","42",ProductServiceAction::DriveReview,now()));
        let payload=r#"{"issue_number":17,"title":"A bounded task","body":"original input"}"#;
        let updated=now().to_rfc3339();
        let version=crate::product::delivery_version_digest("issue:17",&updated,"github-issue",payload);
        let delivery=crate::ProductDelivery{delivery_id:crate::product::github_delivery_id(&config().installation_id,"gh","https://api.github.com","42","issue:17",&version),connector_id:"gh".into(),repository_id:"42".into(),issue_id:"issue:17".into(),observed_updated_at:updated.clone(),version_digest:version,task_kind:"github-issue".into(),payload:payload.into()};
        let cursor=crate::ProductPollCursor{connector_id:"gh".into(),query_fingerprint:"task-query".into(),sweep_id:"task-sweep".into(),next_page:1,page_ceiling:1,updated_at:updated};
        require(store.persist_intake_page_for_service(&service,&cursor,&[delivery.clone()],&[],now()));
        let receipt=require(store.admit_intake_delivery_for_service(&service,&delivery.delivery_id,"task-order",now()));
        require(store.mark_intake_acked_for_service(&service,&delivery.delivery_id,&receipt,now()));
        let view=require(store.read_scoped_order(&session,&receipt.order_id,now()));
        let db=read_db(store.path());
        let account_revision=require(db.query_row("SELECT revision FROM product_accounts_v1 WHERE actor_id='owner'",[],|r|r.get(0)));
        let (source_generation,source_instance_revision,connector_revision,provider,api_origin,repository_id,api_owner,api_repo):(i64,i64,i64,String,String,String,String,String)=require(db.query_row("SELECT i.generation,i.revision,s.revision,r.provider,r.api_origin,r.stable_repository_id,l.api_owner,l.api_repo FROM product_scope_source_instances_v1 i JOIN product_scope_repositories_v1 r USING(repository_key) JOIN product_scope_api_locators_v1 l USING(source_instance_id) JOIN product_sources_v1 s ON s.connector_id=i.connector_id WHERE i.source_instance_id='task-source'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))));
        let (service_revision,credential_revision,drive_review_grant_revision):(i64,i64,i64)=require(db.query_row("SELECT s.revision,c.credential_revision,g.grant_revision FROM product_services_v1 s JOIN product_service_credentials_v1 c USING(service_id) JOIN product_service_grants_v1 g USING(service_id) WHERE g.action='drive_review'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))));
        let raw_payload_digest=task_raw_digest(&delivery.task_kind,&delivery.payload);
        let authorization=A01AuthorizeTaskRequest{bridge_permission,request_key:"authorize".into(),expected_revision:0,
            admission:A01TaskAdmissionExpectation{store:A01TaskStoreRef{installation_id:config().installation_id,database_id:config().database_id},order_id:receipt.order_id,lifecycle:receipt.lifecycle,delivery_id:delivery.delivery_id,admission_seq:receipt.sequence,admission_hash:receipt.event_hash,raw_kind:delivery.task_kind,raw_payload_digest,source_instance_id:view.source_instance_id,source_generation,source_instance_revision,connector_id:"gh".into(),connector_revision,repository_key:view.repository_key,repository_provider:provider,api_origin,stable_repository_id:repository_id,api_owner,api_repo},
            owner:A01TaskOwnerExpectation{account_revision,scope_revision:view.scope_revision,read_order_grant_revision:view.grant_revision},
            service:A01TaskServiceExpectation{service_id:"task-service".into(),service_revision,credential_revision,drive_review_grant_revision,service_lease_until_ns:require((now()+Duration::hours(4)).timestamp_nanos_opt().ok_or(())),credential_expires_at_ns:require((now()+Duration::hours(3)).timestamp_nanos_opt().ok_or(()))},
            expires_at_ns:require((now()+Duration::hours(2)).timestamp_nanos_opt().ok_or(()))};
        drop(db);
        Self{dir,store,identity,clock,session,service,authorization}
    }
    fn authorize(&mut self) -> crate::product::A01TaskReceipt {
        require(self.store.authorize_a01_task(&self.session,&self.service,&self.authorization))
    }
    fn claim(&mut self) -> crate::ProductWorkClaim {
        match require(self.store.claim_product_work_for_service(&self.service,"task-worker",now()-Duration::days(1),Duration::minutes(10))) {
            crate::ProductWorkClaimResult::Claimed(c)=>c,_=>panic!("A01 task claim missing"),
        }
    }
    fn revoke_request(&self, revision: i64) -> A01RevokeTaskRequest {
        A01RevokeTaskRequest{request_key:format!("revoke-{revision}"),expected_revision:revision,admission:self.authorization.admission.clone(),owner:self.authorization.owner.clone()}
    }
    fn reopen(self) -> Self {
        let Self{dir,store,identity,clock,session,service,authorization}=self;
        drop(store);
        let store=require(ProductStore::reopen_a01_task_claim_test_store(&dir.0,"store.db",config(),clock.clone(),&identity));
        Self{dir,store,identity,clock,session,service,authorization}
    }
    fn empty(&mut self) {
        assert!(matches!(require(self.store.claim_product_work_for_service(&self.service,"another-worker",now(),Duration::minutes(5))),crate::ProductWorkClaimResult::Empty),"A01 duplicate acquisition");
    }
}
fn task_raw_digest(kind: &str,payload: &str) -> String {
    use sha2::{Digest,Sha256};
    let mut h=Sha256::new();h.update(b"foundry-product-intake-v1\0");
    for bytes in [kind.as_bytes(),payload.as_bytes()] {h.update((bytes.len() as u64).to_be_bytes());h.update(bytes);}
    hex::encode(h.finalize())
}

    fn injected(name: &str,target: &str) -> Result<(),ProductError> {
        if name==target {Err(ProductError::Invariant("A01 test checkpoint"))} else {Ok(())}
    }
    #[test]
    fn a01_task_initialization_dispatch_and_rollback() {
        use super::{initialize_product_schema_in,StoreOpenMode};
        for mode in [StoreOpenMode::Normal,StoreOpenMode::A01Create,StoreOpenMode::A01TaskCreate] {
            let mut conn=require(Connection::open_in_memory());
            require(conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA recursive_triggers=ON; PRAGMA ignore_check_constraints=OFF;"));
            let tx=require(conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate));
            require(initialize_product_schema_in(&tx,&config(),mode));
            let tables:i64=require(tx.query_row("SELECT count(*) FROM sqlite_schema WHERE name LIKE 'product_a01_%' AND type='table'",[],|r|r.get(0)));
            assert!(tables==if mode==StoreOpenMode::A01TaskCreate {3} else {0},"A01 initialization dispatch mismatch");
            require(tx.rollback());
        }
        let targets:Vec<String>=(0..12).map(|i|format!("schema-{i}")).chain(["profile".into(),"precommit".into()]).collect();
        for target in targets {
            let mut conn=require(Connection::open_in_memory());require(conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA recursive_triggers=ON; PRAGMA ignore_check_constraints=OFF;"));
            let tx=require(conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate));
            require(initialize_product_schema_in(&tx,&config(),StoreOpenMode::A01Create));
            let mut callback=|name:&str|injected(name,&target);
            let mut hooks=a01_task::TaskHooks::with_callback(&mut callback);
            let result=a01_task::initialize_in(&tx,&config(),&mut hooks);
            assert!(a01_task::finish(tx,result,&mut hooks).is_err(),"A01 initialization fault missed");
            let tables:i64=require(conn.query_row("SELECT count(*) FROM sqlite_schema WHERE type='table'",[],|r|r.get(0)));
            assert!(tables==0 && conn.is_autocommit(),"A01 partial initialization committed");
        }
    }
    #[test]
    fn a01_task_write_boundaries_roll_back() {
        for (operation,targets) in [
            ("authorize",&["authorize-insert","authorize-ready","precommit"][..]),
            ("revoke",&["revoke-insert","revoke-backlog","precommit"][..]),
            ("claim",&["fence","claim","binding","backlog","snapshot","precommit"][..]),
        ] {
            for target in targets {
                let mut f=TaskClaimFixture::new();if operation!="authorize" {f.authorize();}
                let before=preserved(&f.dir);let bytes=files(&f.dir.0,true);
                let mut callback=|name:&str|injected(name,target);
                let mut hooks=a01_task::TaskHooks::with_callback(&mut callback);
                let failed=match operation {
                    "authorize"=>a01_task::authorize(&mut f.store,&f.session,&f.service,&f.authorization,&mut hooks).is_err(),
                    "revoke"=>{let r=f.revoke_request(1);a01_task::revoke(&mut f.store,&f.session,&r,&mut hooks).is_err()},
                    _=>a01_task::claim(&mut f.store,&f.service,"worker",Duration::minutes(1),&mut hooks).is_err(),
                };
                assert!(failed && f.store.conn.is_autocommit(),"A01 write checkpoint failed to roll back");
                assert!(before==preserved(&f.dir) && bytes==files(&f.dir.0,true),"A01 write checkpoint committed partial state");
            }
        }
    }
    #[test]
    fn a01_task_after_lock_time_is_authoritative() {
        use std::sync::mpsc::sync_channel;
        let mut f=TaskClaimFixture::new();
        f.authorization.expires_at_ns=require((now()+Duration::seconds(1)).timestamp_nanos_opt().ok_or(()));f.authorize();
        let mut competing=require(Connection::open(f.store.path()));
        let guard=require(competing.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate));
        let (sent,ack)=sync_channel(1);
        let clock=f.clock.clone();
        let result=std::thread::scope(|scope| {
            let worker=scope.spawn(|| {
                let mut callback=|name:&str| {if name=="before-lock" {require(sent.send(()));} Ok(())};
                a01_task::claim(&mut f.store,&f.service,"worker",Duration::minutes(1),&mut a01_task::TaskHooks::with_callback(&mut callback))
            });
            require(ack.recv_timeout(std::time::Duration::from_secs(2)));
            clock.set(now()+Duration::seconds(1));
            require(guard.commit());require(worker.join())
        });
        assert!(matches!(require(result),crate::ProductWorkClaimResult::Empty),"A01 caller or pre-lock time authorized expired claim");
        assert!(count(f.store.path(),"product_work_claims_v1")==0,"A01 post-lock expiry wrote claim");
    }
    #[test]
    fn a01_task_revoke_claim_lock_orders_and_two_claimers() {
        use std::sync::mpsc::sync_channel;
        for revoke_first in [true,false] {
            let mut f=TaskClaimFixture::new();f.authorize();let revoke=f.revoke_request(1);
            let mut contender=require(ProductStore::reopen_a01_task_claim_test_store(&f.dir.0,"store.db",config(),f.clock.clone(),&f.identity));
            let (first_sent,first_ack)=sync_channel(1);let (second_sent,second_ack)=sync_channel(1);let (release,wait)=sync_channel(1); let wait=std::sync::Mutex::new(wait);
            std::thread::scope(|scope| {
                let first=scope.spawn(|| {
                    let mut callback=|name:&str| {if name=="after-lock" {require(first_sent.send(()));require(require(wait.lock()).recv_timeout(std::time::Duration::from_secs(3)));} Ok(())};
                    let mut hooks=a01_task::TaskHooks::with_callback(&mut callback);
                    if revoke_first {a01_task::revoke(&mut f.store,&f.session,&revoke,&mut hooks).map(|_|false)}
                    else {a01_task::claim(&mut f.store,&f.service,"first",Duration::minutes(1),&mut hooks).map(|r|matches!(r,crate::ProductWorkClaimResult::Claimed(_)))}
                });
                require(first_ack.recv_timeout(std::time::Duration::from_secs(2)));
                let second=scope.spawn(|| {
                    let mut callback=|name:&str| {if name=="before-lock" {require(second_sent.send(()));} Ok(())};
                    let mut hooks=a01_task::TaskHooks::with_callback(&mut callback);
                    if revoke_first {a01_task::claim(&mut contender,&f.service,"second",Duration::minutes(1),&mut hooks).map(|r|matches!(r,crate::ProductWorkClaimResult::Claimed(_)))}
                    else {a01_task::revoke(&mut contender,&f.session,&revoke,&mut hooks).map(|_|false)}
                });
                require(second_ack.recv_timeout(std::time::Duration::from_secs(2)));require(release.send(()));
                assert!(require(require(first.join()))==!revoke_first,"A01 first lock order mismatch");
                assert!(!require(require(second.join())),"A01 revoke lock order allowed new claim");
            });
            assert!(count(f.store.path(),"product_work_claims_v1")==if revoke_first {0} else {1},"A01 revoke/claim serialization mismatch");
            let f=f.reopen();assert!(count(f.store.path(),"product_a01_task_revisions_v1")==2,"A01 lock order lost history");
        }
        let mut f=TaskClaimFixture::new();f.authorize();
        let mut other=require(ProductStore::reopen_a01_task_claim_test_store(&f.dir.0,"store.db",config(),f.clock.clone(),&f.identity));
        let (sent,ack)=sync_channel(1);let (release,wait)=sync_channel(1); let wait=std::sync::Mutex::new(wait);let (sent2,ack2)=sync_channel(1);
        std::thread::scope(|scope| {
            let first=scope.spawn(|| {
                let mut callback=|name:&str| {if name=="after-lock" {require(sent.send(()));require(require(wait.lock()).recv_timeout(std::time::Duration::from_secs(3)));} Ok(())};
                a01_task::claim(&mut f.store,&f.service,"first",Duration::minutes(1),&mut a01_task::TaskHooks::with_callback(&mut callback))
            });
            require(ack.recv_timeout(std::time::Duration::from_secs(2)));
            let second=scope.spawn(|| {
                let mut callback=|name:&str| {if name=="before-lock" {require(sent2.send(()));} Ok(())};
                a01_task::claim(&mut other,&f.service,"second",Duration::minutes(1),&mut a01_task::TaskHooks::with_callback(&mut callback))
            });
            require(ack2.recv_timeout(std::time::Duration::from_secs(2)));require(release.send(()));
            assert!(matches!(require(require(first.join())),crate::ProductWorkClaimResult::Claimed(_)),"A01 first claimer lost claim");
            assert!(matches!(require(require(second.join())),crate::ProductWorkClaimResult::Empty),"A01 two claimers both acquired");
        });
        assert!(count(f.store.path(),"product_a01_claim_snapshots_v1")==1,"A01 two claimers wrote extra snapshots");
    }
    #[test]
    fn a01_task_integer_and_time_boundaries() {
        for (time,expected) in [("1970-01-01T00:00:00Z",0),("1970-01-01T10:00:00.000000001+10:00",1),("2262-04-11T23:47:16.854775807Z",i64::MAX)] {
            assert!(require(a01_task::time_ns(time))==expected,"A01 timestamp boundary changed");
        }
        for bad in ["1969-12-31T23:59:59.999999999Z","2262-04-11T23:47:16.854775808Z","2016-12-31T23:59:60Z","2026-01-01T00:00:00.0000000000Z","2026-01-01t00:00:00z","2026-02-30T00:00:00Z","2026-01-01T00:00:00Z trailing"] {
            assert!(a01_task::time_ns(bad).is_err(),"A01 invalid time accepted");
        }
        let mut f=TaskClaimFixture::new();assert!(f.authorization.expected_revision==0 && f.authorize().revision==1,"A01 zero-to-positive revision failed");
        for sql in ["UPDATE product_fence_counter_v1 SET next=9223372036854775807","UPDATE product_fence_counter_v1 SET next=-1","UPDATE product_work_backlog_v1 SET attempts=9223372036854775807"] {
            let mut f=TaskClaimFixture::new();f.authorize();require(require(Connection::open(f.store.path())).execute_batch(sql));let before=preserved(&f.dir);
            assert!(f.store.claim_product_work_for_service(&f.service,"worker",now(),Duration::minutes(1)).is_err(),"A01 integer overflow accepted");
            assert!(before==preserved(&f.dir),"A01 integer overflow mutated state");
        }
        let mut f=TaskClaimFixture::new();f.authorization.expected_revision=i64::MAX;let before=preserved(&f.dir);
        assert!(matches!(f.store.authorize_a01_task(&f.session,&f.service,&f.authorization),Err(ProductError::InvalidConfig)),"A01 revision overflow accepted");assert!(before==preserved(&f.dir),"A01 revision overflow wrote evidence");
        for (table,column) in [("product_services_v1","lease_until"),("product_service_credentials_v1","expires_at"),("product_work_backlog_v1","available_at")] {
            for bad in ["bad","2016-12-31T23:59:60Z","2026-01-01T00:00:00.1234567890Z"] {
                let mut f=TaskClaimFixture::new();f.authorize();require(require(Connection::open(f.store.path())).execute(&format!("UPDATE {table} SET {column}=?1"),[bad]));let before=preserved(&f.dir);
                assert!(f.store.claim_product_work_for_service(&f.service,"worker",now(),Duration::minutes(1)).is_err(),"A01 malformed participating time accepted");assert!(before==preserved(&f.dir),"A01 malformed time wrote state");
            }
        }
    }

    #[test]
    fn a01_task_sql_immutability_and_conflict_modes() {
        let mut f=TaskClaimFixture::new();f.authorize();f.claim();
        for (table,keys) in [
            ("product_a01_profile_v1",&["primary"][..]),
            ("product_a01_task_revisions_v1",&["primary","actor-key"][..]),
            ("product_a01_claim_snapshots_v1",&["primary","fence"][..]),
        ] {
            for key in keys {
                for mode in ["INSERT","INSERT OR IGNORE","INSERT OR REPLACE","UPSERT"] {
                    let before=preserved(&f.dir);let bytes=files(&f.dir.0,true);
                    let db=require(Connection::open(f.store.path()));require(db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA recursive_triggers=ON; BEGIN IMMEDIATE; UPDATE product_fence_counter_v1 SET next=next+1"));
                    let columns:Vec<String>={let mut st=require(db.prepare(&format!("PRAGMA table_info({table})")));require(require(st.query_map([],|r|r.get(1))).collect())};
                    let select=columns.iter().map(|c|match (table,*key,c.as_str()) {
                        ("product_a01_task_revisions_v1","primary","request_key")=>"'collision-key'".into(),
                        ("product_a01_task_revisions_v1","actor-key","revision")=>"revision+1".into(),
                        ("product_a01_task_revisions_v1","actor-key","expected_revision")=>"expected_revision+1".into(),
                        ("product_a01_claim_snapshots_v1","primary","fence")=>"fence+1".into(),
                        ("product_a01_claim_snapshots_v1","fence","order_id")=>"'another-order'".into(),
                        _=>format!("\"{c}\""),
                    }).collect::<Vec<String>>().join(",");
                    let names=columns.iter().map(|c|format!("\"{c}\"")).collect::<Vec<_>>().join(",");
                    let prefix=if mode=="UPSERT" {"INSERT"} else {mode};let suffix=if mode=="UPSERT" {" ON CONFLICT DO UPDATE SET lifecycle=excluded.lifecycle"} else {""};
                    let suffix=if mode=="UPSERT" && table=="product_a01_profile_v1" {" ON CONFLICT DO NOTHING"} else {suffix};
                    assert!(db.execute_batch(&format!("{prefix} INTO {table}({names}) SELECT {select} FROM {table} WHERE 1{suffix}")).is_err() && db.is_autocommit(),"A01 unique conflict did not roll back transaction");
                    drop(db);assert!(before==preserved(&f.dir) && bytes==files(&f.dir.0,true),"A01 conflict committed unrelated write");
                }
            }
            for operation in [format!("UPDATE {table} SET {}={}",if table=="product_a01_profile_v1" {"version"} else {"lifecycle"},if table=="product_a01_profile_v1" {"version"} else {"lifecycle"}),format!("DELETE FROM {table}")] {
                let before=preserved(&f.dir);let db=require(Connection::open(f.store.path()));
                require(db.execute_batch("PRAGMA recursive_triggers=ON; BEGIN IMMEDIATE; UPDATE product_fence_counter_v1 SET next=next+1"));
                assert!(db.execute_batch(&operation).is_err() && db.is_autocommit(),"A01 matched mutation did not roll back");drop(db);
                assert!(before==preserved(&f.dir),"A01 matched mutation committed earlier write");
            }
        }
        // These probes reach ordinary constraints, rather than a duplicate guard.
        // A new revoke identity is legal at the BEFORE INSERT trigger boundary.
        for (field,value,ignore) in [("task_bytes","NULL",false),("task_bytes","NULL",true),("task_bytes","'wrong storage class'",false),("task_kind","'wrong-kind'",false),("task_kind","'wrong-kind'",true),("actor_id","'missing-actor'",false)] {
            let before=preserved(&f.dir);let db=require(Connection::open(f.store.path()));
            require(db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA recursive_triggers=ON; BEGIN IMMEDIATE; UPDATE product_fence_counter_v1 SET next=next+1"));
            let columns:Vec<String>={let mut st=require(db.prepare("PRAGMA table_info(product_a01_task_revisions_v1)"));require(require(st.query_map([],|r|r.get(1))).collect())};
            let select=columns.iter().map(|c|if c==field {value.to_owned()} else {match c.as_str() {"operation"=>"'revoke'".into(),"request_key"=>"'constraint-probe'".into(),"revision"=>"2".into(),"expected_revision"=>"1".into(),_=>format!("\"{c}\"")}}).collect::<Vec<_>>().join(",");
            let prefix=if ignore {"INSERT OR IGNORE"} else {"INSERT"};
            let result=db.execute(&format!("{prefix} INTO product_a01_task_revisions_v1 SELECT {select} FROM product_a01_task_revisions_v1"),[]);
            assert!(if ignore {matches!(result,Ok(0))} else {result.is_err()},"A01 ordinary constraint result mismatch");
            assert!(!db.is_autocommit(),"A01 ordinary statement constraint unexpectedly rolled back transaction");
            let counter:i64=require(db.query_row("SELECT next FROM product_fence_counter_v1 WHERE singleton=1",[],|r|r.get(0)));assert!(counter==2,"A01 statement-abort control lost earlier write");
            require(db.execute_batch("ROLLBACK"));drop(db);assert!(before==preserved(&f.dir),"A01 explicit constraint rollback changed evidence");
        }
    }
    fn damage_restore(path: &Path, table: &str, sql: &str) {
        let mut db=require(Connection::open(path));
        require(db.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA ignore_check_constraints=ON"));
        let triggers:Vec<(String,String)>={let mut st=require(db.prepare("SELECT name,sql FROM sqlite_schema WHERE type='trigger' AND tbl_name=?1"));require(require(st.query_map([table],|r|Ok((r.get(0)?,r.get(1)?)))).collect())};
        let tx=require(db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate));
        for (name,_) in &triggers {require(tx.execute_batch(&format!("DROP TRIGGER \"{name}\"")));}
        require(tx.execute_batch(sql));
        for (_,ddl) in &triggers {require(tx.execute_batch(ddl));}
        require(tx.commit());
    }
    fn rejected_without_repair(f: &TaskClaimFixture) {
        let before=preserved(&f.dir);let bytes=files(&f.dir.0,true);
        assert!(matches!(ProductStore::reopen_a01_task_claim_test_store(&f.dir.0,"store.db",config(),f.clock.clone(),&f.identity),Err(ProductError::ContractMismatch)),"A01 damaged history reopened");
        assert!(before==preserved(&f.dir) && bytes==files(&f.dir.0,true),"A01 damaged history was repaired");
    }
    #[test]
    fn a01_task_snapshot_and_history_census() {
        for case in ["gap","initial-revoke","repeated-authorize"] {
            let mut f=TaskClaimFixture::new();f.authorize();
            let key=if case=="repeated-authorize" {
                let r=f.revoke_request(1);require(f.store.revoke_a01_task(&f.session,&r));r.request_key
            } else {"authorize".into()};
            require(a01_task::forge_history_for_test(&f.store.conn,&key,case));
            rejected_without_repair(&f);
        }
        for (column,value) in [("order_id","'missing'"),("lifecycle","2"),("task_revision","2"),("delivery_id","'substitution'"),("worker_id","'other-worker'"),("epoch","2"),("fence","2"),("claimed_at_ns","0"),("lease_until_ns","lease_until_ns+1"),("lease_until_text","'2026-01-01T00:10:00.000000001Z'"),("source_revision","2"),("payload_digest","'invalid'"),("service_id","'other-service'"),("service_revision","2"),("credential_revision","2"),("source_instance_id","'other-source'")] {
            let mut f=TaskClaimFixture::new();f.authorize();f.claim();
            damage_restore(f.store.path(),"product_a01_claim_snapshots_v1",&format!("UPDATE product_a01_claim_snapshots_v1 SET {column}={value}"));
            rejected_without_repair(&f);
        }
        for sql in [
            "UPDATE product_a01_task_revisions_v1 SET revision=3,expected_revision=2",
            "UPDATE product_a01_task_revisions_v1 SET operation='revoke'",
            "UPDATE product_a01_task_revisions_v1 SET receipt_bytes=CAST(' {}' AS BLOB)",
            "UPDATE product_a01_task_revisions_v1 SET request_bytes=CAST('{}' AS BLOB)",
            "UPDATE product_a01_task_revisions_v1 SET raw_payload_digest='invalid'",
            "UPDATE product_a01_task_revisions_v1 SET admission_seq=999",
            "UPDATE product_a01_task_revisions_v1 SET expires_at_ns=authorized_at_ns",
        ] {
            let mut f=TaskClaimFixture::new();f.authorize();damage_restore(f.store.path(),"product_a01_task_revisions_v1",sql);rejected_without_repair(&f);
        }
        for sql in ["UPDATE product_work_claims_v1 SET owner_id='other'","UPDATE product_work_claims_v1 SET lease_until='malformed'","UPDATE product_service_work_bindings_v1 SET service_revision=2","UPDATE product_work_backlog_v1 SET attempts=2"] {
            let mut f=TaskClaimFixture::new();f.authorize();f.claim();require(require(Connection::open(f.store.path())).execute_batch(sql));rejected_without_repair(&f);
        }
        let mut f=TaskClaimFixture::new();f.authorize();f.claim();let r=f.revoke_request(1);require(f.store.revoke_a01_task(&f.session,&r));
        f.clock.set(now()+Duration::hours(6));
        require(require(Connection::open(f.store.path())).execute_batch("UPDATE product_accounts_v1 SET revision=revision+1; UPDATE product_services_v1 SET revision=revision+1; UPDATE product_scope_source_instances_v1 SET enabled=0,revision=revision+1"));
        let before=preserved(&f.dir);let f=f.reopen();assert!(before==preserved(&f.dir),"A01 historical reopen repaired expired/revoked authority");
    }

}
