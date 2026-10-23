//! Product-owned local review result finalization and its immutable v3 wire.
//!
//! A local result is deliberately distinct from exec's emit and promotion
//! receipts. This module only records evidence already accepted by the
//! ProductStore-backed review host; it never dispatches work or changes S1.

use super::*;
use foundry_exec::{
    ArtifactLocation, ArtifactTuple, Attempt, Generation, HostKey, HostResult, ImmutableCommit,
    OrderId, RunId,
};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

#[cfg(feature = "p2-test-support")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum P2ReviewPreparation {
    Queued,
    Waiting,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductResultExpectation {
    pub host_key: HostKey,
    pub host_result_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProductResultInfo {
    pub result_id: String,
    pub order_id: String,
    pub lifecycle: i64,
    pub attempt: i64,
    pub run_id: i64,
    pub activation: i64,
    pub generation: i64,
    pub base_commit: String,
    pub result_commit: String,
    pub host_result_digest: String,
    pub tuple_digest: String,
    pub binding_digest: String,
    pub final_artifact_id: String,
    pub artifact_ids: Vec<String>,
    pub finalized_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductResultArtifactContent {
    pub result: ProductResultInfo,
    pub artifact: ProductArtifactContent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProductResultError {
    #[error("invalid product result input")]
    InvalidInput,
    #[error("product result caller is unauthorized")]
    Unauthorized,
    #[error("product result contract is unavailable")]
    Unavailable,
    #[error("product result is ineligible")]
    Ineligible,
    #[error("product result binding is stale")]
    StaleBinding,
    #[error("product result evidence is invalid")]
    InvalidEvidence,
    #[error("product result conflicts with stored evidence")]
    Conflict,
    #[error("stored product result evidence is corrupt")]
    CorruptEvidence,
    #[error("product result storage failed")]
    Storage,
    #[error("product result commit outcome is uncertain")]
    CommitUncertain,
}
impl From<rusqlite::Error> for ProductResultError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Storage
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultAuthorityV1 {
    service_id: String,
    service_revision: i64,
    credential_revision: i64,
    drive_review_grant_revision: i64,
    source_instance_id: String,
    source_generation: i64,
    source_instance_revision: i64,
    connector_revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultWorkV1 {
    delivery_id: String,
    worker_id: String,
    claim_epoch: i64,
    claim_fence: i64,
    source_revision: i64,
    payload_digest: String,
    task_kind: String,
    payload_snapshot: String,
    authority: ResultAuthorityV1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultArtifactV1 {
    artifact_id: String,
    provenance_digest: String,
    artifact_sha256: String,
    order_id: String,
    lifecycle: i64,
    source_instance_id: String,
    repository_key: String,
    admission_seq: i64,
    admission_hash: String,
    attempt: i64,
    journal_run: String,
    generation: i64,
    stage_sequence: i64,
    stage_id: String,
    pack_digest: String,
    relative_path: String,
    output_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultAcceptedHoldV1 {
    operation_id: String,
    receipt_json: String,
    snapshot_digest: String,
    progress_revision: i64,
    owner_id: String,
    epoch: i64,
    fence: i64,
    reserved_run_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultPayloadV1 {
    version: u32,
    installation_id: String,
    database_id: String,
    order_id: String,
    lifecycle: i64,
    source_instance_id: String,
    repository_key: String,
    source_generation: i64,
    source_instance_revision: i64,
    source_revision: i64,
    admission_seq: i64,
    admission_hash: String,
    intent: foundry_exec::ExecutionIntent,
    journal_run: String,
    host_result: HostResult,
    host_result_digest: String,
    tuple: ArtifactTuple,
    tuple_digest: String,
    binary_provenance: foundry_exec::ReviewBinaryProvenance,
    work: ResultWorkV1,
    accepted_holds: Vec<ResultAcceptedHoldV1>,
    artifacts: Vec<ResultArtifactV1>,
    s1_sequence: i64,
    s1_event_hash: String,
    final_artifact_id: String,
    finalized_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalResultReceiptV1 {
    version: u32,
    kind: String,
    installation_id: String,
    database_id: String,
    result_id: String,
    order_id: String,
    lifecycle: i64,
    attempt: i64,
    run_id: i64,
    activation: i64,
    generation: i64,
    host_result_digest: String,
    tuple_digest: String,
    binding_digest: String,
    final_artifact_id: String,
    finalized_at: String,
}

const V3_SCHEMA: &str = r#"
CREATE TABLE product_result_meta_v1(
 singleton INTEGER NOT NULL PRIMARY KEY CHECK(singleton=1),
 installation_id TEXT NOT NULL,database_id TEXT NOT NULL,
 result_contract_version INTEGER NOT NULL CHECK(result_contract_version=1),
 legacy_approval_cutoff_seq INTEGER NOT NULL CHECK(legacy_approval_cutoff_seq>=0),
 FOREIGN KEY(singleton) REFERENCES product_meta_v1(singleton));
CREATE TRIGGER product_result_meta_no_update BEFORE UPDATE ON product_result_meta_v1 BEGIN SELECT RAISE(ABORT,'result metadata is immutable'); END;
CREATE TRIGGER product_result_meta_no_delete BEFORE DELETE ON product_result_meta_v1 BEGIN SELECT RAISE(ABORT,'result metadata is immutable'); END;
CREATE TRIGGER product_result_meta_no_replace BEFORE INSERT ON product_result_meta_v1 WHEN EXISTS(SELECT 1 FROM product_result_meta_v1 WHERE singleton=NEW.singleton) BEGIN SELECT RAISE(ABORT,'result metadata already exists'); END;
CREATE TABLE product_result_legacy_hosts_v1(
 order_id TEXT NOT NULL,attempt INTEGER NOT NULL,run_id INTEGER NOT NULL,activation INTEGER NOT NULL,
 PRIMARY KEY(order_id,attempt,run_id,activation),
 FOREIGN KEY(order_id,attempt,run_id,activation) REFERENCES product_review_host_accept_v1(order_id,attempt,run_id,activation));
CREATE TRIGGER product_result_legacy_hosts_no_insert BEFORE INSERT ON product_result_legacy_hosts_v1 BEGIN SELECT RAISE(ABORT,'legacy host census is sealed'); END;
CREATE TRIGGER product_result_legacy_hosts_no_update BEFORE UPDATE ON product_result_legacy_hosts_v1 BEGIN SELECT RAISE(ABORT,'legacy host census is sealed'); END;
CREATE TRIGGER product_result_legacy_hosts_no_delete BEFORE DELETE ON product_result_legacy_hosts_v1 BEGIN SELECT RAISE(ABORT,'legacy host census is sealed'); END;
CREATE TABLE product_result_legacy_tuples_v1(
 order_id TEXT NOT NULL,attempt INTEGER NOT NULL,run_id INTEGER NOT NULL,generation INTEGER NOT NULL,
 PRIMARY KEY(order_id,attempt,run_id,generation),
 FOREIGN KEY(order_id,attempt,run_id,generation) REFERENCES artifact_tuples(order_id,attempt,run_id,generation));
CREATE TRIGGER product_tuples_no_update BEFORE UPDATE ON artifact_tuples BEGIN SELECT RAISE(ABORT,'product tuples are immutable'); END;
CREATE TRIGGER product_tuples_no_delete BEFORE DELETE ON artifact_tuples BEGIN SELECT RAISE(ABORT,'product tuples are immutable'); END;
CREATE TRIGGER product_tuples_no_replace BEFORE INSERT ON artifact_tuples WHEN EXISTS(SELECT 1 FROM artifact_tuples t WHERE t.order_id=NEW.order_id AND t.attempt=NEW.attempt AND t.run_id=NEW.run_id AND t.generation=NEW.generation) BEGIN SELECT RAISE(ABORT,'product tuple already exists'); END;
CREATE TRIGGER product_result_legacy_tuples_no_insert BEFORE INSERT ON product_result_legacy_tuples_v1 BEGIN SELECT RAISE(ABORT,'legacy tuple census is sealed'); END;
CREATE TRIGGER product_result_legacy_tuples_no_update BEFORE UPDATE ON product_result_legacy_tuples_v1 BEGIN SELECT RAISE(ABORT,'legacy tuple census is sealed'); END;
CREATE TRIGGER product_result_legacy_tuples_no_delete BEFORE DELETE ON product_result_legacy_tuples_v1 BEGIN SELECT RAISE(ABORT,'legacy tuple census is sealed'); END;
CREATE TABLE product_review_authority_v1(
 key_json TEXT NOT NULL PRIMARY KEY,order_id TEXT NOT NULL,lifecycle INTEGER NOT NULL CHECK(lifecycle>0),
 attempt INTEGER NOT NULL CHECK(attempt>=0),run_id INTEGER NOT NULL CHECK(run_id>=0),activation INTEGER NOT NULL CHECK(activation>=0),
 journal_run TEXT NOT NULL,host_epoch INTEGER NOT NULL CHECK(host_epoch>=0),service_id TEXT NOT NULL,
 service_revision INTEGER NOT NULL CHECK(service_revision>0),credential_revision INTEGER NOT NULL CHECK(credential_revision>0),
 drive_review_grant_revision INTEGER NOT NULL CHECK(drive_review_grant_revision>0),source_instance_id TEXT NOT NULL,
 source_generation INTEGER NOT NULL CHECK(source_generation>0),source_instance_revision INTEGER NOT NULL CHECK(source_instance_revision>0),
 connector_revision INTEGER NOT NULL CHECK(connector_revision>0),
 UNIQUE(order_id,attempt,run_id,activation),UNIQUE(order_id,attempt,journal_run),
 FOREIGN KEY(key_json) REFERENCES product_review_bindings_v1(key_json),
 FOREIGN KEY(order_id,attempt,run_id,activation) REFERENCES product_review_host_accept_v1(order_id,attempt,run_id,activation),
 FOREIGN KEY(order_id,attempt,journal_run) REFERENCES product_review_work_bindings_v1(order_id,attempt,run_id));
CREATE TRIGGER product_review_authority_no_update BEFORE UPDATE ON product_review_authority_v1 BEGIN SELECT RAISE(ABORT,'review authority is immutable'); END;
CREATE TRIGGER product_review_authority_no_delete BEFORE DELETE ON product_review_authority_v1 BEGIN SELECT RAISE(ABORT,'review authority is immutable'); END;
CREATE TRIGGER product_review_authority_no_replace BEFORE INSERT ON product_review_authority_v1 WHEN EXISTS(SELECT 1 FROM product_review_authority_v1 a WHERE a.key_json=NEW.key_json OR (a.order_id=NEW.order_id AND a.attempt=NEW.attempt AND a.run_id=NEW.run_id AND a.activation=NEW.activation) OR (a.order_id=NEW.order_id AND a.attempt=NEW.attempt AND a.journal_run=NEW.journal_run)) BEGIN SELECT RAISE(ABORT,'review authority already exists'); END;
CREATE TRIGGER product_review_authority_no_legacy BEFORE INSERT ON product_review_authority_v1 WHEN EXISTS(SELECT 1 FROM product_result_legacy_hosts_v1 h WHERE h.order_id=NEW.order_id AND h.attempt=NEW.attempt AND h.run_id=NEW.run_id AND h.activation=NEW.activation) BEGIN SELECT RAISE(ABORT,'legacy authority cannot be backfilled'); END;
CREATE TABLE product_result_bindings_v1(
 result_id TEXT NOT NULL PRIMARY KEY,order_id TEXT NOT NULL,lifecycle INTEGER NOT NULL,attempt INTEGER NOT NULL,run_id INTEGER NOT NULL,activation INTEGER NOT NULL,generation INTEGER NOT NULL,
 host_result_digest TEXT NOT NULL,tuple_digest TEXT NOT NULL,binding_digest TEXT NOT NULL,payload_json TEXT NOT NULL,receipt_json TEXT NOT NULL,
 UNIQUE(order_id,lifecycle,attempt,run_id,activation,generation),UNIQUE(order_id,attempt,run_id,generation),UNIQUE(result_id,binding_digest,tuple_digest),
 FOREIGN KEY(order_id,attempt,run_id,generation) REFERENCES artifact_tuples(order_id,attempt,run_id,generation),
 FOREIGN KEY(order_id,attempt,run_id,activation) REFERENCES product_review_host_accept_v1(order_id,attempt,run_id,activation));
CREATE TRIGGER product_result_bindings_no_update BEFORE UPDATE ON product_result_bindings_v1 BEGIN SELECT RAISE(ABORT,'product result is immutable'); END;
CREATE TRIGGER product_result_bindings_no_delete BEFORE DELETE ON product_result_bindings_v1 BEGIN SELECT RAISE(ABORT,'product result is immutable'); END;
CREATE TRIGGER product_result_bindings_no_replace BEFORE INSERT ON product_result_bindings_v1 WHEN EXISTS(SELECT 1 FROM product_result_bindings_v1 r WHERE r.result_id=NEW.result_id OR (r.order_id=NEW.order_id AND r.lifecycle=NEW.lifecycle AND r.attempt=NEW.attempt AND r.run_id=NEW.run_id AND r.activation=NEW.activation AND r.generation=NEW.generation) OR (r.order_id=NEW.order_id AND r.attempt=NEW.attempt AND r.run_id=NEW.run_id AND r.generation=NEW.generation) OR (r.result_id=NEW.result_id AND r.binding_digest=NEW.binding_digest AND r.tuple_digest=NEW.tuple_digest)) BEGIN SELECT RAISE(ABORT,'product result already exists'); END;
CREATE TRIGGER product_result_bindings_no_legacy_tuple BEFORE INSERT ON product_result_bindings_v1 WHEN EXISTS(SELECT 1 FROM product_result_legacy_tuples_v1 t WHERE t.order_id=NEW.order_id AND t.attempt=NEW.attempt AND t.run_id=NEW.run_id AND t.generation=NEW.generation) BEGIN SELECT RAISE(ABORT,'legacy tuple cannot be adopted'); END;
CREATE TABLE product_approval_result_bindings_v1(
 operation_id TEXT NOT NULL PRIMARY KEY,result_id TEXT NOT NULL,binding_digest TEXT NOT NULL,tuple_digest TEXT NOT NULL,
 UNIQUE(result_id,binding_digest,tuple_digest),
 FOREIGN KEY(operation_id) REFERENCES product_outcomes_v1(operation_id),
 FOREIGN KEY(result_id,binding_digest,tuple_digest) REFERENCES product_result_bindings_v1(result_id,binding_digest,tuple_digest));
CREATE TRIGGER product_approval_result_bindings_no_update BEFORE UPDATE ON product_approval_result_bindings_v1 BEGIN SELECT RAISE(ABORT,'approval result binding is immutable'); END;
CREATE TRIGGER product_approval_result_bindings_no_delete BEFORE DELETE ON product_approval_result_bindings_v1 BEGIN SELECT RAISE(ABORT,'approval result binding is immutable'); END;
CREATE TRIGGER product_approval_result_bindings_no_replace BEFORE INSERT ON product_approval_result_bindings_v1 WHEN EXISTS(SELECT 1 FROM product_approval_result_bindings_v1 a WHERE a.operation_id=NEW.operation_id OR (a.result_id=NEW.result_id AND a.binding_digest=NEW.binding_digest AND a.tuple_digest=NEW.tuple_digest)) BEGIN SELECT RAISE(ABORT,'approval result binding already exists'); END;
"#;

pub(super) fn initialize_fresh_v3(
    tx: &Transaction<'_>,
    installation: &str,
    database: &str,
) -> Result<(), ProductError> {
    tx.execute_batch("CREATE TABLE IF NOT EXISTS artifact_tuples(order_id TEXT NOT NULL,attempt INTEGER NOT NULL,run_id INTEGER NOT NULL,generation INTEGER NOT NULL,immutable_commit TEXT NOT NULL,artifact_location TEXT NOT NULL,generations_json TEXT NOT NULL,PRIMARY KEY(order_id,attempt,run_id,generation));")?;
    tx.execute_batch(V3_SCHEMA)?;
    tx.execute("INSERT INTO product_result_meta_v1(singleton,installation_id,database_id,result_contract_version,legacy_approval_cutoff_seq) VALUES(1,?1,?2,1,0)",params![installation,database])?;
    Ok(())
}

pub(super) fn validate_v3_connection(
    conn: &Connection,
    installation: &str,
    database: &str,
) -> Result<(), ProductError> {
    let meta: Option<(String,String,i64)> = conn.query_row(
        "SELECT installation_id,database_id,result_contract_version FROM product_result_meta_v1 WHERE singleton=1", [],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).optional()?;
    if !matches!(meta,Some((ref i,ref d,1)) if i==installation && d==database) {
        return Err(ProductError::ContractMismatch);
    }
    for table in [
        "product_result_meta_v1",
        "product_review_authority_v1",
        "product_result_legacy_hosts_v1",
        "product_result_legacy_tuples_v1",
        "product_result_bindings_v1",
        "product_approval_result_bindings_v1",
        "artifact_tuples",
    ] {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(ProductError::ContractMismatch);
        }
    }
    for (table, required) in [
        (
            "artifact_tuples",
            &[
                "order_id",
                "attempt",
                "run_id",
                "generation",
                "immutable_commit",
                "artifact_location",
                "generations_json",
            ][..],
        ),
        (
            "product_review_authority_v1",
            &[
                "key_json",
                "order_id",
                "lifecycle",
                "attempt",
                "run_id",
                "activation",
                "journal_run",
                "host_epoch",
                "service_id",
                "service_revision",
                "credential_revision",
                "drive_review_grant_revision",
                "source_instance_id",
                "source_generation",
                "source_instance_revision",
                "connector_revision",
            ][..],
        ),
        (
            "product_result_bindings_v1",
            &[
                "result_id",
                "order_id",
                "lifecycle",
                "attempt",
                "run_id",
                "activation",
                "generation",
                "host_result_digest",
                "tuple_digest",
                "binding_digest",
                "payload_json",
                "receipt_json",
            ][..],
        ),
        (
            "product_approval_result_bindings_v1",
            &[
                "operation_id",
                "result_id",
                "binding_digest",
                "tuple_digest",
            ][..],
        ),
    ] {
        let columns: Vec<String> = {
            let mut st = conn.prepare(&format!("PRAGMA table_info({table})"))?;
            st.query_map([], |r| r.get(1))?.collect::<Result<_, _>>()?
        };
        if required
            .iter()
            .any(|name| !columns.iter().any(|c| c == name))
        {
            return Err(ProductError::ContractMismatch);
        }
    }
    for trigger in [
        "product_tuples_no_update",
        "product_tuples_no_delete",
        "product_tuples_no_replace",
        "product_result_meta_no_update",
        "product_result_meta_no_delete",
        "product_result_meta_no_replace",
        "product_result_legacy_hosts_no_insert",
        "product_result_legacy_hosts_no_update",
        "product_result_legacy_hosts_no_delete",
        "product_result_legacy_tuples_no_insert",
        "product_result_legacy_tuples_no_update",
        "product_result_legacy_tuples_no_delete",
        "product_review_authority_no_update",
        "product_review_authority_no_delete",
        "product_review_authority_no_replace",
        "product_review_authority_no_legacy",
        "product_result_bindings_no_update",
        "product_result_bindings_no_delete",
        "product_result_bindings_no_replace",
        "product_result_bindings_no_legacy_tuple",
        "product_approval_result_bindings_no_update",
        "product_approval_result_bindings_no_delete",
        "product_approval_result_bindings_no_replace",
    ] {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='trigger' AND name=?1)",
            [trigger],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(ProductError::ContractMismatch);
        }
    }
    Ok(())
}

impl ProductStore {
    #[cfg(feature = "p2-test-support")]
    pub fn prepare_p2_review_fixture(
        &mut self,
        service: &ProductServiceContext,
        admission: &AdmissionReceipt,
        delivery_id: &str,
        target: P2ReviewPreparation,
    ) -> Result<(), ProductError> {
        if delivery_id.trim().is_empty() || admission.lifecycle <= 0 {
            return Err(ProductError::InvalidConfig);
        }
        let now = self.clock.now();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        service::require_service_context_in(&tx, service, ProductServiceAction::DriveReview, now)?;
        let exact:bool=tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM product_orders_v1 o JOIN product_intake_inbox_v1 i ON i.delivery_id=?2 JOIN product_intake_source_bindings_v1 b ON b.delivery_id=i.delivery_id JOIN product_transition_receipts_v1 r ON r.seq=o.admission_seq AND r.event_hash=o.admission_hash JOIN product_scope_order_provenance_v1 p ON p.order_id=o.order_id AND p.lifecycle=o.lifecycle WHERE o.order_id=?1 AND o.lifecycle=?3 AND o.admission_seq=?4 AND o.admission_hash=?5 AND o.connector_id=?6 AND o.repository_id=?7 AND i.state='acked' AND i.connector_id=?6 AND i.repository_id=?7 AND b.source_instance_id=?8 AND b.source_revision=?9 AND b.source_instance_revision=?10 AND b.source_generation=?11 AND p.source_instance_id=?8 AND p.verified=1 AND r.operation='github-admission' AND EXISTS(SELECT 1 FROM product_work_backlog_v1 w WHERE w.order_id=o.order_id AND w.lifecycle=o.lifecycle AND w.delivery_id=?2 AND w.state='waiting_unsupported') AND NOT EXISTS(SELECT 1 FROM product_work_claims_v1 c WHERE c.order_id=o.order_id AND c.lifecycle=o.lifecycle) AND NOT EXISTS(SELECT 1 FROM product_review_host_accept_v1 h WHERE h.order_id=o.order_id) AND NOT EXISTS(SELECT 1 FROM product_review_run_reservations_v1 x WHERE x.order_id=o.order_id))",
            params![admission.order_id,delivery_id,admission.lifecycle,admission.sequence,admission.event_hash,service.connector_id,service.repository_id,service.source_instance_id,service.connector_revision,service.source_instance_revision,service.source_generation],|r|r.get(0),
        )?;
        if !exact {
            return Err(ProductError::Invariant(
                "P2 preparation requires exact acknowledged admission and fresh work",
            ));
        }
        let (head, _hash, lifecycle, attempt, state) = verified_order_in(&tx, &admission.order_id)
            .map_err(|_| ProductError::Invariant("P2 source lifecycle is invalid"))?;
        if i64::from(lifecycle) + 1 != admission.lifecycle || state != "queued" || attempt != 0 {
            return Err(ProductError::Invariant(
                "P2 preparation requires initial queued lifecycle",
            ));
        }
        let changed=tx.execute("UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id=?1 AND lifecycle=?2 AND delivery_id=?3 AND state='waiting_unsupported'",params![admission.order_id,admission.lifecycle,delivery_id])?;
        if changed != 1 {
            return Err(ProductError::Invariant("P2 backlog CAS failed"));
        }
        if target == P2ReviewPreparation::Waiting {
            let mut rows = read_ledger_rows(&tx)?;
            let mut order = crate::replay::replay(&rows, crate::sqlite::SqliteLedger::verify_rows)
                .map_err(|_| ProductError::Invariant("P2 S1 replay failed"))?
                .into_iter()
                .rev()
                .find(|r| r.order_id == admission.order_id)
                .ok_or(ProductError::Invariant("P2 order missing"))?
                .order;
            for event in [
                foundry_domain::OrderEvent::DieStarted,
                foundry_domain::OrderEvent::DieSucceeded { attempt: 1 },
                foundry_domain::OrderEvent::InspectionPassed,
            ] {
                append_p2_transition(
                    &tx,
                    &mut rows,
                    &mut order,
                    &admission.order_id,
                    admission.lifecycle,
                    event,
                    now,
                )?;
            }
            let _ = head;
        }
        tx.commit()?;
        Ok(())
    }

    /// Validate an existing v3 database using a read-only connection. This
    /// never creates, repairs, migrates, or changes SQLite pragmas.
    pub fn validate_product_results_v3_read_only(
        path: &std::path::Path,
        config: &ProductConfig,
    ) -> Result<(), ProductError> {
        use rusqlite::OpenFlags;
        let conn = rusqlite::Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let contract: Option<String> = conn
            .query_row(
                "SELECT contract FROM foundry_contract WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let meta:Option<(String,String,i64)>=conn.query_row("SELECT installation_id,database_id,schema_version FROM product_meta_v1 WHERE singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let review:Option<(String,String,i64)>=conn.query_row("SELECT installation_id,database_id,schema_version FROM product_review_meta_v1 WHERE singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if contract.as_deref() != Some("ProductAtomicV1")
            || !matches!(meta,Some((ref i,ref d,3)) if i==&config.installation_id&&d==&config.database_id)
            || !matches!(review,Some((ref i,ref d,1)) if i==&config.installation_id&&d==&config.database_id)
        {
            return Err(ProductError::ContractMismatch);
        }
        validate_v3_connection(&conn, &config.installation_id, &config.database_id)
    }

    /// Explicit maintenance-only, additive ProductAtomicV1 schema migration.
    /// This path deliberately does not call `open` and never creates a file.
    pub fn migrate_product_results_v2(
        path: &std::path::Path,
        config: &ProductConfig,
    ) -> Result<(), ProductError> {
        use rusqlite::OpenFlags;
        if config.installation_id.trim().is_empty()
            || config.database_id.trim().is_empty()
            || !path.is_file()
        {
            return Err(ProductError::InvalidConfig);
        }
        let canonical = path
            .canonicalize()
            .map_err(|_| ProductError::InvalidConfig)?;
        let mut conn = rusqlite::Connection::open_with_flags(
            canonical,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let contract: Option<String> = tx
            .query_row(
                "SELECT contract FROM foundry_contract WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let meta: Option<(String,String,i64)> = tx.query_row(
            "SELECT installation_id,database_id,schema_version FROM product_meta_v1 WHERE singleton=1", [],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        ).optional()?;
        let review: Option<(String,String,i64)> = tx.query_row(
            "SELECT installation_id,database_id,schema_version FROM product_review_meta_v1 WHERE singleton=1", [],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        ).optional()?;
        if contract.as_deref() == Some("ProductAtomicV1")
            && matches!(meta,Some((ref i,ref d,3)) if i==&config.installation_id&&d==&config.database_id)
            && matches!(review,Some((ref i,ref d,1)) if i==&config.installation_id&&d==&config.database_id)
        {
            validate_v3_connection(&tx, &config.installation_id, &config.database_id)?;
            tx.commit()?;
            return Ok(());
        }
        if contract.as_deref() != Some("ProductAtomicV1")
            || !matches!(meta,Some((ref i,ref d,2)) if i==&config.installation_id && d==&config.database_id)
            || !matches!(review,Some((ref i,ref d,1)) if i==&config.installation_id && d==&config.database_id)
        {
            return Err(ProductError::ContractMismatch);
        }
        for required in [
            "ledger",
            "product_outcomes_v1",
            "product_review_host_accept_v1",
            "product_review_bindings_v1",
            "product_review_work_bindings_v1",
        ] {
            let present: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                [required],
                |r| r.get(0),
            )?;
            if !present {
                return Err(ProductError::ContractMismatch);
            }
        }
        let tuple_present: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='artifact_tuples')",[],|r|r.get(0))?;
        if !tuple_present {
            tx.execute_batch("CREATE TABLE artifact_tuples(order_id TEXT NOT NULL,attempt INTEGER NOT NULL,run_id INTEGER NOT NULL,generation INTEGER NOT NULL,immutable_commit TEXT NOT NULL,artifact_location TEXT NOT NULL,generations_json TEXT NOT NULL,PRIMARY KEY(order_id,attempt,run_id,generation));")?;
        } else {
            let columns: Vec<String> = {
                let mut st = tx.prepare("PRAGMA table_info(artifact_tuples)")?;
                st.query_map([], |r| r.get(1))?.collect::<Result<_, _>>()?
            };
            let expected = [
                "order_id",
                "attempt",
                "run_id",
                "generation",
                "immutable_commit",
                "artifact_location",
                "generations_json",
            ];
            if expected.iter().any(|c| !columns.iter().any(|v| v == c)) {
                return Err(ProductError::ContractMismatch);
            }
        }
        tx.execute_batch(V3_SCHEMA)?;
        for name in [
            "product_result_legacy_hosts_no_insert",
            "product_result_legacy_hosts_no_update",
            "product_result_legacy_hosts_no_delete",
            "product_result_legacy_tuples_no_insert",
            "product_result_legacy_tuples_no_update",
            "product_result_legacy_tuples_no_delete",
        ] {
            tx.execute_batch(&format!("DROP TRIGGER {name};"))?;
        }
        tx.execute("INSERT INTO product_result_legacy_hosts_v1(order_id,attempt,run_id,activation) SELECT order_id,attempt,run_id,activation FROM product_review_host_accept_v1",[])?;
        tx.execute("INSERT INTO product_result_legacy_tuples_v1(order_id,attempt,run_id,generation) SELECT order_id,attempt,run_id,generation FROM artifact_tuples",[])?;
        for ddl in [
            "CREATE TRIGGER product_result_legacy_hosts_no_insert BEFORE INSERT ON product_result_legacy_hosts_v1 BEGIN SELECT RAISE(ABORT,'legacy host census is sealed'); END;",
            "CREATE TRIGGER product_result_legacy_hosts_no_update BEFORE UPDATE ON product_result_legacy_hosts_v1 BEGIN SELECT RAISE(ABORT,'legacy host census is sealed'); END;",
            "CREATE TRIGGER product_result_legacy_hosts_no_delete BEFORE DELETE ON product_result_legacy_hosts_v1 BEGIN SELECT RAISE(ABORT,'legacy host census is sealed'); END;",
            "CREATE TRIGGER product_result_legacy_tuples_no_insert BEFORE INSERT ON product_result_legacy_tuples_v1 BEGIN SELECT RAISE(ABORT,'legacy tuple census is sealed'); END;",
            "CREATE TRIGGER product_result_legacy_tuples_no_update BEFORE UPDATE ON product_result_legacy_tuples_v1 BEGIN SELECT RAISE(ABORT,'legacy tuple census is sealed'); END;",
            "CREATE TRIGGER product_result_legacy_tuples_no_delete BEFORE DELETE ON product_result_legacy_tuples_v1 BEGIN SELECT RAISE(ABORT,'legacy tuple census is sealed'); END;",
        ] {
            tx.execute_batch(ddl)?;
        }
        let cutoff: i64 =
            tx.query_row("SELECT COALESCE(MAX(seq),0) FROM ledger", [], |r| r.get(0))?;
        tx.execute("INSERT INTO product_result_meta_v1(singleton,installation_id,database_id,result_contract_version,legacy_approval_cutoff_seq) VALUES(1,?1,?2,1,?3)",params![config.installation_id,config.database_id,cutoff])?;
        let changed = tx.execute(
            "UPDATE product_meta_v1 SET schema_version=3 WHERE singleton=1 AND schema_version=2",
            [],
        )?;
        if changed != 1 {
            return Err(ProductError::ContractMismatch);
        }
        tx.commit()?;
        Ok(())
    }
}

pub fn product_host_result_digest(result: &HostResult) -> Result<String, ProductResultError> {
    let encoded = serde_json::to_vec(result).map_err(|_| ProductResultError::InvalidEvidence)?;
    Ok(crate::product_scope::framed_hash(
        "foundry-product-host-result-v1",
        &[std::str::from_utf8(&encoded).map_err(|_| ProductResultError::InvalidEvidence)?],
    ))
}

impl ProductStore {
    /// Atomically records a local result tuple and its immutable binding after
    /// validating the genuine ProductStore host's terminal evidence.
    pub fn finalize_product_review_result(
        &mut self,
        service: &ProductServiceContext,
        expected: &ProductResultExpectation,
    ) -> Result<ProductResultInfo, ProductResultError> {
        validate_expectation(expected)?;
        let now = self.clock.now();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| ProductResultError::Storage)?;
        service::require_service_context_in(&tx, service, ProductServiceAction::DriveReview, now)
            .map_err(service_result_error)?;
        let coordinates = coordinates(&expected.host_key)?;
        let (order_id, attempt, run_id, activation) = coordinates;
        let (head_seq, head_hash, lifecycle, actual_attempt, state) =
            verified_order_in(&tx, &order_id)?;
        let lifecycle_number = i64::from(lifecycle)
            .checked_add(1)
            .ok_or(ProductResultError::InvalidInput)?;
        require_requested_source_in(&tx, service, &order_id, lifecycle_number)?;
        let identity = result_identity(
            &self.installation_id,
            &self.database_id,
            &order_id,
            lifecycle_number,
            attempt,
            run_id,
            activation,
            0,
        );
        if let Some(info) = read_stored_result_in(
            &tx,
            &identity,
            &expected.host_result_digest,
            &order_id,
            attempt,
            run_id,
            0,
        )? {
            tx.commit().map_err(|_| ProductResultError::Storage)?;
            return Ok(info);
        }
        // A failed or aborted accepted hold is an ineligible terminal outcome,
        // even when continuation has already advanced the active binding or
        // work claim beyond the run being finalized. Classify it before those
        // live-currency checks; successful decisions retain their usual
        // StaleBinding behavior when authority is revoked.
        if has_failed_accepted_hold_decision_in(&tx, &order_id, lifecycle_number, attempt)? {
            return Err(ProductResultError::Ineligible);
        }
        if actual_attempt != u32::try_from(attempt).map_err(|_| ProductResultError::InvalidInput)? {
            return Err(ProductResultError::StaleBinding);
        }
        let order: (i64,String,String,i64,String,String,String) = tx.query_row(
            "SELECT lifecycle,connector_id,repository_id,source_revision,admission_hash,task_kind,payload_snapshot FROM product_orders_v1 WHERE order_id=?1",
            [&order_id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)),
        ).map_err(|_|ProductResultError::Storage)?;
        let (
            recorded_lifecycle,
            connector_id,
            repository_id,
            source_revision,
            admission_hash,
            task_kind,
            payload_snapshot,
        ) = order;
        if recorded_lifecycle != lifecycle_number
            || source_revision != service.connector_revision
            || connector_id != service.connector_id
            || repository_id != service.repository_id
        {
            return Err(ProductResultError::StaleBinding);
        }
        let scoped_repository: Option<(String, String, i64, String, i64)> = tx
            .query_row(
                "SELECT p.source_instance_id,i.repository_key,p.admission_seq,p.admission_hash,p.verified
                 FROM product_scope_order_provenance_v1 p
                 JOIN product_scope_source_instances_v1 i USING(source_instance_id)
                 WHERE p.order_id=?1 AND p.lifecycle=?2",
                params![order_id, lifecycle_number],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()
            .map_err(|_| ProductResultError::Storage)?;
        let Some((
            bound_source,
            repository_key,
            bound_admission_seq,
            bound_admission_hash,
            verified,
        )) = scoped_repository
        else {
            return Err(ProductResultError::InvalidEvidence);
        };
        let admission_seq: i64 = tx
            .query_row(
                "SELECT admission_seq FROM product_orders_v1 WHERE order_id=?1",
                [&order_id],
                |r| r.get(0),
            )
            .map_err(|_| ProductResultError::Storage)?;
        if verified != 1
            || bound_source != service.source_instance_id
            || bound_admission_seq != admission_seq
            || bound_admission_hash != admission_hash
        {
            return Err(ProductResultError::InvalidEvidence);
        }
        let requested_key = host_key_json(&expected.host_key)?;
        // The durable active row must be internally consistent with its own binding
        // and epoch row, independent of the caller: admission writes them in one
        // transaction, so divergence is a malformed existing set (contract: existing
        // set malformed/inconsistent -> CorruptEvidence), never caller staleness.
        // Caller-coordinate mismatches below remain StaleBinding.
        let active_own: Option<(String,String,i64,i64)>=tx.query_row(
            "SELECT b.intent_json,a.journal_run,a.epoch,e.epoch FROM product_review_bindings_v1 b JOIN product_review_active_v1 a ON a.order_id=?1 JOIN product_review_epochs_v1 e ON e.order_id=?1 WHERE b.key_json=a.key_json",
            params![order_id],
            |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
        ).optional().map_err(|_| ProductResultError::Storage)?;
        let Some((own_intent_json,own_journal,own_active_epoch,own_epoch_row)) = active_own else {
            return Err(ProductResultError::CorruptEvidence);
        };
        let own_intent: foundry_exec::ExecutionIntent =
            serde_json::from_str(&own_intent_json).map_err(|_| ProductResultError::CorruptEvidence)?;
        let own_expected_journal = format!(
            "{}:{}:{}:{}",
            own_intent.order.0, own_intent.attempt.0, own_intent.run.0, own_intent.activation
        );
        if own_expected_journal != own_journal
            || own_active_epoch
                != i64::try_from(own_intent.epoch.0).map_err(|_| ProductResultError::InvalidInput)?
            || own_epoch_row != own_active_epoch
        {
            return Err(ProductResultError::CorruptEvidence);
        }
        let binding: Option<(String,String,String)>=tx.query_row(
            "SELECT b.intent_json,a.journal_run,a.key_json FROM product_review_bindings_v1 b JOIN product_review_active_v1 a ON a.order_id=?1 WHERE b.key_json=?2",
            params![order_id,requested_key],
            |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        ).optional().map_err(|_| ProductResultError::Storage)?;
        let (intent_json, journal_run, active_key) =
            binding.ok_or(ProductResultError::Ineligible)?;
        let intent: foundry_exec::ExecutionIntent =
            serde_json::from_str(&intent_json).map_err(|_| ProductResultError::CorruptEvidence)?;
        let intent_generation =
            i64::try_from(intent.generation.0).map_err(|_| ProductResultError::InvalidInput)?;
        if intent.order.0 != order_id
            || i64::try_from(intent.attempt.0).ok() != Some(attempt)
            || i64::try_from(intent.run.0).ok() != Some(run_id)
            || i64::try_from(intent.activation).ok() != Some(activation)
            || active_key != requested_key
        {
            return Err(ProductResultError::StaleBinding);
        }
        let host_row: Option<(String,String,String,String,String,String)>=tx.query_row(
            "SELECT h.result_json,r.status,r.base_commit,r.result_commit,r.result_json,r.provenance_json FROM product_review_host_results_v1 h JOIN product_review_runs_v1 r ON r.order_id=h.order_id AND r.attempt=h.attempt AND r.run_id=?4 WHERE h.order_id=?1 AND h.attempt=?2 AND h.run_id=?3 AND h.activation=?5",
            params![order_id,attempt,run_id,journal_run,activation],
            |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get::<_,Option<String>>(5)?.unwrap_or_default())),
        ).optional().map_err(|_|ProductResultError::Storage)?;
        let (host_json, run_status, run_base, _run_commit, run_result, provenance_json) =
            host_row.ok_or(ProductResultError::Ineligible)?;
        let host: HostResult =
            serde_json::from_str(&host_json).map_err(|_| ProductResultError::CorruptEvidence)?;
        let run_host: HostResult =
            serde_json::from_str(&run_result).map_err(|_| ProductResultError::CorruptEvidence)?;
        if host != run_host {
            return Err(ProductResultError::CorruptEvidence);
        }
        let digest = product_host_result_digest(&host)?;
        if digest != expected.host_result_digest {
            return Err(ProductResultError::InvalidEvidence);
        }
        if !matches!(run_status.as_str(), "passed" | "resolved")
            || host.exit_code != 0
            || host.held.is_some()
            || run_base != host.base_commit
            || !full_commit(&host.base_commit)
            || !full_commit(&host.result_commit)
        {
            return Err(ProductResultError::Ineligible);
        }
        if host.generations.is_empty() {
            return Err(ProductResultError::InvalidEvidence);
        }
        if state != "waiting" {
            return Err(ProductResultError::StaleBinding);
        }
        let work: Option<(String,String,i64,i64,i64,String,String,String)> = tx.query_row(
            "SELECT delivery_id,worker_id,claim_epoch,claim_fence,source_revision,payload_digest,task_kind,payload_snapshot FROM product_review_work_bindings_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3",
            params![order_id,attempt,journal_run],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?)),
        ).optional().map_err(|_|ProductResultError::Storage)?;
        let work = work.ok_or(ProductResultError::CorruptEvidence)?;
        let current_claim: Option<(String,i64,i64,String,String)> = tx.query_row(
            "SELECT owner_id,epoch,fence,state,lease_until FROM product_work_claims_v1 WHERE order_id=?1 AND lifecycle=?2",
            params![order_id,lifecycle_number],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
        ).optional().map_err(|_|ProductResultError::Storage)?;
        let backlog: Option<(String,String)> = tx.query_row("SELECT delivery_id,state FROM product_work_backlog_v1 WHERE order_id=?1 AND lifecycle=?2",params![order_id,lifecycle_number],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|_|ProductResultError::Storage)?;
        let Some((owner, epoch, fence, claim_state, _lease)) = current_claim else {
            return Err(ProductResultError::StaleBinding);
        };
        let Some((backlog_delivery, backlog_state)) = backlog else {
            return Err(ProductResultError::StaleBinding);
        };
        if owner != work.1
            || epoch != work.2
            || fence != work.3
            || work.4 != source_revision
            || work.0 != backlog_delivery
            || work.5 != crate::product::payload_digest(&work.6, &work.7)
            || work.5 != crate::product::payload_digest(&task_kind, &payload_snapshot)
        {
            return Err(ProductResultError::StaleBinding);
        }
        let (lineage_runs, accepted_holds) = completed_result_ancestry_in(
            &tx,
            &self.installation_id,
            &order_id,
            lifecycle_number,
            &intent,
        )?;
        let continuation = !accepted_holds.is_empty();
        let expected_state = if continuation { "claimed" } else { "completed" };
        if claim_state != expected_state || backlog_state != expected_state {
            return Err(ProductResultError::Ineligible);
        }
        let key_json_value = requested_key;
        let authority: Option<(String,i64,i64,i64,String,i64,i64,i64)> = tx.query_row(
            "SELECT service_id,service_revision,credential_revision,drive_review_grant_revision,source_instance_id,source_generation,source_instance_revision,connector_revision FROM product_review_authority_v1 WHERE key_json=?1",
            [&key_json_value],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?)),
        ).optional().map_err(|_|ProductResultError::Storage)?;
        let census_member:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM product_result_legacy_hosts_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND activation=?4)",params![order_id,attempt,run_id,activation],|r|r.get(0)).map_err(|_|ProductResultError::Storage)?;
        if census_member && authority.is_some() {
            return Err(ProductResultError::CorruptEvidence);
        }
        let Some((
            service_id,
            service_revision,
            credential_revision,
            grant_revision,
            source_instance_id,
            source_generation,
            source_instance_revision,
            connector_revision,
        )) = authority
        else {
            let legacy = legacy_host_or_descendant_in(&tx, &order_id, attempt, run_id, activation)?;
            return Err(if legacy {
                ProductResultError::Ineligible
            } else {
                ProductResultError::CorruptEvidence
            });
        };
        let live: Option<(i64,i64,i64,i64)> = tx.query_row(
            "SELECT s.revision,c.credential_revision,g.grant_revision,i.generation FROM product_services_v1 s JOIN product_service_credentials_v1 c USING(service_id) JOIN product_service_grants_v1 g ON g.service_id=s.service_id AND g.source_instance_id=?2 AND g.action='drive_review' AND g.enabled=1 JOIN product_scope_source_instances_v1 i ON i.source_instance_id=?2 WHERE s.service_id=?1 AND s.enabled=1 AND s.lease_until>?3 AND c.expires_at>?3 AND i.enabled=1",
            params![service_id,source_instance_id,now.to_rfc3339()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
        ).optional().map_err(|_|ProductResultError::Storage)?;
        if !matches!(live,Some((sr,cr,gr,sg)) if sr==service_revision && cr==credential_revision && gr==grant_revision && sg==source_generation)
            || service_id != service.service_id
            || source_instance_id != service.source_instance_id
            || connector_revision != service.connector_revision
            || source_instance_revision != service.source_instance_revision
        {
            return Err(ProductResultError::StaleBinding);
        }
        let mut artifact_rows: Vec<ResultArtifactV1> = Vec::new();
        for ancestor_run in &lineage_runs {
            let mut statement=tx.prepare("SELECT a.artifact_id,a.provenance_digest,a.artifact_sha256,a.order_id,a.lifecycle,a.source_instance_id,a.repository_key,a.admission_seq,a.admission_hash,a.attempt,a.run_id,a.generation,a.stage_sequence,a.stage_id,r.pack_digest,a.relative_path,s.output_json FROM product_artifacts_v1 a JOIN product_review_stages_v1 s ON s.order_id=a.order_id AND s.run_id=a.run_id AND s.sequence=a.stage_sequence AND s.stage_id=a.stage_id JOIN product_review_runs_v1 r ON r.order_id=a.order_id AND r.attempt=a.attempt AND r.run_id=a.run_id WHERE a.order_id=?1 AND a.attempt=?2 AND a.run_id=?3 AND s.status='complete' ORDER BY a.stage_sequence").map_err(|_|ProductResultError::Storage)?;
            let rows = statement
                .query_map(params![order_id, attempt, &ancestor_run.2], |r| {
                    Ok(ResultArtifactV1 {
                        artifact_id: r.get(0)?,
                        provenance_digest: r.get(1)?,
                        artifact_sha256: r.get(2)?,
                        order_id: r.get(3)?,
                        lifecycle: r.get(4)?,
                        source_instance_id: r.get(5)?,
                        repository_key: r.get(6)?,
                        admission_seq: r.get(7)?,
                        admission_hash: r.get(8)?,
                        attempt: r.get(9)?,
                        journal_run: r.get(10)?,
                        generation: r.get(11)?,
                        stage_sequence: r.get(12)?,
                        stage_id: r.get(13)?,
                        pack_digest: r.get(14)?,
                        relative_path: r.get(15)?,
                        output_json: r.get(16)?,
                    })
                })
                .map_err(|_| ProductResultError::Storage)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| ProductResultError::CorruptEvidence)?;
            artifact_rows.extend(rows);
        }
        if artifact_rows.is_empty() {
            return Err(ProductResultError::InvalidEvidence);
        }
        let mut artifact_ids = std::collections::BTreeSet::new();
        for (ancestor_number, _ancestor_activation, ancestor_run) in &lineage_runs {
            let completed_stages:i64=tx.query_row("SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND run_id=?2 AND status='complete'",params![order_id,ancestor_run],|r|r.get(0)).map_err(|_|ProductResultError::Storage)?;
            let run_artifact_count = artifact_rows
                .iter()
                .filter(|artifact| artifact.journal_run == *ancestor_run)
                .count();
            if completed_stages
                != i64::try_from(run_artifact_count)
                    .map_err(|_| ProductResultError::CorruptEvidence)?
                || artifact_rows.iter().any(|artifact| {
                    artifact.journal_run == *ancestor_run && artifact.attempt != attempt
                })
            {
                return Err(ProductResultError::InvalidEvidence);
            }
            let recorded_run: Option<String> = tx.query_row(
                "SELECT run_id FROM product_review_runs_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3",
                params![order_id, attempt, ancestor_run],
                |row| row.get(0),
            ).optional().map_err(|_| ProductResultError::Storage)?;
            if recorded_run.as_deref() != Some(ancestor_run.as_str()) || *ancestor_number < 0 {
                return Err(ProductResultError::CorruptEvidence);
            }
        }
        for artifact in &artifact_rows {
            if !artifact_ids.insert(artifact.artifact_id.as_str()) {
                return Err(ProductResultError::CorruptEvidence);
            }
        }
        let root_path = self
            .artifact_root
            .as_deref()
            .ok_or(ProductResultError::Storage)?;
        let terminal_artifact = artifact_rows
            .last()
            .ok_or(ProductResultError::InvalidEvidence)?;
        if terminal_artifact.journal_run != journal_run
            || !registered_path_matches(root_path, &host.log_path, &terminal_artifact.relative_path)
        {
            return Err(ProductResultError::InvalidEvidence);
        }
        let final_artifact_id = terminal_artifact.artifact_id.clone();
        let root = self
            .artifact_root_handle
            .as_deref()
            .ok_or(ProductResultError::Storage)?;
        for artifact in &artifact_rows {
            if artifact.order_id != order_id
                || artifact.lifecycle != lifecycle_number
                || artifact.source_instance_id != service.source_instance_id
                || artifact.repository_key != repository_key
                || artifact.admission_seq != admission_seq
                || artifact.admission_hash != admission_hash
                || artifact.attempt != attempt
                || !lineage_runs
                    .iter()
                    .any(|(_, _, run)| run == &artifact.journal_run)
                || artifact.generation != intent_generation
                || artifact.pack_digest
                    != serde_json::from_str::<foundry_exec::ReviewBinaryProvenance>(
                        &provenance_json,
                    )
                    .map_err(|_| ProductResultError::CorruptEvidence)?
                    .pack_digest
            {
                return Err(ProductResultError::InvalidEvidence);
            }
            let expected_provenance = crate::product_scope::framed_hash(
                "foundry-product-stage-artifact-provenance-v1",
                &[
                    &artifact.order_id,
                    &artifact.lifecycle.to_string(),
                    &artifact.source_instance_id,
                    &artifact.repository_key,
                    &artifact.admission_seq.to_string(),
                    &artifact.admission_hash,
                    &artifact.attempt.to_string(),
                    &artifact.journal_run,
                    &artifact.generation.to_string(),
                    &artifact.stage_sequence.to_string(),
                    &artifact.stage_id,
                    &artifact.pack_digest,
                    &artifact.artifact_sha256,
                    &artifact.relative_path,
                ],
            );
            let expected_id = crate::product_scope::framed_hash(
                "foundry-product-artifact-id-v1",
                &[&expected_provenance],
            );
            if artifact.provenance_digest != expected_provenance
                || artifact.artifact_id != expected_id
                || serde_json::from_str::<serde_json::Value>(&artifact.output_json).is_err()
            {
                return Err(ProductResultError::InvalidEvidence);
            }
            super::review::read_confined_artifact(
                root,
                &artifact.relative_path,
                &artifact.artifact_sha256,
            )
            .map_err(|_| ProductResultError::InvalidEvidence)?;
        }
        let intent_commit = host
            .generations
            .first()
            .ok_or(ProductResultError::InvalidEvidence)?;
        let root_run = lineage_runs
            .first()
            .map(|(_, _, journal)| journal)
            .ok_or(ProductResultError::CorruptEvidence)?;
        let root_commit: String = tx
            .query_row(
                "SELECT result_commit FROM product_review_runs_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3",
                params![order_id, attempt, root_run],
                |row| row.get(0),
            )
            .map_err(|_| ProductResultError::Storage)?;
        if intent_commit.generation != 0
            || !intent_commit.fix_log_path.is_empty()
            || intent_commit.commit != root_commit
        {
            return Err(ProductResultError::InvalidEvidence);
        }
        for (index, generation) in host.generations.iter().enumerate() {
            let chain_index = u64::try_from(index).map_err(|_| ProductResultError::InvalidInput)?;
            if generation.generation != chain_index || !full_commit(&generation.commit) {
                return Err(ProductResultError::InvalidEvidence);
            }
        }
        let mut head = root_commit;
        let mut next_generation = 1usize;
        for artifact in &artifact_rows {
            let output: serde_json::Value = serde_json::from_str(&artifact.output_json)
                .map_err(|_| ProductResultError::CorruptEvidence)?;
            let producer = host
                .generations
                .iter()
                .enumerate()
                .skip(1)
                .find(|(_, generation)| {
                    registered_path_matches(
                        root_path,
                        &generation.fix_log_path,
                        &artifact.relative_path,
                    )
                });
            if let Some((index, generation)) = producer {
                if index != next_generation
                    || output
                        .get("produced_commit")
                        .and_then(serde_json::Value::as_str)
                        != Some(generation.commit.as_str())
                    || output
                        .get("inspected_commit")
                        .and_then(serde_json::Value::as_str)
                        != Some(head.as_str())
                {
                    return Err(ProductResultError::InvalidEvidence);
                }
                head = generation.commit.clone();
                next_generation += 1;
            }
            if output.get("role").and_then(serde_json::Value::as_str) == Some("verify") {
                if output
                    .get("inspected_commit")
                    .and_then(serde_json::Value::as_str)
                    != Some(head.as_str())
                {
                    return Err(ProductResultError::InvalidEvidence);
                }
            }
        }
        if next_generation != host.generations.len() || head != host.result_commit {
            return Err(ProductResultError::InvalidEvidence);
        }
        let tuple = ArtifactTuple {
            order: OrderId(order_id.clone()),
            attempt: Attempt(u64::try_from(attempt).map_err(|_| ProductResultError::InvalidInput)?),
            run: RunId(u64::try_from(run_id).map_err(|_| ProductResultError::InvalidInput)?),
            generation: Generation(
                u64::try_from(intent_generation).map_err(|_| ProductResultError::InvalidInput)?,
            ),
            immutable_commit: ImmutableCommit(host.result_commit.clone()),
            artifact_location: ArtifactLocation(host.log_path.clone()),
            generations: host.generations.clone(),
        };
        let tuple_digest = tuple.digest();
        let artifacts = artifact_rows;
        let s1_event_hash = head_hash;
        let binary_provenance: foundry_exec::ReviewBinaryProvenance =
            serde_json::from_str(&provenance_json)
                .map_err(|_| ProductResultError::CorruptEvidence)?;
        let work_payload = ResultWorkV1 {
            delivery_id: work.0.clone(),
            worker_id: work.1.clone(),
            claim_epoch: work.2,
            claim_fence: work.3,
            source_revision: work.4,
            payload_digest: work.5.clone(),
            task_kind: work.6.clone(),
            payload_snapshot: work.7.clone(),
            authority: ResultAuthorityV1 {
                service_id,
                service_revision,
                credential_revision,
                drive_review_grant_revision: grant_revision,
                source_instance_id,
                source_generation,
                source_instance_revision,
                connector_revision,
            },
        };
        let finalized_at = now.to_rfc3339();
        let payload = ResultPayloadV1 {
            version: 1,
            installation_id: self.installation_id.clone(),
            database_id: self.database_id.clone(),
            order_id: order_id.clone(),
            lifecycle: lifecycle_number,
            source_instance_id: service.source_instance_id.clone(),
            repository_key,
            source_generation,
            source_instance_revision,
            source_revision,
            admission_seq,
            admission_hash,
            intent: intent.clone(),
            journal_run: journal_run.clone(),
            host_result: host.clone(),
            host_result_digest: digest.clone(),
            tuple: tuple.clone(),
            tuple_digest: tuple_digest.clone(),
            binary_provenance,
            work: work_payload,
            accepted_holds,
            artifacts,
            s1_sequence: head_seq,
            s1_event_hash,
            final_artifact_id: final_artifact_id.clone(),
            finalized_at: finalized_at.clone(),
        };
        let payload_json =
            serde_json::to_string(&payload).map_err(|_| ProductResultError::InvalidEvidence)?;
        let binding_digest = crate::product_scope::framed_hash(
            "foundry-product-result-binding-v1",
            &[&payload_json],
        );
        let result_id = identity;
        let receipt = LocalResultReceiptV1 {
            version: 1,
            kind: "local_review_result".into(),
            installation_id: self.installation_id.clone(),
            database_id: self.database_id.clone(),
            result_id: result_id.clone(),
            order_id: order_id.clone(),
            lifecycle: lifecycle_number,
            attempt,
            run_id,
            activation,
            generation: intent_generation,
            host_result_digest: digest.clone(),
            tuple_digest: tuple_digest.clone(),
            binding_digest: binding_digest.clone(),
            final_artifact_id: payload.final_artifact_id.clone(),
            finalized_at: finalized_at.clone(),
        };
        let receipt_json =
            serde_json::to_string(&receipt).map_err(|_| ProductResultError::InvalidEvidence)?;
        let existing_tuple:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM artifact_tuples WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND generation=?4)",params![order_id,attempt,run_id,receipt.generation],|r|r.get(0)).map_err(|_|ProductResultError::Storage)?;
        if existing_tuple {
            let legacy:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM product_result_legacy_tuples_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND generation=?4)",params![order_id,attempt,run_id,receipt.generation],|r|r.get(0)).map_err(|_|ProductResultError::Storage)?;
            if legacy {
                return Err(ProductResultError::Conflict);
            }
            let bound:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM product_result_bindings_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND generation=?4)",params![order_id,attempt,run_id,receipt.generation],|r|r.get(0)).map_err(|_|ProductResultError::Storage)?;
            return Err(if bound {
                ProductResultError::Conflict
            } else {
                ProductResultError::CorruptEvidence
            });
        }
        tx.execute("INSERT INTO artifact_tuples(order_id,attempt,run_id,generation,immutable_commit,artifact_location,generations_json) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![order_id,attempt,run_id,receipt.generation,tuple.immutable_commit.0,tuple.artifact_location.0,serde_json::to_string(&tuple.generations).map_err(|_|ProductResultError::InvalidEvidence)?]).map_err(|_|ProductResultError::Conflict)?;
        #[cfg(test)]
        result_finalizer_killpoint("after_tuple_insert");
        tx.execute("INSERT INTO product_result_bindings_v1(result_id,order_id,lifecycle,attempt,run_id,activation,generation,host_result_digest,tuple_digest,binding_digest,payload_json,receipt_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",params![result_id,order_id,lifecycle_number,attempt,run_id,activation,receipt.generation,digest,tuple_digest,binding_digest,payload_json,receipt_json]).map_err(|_|ProductResultError::Conflict)?;
        #[cfg(test)]
        result_finalizer_killpoint("after_result_binding_insert");
        let info = project_info(&payload, &result_id, &binding_digest)?;
        tx.commit()
            .map_err(|_| ProductResultError::CommitUncertain)?;
        #[cfg(test)]
        result_finalizer_killpoint("after_commit_before_response");
        Ok(info)
    }

    pub fn lookup_product_review_result(
        &mut self,
        service: &ProductServiceContext,
        expected: &ProductResultExpectation,
    ) -> Result<Option<ProductResultInfo>, ProductResultError> {
        validate_expectation(expected)?;
        let now = self.clock.now();
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|_| ProductResultError::Storage)?;
        service::require_service_context_in(&tx, service, ProductServiceAction::DriveReview, now)
            .map_err(service_result_error)?;
        let (order, attempt, run, activation) = coordinates(&expected.host_key)?;
        let (_, _, replay_lifecycle, actual_attempt, state) = verified_order_in(&tx, &order)?;
        let lifecycle = i64::from(replay_lifecycle) + 1;
        require_requested_source_in(&tx, service, &order, lifecycle)?;
        if actual_attempt != u32::try_from(attempt).map_err(|_| ProductResultError::InvalidInput)?
            || !matches!(state.as_str(), "waiting" | "publishing")
        {
            return Err(ProductResultError::StaleBinding);
        }
        let id = result_identity(
            &self.installation_id,
            &self.database_id,
            &order,
            lifecycle,
            attempt,
            run,
            activation,
            0,
        );
        let result = read_stored_result_in(
            &tx,
            &id,
            &expected.host_result_digest,
            &order,
            attempt,
            run,
            0,
        )?;
        tx.commit().map_err(|_| ProductResultError::Storage)?;
        Ok(result)
    }

    /// Authenticated result metadata read. The original source grant is
    /// checked before a result identifier is resolved.
    pub fn read_product_result(
        &mut self,
        session_digest: &str,
        order_id: &str,
        result_id: &str,
    ) -> Result<ProductResultInfo, ProductResultError> {
        if order_id.trim().is_empty() || !hex64(result_id) {
            return Err(ProductResultError::InvalidInput);
        }
        let now = self.clock.now();
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|_| ProductResultError::Storage)?;
        if !scope_enforced_in(&tx).map_err(|_| ProductResultError::Unavailable)? {
            return Err(ProductResultError::Unauthorized);
        }
        let principal = resolve_session_in(&tx, session_digest, now)
            .map_err(|_| ProductResultError::Unauthorized)?;
        if !ProductCapability::ReadArtifact.allowed_for_role(&principal.role) {
            return Err(ProductResultError::Unauthorized);
        }
        let scoped = require_order_capability_in(
            &tx,
            &principal,
            order_id,
            ProductCapability::ReadArtifact,
            false,
        )
        .map_err(|_| ProductResultError::Unauthorized)?;
        let row:Option<(String,String,String)>=tx.query_row("SELECT payload_json,receipt_json,host_result_digest FROM product_result_bindings_v1 WHERE result_id=?1 AND order_id=?2",params![result_id,order_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(|_|ProductResultError::Storage)?;
        let Some((payload_json, _receipt, host_digest)) = row else {
            return Err(ProductResultError::Unauthorized);
        };
        let info = read_stored_result_in(
            &tx,
            result_id,
            &host_digest,
            order_id,
            tx.query_row(
                "SELECT attempt FROM product_result_bindings_v1 WHERE result_id=?1",
                [result_id],
                |r| r.get(0),
            )
            .map_err(|_| ProductResultError::Storage)?,
            tx.query_row(
                "SELECT run_id FROM product_result_bindings_v1 WHERE result_id=?1",
                [result_id],
                |r| r.get(0),
            )
            .map_err(|_| ProductResultError::Storage)?,
            tx.query_row(
                "SELECT generation FROM product_result_bindings_v1 WHERE result_id=?1",
                [result_id],
                |r| r.get(0),
            )
            .map_err(|_| ProductResultError::Storage)?,
        )?
        .ok_or(ProductResultError::CorruptEvidence)?;
        let payload: ResultPayloadV1 =
            serde_json::from_str(&payload_json).map_err(|_| ProductResultError::CorruptEvidence)?;
        if payload.order_id != order_id || payload.source_instance_id != scoped.source_instance_id {
            return Err(ProductResultError::CorruptEvidence);
        }
        // Defect 6 (Gate2 rubric R3, contract lines 548/1215): result metadata
        // must not be a partial success that hides artifact-set or file
        // corruption until the artifact route is hit. Validate every
        // payload-bound artifact row and its confined bytes in this same
        // snapshot before returning metadata; row/byte mismatch is
        // CorruptEvidence (404), a missing artifact root is Storage (503).
        let root = self
            .artifact_root_handle
            .as_deref()
            .ok_or(ProductResultError::Storage)?;
        for artifact in &payload.artifacts {
            let row: Option<(String, String, String, String, String, i64, String, String, String)> =
                tx.query_row(
                    "SELECT provenance_digest,artifact_sha256,order_id,source_instance_id,repository_key,admission_seq,admission_hash,run_id,relative_path FROM product_artifacts_v1 WHERE artifact_id=?1 AND order_id=?2",
                    params![artifact.artifact_id, order_id],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                            r.get(7)?,
                            r.get(8)?,
                        ))
                    },
                )
                .optional()
                .map_err(|_| ProductResultError::Storage)?;
            let Some((
                provenance,
                sha,
                stored_order,
                source,
                repository,
                admission_seq,
                admission_hash,
                journal_run,
                path,
            )) = row
            else {
                return Err(ProductResultError::CorruptEvidence);
            };
            if provenance != artifact.provenance_digest
                || sha != artifact.artifact_sha256
                || stored_order != artifact.order_id
                || source != artifact.source_instance_id
                || repository != artifact.repository_key
                || admission_seq != artifact.admission_seq
                || admission_hash != artifact.admission_hash
                || journal_run != artifact.journal_run
                || path != artifact.relative_path
            {
                return Err(ProductResultError::CorruptEvidence);
            }
            review::read_confined_artifact(
                root,
                &artifact.relative_path,
                &artifact.artifact_sha256,
            )
            .map_err(|_| ProductResultError::CorruptEvidence)?;
        }
        tx.commit().map_err(|_| ProductResultError::Storage)?;
        Ok(info)
    }

    /// Authenticated result-bound artifact read. Result membership, current
    /// original-source grant, immutable artifact row and file digest are
    /// checked in one SQLite snapshot before any bytes are returned.
    pub fn read_product_result_artifact(
        &mut self,
        session_digest: &str,
        order_id: &str,
        result_id: &str,
        artifact_id: &str,
    ) -> Result<ProductResultArtifactContent, ProductResultError> {
        if order_id.trim().is_empty() || !hex64(result_id) || !hex64(artifact_id) {
            return Err(ProductResultError::InvalidInput);
        }
        let now = self.clock.now();
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|_| ProductResultError::Storage)?;
        if !scope_enforced_in(&tx).map_err(|_| ProductResultError::Unavailable)? {
            return Err(ProductResultError::Unauthorized);
        }
        let principal = resolve_session_in(&tx, session_digest, now)
            .map_err(|_| ProductResultError::Unauthorized)?;
        if !ProductCapability::ReadArtifact.allowed_for_role(&principal.role) {
            return Err(ProductResultError::Unauthorized);
        }
        let scoped = require_order_capability_in(
            &tx,
            &principal,
            order_id,
            ProductCapability::ReadArtifact,
            false,
        )
        .map_err(|_| ProductResultError::Unauthorized)?;
        let row:Option<(String,String,i64,i64,i64)>=tx.query_row("SELECT payload_json,host_result_digest,attempt,run_id,generation FROM product_result_bindings_v1 WHERE result_id=?1 AND order_id=?2",params![result_id,order_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(|_|ProductResultError::Storage)?;
        let Some((payload_json, host_digest, attempt, run, generation)) = row else {
            return Err(ProductResultError::Unauthorized);
        };
        let info = read_stored_result_in(
            &tx,
            result_id,
            &host_digest,
            order_id,
            attempt,
            run,
            generation,
        )?
        .ok_or(ProductResultError::CorruptEvidence)?;
        let payload: ResultPayloadV1 =
            serde_json::from_str(&payload_json).map_err(|_| ProductResultError::CorruptEvidence)?;
        if payload.source_instance_id != scoped.source_instance_id {
            return Err(ProductResultError::CorruptEvidence);
        }
        let artifact = payload
            .artifacts
            .iter()
            .find(|a| a.artifact_id == artifact_id)
            .ok_or(ProductResultError::Unauthorized)?;
        let row:Option<(String,String,String,String,String,i64,String,String,String)>=tx.query_row(
            "SELECT provenance_digest,artifact_sha256,order_id,source_instance_id,repository_key,admission_seq,admission_hash,run_id,relative_path FROM product_artifacts_v1 WHERE artifact_id=?1 AND order_id=?2",
            params![artifact_id,order_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?)),
        ).optional().map_err(|_|ProductResultError::Storage)?;
        let Some((
            provenance,
            sha,
            stored_order,
            source,
            repository,
            admission_seq,
            admission_hash,
            journal_run,
            path,
        )) = row
        else {
            return Err(ProductResultError::CorruptEvidence);
        };
        if provenance != artifact.provenance_digest
            || sha != artifact.artifact_sha256
            || stored_order != artifact.order_id
            || source != artifact.source_instance_id
            || repository != artifact.repository_key
            || admission_seq != artifact.admission_seq
            || admission_hash != artifact.admission_hash
            || journal_run != artifact.journal_run
            || path != artifact.relative_path
        {
            return Err(ProductResultError::CorruptEvidence);
        }
        let root = self
            .artifact_root_handle
            .as_deref()
            .ok_or(ProductResultError::Storage)?;
        let bytes = review::read_confined_artifact(
            root,
            &artifact.relative_path,
            &artifact.artifact_sha256,
        )
        .map_err(|_| ProductResultError::CorruptEvidence)?;
        let content = ProductArtifactContent {
            info: ProductArtifactInfo {
                artifact_id: artifact.artifact_id.clone(),
                order_id: artifact.order_id.clone(),
                lifecycle: artifact.lifecycle,
                attempt: artifact.attempt,
                run_id: artifact.journal_run.clone(),
                generation: artifact.generation,
                stage_sequence: artifact.stage_sequence,
                stage_id: artifact.stage_id.clone(),
                sha256: artifact.artifact_sha256.clone(),
            },
            bytes,
        };
        tx.commit().map_err(|_| ProductResultError::Storage)?;
        Ok(ProductResultArtifactContent {
            result: info,
            artifact: content,
        })
    }
}

#[cfg(test)]
fn result_finalizer_killpoint(boundary: &str) {
    if std::env::var("B1_PRODUCT_KILL_BOUNDARY").as_deref() != Ok(boundary) {
        return;
    }
    use std::io::{Read, Write};
    println!("B1_PRODUCT_RESULT_BOUNDARY:{boundary}");
    let _ = std::io::stdout().flush();
    let mut byte = [0_u8; 1];
    let _ = std::io::stdin().read(&mut byte);
}

#[cfg(test)]
mod finalizer_child_tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use serde::Deserialize;
    use std::io::Write;
    use std::path::PathBuf;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ChildManifestV1 {
        version: u32,
        operation: String,
        db_path: PathBuf,
        installation_id: String,
        database_id: String,
        artifact_root: PathBuf,
        session_root: PathBuf,
        claim_root: PathBuf,
        artifact_root_device: u64,
        artifact_root_inode: u64,
        connector_id: String,
        api_origin: String,
        repository_id: String,
        source_instance_id: String,
        source_generation: i64,
        source_instance_revision: i64,
        connector_revision: i64,
        service_id: String,
        service_bearer_path: PathBuf,
        test_time: String,
        host_order: String,
        host_attempt: i64,
        host_run: i64,
        host_activation: i64,
        host_result_digest: String,
        human_session_digest: Option<String>,
        human_session_bearer_path: Option<PathBuf>,
        approval: Option<ChildApprovalManifestV1>,
        expected_lifecycle: i64,
        delivery_id: String,
        worker_id: String,
        hold_operation_id: Option<String>,
        request_identity: String,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ChildApprovalManifestV1 {
        operation_id: String,
        request_key: String,
        grant_id: String,
        token_digest: String,
        order_id: String,
        expected_source_revision: i64,
        expected_attempt: i64,
        run_id: i64,
        generation: i64,
        artifact_digest: String,
        tuple_digest: String,
        intent: String,
        now: String,
    }

    fn read_manifest() -> Option<ChildManifestV1> {
        let path = std::env::var_os("B1_PRODUCT_RESULT_CHILD_MANIFEST")?;
        let bytes = std::fs::read(path).expect("read private test manifest");
        Some(serde_json::from_slice(&bytes).expect("strict test manifest schema"))
    }

    struct ManifestClock(DateTime<Utc>);
    impl crate::product::ProductClock for ManifestClock {
        fn now(&self) -> DateTime<Utc> {
            self.0
        }
    }

    #[test]
    fn child_pauses_at_selected_real_result_finalizer_boundary() {
        let Some(manifest) = read_manifest() else {
            return;
        };
        assert_eq!(manifest.version, 1);
        assert_eq!(manifest.operation, "finalize_result");
        assert!(manifest.db_path.is_absolute());
        assert!(manifest.artifact_root.is_absolute());
        assert!(manifest.session_root.is_absolute());
        assert!(manifest.claim_root.is_absolute());
        assert!(manifest.service_bearer_path.is_absolute());
        assert_eq!(manifest.expected_lifecycle, 1);
        assert_eq!(manifest.host_attempt, 1);
        assert_eq!(manifest.host_run, 7);
        assert_eq!(manifest.host_activation, 3);
        assert!(!manifest.delivery_id.is_empty());
        assert!(!manifest.worker_id.is_empty());
        assert!(!manifest.request_identity.is_empty());
        assert!(manifest.hold_operation_id.is_none());
        if let Some(approval) = &manifest.approval {
            let _coordinates = (
                &approval.request_key,
                &approval.grant_id,
                &approval.token_digest,
                &approval.order_id,
                approval.expected_source_revision,
                approval.expected_attempt,
                approval.run_id,
                approval.generation,
                &approval.artifact_digest,
                &approval.tuple_digest,
                &approval.intent,
                &approval.now,
            );
        }
        assert_eq!(
            manifest.human_session_digest.is_some(),
            manifest.human_session_bearer_path.is_some(),
            "human session digest and protected bearer path travel together"
        );
        if let Some(path) = &manifest.human_session_bearer_path {
            assert!(path.is_absolute());
            assert!(path.is_file());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let metadata = std::fs::symlink_metadata(path).unwrap();
                assert!(metadata.file_type().is_file());
                assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
            }
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let root_meta =
                std::fs::metadata(&manifest.artifact_root).expect("original artifact root");
            assert_eq!(root_meta.dev(), manifest.artifact_root_device);
            assert_eq!(root_meta.ino(), manifest.artifact_root_inode);
        }
        assert!(manifest.service_bearer_path.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = std::fs::symlink_metadata(&manifest.service_bearer_path).unwrap();
            assert!(metadata.file_type().is_file());
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
        let bearer = std::fs::read_to_string(&manifest.service_bearer_path)
            .expect("read same synthetic fixture-issued service bearer");
        let test_time: DateTime<Utc> = manifest.test_time.parse().expect("fixture clock");
        let config = ProductConfig {
            installation_id: manifest.installation_id.clone(),
            database_id: manifest.database_id.clone(),
        };
        let mut store = ProductStore::open_with_clock(
            &manifest.db_path,
            config,
            std::sync::Arc::new(ManifestClock(test_time.clone())),
        )
        .expect("open exact fixture database");
        store
            .configure_product_artifact_root(&manifest.artifact_root)
            .expect("configure original pinned artifact root");
        let service = store
            .authenticate_product_service_for_action(
                &bearer,
                &manifest.connector_id,
                &manifest.api_origin,
                &manifest.repository_id,
                ProductServiceAction::DriveReview,
                test_time,
            )
            .expect("authenticate the fixture-issued service identity");
        assert_eq!(service.service_id, manifest.service_id);
        assert_eq!(service.source_instance_id, manifest.source_instance_id);
        assert_eq!(service.source_generation, manifest.source_generation);
        assert_eq!(
            service.source_instance_revision,
            manifest.source_instance_revision
        );
        assert_eq!(service.connector_revision, manifest.connector_revision);
        assert_eq!(service.connector_id, manifest.connector_id);
        assert_eq!(service.api_origin, manifest.api_origin);
        assert_eq!(service.repository_id, manifest.repository_id);
        let expected = ProductResultExpectation {
            host_key: HostKey {
                order: manifest.host_order,
                attempt: u64::try_from(manifest.host_attempt)
                    .expect("nonnegative manifest attempt"),
                run: u64::try_from(manifest.host_run).expect("nonnegative manifest run"),
                activation: u64::try_from(manifest.host_activation)
                    .expect("nonnegative manifest activation"),
            },
            host_result_digest: manifest.host_result_digest,
        };
        let _ = store
            .finalize_product_review_result(&service, &expected)
            .expect("invoke actual ProductStore result finalizer");
    }

    #[test]
    fn child_pauses_at_selected_real_product_approval_boundary() {
        let Some(manifest) = read_manifest() else {
            return;
        };
        assert_eq!(manifest.version, 1);
        assert_eq!(manifest.operation, "approve_product");
        assert!(manifest.db_path.is_absolute());
        assert!(manifest.artifact_root.is_absolute());
        assert!(manifest.session_root.is_absolute());
        assert!(manifest.claim_root.is_absolute());
        assert!(manifest.service_bearer_path.is_absolute());
        assert_eq!(manifest.expected_lifecycle, 1);
        assert!(!manifest.delivery_id.is_empty());
        assert!(!manifest.worker_id.is_empty());
        assert!(!manifest.request_identity.is_empty());
        assert!(manifest.hold_operation_id.is_none());

        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let artifact_root = std::fs::metadata(&manifest.artifact_root)
                .expect("original configured artifact root");
            assert_eq!(artifact_root.dev(), manifest.artifact_root_device);
            assert_eq!(artifact_root.ino(), manifest.artifact_root_inode);
            let service_metadata = std::fs::symlink_metadata(&manifest.service_bearer_path)
                .expect("synthetic service bearer metadata");
            assert!(service_metadata.file_type().is_file());
            assert_eq!(service_metadata.permissions().mode() & 0o777, 0o600);
        }

        let approval = manifest
            .approval
            .as_ref()
            .expect("approval operation carries exact persisted intent");
        assert!(!approval.operation_id.is_empty());
        let bearer_path = manifest
            .human_session_bearer_path
            .as_ref()
            .expect("approval child requires issued human bearer file");
        assert!(bearer_path.is_absolute() && bearer_path.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = std::fs::symlink_metadata(bearer_path).unwrap();
            assert!(metadata.file_type().is_file());
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
        let bearer = std::fs::read_to_string(bearer_path).expect("issued synthetic human bearer");
        let human_session_digest = manifest
            .human_session_digest
            .as_deref()
            .expect("human session digest accompanies bearer");
        assert_eq!(
            super::super::sha256_hex(bearer.as_bytes()),
            human_session_digest
        );

        let test_time: DateTime<Utc> = manifest.test_time.parse().expect("fixture clock");
        let config = ProductConfig {
            installation_id: manifest.installation_id.clone(),
            database_id: manifest.database_id.clone(),
        };
        let mut store = ProductStore::open_with_clock(
            &manifest.db_path,
            config,
            std::sync::Arc::new(ManifestClock(test_time)),
        )
        .expect("open exact G-final database and config");
        store
            .configure_product_artifact_root(&manifest.artifact_root)
            .expect("reopen original pinned artifact root");

        // Validate the fixture's original service/source identity as a
        // separate check. Only the human session authorizes the approval.
        let service_bearer = std::fs::read_to_string(&manifest.service_bearer_path)
            .expect("same synthetic service bearer file");
        let service_time: DateTime<Utc> = manifest.test_time.parse().expect("fixture clock");
        let service = store
            .authenticate_product_service_for_action(
                &service_bearer,
                &manifest.connector_id,
                &manifest.api_origin,
                &manifest.repository_id,
                ProductServiceAction::DriveReview,
                service_time,
            )
            .expect("fixture service remains bound to original source");
        assert_eq!(service.service_id, manifest.service_id);
        assert_eq!(service.source_instance_id, manifest.source_instance_id);
        assert_eq!(service.source_generation, manifest.source_generation);
        assert_eq!(
            service.source_instance_revision,
            manifest.source_instance_revision
        );
        assert_eq!(service.connector_revision, manifest.connector_revision);

        let approval_now: DateTime<Utc> = approval.now.parse().expect("approval intent time");
        let intent = ProductApprovalIntent {
            request_key: approval.request_key.clone(),
            grant_id: approval.grant_id.clone(),
            token_digest: approval.token_digest.clone(),
            order_id: approval.order_id.clone(),
            expected_source_revision: approval.expected_source_revision,
            expected_attempt: approval.expected_attempt,
            run_id: approval.run_id,
            generation: approval.generation,
            artifact_digest: approval.artifact_digest.clone(),
            tuple_digest: approval.tuple_digest.clone(),
            intent: approval.intent.clone(),
            now: approval_now,
        };
        assert_eq!(intent.order_id, manifest.host_order);
        assert_eq!(intent.expected_attempt, manifest.host_attempt);
        assert_eq!(intent.run_id, manifest.host_run);
        assert_eq!(intent.generation, 0);
        assert_eq!(intent.artifact_digest.len(), 40);
        assert_eq!(intent.tuple_digest.len(), 64);
        assert!(!intent.request_key.is_empty());
        assert!(!intent.grant_id.is_empty());
        assert!(super::super::is_sha256_hex(&intent.token_digest));

        let proof = store
            .prepare_approval(human_session_digest, &intent)
            .expect("load private real reservation proof for persisted intent");
        assert_eq!(proof.operation_id(), approval.operation_id);
        let receipt = store
            .approve_product(human_session_digest, &intent, &proof)
            .expect("execute real strict product approval transaction");
        let receipt_json = serde_json::to_string(&receipt).expect("receipt encoding");
        println!("B1_PRODUCT_APPROVAL_RECEIPT:{receipt_json}");
        let _ = std::io::stdout().flush();
    }
}

#[cfg(test)]
mod approval_child_tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use std::io::Write;
    use std::path::Path;
    use std::time::{Duration, Instant};

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ApprovalChildManifestV1 {
        version: u32,
        operation: String,
        db_path: std::path::PathBuf,
        installation_id: String,
        database_id: String,
        artifact_root: std::path::PathBuf,
        session_root: std::path::PathBuf,
        claim_root: std::path::PathBuf,
        artifact_root_device: u64,
        artifact_root_inode: u64,
        connector_id: String,
        api_origin: String,
        repository_id: String,
        source_instance_id: String,
        source_generation: i64,
        source_instance_revision: i64,
        connector_revision: i64,
        service_id: String,
        service_bearer_path: std::path::PathBuf,
        test_time: String,
        host_order: String,
        host_attempt: i64,
        host_run: i64,
        host_activation: i64,
        host_result_digest: String,
        human_session_digest: Option<String>,
        human_session_bearer_path: Option<std::path::PathBuf>,
        approval: Option<ApprovalChildIntentV1>,
        expected_lifecycle: i64,
        delivery_id: String,
        worker_id: String,
        hold_operation_id: Option<String>,
        request_identity: String,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ApprovalChildIntentV1 {
        operation_id: String,
        request_key: String,
        grant_id: String,
        token_digest: String,
        order_id: String,
        expected_source_revision: i64,
        expected_attempt: i64,
        run_id: i64,
        generation: i64,
        artifact_digest: String,
        tuple_digest: String,
        intent: String,
        now: String,
    }

    struct ManifestClock(DateTime<Utc>);
    impl crate::product::ProductClock for ManifestClock {
        fn now(&self) -> DateTime<Utc> {
            self.0
        }
    }

    #[test]
    fn child_approves_exact_genuine_result_with_issued_human_session() {
        let Some(manifest_path) = std::env::var_os("B1_PRODUCT_RESULT_CHILD_MANIFEST") else {
            return;
        };
        let manifest: ApprovalChildManifestV1 = serde_json::from_slice(
            &std::fs::read(manifest_path).expect("read private strict child manifest"),
        )
        .expect("deny unknown or malformed child manifest fields");
        assert_eq!(manifest.version, 1);
        assert_eq!(manifest.operation, "approve_product");
        assert!(manifest.db_path.is_absolute());
        assert!(manifest.artifact_root.is_absolute());
        assert!(manifest.session_root.is_absolute());
        assert!(manifest.claim_root.is_absolute());
        assert_eq!(manifest.expected_lifecycle, 1);
        assert!(!manifest.delivery_id.is_empty());
        assert!(!manifest.worker_id.is_empty());
        assert!(!manifest.request_identity.is_empty());
        assert!(manifest.hold_operation_id.is_none());

        let approval = manifest
            .approval
            .as_ref()
            .expect("approval operation carries exact persisted intent");
        let human_session_digest = manifest
            .human_session_digest
            .as_deref()
            .expect("issued human session digest");
        let human_bearer_path = manifest
            .human_session_bearer_path
            .as_ref()
            .expect("issued human session bearer path");
        assert!(human_bearer_path.is_absolute() && human_bearer_path.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let root =
                std::fs::metadata(&manifest.artifact_root).expect("pinned original artifact root");
            assert_eq!(root.dev(), manifest.artifact_root_device);
            assert_eq!(root.ino(), manifest.artifact_root_inode);
            for path in [human_bearer_path, &manifest.service_bearer_path] {
                let metadata = std::fs::symlink_metadata(path).expect("private bearer metadata");
                assert!(metadata.file_type().is_file());
                assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
            }
        }
        let human_bearer = std::fs::read_to_string(human_bearer_path)
            .expect("read fixture-issued synthetic human bearer");
        assert_eq!(
            super::super::sha256_hex(human_bearer.as_bytes()),
            human_session_digest
        );

        let test_time: DateTime<Utc> = manifest.test_time.parse().expect("fixture clock");
        let config = ProductConfig {
            installation_id: manifest.installation_id.clone(),
            database_id: manifest.database_id.clone(),
        };
        let mut store = ProductStore::open_with_clock(
            &manifest.db_path,
            config,
            std::sync::Arc::new(ManifestClock(test_time)),
        )
        .expect("open exact product config");
        store
            .configure_product_artifact_root(&manifest.artifact_root)
            .expect("use original pinned artifact root");

        // The service identity is checked for fixture/source integrity only;
        // the approval authority is the issued human session below.
        let service_bearer = std::fs::read_to_string(&manifest.service_bearer_path)
            .expect("read same fixture-issued synthetic service bearer");
        let service_time: DateTime<Utc> = manifest.test_time.parse().expect("fixture clock");
        let service = store
            .authenticate_product_service_for_action(
                &service_bearer,
                &manifest.connector_id,
                &manifest.api_origin,
                &manifest.repository_id,
                ProductServiceAction::DriveReview,
                service_time,
            )
            .expect("validate original source service identity");
        assert_eq!(service.service_id, manifest.service_id);
        assert_eq!(service.source_instance_id, manifest.source_instance_id);
        assert_eq!(service.source_generation, manifest.source_generation);
        assert_eq!(
            service.source_instance_revision,
            manifest.source_instance_revision
        );
        assert_eq!(service.connector_revision, manifest.connector_revision);

        let expected_result = ProductResultExpectation {
            host_key: HostKey {
                order: manifest.host_order.clone(),
                attempt: u64::try_from(manifest.host_attempt).expect("host attempt range"),
                run: u64::try_from(manifest.host_run).expect("host run range"),
                activation: u64::try_from(manifest.host_activation).expect("host activation range"),
            },
            host_result_digest: manifest.host_result_digest.clone(),
        };
        let result = store
            .lookup_product_review_result(&service, &expected_result)
            .expect("strict result lookup through original service binding")
            .expect("G-final result remains present before human approval");
        assert_eq!(result.order_id, manifest.host_order);
        assert_eq!(result.tuple_digest, approval.tuple_digest);
        assert_eq!(result.result_commit, approval.artifact_digest);

        let now: DateTime<Utc> = approval.now.parse().expect("persisted intent time");
        let intent = ProductApprovalIntent {
            request_key: approval.request_key.clone(),
            grant_id: approval.grant_id.clone(),
            token_digest: approval.token_digest.clone(),
            order_id: approval.order_id.clone(),
            expected_source_revision: approval.expected_source_revision,
            expected_attempt: approval.expected_attempt,
            run_id: approval.run_id,
            generation: approval.generation,
            artifact_digest: approval.artifact_digest.clone(),
            tuple_digest: approval.tuple_digest.clone(),
            intent: approval.intent.clone(),
            now,
        };
        assert_eq!(intent.order_id, manifest.host_order);
        assert_eq!(intent.expected_attempt, manifest.host_attempt);
        assert_eq!(intent.run_id, manifest.host_run);
        assert_eq!(intent.generation, 0);
        assert_eq!(intent.expected_source_revision, manifest.connector_revision);
        assert_eq!(intent.artifact_digest.len(), 40);
        assert_eq!(intent.tuple_digest.len(), 64);
        assert!(!intent.request_key.is_empty());
        assert!(!intent.grant_id.is_empty());
        assert!(super::super::is_sha256_hex(&intent.token_digest));

        let proof = store
            .prepare_approval(human_session_digest, &intent)
            .expect("prepare exact persisted intent and receive opaque proof");
        assert_eq!(proof.operation_id(), approval.operation_id);

        if let (Some(ready), Some(gate)) = (
            std::env::var_os("B1_APPROVAL_READY"),
            std::env::var_os("B1_APPROVAL_GATE"),
        ) {
            let ready = Path::new(&ready);
            let gate = Path::new(&gate);
            std::fs::write(ready, b"prepared").expect("publish child-ready marker");
            let deadline = Instant::now() + Duration::from_secs(30);
            while !gate.exists() {
                assert!(
                    Instant::now() < deadline,
                    "parent did not release approval child"
                );
                std::thread::yield_now();
            }
        }

        match store.approve_product(human_session_digest, &intent, &proof) {
            Ok(receipt) => {
                let receipt_json = serde_json::to_string(&receipt).expect("receipt JSON");
                println!("B1_PRODUCT_APPROVAL_RECEIPT:{receipt_json}");
            }
            Err(error) => println!("B1_PRODUCT_APPROVAL_DENIED:{error}"),
        }
        let _ = std::io::stdout().flush();
    }
}

fn result_identity(
    installation: &str,
    database: &str,
    order: &str,
    lifecycle: i64,
    attempt: i64,
    run: i64,
    activation: i64,
    generation: i64,
) -> String {
    crate::product_scope::framed_hash(
        "foundry-product-result-id-v1",
        &[
            installation,
            database,
            order,
            &lifecycle.to_string(),
            &attempt.to_string(),
            &run.to_string(),
            &activation.to_string(),
            &generation.to_string(),
        ],
    )
}

fn read_stored_result_in(
    tx: &Transaction<'_>,
    id: &str,
    expected_digest: &str,
    order: &str,
    attempt: i64,
    run: i64,
    generation: i64,
) -> Result<Option<ProductResultInfo>, ProductResultError> {
    let stored:Option<(String,String,String,String,String,i64,i64,i64,i64,i64,String)>=tx.query_row("SELECT payload_json,receipt_json,binding_digest,tuple_digest,order_id,lifecycle,attempt,run_id,activation,generation,host_result_digest FROM product_result_bindings_v1 WHERE result_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get(10)?))).optional().map_err(|_|ProductResultError::Storage)?;
    let Some((
        payload_json,
        receipt_json,
        binding_digest,
        tuple_digest,
        order,
        lifecycle,
        attempt,
        run,
        activation,
        generation,
        stored_host_digest,
    )) = stored
    else {
        let alias: Option<(String, String)> = tx
            .query_row(
                "SELECT result_id,host_result_digest FROM product_result_bindings_v1
                 WHERE order_id=?1 AND run_id=?2 AND generation=?3",
                params![order, run, generation],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|_| ProductResultError::Storage)?;
        if let Some((alias_id, alias_digest)) = alias {
            read_stored_result_in(
                tx,
                &alias_id,
                &alias_digest,
                order,
                attempt,
                run,
                generation,
            )?
            .ok_or(ProductResultError::CorruptEvidence)?;
            return Err(ProductResultError::Conflict);
        }
        let legacy:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM product_result_legacy_tuples_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND generation=?4)",params![order,attempt,run,generation],|r|r.get(0)).map_err(|_|ProductResultError::Storage)?;
        let tuple:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM artifact_tuples WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND generation=?4)",params![order,attempt,run,generation],|r|r.get(0)).map_err(|_|ProductResultError::Storage)?;
        return if legacy {
            Err(ProductResultError::Conflict)
        } else if tuple {
            Err(ProductResultError::CorruptEvidence)
        } else {
            Ok(None)
        };
    };
    if !hex64(&binding_digest) || !hex64(&tuple_digest) {
        return Err(ProductResultError::CorruptEvidence);
    }
    let payload: ResultPayloadV1 =
        serde_json::from_str(&payload_json).map_err(|_| ProductResultError::CorruptEvidence)?;
    if payload.version != 1
        || serde_json::to_string(&payload).map_err(|_| ProductResultError::CorruptEvidence)?
            != payload_json
        || crate::product_scope::framed_hash("foundry-product-result-binding-v1", &[&payload_json])
            != binding_digest
        || payload.tuple.digest() != tuple_digest
        || payload.host_result_digest != product_host_result_digest(&payload.host_result)?
        || payload.order_id != order
        || payload.lifecycle != lifecycle
        || payload.host_result_digest != stored_host_digest
        || payload.intent.order.0 != order
        || payload.intent.attempt.0 as i64 != attempt
        || payload.intent.run.0 as i64 != run
        || payload.intent.activation as i64 != activation
        || i64::try_from(payload.intent.generation.0).ok() != Some(generation)
        || result_identity(
            &payload.installation_id,
            &payload.database_id,
            &order,
            lifecycle,
            attempt,
            run,
            activation,
            generation,
        ) != id
    {
        return Err(ProductResultError::CorruptEvidence);
    }
    let key = HostKey {
        order: payload.intent.order.0.clone(),
        attempt: payload.intent.attempt.0,
        run: payload.intent.run.0,
        activation: payload.intent.activation,
    };
    let key_json = host_key_json(&key)?;
    let authority:Option<(String,i64,i64,i64,String,i64,i64,i64)>=tx.query_row("SELECT service_id,service_revision,credential_revision,drive_review_grant_revision,source_instance_id,source_generation,source_instance_revision,connector_revision FROM product_review_authority_v1 WHERE key_json=?1",[&key_json],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).optional().map_err(|_|ProductResultError::Storage)?;
    let expected_authority = &payload.work.authority;
    if authority
        != Some((
            expected_authority.service_id.clone(),
            expected_authority.service_revision,
            expected_authority.credential_revision,
            expected_authority.drive_review_grant_revision,
            expected_authority.source_instance_id.clone(),
            expected_authority.source_generation,
            expected_authority.source_instance_revision,
            expected_authority.connector_revision,
        ))
    {
        return Err(ProductResultError::CorruptEvidence);
    }
    let work:Option<(String,String,i64,i64,i64,String,String,String)>=tx.query_row("SELECT delivery_id,worker_id,claim_epoch,claim_fence,source_revision,payload_digest,task_kind,payload_snapshot FROM product_review_work_bindings_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3",params![order,attempt,payload.journal_run],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).optional().map_err(|_|ProductResultError::Storage)?;
    if work
        != Some((
            payload.work.delivery_id.clone(),
            payload.work.worker_id.clone(),
            payload.work.claim_epoch,
            payload.work.claim_fence,
            payload.work.source_revision,
            payload.work.payload_digest.clone(),
            payload.work.task_kind.clone(),
            payload.work.payload_snapshot.clone(),
        ))
    {
        return Err(ProductResultError::CorruptEvidence);
    }
    let host_pair:Option<(String,String)>=tx.query_row("SELECT h.result_json,r.result_json FROM product_review_host_results_v1 h JOIN product_review_runs_v1 r ON r.order_id=h.order_id AND r.attempt=h.attempt AND r.run_id=?4 WHERE h.order_id=?1 AND h.attempt=?2 AND h.run_id=?3 AND h.activation=?5",params![order,attempt,run,payload.journal_run,activation],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|_|ProductResultError::Storage)?;
    if !matches!(host_pair,Some((ref h,ref r)) if h==r&&serde_json::from_str::<HostResult>(h).ok().as_ref()==Some(&payload.host_result))
    {
        return Err(ProductResultError::CorruptEvidence);
    }
    let anchor: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM ledger WHERE seq=?1 AND hash=?2)",
            params![payload.s1_sequence, payload.s1_event_hash],
            |r| r.get(0),
        )
        .map_err(|_| ProductResultError::Storage)?;
    if !anchor {
        return Err(ProductResultError::CorruptEvidence);
    }
    let tuple:Option<(String,String,String,String)>=tx.query_row("SELECT immutable_commit,artifact_location,generations_json,order_id FROM artifact_tuples WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND generation=?4",params![order,attempt,run,generation],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(|_|ProductResultError::Storage)?;
    let Some((commit, location, generations, tuple_order)) = tuple else {
        return Err(ProductResultError::CorruptEvidence);
    };
    let generations: Vec<foundry_exec::GenerationCommit> =
        serde_json::from_str(&generations).map_err(|_| ProductResultError::CorruptEvidence)?;
    if tuple_order != order
        || commit != payload.tuple.immutable_commit.0
        || location != payload.tuple.artifact_location.0
        || generations != payload.tuple.generations
    {
        return Err(ProductResultError::CorruptEvidence);
    }
    let receipt: LocalResultReceiptV1 =
        serde_json::from_str(&receipt_json).map_err(|_| ProductResultError::CorruptEvidence)?;
    if receipt.version != 1
        || receipt.kind != "local_review_result"
        || serde_json::to_string(&receipt).map_err(|_| ProductResultError::CorruptEvidence)?
            != receipt_json
        || receipt.binding_digest != binding_digest
        || receipt.tuple_digest != tuple_digest
        || receipt.result_id != id
        || receipt.host_result_digest != payload.host_result_digest
        || receipt.final_artifact_id != payload.final_artifact_id
        || receipt.installation_id != payload.installation_id
        || receipt.database_id != payload.database_id
        || receipt.order_id != order
        || receipt.lifecycle != lifecycle
        || receipt.attempt != attempt
        || receipt.run_id != run
        || receipt.activation != activation
        || receipt.generation != generation
        || receipt.finalized_at != payload.finalized_at
    {
        return Err(ProductResultError::CorruptEvidence);
    }
    if payload.host_result_digest != expected_digest {
        return Err(ProductResultError::Conflict);
    }
    validate_stored_hold_progress_in(tx, &payload)?;
    project_info(&payload, id, &binding_digest).map(Some)
}

fn validate_stored_hold_progress_in(
    tx: &Transaction<'_>,
    payload: &ResultPayloadV1,
) -> Result<(), ProductResultError> {
    for hold in &payload.accepted_holds {
        let live: Option<(i64, i64, i64, i64, String)> = tx
            .query_row(
                "SELECT progress_revision,epoch,fence,reserved_run_id,owner_id
                 FROM product_review_hold_progress_v1 WHERE operation_id=?1",
                [&hold.operation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .optional()
            .map_err(|_| ProductResultError::Storage)?;
        if live
            != Some((
                hold.progress_revision,
                hold.epoch,
                hold.fence,
                hold.reserved_run_id,
                hold.owner_id.clone(),
            ))
        {
            return Err(ProductResultError::CorruptEvidence);
        }
    }
    Ok(())
}

pub(super) fn approval_result_in(
    tx: &Transaction<'_>,
    order: &str,
    lifecycle: i64,
    attempt: i64,
    run: i64,
    generation: i64,
    expected_tuple_digest: &str,
    expected_commit: &str,
) -> Result<(String, String, String), ProductResultError> {
    let rows: Vec<(String, String, String, String)> = {
        let mut st=tx.prepare("SELECT result_id,binding_digest,tuple_digest,host_result_digest FROM product_result_bindings_v1 WHERE order_id=?1 AND lifecycle=?2 AND attempt=?3 AND run_id=?4 AND generation=?5").map_err(|_|ProductResultError::Storage)?;
        st.query_map(params![order, lifecycle, attempt, run, generation], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .map_err(|_| ProductResultError::Storage)?
        .collect::<Result<_, _>>()
        .map_err(|_| ProductResultError::Storage)?
    };
    if rows.len() != 1 {
        return Err(ProductResultError::Conflict);
    }
    let (id, binding, tuple, host_digest) = &rows[0];
    if tuple != expected_tuple_digest {
        return Err(ProductResultError::Conflict);
    }
    let info = read_stored_result_in(tx, id, host_digest, order, attempt, run, generation)?
        .ok_or(ProductResultError::CorruptEvidence)?;
    if info.lifecycle != lifecycle
        || info.base_commit.is_empty()
        || info.result_commit != expected_commit
    {
        return Err(ProductResultError::CorruptEvidence);
    }
    Ok((id.clone(), binding.clone(), tuple.clone()))
}

fn parse_hold_acceptance_receipt(
    receipt_json: &str,
    operation: &str,
    order: &str,
    lifecycle: i64,
    attempt: i64,
) -> Result<super::ProductHoldAcceptance, ProductResultError> {
    let receipt: super::ProductHoldAcceptance =
        serde_json::from_str(receipt_json).map_err(|_| ProductResultError::CorruptEvidence)?;
    if serde_json::to_string(&receipt).map_err(|_| ProductResultError::CorruptEvidence)?
        != receipt_json
        || receipt.operation_id != operation
        || receipt.order_id != order
        || receipt.lifecycle != lifecycle
        || receipt.attempt != attempt
    {
        return Err(ProductResultError::CorruptEvidence);
    }
    Ok(receipt)
}

fn has_failed_accepted_hold_decision_in(
    tx: &Transaction<'_>,
    order: &str,
    lifecycle: i64,
    attempt: i64,
) -> Result<bool, ProductResultError> {
    let mut statement = tx
        .prepare(
            "SELECT operation_id,receipt_json FROM product_review_hold_acceptances_v1
             WHERE order_id=?1 AND lifecycle=?2 AND attempt=?3 ORDER BY operation_id",
        )
        .map_err(|_| ProductResultError::Storage)?;
    let rows = statement
        .query_map(params![order, lifecycle, attempt], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|_| ProductResultError::Storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ProductResultError::Storage)?;
    for (operation, receipt_json) in rows {
        let receipt = parse_hold_acceptance_receipt(
            &receipt_json,
            &operation,
            order,
            lifecycle,
            attempt,
        )?;
        if matches!(
            receipt.decision,
            super::ProductHoldDecision::CommitFail | super::ProductHoldDecision::Abort
        ) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn completed_result_ancestry_in(
    tx: &Transaction<'_>,
    installation_id: &str,
    order: &str,
    lifecycle: i64,
    terminal: &foundry_exec::ExecutionIntent,
) -> Result<(Vec<(i64, i64, String)>, Vec<ResultAcceptedHoldV1>), ProductResultError> {
    let attempt =
        i64::try_from(terminal.attempt.0).map_err(|_| ProductResultError::InvalidInput)?;
    let terminal_run =
        i64::try_from(terminal.run.0).map_err(|_| ProductResultError::InvalidInput)?;
    let terminal_activation =
        i64::try_from(terminal.activation).map_err(|_| ProductResultError::InvalidInput)?;
    if terminal.order.0 != order {
        return Err(ProductResultError::StaleBinding);
    }
    let mut reverse_runs = vec![terminal_run];
    let mut resume_operations = std::collections::BTreeSet::new();
    let mut current = terminal_run;
    loop {
        let mut statement = tx
            .prepare(
                "SELECT originating_run,operation_id FROM product_review_run_reservations_v1
                 WHERE order_id=?1 AND attempt=?2 AND followup_run_id=?3",
            )
            .map_err(|_| ProductResultError::Storage)?;
        let parents = statement
            .query_map(params![order, attempt, current], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|_| ProductResultError::Storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| ProductResultError::Storage)?;
        match parents.as_slice() {
            [] => break,
            [(origin, operation)] => {
                if *origin < 0
                    || *origin >= current
                    || reverse_runs.contains(origin)
                    || !resume_operations.insert(operation.clone())
                {
                    return Err(ProductResultError::CorruptEvidence);
                }
                reverse_runs.push(*origin);
                current = *origin;
            }
            _ => return Err(ProductResultError::CorruptEvidence),
        }
    }
    reverse_runs.reverse();
    let lineage_runs = reverse_runs
        .iter()
        .map(|run| {
            let mut statement = tx
                .prepare("SELECT activation FROM product_review_host_accept_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3")
                .map_err(|_| ProductResultError::Storage)?;
            let activations = statement
                .query_map(params![order, attempt, run], |row| row.get::<_, i64>(0))
                .map_err(|_| ProductResultError::Storage)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| ProductResultError::Storage)?;
            if activations.len() != 1 {
                return Err(ProductResultError::CorruptEvidence);
            }
            let activation = activations[0];
            let journal_run = format!("{order}:{attempt}:{run}:{activation}");
            let coordinates: Option<(i64, i64, String, String, String, i64, String, String)> = tx
                .query_row(
                    "SELECT product_lifecycle,attempt,pack_digest,status,source_revision,admission_hash,base_commit,run_id
                     FROM product_review_runs_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3",
                    params![order, attempt, journal_run],
                    |row| Ok((row.get(0)?,row.get(1)?,row.get(7)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?)),
                )
                .optional()
                .map_err(|_| ProductResultError::Storage)?;
            let Some((run_lifecycle, run_attempt, run_number, pack, status, _source_revision, _admission_hash, _base)) = coordinates else {
                return Err(ProductResultError::CorruptEvidence);
            };
            if run_lifecycle != lifecycle
                || run_attempt != attempt
                || run_number != journal_run
                || pack.trim().is_empty()
                || !matches!(status.as_str(), "passed" | "held" | "resolved")
            {
                return Err(ProductResultError::CorruptEvidence);
            }
            let key = host_key_json(&HostKey {
                order: order.to_owned(),
                attempt: u64::try_from(attempt).map_err(|_| ProductResultError::InvalidInput)?,
                run: u64::try_from(*run).map_err(|_| ProductResultError::InvalidInput)?,
                activation: u64::try_from(activation).map_err(|_| ProductResultError::InvalidInput)?,
            })?;
            let intent_json: Option<String> = tx
                .query_row(
                    "SELECT intent_json FROM product_review_bindings_v1 WHERE key_json=?1",
                    [&key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|_| ProductResultError::Storage)?;
            let saved_intent: foundry_exec::ExecutionIntent = serde_json::from_str(
                &intent_json.ok_or(ProductResultError::CorruptEvidence)?,
            ).map_err(|_| ProductResultError::CorruptEvidence)?;
            if saved_intent.order.0 != order
                || i64::try_from(saved_intent.attempt.0).ok() != Some(attempt)
                || i64::try_from(saved_intent.run.0).ok() != Some(*run)
                || i64::try_from(saved_intent.activation).ok() != Some(activation)
                || (*run == terminal_run && (activation != terminal_activation || saved_intent != *terminal))
            {
                return Err(ProductResultError::CorruptEvidence);
            }
            Ok((*run, activation, journal_run))
        })
        .collect::<Result<Vec<_>, ProductResultError>>()?;

    // Record authority gaps now, but do not classify them yet. The complete
    // accepted-hold/reservation chain below must first validate; otherwise a
    // corrupt edge reachable from a legacy census root could be hidden as
    // merely ineligible history.
    let mut original_authority: Option<(String, i64, i64, i64, String, i64, i64, i64)> = None;
    let mut legacy_authority_missing = false;
    let mut invalid_authority_missing = false;
    let mut authority_after_missing_legacy = false;
    for (ancestor_run, ancestor_activation, _journal_run) in &lineage_runs {
        let authority_key = host_key_json(&HostKey {
            order: order.to_owned(),
            attempt: u64::try_from(attempt).map_err(|_| ProductResultError::InvalidInput)?,
            run: u64::try_from(*ancestor_run).map_err(|_| ProductResultError::InvalidInput)?,
            activation: u64::try_from(*ancestor_activation)
                .map_err(|_| ProductResultError::InvalidInput)?,
        })?;
        let authority: Option<(String, i64, i64, i64, String, i64, i64, i64)> = tx
            .query_row(
                "SELECT service_id,service_revision,credential_revision,
                        drive_review_grant_revision,source_instance_id,source_generation,
                        source_instance_revision,connector_revision
                 FROM product_review_authority_v1
                 WHERE key_json=?1",
                [&authority_key],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                },
            )
            .optional()
            .map_err(|_| ProductResultError::Storage)?;
        let Some(authority) = authority else {
            if legacy_host_or_descendant_in(
                tx,
                order,
                attempt,
                *ancestor_run,
                *ancestor_activation,
            )? {
                legacy_authority_missing = true;
            } else {
                invalid_authority_missing = true;
            }
            continue;
        };
        let is_census_member: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM product_result_legacy_hosts_v1
                 WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND activation=?4)",
                params![order, attempt, ancestor_run, ancestor_activation],
                |row| row.get(0),
            )
            .map_err(|_| ProductResultError::Storage)?;
        if is_census_member {
            return Err(ProductResultError::CorruptEvidence);
        }
        if legacy_authority_missing {
            // A Resume descending from an unattested legacy host must copy
            // no authority. A later authority row would turn that historical
            // gap into an unauthorized new result producer.
            authority_after_missing_legacy = true;
        }
        if original_authority
            .as_ref()
            .is_some_and(|original| original != &authority)
        {
            return Err(ProductResultError::CorruptEvidence);
        }
        original_authority.get_or_insert(authority);
    }

    let mut accepted_holds = Vec::new();
    let mut seen_operations = std::collections::BTreeSet::new();
    let mut accepted_decision_ineligible = false;
    let mut statement = tx
        .prepare(
            "SELECT operation_id,order_id,lifecycle,attempt,originating_run,hold_instance_id,followup_run_id
             FROM product_review_run_reservations_v1
             WHERE order_id=?1 AND attempt=?2 ORDER BY reservation_id",
        )
        .map_err(|_| ProductResultError::Storage)?;
    let reservations = statement
        .query_map(params![order, attempt], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<i64>>(6)?,
            ))
        })
        .map_err(|_| ProductResultError::Storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ProductResultError::Storage)?;
    for (
        operation,
        reservation_order,
        reservation_lifecycle,
        reservation_attempt,
        origin,
        hold_id,
        followup,
    ) in reservations
    {
        if !reverse_runs.contains(&origin)
            && !followup.is_some_and(|run| reverse_runs.contains(&run))
        {
            continue;
        }
        if reservation_order != order
            || reservation_lifecycle != lifecycle
            || reservation_attempt != attempt
            || !seen_operations.insert(operation.clone())
        {
            return Err(ProductResultError::CorruptEvidence);
        }
        let (receipt_json, acceptance_actor_id, request_key, stored_request_digest):
            (String, String, String, String) = tx
            .query_row(
                "SELECT receipt_json,actor_id,request_key,request_digest FROM product_review_hold_acceptances_v1 WHERE operation_id=?1
                 AND order_id=?2 AND lifecycle=?3 AND attempt=?4 AND originating_run=?5 AND hold_instance_id=?6",
                params![operation, order, reservation_lifecycle, attempt, origin, hold_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(|_| ProductResultError::Storage)?
            .ok_or(ProductResultError::CorruptEvidence)?;
        let receipt = parse_hold_acceptance_receipt(
            &receipt_json,
            &operation,
            order,
            reservation_lifecycle,
            attempt,
        )?;
        let hold: Option<(i64, i64, String, String, String, i64, i64, String, i64, i64, String)> = tx
            .query_row(
                "SELECT activation,host_epoch,snapshot_json,snapshot_digest,state,hold_revision,
                        expected_predecessor,allowed_targets_json,source_revision,admission_seq,admission_hash
                 FROM product_review_holds_v1 WHERE order_id=?1 AND lifecycle=?2 AND attempt=?3
                   AND originating_run=?4 AND hold_instance_id=?5",
                params![order, reservation_lifecycle, attempt, origin, hold_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                    ))
                },
            )
            .optional()
            .map_err(|_| ProductResultError::Storage)?;
        let Some((
            activation,
            host_epoch,
            frozen_snapshot,
            frozen_digest,
            hold_state,
            hold_revision,
            expected_predecessor,
            allowed_targets_json,
            hold_source_revision,
            hold_admission_seq,
            hold_admission_hash,
        )) = hold
        else {
            return Err(ProductResultError::CorruptEvidence);
        };
        let progress: Option<(String, i64, String, i64, i64, i64, Option<String>)> = tx
            .query_row(
                "SELECT phase,progress_revision,owner_id,epoch,fence,reserved_run_id,recovery_reason
                 FROM product_review_hold_progress_v1 WHERE operation_id=?1",
                [&operation],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
            )
            .optional()
            .map_err(|_| ProductResultError::Storage)?;
        let Some((phase, progress_revision, owner_id, epoch, fence, reserved_run, recovery_reason)) =
            progress
        else {
            return Err(ProductResultError::CorruptEvidence);
        };
        let request_digest = super::product_hold_request_digest(
            installation_id,
            &receipt.actor_id,
            receipt.authority_revision,
            &request_key,
            &receipt.order_id,
            receipt.lifecycle,
            &receipt.hold_instance_id,
            receipt.hold_revision,
            receipt.attempt,
            receipt.originating_run,
            receipt.activation,
            receipt.host_epoch,
            receipt.source_revision,
            receipt.predecessor_seq,
            &frozen_digest,
            &receipt.decision,
            &receipt.source_instance_id,
            receipt.scope_revision,
            receipt.grant_revision,
        );
        let accepted_at_is_canonical = chrono::DateTime::parse_from_rfc3339(&receipt.accepted_at)
            .is_ok_and(|accepted_at| accepted_at.to_rfc3339() == receipt.accepted_at);
        if receipt.operation_id != operation
            || receipt.actor_id != acceptance_actor_id
            || request_digest != stored_request_digest
            || !accepted_at_is_canonical
            || receipt.order_id != order
            || receipt.lifecycle != reservation_lifecycle
            || receipt.attempt != attempt
            || receipt.originating_run != origin
            || receipt.activation != activation
            || receipt.host_epoch != host_epoch
            || receipt.hold_instance_id != hold_id
            || receipt.hold_revision != hold_revision
            || receipt.predecessor_seq != expected_predecessor
            || receipt.source_revision != hold_source_revision
            || hold_admission_seq <= 0
            || hold_admission_hash.is_empty()
            || receipt.reserved_run_id != reserved_run
            || followup != Some(reserved_run)
            || hold_state != "accepted"
            || phase != "completed"
            || progress_revision <= 0
            || recovery_reason.is_some()
            || owner_id != format!("{operation}:{epoch}:{fence}")
            || super::product_review_snapshot_digest(&frozen_snapshot) != frozen_digest
        {
            return Err(ProductResultError::CorruptEvidence);
        }
        // A failed or aborted acceptance is intrinsically ineligible. Its
        // authority may also have expired since the decision, but that must
        // not change the classification of the accepted decision itself.
        if matches!(
            &receipt.decision,
            super::ProductHoldDecision::CommitFail | super::ProductHoldDecision::Abort
        ) {
            return Err(ProductResultError::Ineligible);
        }
        super::review::ensure_current_acceptance_authority_in(tx, &operation).map_err(|error| {
            match error {
                foundry_exec::HostError::Persistence(_) => ProductResultError::Storage,
                _ => ProductResultError::StaleBinding,
            }
        })?;
        let frozen: serde_json::Value = serde_json::from_str(&frozen_snapshot)
            .map_err(|_| ProductResultError::CorruptEvidence)?;
        let frozen_intent: foundry_exec::ExecutionIntent = serde_json::from_value(
            frozen
                .get("intent")
                .cloned()
                .ok_or(ProductResultError::CorruptEvidence)?,
        )
        .map_err(|_| ProductResultError::CorruptEvidence)?;
        let origin_journal = format!("{order}:{attempt}:{origin}:{activation}");
        let snapshot_run = frozen
            .get("snapshot_run_id")
            .and_then(serde_json::Value::as_str)
            .ok_or(ProductResultError::CorruptEvidence)?;
        let hold = frozen
            .get("hold")
            .ok_or(ProductResultError::CorruptEvidence)?;
        if hold
            .get("hold_instance_id")
            .and_then(serde_json::Value::as_str)
            != Some(hold_id.as_str())
            || frozen
                .pointer("/intent/order")
                .and_then(serde_json::Value::as_str)
                != Some(order)
            || frozen.get("run_id").and_then(serde_json::Value::as_str)
                != Some(origin_journal.as_str())
            || !lineage_runs
                .iter()
                .any(|(lineage_run, _, journal)| *lineage_run <= origin && journal == snapshot_run)
            || hold
                .get("resume_targets")
                .and_then(serde_json::Value::as_array)
                .and_then(|targets| serde_json::to_string(targets).ok())
                .as_deref()
                != Some(allowed_targets_json.as_str())
            || frozen_intent.order.0 != order
            || i64::try_from(frozen_intent.attempt.0).ok() != Some(attempt)
            || i64::try_from(frozen_intent.run.0).ok() != Some(origin)
            || i64::try_from(frozen_intent.activation).ok() != Some(activation)
            || i64::try_from(frozen_intent.epoch.0).ok() != Some(host_epoch)
        {
            return Err(ProductResultError::CorruptEvidence);
        }
        match &receipt.decision {
            super::ProductHoldDecision::Resume(target) => {
                if followup.is_none_or(|run| run <= origin || !reverse_runs.contains(&run))
                    || !hold
                        .get("resume_targets")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|targets| {
                            targets.iter().any(|value| value.as_str() == Some(target))
                        })
                {
                    return Err(ProductResultError::CorruptEvidence);
                }
                if !resume_operations.contains(&operation) {
                    return Err(ProductResultError::CorruptEvidence);
                }
            }
            super::ProductHoldDecision::CommitPass => {
                if origin != terminal_run {
                    return Err(ProductResultError::CorruptEvidence);
                }
            }
            super::ProductHoldDecision::CommitFail | super::ProductHoldDecision::Abort => {
                accepted_decision_ineligible = true;
            }
        }
        accepted_holds.push(ResultAcceptedHoldV1 {
            operation_id: operation,
            receipt_json,
            snapshot_digest: frozen_digest,
            progress_revision,
            owner_id,
            epoch,
            fence,
            reserved_run_id: reserved_run,
        });
    }
    // A verified replay must agree with the immutable accepted-hold progress
    // coordinates already captured by an earlier result in this lineage.
    // With no prior result, this is the first-finalization path and remains
    // governed by the checks above.
    let mut prior_results = tx
        .prepare(
            "SELECT payload_json FROM product_result_bindings_v1
             WHERE order_id=?1 AND lifecycle=?2 ORDER BY result_id",
        )
        .map_err(|_| ProductResultError::Storage)?;
    let prior_payloads = prior_results
        .query_map(params![order, lifecycle], |row| row.get::<_, String>(0))
        .map_err(|_| ProductResultError::Storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ProductResultError::Storage)?;
    for payload_json in prior_payloads {
        let prior: ResultPayloadV1 = serde_json::from_str(&payload_json)
            .map_err(|_| ProductResultError::CorruptEvidence)?;
        if prior.order_id != order || prior.lifecycle != lifecycle {
            return Err(ProductResultError::CorruptEvidence);
        }
        for prior_hold in prior.accepted_holds {
            if let Some(replayed_hold) = accepted_holds
                .iter()
                .find(|hold| hold.operation_id == prior_hold.operation_id)
            {
                if replayed_hold.progress_revision != prior_hold.progress_revision
                    || replayed_hold.epoch != prior_hold.epoch
                    || replayed_hold.fence != prior_hold.fence
                    || replayed_hold.reserved_run_id != prior_hold.reserved_run_id
                    || replayed_hold.owner_id != prior_hold.owner_id
                {
                    return Err(ProductResultError::CorruptEvidence);
                }
            }
        }
    }
    if seen_operations.len() < resume_operations.len() {
        return Err(ProductResultError::CorruptEvidence);
    }
    let mut prior_rank = 0usize;
    for hold in &accepted_holds {
        let receipt: super::ProductHoldAcceptance = serde_json::from_str(&hold.receipt_json)
            .map_err(|_| ProductResultError::CorruptEvidence)?;
        let rank = reverse_runs
            .iter()
            .position(|run| *run == receipt.originating_run)
            .ok_or(ProductResultError::CorruptEvidence)?;
        if rank < prior_rank {
            return Err(ProductResultError::CorruptEvidence);
        }
        prior_rank = rank;
    }
    if invalid_authority_missing || authority_after_missing_legacy {
        return Err(ProductResultError::CorruptEvidence);
    }
    if legacy_authority_missing || accepted_decision_ineligible {
        return Err(ProductResultError::Ineligible);
    }
    Ok((lineage_runs, accepted_holds))
}

fn validate_expectation(value: &ProductResultExpectation) -> Result<(), ProductResultError> {
    if value.host_key.order.trim().is_empty()
        || !hex64(&value.host_result_digest)
        || value.host_key.attempt > i64::MAX as u64
        || value.host_key.run > i64::MAX as u64
        || value.host_key.activation > i64::MAX as u64
    {
        return Err(ProductResultError::InvalidInput);
    }
    Ok(())
}

fn service_result_error(error: super::ProductError) -> ProductResultError {
    match error {
        super::ProductError::Database(_) => ProductResultError::Storage,
        super::ProductError::ContractMismatch | super::ProductError::InvalidConfig => {
            ProductResultError::Unavailable
        }
        super::ProductError::Invariant(_) => ProductResultError::Unauthorized,
    }
}

fn require_requested_source_in(
    tx: &Transaction<'_>,
    service: &ProductServiceContext,
    order: &str,
    lifecycle: i64,
) -> Result<(), ProductResultError> {
    let source: Option<String> = tx
        .query_row(
            "SELECT source_instance_id FROM product_scope_order_provenance_v1
             WHERE order_id=?1 AND lifecycle=?2 AND verified=1",
            params![order, lifecycle],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| ProductResultError::Storage)?;
    if source.as_deref() != Some(service.source_instance_id.as_str()) {
        return Err(ProductResultError::Unauthorized);
    }
    Ok(())
}

fn coordinates(key: &HostKey) -> Result<(String, i64, i64, i64), ProductResultError> {
    Ok((
        key.order.clone(),
        i64::try_from(key.attempt).map_err(|_| ProductResultError::InvalidInput)?,
        i64::try_from(key.run).map_err(|_| ProductResultError::InvalidInput)?,
        i64::try_from(key.activation).map_err(|_| ProductResultError::InvalidInput)?,
    ))
}
fn full_commit(value: &str) -> bool {
    (value.len() == 40 || value.len() == 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn registered_path_matches(
    root: &std::path::Path,
    host_path: &str,
    registered_relative: &str,
) -> bool {
    std::path::Path::new(host_path)
        .strip_prefix(root)
        .ok()
        .and_then(std::path::Path::to_str)
        == Some(registered_relative)
}
fn hex64(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn host_key_json(key: &HostKey) -> Result<String, ProductResultError> {
    serde_json::to_string(key).map_err(|_| ProductResultError::InvalidInput)
}
fn project_info(
    payload: &ResultPayloadV1,
    id: &str,
    binding: &str,
) -> Result<ProductResultInfo, ProductResultError> {
    Ok(ProductResultInfo {
        result_id: id.into(),
        order_id: payload.order_id.clone(),
        lifecycle: payload.lifecycle,
        attempt: i64::try_from(payload.intent.attempt.0)
            .map_err(|_| ProductResultError::CorruptEvidence)?,
        run_id: i64::try_from(payload.intent.run.0)
            .map_err(|_| ProductResultError::CorruptEvidence)?,
        activation: i64::try_from(payload.intent.activation)
            .map_err(|_| ProductResultError::CorruptEvidence)?,
        generation: i64::try_from(payload.intent.generation.0)
            .map_err(|_| ProductResultError::CorruptEvidence)?,
        base_commit: payload.host_result.base_commit.clone(),
        result_commit: payload.host_result.result_commit.clone(),
        host_result_digest: payload.host_result_digest.clone(),
        tuple_digest: payload.tuple_digest.clone(),
        binding_digest: binding.into(),
        final_artifact_id: payload.final_artifact_id.clone(),
        artifact_ids: payload
            .artifacts
            .iter()
            .map(|a| a.artifact_id.clone())
            .collect(),
        finalized_at: payload.finalized_at.clone(),
    })
}

pub(super) fn verified_order_in(
    tx: &Transaction<'_>,
    order_id: &str,
) -> Result<(i64, String, u32, u32, String), ProductResultError> {
    let mut statement=tx.prepare("SELECT seq,order_id,from_state,to_state,event,actor,intent,at,prev_hash,hash,event_json FROM ledger ORDER BY seq").map_err(|_|ProductResultError::Storage)?;
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
        .map_err(|_| ProductResultError::Storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ProductResultError::Storage)?;
    if !crate::sqlite::SqliteLedger::verify_rows(&rows) {
        return Err(ProductResultError::CorruptEvidence);
    }
    let replay = crate::replay::replay(&rows, crate::sqlite::SqliteLedger::verify_rows)
        .map_err(|_| ProductResultError::CorruptEvidence)?;
    let current = replay
        .iter()
        .rev()
        .find(|r| r.order_id == order_id)
        .ok_or(ProductResultError::StaleBinding)?;
    let head_hash = rows
        .iter()
        .find(|row| row.seq == current.last_seq && row.order_id == order_id)
        .map(|row| row.hash.clone())
        .ok_or(ProductResultError::CorruptEvidence)?;
    let state = current.order.state().to_string();
    Ok((
        current.last_seq,
        head_hash,
        current.lifecycle,
        current.order.attempt(),
        state,
    ))
}

fn legacy_host_or_descendant_in(
    tx: &Transaction<'_>,
    order: &str,
    attempt: i64,
    run: i64,
    activation: i64,
) -> Result<bool, ProductResultError> {
    let direct:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM product_result_legacy_hosts_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND activation=?4)",params![order,attempt,run,activation],|r|r.get(0)).map_err(|_|ProductResultError::Storage)?;
    if direct {
        return Ok(true);
    }
    let roots:i64=tx.query_row(
        "WITH RECURSIVE ancestry(run_id) AS (
           SELECT ?3
           UNION
           SELECT r.originating_run FROM product_review_run_reservations_v1 r JOIN ancestry a ON r.followup_run_id=a.run_id WHERE r.order_id=?1 AND r.attempt=?2
         ) SELECT COUNT(DISTINCT h.run_id) FROM ancestry a JOIN product_result_legacy_hosts_v1 h ON h.order_id=?1 AND h.attempt=?2 AND h.run_id=a.run_id",
        params![order,attempt,run],|r|r.get(0),
    ).map_err(|_|ProductResultError::Storage)?;
    Ok(roots == 1)
}

#[cfg(feature = "p2-test-support")]
fn read_ledger_rows(tx: &Transaction<'_>) -> Result<Vec<crate::sqlite::LedgerRow>, ProductError> {
    let mut st=tx.prepare("SELECT seq,order_id,from_state,to_state,event,actor,intent,at,prev_hash,hash,event_json FROM ledger ORDER BY seq")?;
    Ok(st
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
        .collect::<Result<Vec<_>, _>>()?)
}

#[cfg(feature = "p2-test-support")]
fn append_p2_transition(
    tx: &Transaction<'_>,
    rows: &mut Vec<crate::sqlite::LedgerRow>,
    order: &mut foundry_domain::Order,
    order_id: &str,
    lifecycle: i64,
    event: foundry_domain::OrderEvent,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), ProductError> {
    let attempt = match &event {
        foundry_domain::OrderEvent::DieStarted => 1,
        foundry_domain::OrderEvent::DieSucceeded { attempt }
        | foundry_domain::OrderEvent::DieFailed { attempt, .. } => *attempt,
        _ => order.attempt().max(1),
    };
    let from = order.state().to_string();
    let to = order
        .transition(event.clone(), attempt)
        .map_err(|_| ProductError::Invariant("P2 domain transition rejected"))?
        .to_string();
    let sequence = rows.last().map(|r| r.seq + 1).unwrap_or(1);
    let previous = rows
        .last()
        .map(|r| r.hash.as_str())
        .unwrap_or("GENESIS")
        .to_owned();
    let actor = "Agent/p2-synthetic-test";
    let event_text = format!("{event:?}");
    let event_json = crate::event_json::event_to_json(&event);
    let at = now.to_rfc3339();
    let intent = "p2-test-support";
    let base =
        format!("{sequence}|{order_id}|{from}|{to}|{event_text}|{actor}|{intent}|{at}|{previous}");
    use sha2::Digest as _;
    let hash = hex::encode(sha2::Sha256::digest(
        crate::event_json::v2_hash_payload(&base, &event_json).as_bytes(),
    ));
    let predecessor: i64 = tx.query_row(
        "SELECT COALESCE(MAX(seq),0) FROM ledger WHERE order_id=?1",
        [order_id],
        |r| r.get(0),
    )?;
    tx.execute("INSERT INTO product_transition_intents_v1(order_id,lifecycle,expected_predecessor,operation) VALUES(?1,?2,?3,'p2-test-preparation')",params![order_id,lifecycle,predecessor])?;
    tx.execute("INSERT INTO ledger(order_id,from_state,to_state,event,actor,intent,at,prev_hash,event_json,hash) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![order_id,from,to,event_text,actor,intent,at,previous,event_json,hash])?;
    rows.push(crate::sqlite::LedgerRow {
        seq: sequence,
        order_id: order_id.into(),
        from_state: from,
        to_state: to,
        event: event_text,
        actor: actor.into(),
        intent: intent.into(),
        at,
        prev_hash: previous,
        hash,
        event_json: Some(event_json),
    });
    Ok(())
}

#[cfg(test)]
mod result_wire_tests {
    use super::*;

    fn receipt() -> LocalResultReceiptV1 {
        LocalResultReceiptV1 {
            version: 1,
            kind: "local_review_result".into(),
            installation_id: "i".into(),
            database_id: "d".into(),
            result_id: "r".into(),
            order_id: "o".into(),
            lifecycle: 2,
            attempt: 3,
            run_id: 7,
            activation: 4,
            generation: 0,
            host_result_digest: "a".repeat(64),
            tuple_digest: "b".repeat(64),
            binding_digest: "c".repeat(64),
            final_artifact_id: "f".into(),
            finalized_at: "2026-10-20T00:00:00Z".into(),
        }
    }

    #[test]
    fn local_result_receipt_has_exact_v1_bytes_and_strict_members() {
        let value = receipt();
        let encoded = serde_json::to_string(&value).unwrap();
        let expected = format!(
            "{{\"version\":1,\"kind\":\"local_review_result\",\"installation_id\":\"i\",\"database_id\":\"d\",\"result_id\":\"r\",\"order_id\":\"o\",\"lifecycle\":2,\"attempt\":3,\"run_id\":7,\"activation\":4,\"generation\":0,\"host_result_digest\":\"{}\",\"tuple_digest\":\"{}\",\"binding_digest\":\"{}\",\"final_artifact_id\":\"f\",\"finalized_at\":\"2026-10-20T00:00:00Z\"}}",
            "a".repeat(64),
            "b".repeat(64),
            "c".repeat(64)
        );
        assert_eq!(encoded, expected);
        let decoded: LocalResultReceiptV1 = serde_json::from_str(&encoded).unwrap();
        assert_eq!(serde_json::to_string(&decoded).unwrap(), encoded);

        let unknown = encoded.replacen("\"version\":1", "\"version\":1,\"extra\":0", 1);
        assert!(serde_json::from_str::<LocalResultReceiptV1>(&unknown).is_err());
        let duplicate = encoded.replacen("\"version\":1", "\"version\":1,\"version\":1", 1);
        assert!(serde_json::from_str::<LocalResultReceiptV1>(&duplicate).is_err());
        let missing = encoded.replacen(",\"generation\":0", "", 1);
        assert!(serde_json::from_str::<LocalResultReceiptV1>(&missing).is_err());

        let noncanonical = format!(
            "{{\"kind\":\"local_review_result\",{}}}",
            &encoded[1..encoded.len() - 1].replace("\"kind\":\"local_review_result\",", "")
        );
        let decoded: LocalResultReceiptV1 = serde_json::from_str(&noncanonical).unwrap();
        assert_ne!(serde_json::to_string(&decoded).unwrap(), noncanonical);
    }

    #[test]
    fn result_payload_has_exact_nested_v1_bytes_and_strict_work_members() {
        let at = chrono::DateTime::from_timestamp(0, 0).unwrap();
        let intent = foundry_exec::ExecutionIntent {
            order: OrderId("o".into()),
            attempt: Attempt(1),
            run: RunId(7),
            activation: 3,
            generation: Generation(0),
            epoch: foundry_exec::Epoch(1),
            external_ref: "ref".into(),
            recorded_at: at,
        };
        let host_result = HostResult {
            base_commit: "aaaa".into(),
            result_commit: "bbbb".into(),
            log_path: "host-log".into(),
            exit_code: 0,
            generations: vec![],
            held: None,
        };
        let tuple = ArtifactTuple {
            order: OrderId("o".into()),
            attempt: Attempt(1),
            run: RunId(7),
            generation: Generation(0),
            immutable_commit: ImmutableCommit("bbbb".into()),
            artifact_location: ArtifactLocation("artifacts/a".into()),
            generations: vec![],
        };
        let payload = ResultPayloadV1 {
            version: 1,
            installation_id: "i".into(),
            database_id: "d".into(),
            order_id: "o".into(),
            lifecycle: 1,
            source_instance_id: "source".into(),
            repository_key: "repo".into(),
            source_generation: 2,
            source_instance_revision: 3,
            source_revision: 4,
            admission_seq: 9,
            admission_hash: "hash".into(),
            intent,
            journal_run: "o:1:7:3".into(),
            host_result,
            host_result_digest: "h".into(),
            tuple,
            tuple_digest: "t".into(),
            binary_provenance: foundry_exec::ReviewBinaryProvenance {
                executable: "worker".into(),
                sha256: "sha".into(),
                source_commit: "bbbb".into(),
                pack_digest: "pack".into(),
            },
            work: ResultWorkV1 {
                delivery_id: "delivery".into(),
                worker_id: "worker".into(),
                claim_epoch: 2,
                claim_fence: 3,
                source_revision: 4,
                payload_digest: "digest".into(),
                task_kind: "dep-bump".into(),
                payload_snapshot: " {\"x\":1} ".into(),
                authority: ResultAuthorityV1 {
                    service_id: "svc".into(),
                    service_revision: 1,
                    credential_revision: 2,
                    drive_review_grant_revision: 3,
                    source_instance_id: "source".into(),
                    source_generation: 4,
                    source_instance_revision: 5,
                    connector_revision: 6,
                },
            },
            accepted_holds: vec![ResultAcceptedHoldV1 {
                operation_id: "op".into(),
                receipt_json: " {\"receipt\":1} ".into(),
                snapshot_digest: "snap".into(),
                progress_revision: 5,
                owner_id: "op:6:7".into(),
                epoch: 6,
                fence: 7,
                reserved_run_id: 8,
            }],
            artifacts: vec![ResultArtifactV1 {
                artifact_id: "artifact".into(),
                provenance_digest: "prov".into(),
                artifact_sha256: "sha".into(),
                order_id: "o".into(),
                lifecycle: 1,
                source_instance_id: "source".into(),
                repository_key: "repo".into(),
                admission_seq: 9,
                admission_hash: "hash".into(),
                attempt: 1,
                journal_run: "o:1:7:3".into(),
                generation: 0,
                stage_sequence: 1,
                stage_id: "fix".into(),
                pack_digest: "pack".into(),
                relative_path: "a.log".into(),
                output_json: " {\"ok\": true} ".into(),
            }],
            s1_sequence: 10,
            s1_event_hash: "event".into(),
            final_artifact_id: "artifact".into(),
            finalized_at: "now".into(),
        };
        let encoded = serde_json::to_string(&payload).unwrap();
        let expected = r#"{"version":1,"installation_id":"i","database_id":"d","order_id":"o","lifecycle":1,"source_instance_id":"source","repository_key":"repo","source_generation":2,"source_instance_revision":3,"source_revision":4,"admission_seq":9,"admission_hash":"hash","intent":{"order":"o","attempt":1,"run":7,"activation":3,"generation":0,"epoch":1,"external_ref":"ref","recorded_at":"1970-01-01T00:00:00Z"},"journal_run":"o:1:7:3","host_result":{"base_commit":"aaaa","result_commit":"bbbb","log_path":"host-log","exit_code":0,"generations":[],"held":null},"host_result_digest":"h","tuple":{"order":"o","attempt":1,"run":7,"generation":0,"immutable_commit":"bbbb","artifact_location":"artifacts/a","generations":[]},"tuple_digest":"t","binary_provenance":{"executable":"worker","sha256":"sha","source_commit":"bbbb","pack_digest":"pack"},"work":{"delivery_id":"delivery","worker_id":"worker","claim_epoch":2,"claim_fence":3,"source_revision":4,"payload_digest":"digest","task_kind":"dep-bump","payload_snapshot":" {\"x\":1} ","authority":{"service_id":"svc","service_revision":1,"credential_revision":2,"drive_review_grant_revision":3,"source_instance_id":"source","source_generation":4,"source_instance_revision":5,"connector_revision":6}},"accepted_holds":[{"operation_id":"op","receipt_json":" {\"receipt\":1} ","snapshot_digest":"snap","progress_revision":5,"owner_id":"op:6:7","epoch":6,"fence":7,"reserved_run_id":8}],"artifacts":[{"artifact_id":"artifact","provenance_digest":"prov","artifact_sha256":"sha","order_id":"o","lifecycle":1,"source_instance_id":"source","repository_key":"repo","admission_seq":9,"admission_hash":"hash","attempt":1,"journal_run":"o:1:7:3","generation":0,"stage_sequence":1,"stage_id":"fix","pack_digest":"pack","relative_path":"a.log","output_json":" {\"ok\": true} "}],"s1_sequence":10,"s1_event_hash":"event","final_artifact_id":"artifact","finalized_at":"now"}"#;
        assert_eq!(encoded, expected);
        let decoded: ResultPayloadV1 = serde_json::from_str(&encoded).unwrap();
        assert_eq!(serde_json::to_string(&decoded).unwrap(), encoded);

        for malformed in [
            encoded.replacen("\"version\":1", "\"version\":1,\"extra\":0", 1),
            encoded.replacen("\"version\":1", "\"version\":1,\"version\":1", 1),
            encoded.replacen(",\"source_revision\":4", "", 1),
            encoded.replacen(
                "\"work\":{\"delivery_id\":\"delivery\"",
                "\"work\":{\"delivery_id\":\"delivery\",\"extra\":0",
                1,
            ),
            encoded.replacen(
                "\"work\":{\"delivery_id\":\"delivery\"",
                "\"work\":{\"delivery_id\":\"delivery\",\"delivery_id\":\"delivery\"",
                1,
            ),
            encoded.replacen("\"work\":{\"delivery_id\":\"delivery\",", "\"work\":{", 1),
        ] {
            assert!(serde_json::from_str::<ResultPayloadV1>(&malformed).is_err());
        }

        // Nested authority, acceptance, and artifact records have their own
        // strict member sets. Keep these independent from outer/work failures.
        for malformed in [
            encoded.replacen(
                "\"authority\":{\"service_id\":\"svc\"",
                "\"authority\":{\"service_id\":\"svc\",\"extra\":0",
                1,
            ),
            encoded.replacen(
                "\"authority\":{\"service_id\":\"svc\"",
                "\"authority\":{\"service_id\":\"svc\",\"service_id\":\"svc\"",
                1,
            ),
            encoded.replacen(
                ",\"connector_revision\":6}",
                "}",
                1,
            ),
            encoded.replacen(
                "\"accepted_holds\":[{\"operation_id\":\"op\"",
                "\"accepted_holds\":[{\"operation_id\":\"op\",\"extra\":0",
                1,
            ),
            encoded.replacen(
                "\"accepted_holds\":[{\"operation_id\":\"op\"",
                "\"accepted_holds\":[{\"operation_id\":\"op\",\"operation_id\":\"op\"",
                1,
            ),
            encoded.replacen(",\"reserved_run_id\":8}", "}", 1),
            encoded.replacen(
                "\"artifacts\":[{\"artifact_id\":\"artifact\"",
                "\"artifacts\":[{\"artifact_id\":\"artifact\",\"extra\":0",
                1,
            ),
            encoded.replacen(
                "\"artifacts\":[{\"artifact_id\":\"artifact\"",
                "\"artifacts\":[{\"artifact_id\":\"artifact\",\"artifact_id\":\"artifact\"",
                1,
            ),
            encoded.replacen(",\"output_json\":\" {\\\"ok\\\": true} \"}", "}", 1),
            encoded.replacen(
                "\"intent\":{\"order\":\"o\",\"attempt\":1",
                "\"intent\":{\"order\":\"o\",\"attempt\":-1",
                1,
            ),
            encoded.replacen(
                "\"intent\":{\"order\":\"o\",\"attempt\":1,\"run\":7",
                "\"intent\":{\"order\":\"o\",\"attempt\":1,\"run\":-1",
                1,
            ),
        ] {
            assert!(serde_json::from_str::<ResultPayloadV1>(&malformed).is_err());
        }

        let noncanonical = encoded.replacen(
            "\"work\":{\"delivery_id\":\"delivery\",\"worker_id\":\"worker\"",
            "\"work\":{\"worker_id\":\"worker\",\"delivery_id\":\"delivery\"",
            1,
        );
        let decoded: ResultPayloadV1 = serde_json::from_str(&noncanonical).unwrap();
        assert_ne!(serde_json::to_string(&decoded).unwrap(), noncanonical);
    }

    #[test]
    fn result_coordinates_use_checked_persisted_widths() {
        let valid = HostKey {
            order: "order:colon".into(),
            attempt: 1,
            run: 7,
            activation: 3,
        };
        assert_eq!(
            coordinates(&valid).unwrap(),
            ("order:colon".into(), 1, 7, 3)
        );
        let overflow = HostKey {
            attempt: (i64::MAX as u64) + 1,
            ..valid.clone()
        };
        let expectation = ProductResultExpectation {
            host_key: overflow.clone(),
            host_result_digest: "a".repeat(64),
        };
        assert_eq!(
            validate_expectation(&expectation),
            Err(ProductResultError::InvalidInput)
        );
        assert_eq!(
            coordinates(&overflow),
            Err(ProductResultError::InvalidInput)
        );
        assert_eq!(
            validate_expectation(&ProductResultExpectation {
                host_key: HostKey {
                    run: (i64::MAX as u64) + 1,
                    ..valid.clone()
                },
                host_result_digest: "a".repeat(64),
            }),
            Err(ProductResultError::InvalidInput),
            "the exact first value above SQLite's signed coordinate width is rejected"
        );
        assert_eq!(
            validate_expectation(&ProductResultExpectation {
                host_key: HostKey {
                    activation: (i64::MAX as u64) + 1,
                    ..valid
                },
                host_result_digest: "a".repeat(64),
            }),
            Err(ProductResultError::InvalidInput),
            "activation uses the same checked persisted width"
        );
    }

    #[test]
    fn execution_emit_and_promotion_receipts_are_not_local_result_receipts() {
        let key = foundry_exec::ArtifactTupleKey {
            order: OrderId("o".into()),
            attempt: Attempt(1),
            run: RunId(7),
            generation: Generation(0),
        };
        let at = chrono::DateTime::from_timestamp(0, 0).unwrap();
        let emit = foundry_exec::EmitReceipt {
            key: key.clone(),
            event: "emit-event".into(),
            at,
        };
        let promotion = foundry_exec::PromotionReceipt {
            tuple: key,
            promoted_at: at,
        };
        assert!(
            serde_json::from_str::<LocalResultReceiptV1>(&serde_json::to_string(&emit).unwrap())
                .is_err()
        );
        assert!(
            serde_json::from_str::<LocalResultReceiptV1>(
                &serde_json::to_string(&promotion).unwrap()
            )
            .is_err()
        );
    }
}
