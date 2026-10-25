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
    path: PathBuf,
    installation_id: String,
    database_id: String,
    clock: std::sync::Arc<dyn ProductClock>,
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

    pub fn open(path: &Path, config: ProductConfig) -> Result<Self, ProductError> {
        Self::open_with_clock(path, config, std::sync::Arc::new(SystemProductClock))
    }

    pub fn open_with_clock(
        path: &Path,
        config: ProductConfig,
        clock: std::sync::Arc<dyn ProductClock>,
    ) -> Result<Self, ProductError> {
        if path.as_os_str().is_empty()
            || config.installation_id.trim().is_empty()
            || config.database_id.trim().is_empty()
        {
            return Err(ProductError::InvalidConfig);
        }
        let path = normalized_path(path)?;
        let mut conn = Connection::open(&path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
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
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            create_product_publish_schema(&tx)?;
            tx.commit()?;
            return Ok(Self {
                conn,
                path,
                installation_id: config.installation_id,
                database_id: config.database_id,
                clock,
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
                tx.execute("INSERT INTO product_meta_v1(singleton,installation_id,database_id,schema_version) VALUES(1,?1,?2,3)", params![config.installation_id, config.database_id])?;
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
        tx.commit()?;
        Ok(Self {
            conn,
            path,
            installation_id: config.installation_id,
            database_id: config.database_id,
            clock,
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
        let scope_revision = revision + 1;
        let grant_revision = old.map(|(_, revision, _)| revision + 1).unwrap_or(1);
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
            || current_state != "waiting"
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
