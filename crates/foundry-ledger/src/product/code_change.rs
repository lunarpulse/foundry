//! Explicit, offline code-change authorization and producer journal on v4.
use super::*;
use foundry_exec::{ExecutionIntent, ProductSelectionBinding};
use rusqlite::Transaction;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod retained_git;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TaskCheck {
    argv: Vec<String>,
    timeout_secs: u32,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CodeChangeTask {
    kind: String,
    request: String,
    acceptance: Vec<String>,
    base_commit: String,
    pack_digest: String,
    allowed_paths: Vec<String>,
    checks: Vec<TaskCheck>,
    total_timeout_secs: u32,
    model: String,
    max_model_calls: u32,
    max_cost_microusd: u64,
    max_fix_generations: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Issue {
    issue_number: u64,
    title: String,
    body: String,
}

fn denied() -> ProductError {
    ProductError::Invariant("code-change authority or evidence unavailable")
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn full_hex(s: &str, n: usize) -> bool {
    s.len() == n
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn normalize(snapshot: &str) -> Result<CodeChangeTask, ProductError> {
    let issue: Issue = serde_json::from_str(snapshot).map_err(|_| denied())?;
    if issue.issue_number == 0 || issue.title.len() > 16384 || issue.body.len() > 256 * 1024 {
        return Err(denied());
    }
    let mut t: CodeChangeTask = serde_json::from_str(&issue.body).map_err(|_| denied())?;
    let text = |s: &str, limit| !s.trim().is_empty() && s.len() <= limit && !s.contains('\0');
    if t.kind != "code-change-v1"
        || !text(&t.request, 16384)
        || !(1..=64).contains(&t.acceptance.len())
        || t.acceptance.iter().any(|s| !text(s, 4096))
        || !full_hex(&t.base_commit, 40)
        || !full_hex(&t.pack_digest, 64)
        || !(1..=256).contains(&t.allowed_paths.len())
        || t.allowed_paths.iter().any(|p| {
            !text(p, 4096)
                || p.split('/').any(|c| {
                    matches!(c, "" | "." | "..")
                        || c.eq_ignore_ascii_case(".git")
                        || !c
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                })
        })
        || !(1..=3600).contains(&t.total_timeout_secs)
        || t.checks.len() != 1
        || t.checks[0].argv != ["git", "diff", "--check"]
        || !(1..=t.total_timeout_secs).contains(&t.checks[0].timeout_secs)
        || t.model != "none"
        || t.max_model_calls != 0
        || t.max_cost_microusd != 0
        || t.max_fix_generations != 0
    {
        return Err(denied());
    }
    t.allowed_paths.sort();
    if t.allowed_paths.windows(2).any(|p| p[0] == p[1]) {
        return Err(denied());
    }
    Ok(t)
}

/// Pin the first persisted issue, including unsupported bodies, before the
/// drain can see an edit. On a pre-W2 store the existing inbox supplies the
/// observation history. An existing admission must still match that anchor.
pub(super) fn pin_first_observation(
    tx: &Transaction<'_>,
    source: &str,
    connector: &str,
    repository: &str,
    issue: &str,
) -> Result<(), ProductError> {
    tx.execute_batch("
        CREATE TABLE IF NOT EXISTS product_issue_first_v1(
         source TEXT NOT NULL, connector TEXT NOT NULL, repository TEXT NOT NULL, issue TEXT NOT NULL,
         delivery TEXT NOT NULL UNIQUE REFERENCES product_intake_inbox_v1(delivery_id),
         snapshot BLOB NOT NULL, snapshot_sha256 TEXT NOT NULL, PRIMARY KEY(source,connector,repository,issue));
        CREATE TRIGGER IF NOT EXISTS product_issue_first_no_replace BEFORE INSERT ON product_issue_first_v1
         WHEN EXISTS(SELECT 1 FROM product_issue_first_v1 WHERE delivery=NEW.delivery OR
          (source=NEW.source AND connector=NEW.connector AND repository=NEW.repository AND issue=NEW.issue))
         BEGIN SELECT RAISE(ABORT,'immutable first issue'); END;
        CREATE TRIGGER IF NOT EXISTS product_issue_first_preserve BEFORE UPDATE ON product_issue_first_v1
         BEGIN SELECT RAISE(ABORT,'immutable first issue'); END;
        CREATE TRIGGER IF NOT EXISTS product_issue_first_retain BEFORE DELETE ON product_issue_first_v1
         BEGIN SELECT RAISE(ABORT,'retained first issue'); END;")?;
    let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM product_issue_first_v1 WHERE source=?1 AND connector=?2 AND repository=?3 AND issue=?4)",
        params![source,connector,repository,issue],|r|r.get(0))?;
    if exists {
        return Ok(());
    }
    let (id,snapshot):(String,String)=tx.query_row(
        "SELECT i.delivery_id,i.payload FROM product_intake_inbox_v1 i JOIN product_intake_source_bindings_v1 b USING(delivery_id)
         WHERE b.source_instance_id=?1 AND i.connector_id=?2 AND i.repository_id=?3 AND i.issue_id=?4 ORDER BY i.rowid LIMIT 1",
        params![source,connector,repository,issue],|r|Ok((r.get(0)?,r.get(1)?)))?;
    tx.execute(
        "INSERT INTO product_issue_first_v1 VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![
            source,
            connector,
            repository,
            issue,
            id,
            snapshot.as_bytes(),
            sha(snapshot.as_bytes())
        ],
    )?;
    Ok(())
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS product_code_tasks_v1(
 order_id TEXT PRIMARY KEY, lifecycle INTEGER NOT NULL, actor TEXT NOT NULL, request_key TEXT NOT NULL,
 task_bytes BLOB NOT NULL, receipt TEXT NOT NULL, UNIQUE(actor,request_key));
CREATE TABLE IF NOT EXISTS product_producers_v1(
 order_id TEXT PRIMARY KEY REFERENCES product_code_tasks_v1(order_id), operation_key TEXT NOT NULL,
 service TEXT NOT NULL, service_revision INTEGER NOT NULL, credential_revision INTEGER NOT NULL,
 worker TEXT NOT NULL, epoch INTEGER NOT NULL, fence INTEGER NOT NULL, deadline TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('reserved','sealed','collected','uncertain')),
 result_commit TEXT, artifact BLOB, artifact_sha256 TEXT, UNIQUE(service,operation_key));
CREATE TABLE IF NOT EXISTS product_producer_events_v1(
 order_id TEXT NOT NULL REFERENCES product_producers_v1(order_id), state TEXT NOT NULL, at TEXT NOT NULL,
 PRIMARY KEY(order_id,state));
CREATE TRIGGER IF NOT EXISTS product_task_no_replace BEFORE INSERT ON product_code_tasks_v1
 WHEN EXISTS(SELECT 1 FROM product_code_tasks_v1 WHERE order_id=NEW.order_id OR (actor=NEW.actor AND request_key=NEW.request_key))
 BEGIN SELECT RAISE(ABORT,'immutable task'); END;
CREATE TRIGGER IF NOT EXISTS product_producer_no_replace BEFORE INSERT ON product_producers_v1
 WHEN EXISTS(SELECT 1 FROM product_producers_v1 WHERE order_id=NEW.order_id OR (service=NEW.service AND operation_key=NEW.operation_key))
 BEGIN SELECT RAISE(ABORT,'retained producer'); END;
CREATE TRIGGER IF NOT EXISTS product_producer_no_delete BEFORE DELETE ON product_producers_v1
 BEGIN SELECT RAISE(ABORT,'retained producer'); END;
CREATE TRIGGER IF NOT EXISTS product_producer_event_no_replace BEFORE INSERT ON product_producer_events_v1
 WHEN EXISTS(SELECT 1 FROM product_producer_events_v1 WHERE order_id=NEW.order_id AND state=NEW.state)
 BEGIN SELECT RAISE(ABORT,'retained producer event'); END;
CREATE TRIGGER IF NOT EXISTS product_producer_event_no_update BEFORE UPDATE ON product_producer_events_v1
 BEGIN SELECT RAISE(ABORT,'retained producer event'); END;
CREATE TRIGGER IF NOT EXISTS product_producer_event_no_delete BEFORE DELETE ON product_producer_events_v1
 BEGIN SELECT RAISE(ABORT,'retained producer event'); END;
CREATE TRIGGER IF NOT EXISTS product_task_immutable BEFORE UPDATE ON product_code_tasks_v1
 BEGIN SELECT RAISE(ABORT,'immutable task'); END;
CREATE TRIGGER IF NOT EXISTS product_task_retained BEFORE DELETE ON product_code_tasks_v1
 BEGIN SELECT RAISE(ABORT,'retained task'); END;
CREATE TRIGGER IF NOT EXISTS product_producer_terminal BEFORE UPDATE ON product_producers_v1
 WHEN OLD.state IN ('collected','uncertain') OR
 (OLD.state='sealed' AND (OLD.artifact IS NOT NEW.artifact OR OLD.result_commit IS NOT NEW.result_commit OR OLD.artifact_sha256 IS NOT NEW.artifact_sha256))
 BEGIN SELECT RAISE(ABORT,'sealed producer'); END;";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReceipt {
    pub order_id: String,
    pub lifecycle: i64,
    pub source: String,
    pub installation: String,
    pub database: String,
    pub connector: String,
    pub repository: String,
    pub issue: String,
    pub generation: i64,
    pub source_revision: i64,
    pub instance_revision: i64,
    pub delivery_id: String,
    pub admission_sequence: i64,
    pub admission_hash: String,
    pub snapshot_sha256: String,
    pub task_sha256: String,
    pub base_commit: String,
    pub pack_digest: String,
    pub actor: String,
    pub authority_revision: i64,
    pub scope_revision: i64,
    pub grant_revision: i64,
    pub selection_revision: i64,
    pub approval_revision: i64,
    pub request_key: String,
}

impl TaskReceipt {
    pub fn selection(&self) -> ProductSelectionBinding {
        ProductSelectionBinding {
            order_id: self.order_id.clone(),
            lifecycle: self.lifecycle,
            source_instance_id: self.source.clone(),
            pack_digest: self.pack_digest.clone(),
            selection_revision: self.selection_revision,
            referenced_approval_revision: self.approval_revision,
        }
    }
}

fn selected(tx: &Transaction<'_>, r: &TaskReceipt) -> Result<(), ProductError> {
    let (installation, database): (String, String) = tx.query_row(
        "SELECT installation_id,database_id FROM product_meta_v1 WHERE singleton=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let state = w1_selection_state_in(
        tx,
        &installation,
        &database,
        &r.order_id,
        r.lifecycle,
        &r.source,
    )?;
    if !state.active
        || state.pack_digest != r.pack_digest
        || state.selection_revision != r.selection_revision
        || state.referenced_approval_revision != r.approval_revision
    {
        return Err(denied());
    }
    Ok(())
}

fn load_task(
    tx: &Transaction<'_>,
    order: &str,
) -> Result<(TaskReceipt, CodeChangeTask), ProductError> {
    let (bytes,receipt,snapshot):(Vec<u8>,String,String)=tx.query_row(
        "SELECT t.task_bytes,t.receipt,i.payload FROM product_code_tasks_v1 t
         JOIN product_orders_v1 o ON o.order_id=t.order_id AND o.lifecycle=t.lifecycle
         JOIN product_work_backlog_v1 b ON b.order_id=t.order_id AND b.lifecycle=t.lifecycle
         JOIN product_intake_inbox_v1 i ON i.delivery_id=b.delivery_id AND i.payload=o.payload_snapshot
         JOIN product_issue_first_v1 first ON first.delivery=i.delivery_id AND first.snapshot=CAST(i.payload AS BLOB) AND first.snapshot_sha256=json_extract(t.receipt,'$.snapshot_sha256')
         WHERE t.order_id=?1 AND i.state='acked' AND o.task_kind='github-issue'
         AND o.admission_hash=json_extract(t.receipt,'$.admission_hash')
         AND first.connector=json_extract(t.receipt,'$.connector') AND first.repository=json_extract(t.receipt,'$.repository')
         AND first.issue=json_extract(t.receipt,'$.issue') AND first.source=json_extract(t.receipt,'$.source')
         AND EXISTS(SELECT 1 FROM product_meta_v1 m WHERE m.singleton=1 AND m.installation_id=json_extract(t.receipt,'$.installation') AND m.database_id=json_extract(t.receipt,'$.database'))
         AND o.admission_seq=json_extract(t.receipt,'$.admission_sequence')
         AND i.delivery_id=json_extract(t.receipt,'$.delivery_id')",[order],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    let r: TaskReceipt = serde_json::from_str(&receipt).map_err(|_| denied())?;
    let t = normalize(&snapshot)?;
    if r.order_id != order
        || r.snapshot_sha256 != sha(snapshot.as_bytes())
        || r.task_sha256 != sha(&bytes)
        || bytes != serde_json::to_vec(&t).map_err(|_| denied())?
        || r.base_commit != t.base_commit
        || r.pack_digest != t.pack_digest
    {
        return Err(denied());
    }
    let owner = ProductPrincipal {
        actor_id: r.actor.clone(),
        role: "owner".into(),
        authority_revision: r.authority_revision,
    };
    let current:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM product_accounts_v1 WHERE actor_id=?1 AND enabled=1 AND role='owner' AND revision=?2)",params![r.actor,r.authority_revision],|row|row.get(0))?;
    let view =
        require_order_capability_in(tx, &owner, order, ProductCapability::AuthorizeTask, true)?;
    if !current
        || view.scope_revision != r.scope_revision
        || view.grant_revision != r.grant_revision
        || view.source_instance_id != r.source
        || view.lifecycle != r.lifecycle
    {
        return Err(denied());
    }
    selected(tx, &r)?;
    Ok((r, t))
}

impl ProductStore {
    pub fn authorize_code_change(
        &mut self,
        session: &str,
        order: &str,
        lifecycle: i64,
        admission: &str,
        task_sha256: &str,
        key: &str,
    ) -> Result<TaskReceipt, ProductError> {
        if self.open_mode != StoreOpenMode::Normal || !w1_text(key) {
            return Err(denied());
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_product_pin_version(&tx)?;
        let owner = resolve_session_in(&tx, session, self.clock.now())?;
        if owner.role != "owner" {
            return Err(denied());
        }
        let view = require_order_capability_in(
            &tx,
            &owner,
            order,
            ProductCapability::AuthorizeTask,
            true,
        )?;
        let (connector,repository,issue):(String,String,String)=tx.query_row(
            "SELECT connector_id,repository_id,issue_id FROM product_orders_v1 WHERE order_id=?1 AND lifecycle=?2 AND admission_hash=?3 AND task_kind='github-issue'",
            params![order,lifecycle,admission],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        pin_first_observation(
            &tx,
            &view.source_instance_id,
            &connector,
            &repository,
            &issue,
        )?;
        let (snapshot,delivery_id,generation,instance_revision,source_revision):(String,String,i64,i64,i64)=tx.query_row(
            "SELECT i.payload,i.delivery_id,b.source_generation,b.source_instance_revision,b.source_revision FROM product_orders_v1 o
             JOIN product_work_backlog_v1 w ON w.order_id=o.order_id AND w.lifecycle=o.lifecycle
             JOIN product_intake_inbox_v1 i ON i.delivery_id=w.delivery_id AND i.payload=o.payload_snapshot AND i.state='acked'
             JOIN product_intake_source_bindings_v1 b USING(delivery_id)
             JOIN product_issue_first_v1 first ON first.delivery=i.delivery_id AND first.source=b.source_instance_id AND first.snapshot=CAST(i.payload AS BLOB)
             JOIN product_sources_v1 s ON s.connector_id=o.connector_id AND s.enabled=1 AND s.revision=o.source_revision
             JOIN product_scope_source_instances_v1 si ON si.source_instance_id=b.source_instance_id AND si.enabled=1
             WHERE o.order_id=?1 AND o.lifecycle=?2 AND o.admission_hash=?3 AND o.task_kind='github-issue'
             AND b.source_instance_id=?4 AND si.revision=b.source_instance_revision AND si.generation=b.source_generation
             AND b.source_revision=o.source_revision",params![order,lifecycle,admission,view.source_instance_id],
            |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        let t = normalize(&snapshot)?;
        let bytes = serde_json::to_vec(&t).map_err(|_| denied())?;
        let sel = w1_selection_state_in(
            &tx,
            &self.installation_id,
            &self.database_id,
            order,
            lifecycle,
            &view.source_instance_id,
        )?;
        if sha(&bytes) != task_sha256 || !sel.active || sel.pack_digest != t.pack_digest {
            return Err(denied());
        }
        let r = TaskReceipt {
            order_id: order.into(),
            lifecycle,
            source: view.source_instance_id,
            installation: self.installation_id.clone(),
            database: self.database_id.clone(),
            connector,
            repository,
            issue,
            generation,
            source_revision,
            instance_revision,
            delivery_id,
            admission_sequence: view.admission_sequence,
            admission_hash: admission.into(),
            snapshot_sha256: sha(snapshot.as_bytes()),
            task_sha256: task_sha256.into(),
            base_commit: t.base_commit,
            pack_digest: t.pack_digest,
            actor: owner.actor_id,
            authority_revision: owner.authority_revision,
            scope_revision: view.scope_revision,
            grant_revision: view.grant_revision,
            selection_revision: sel.selection_revision,
            approval_revision: sel.referenced_approval_revision,
            request_key: key.into(),
        };
        tx.execute_batch(SCHEMA)?;
        let encoded = serde_json::to_string(&r).map_err(|_| denied())?;
        if let Some(existing) = tx
            .query_row(
                "SELECT receipt FROM product_code_tasks_v1 WHERE order_id=?1",
                [order],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            if existing != encoded {
                return Err(denied());
            }
        } else {
            tx.execute(
                "INSERT INTO product_code_tasks_v1 VALUES(?1,?2,?3,?4,?5,?6)",
                params![order, lifecycle, r.actor, key, bytes, encoded],
            )?;
        }
        tx.commit()?;
        Ok(r)
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ProducerStatus {
    pub order_id: String,
    pub lifecycle: i64,
    pub operation_key: String,
    pub state: String,
    pub worker: String,
    pub epoch: i64,
    pub fence: i64,
    pub deadline: String,
    pub result_commit: Option<String>,
    pub artifact_sha256: Option<String>,
}

impl ProducerStatus {
    fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            order_id: r.get(0)?,
            lifecycle: r.get(1)?,
            operation_key: r.get(2)?,
            state: r.get(3)?,
            worker: r.get(4)?,
            epoch: r.get(5)?,
            fence: r.get(6)?,
            deadline: r.get(7)?,
            result_commit: r.get(8)?,
            artifact_sha256: r.get(9)?,
        })
    }
}

fn status_in(
    tx: &Transaction<'_>,
    service: &ProductServiceContext,
    order: &str,
    key: &str,
    now: DateTime<Utc>,
) -> Result<ProducerStatus, ProductError> {
    service::require_service_context_in(tx, service, ProductServiceAction::DriveReview, now)?;
    tx.query_row("SELECT p.order_id,t.lifecycle,p.operation_key,p.state,p.worker,p.epoch,p.fence,p.deadline,p.result_commit,p.artifact_sha256
        FROM product_producers_v1 p JOIN product_code_tasks_v1 t USING(order_id)
        WHERE p.order_id=?1 AND p.operation_key=?2 AND p.service=?3 AND json_extract(t.receipt,'$.source')=?4",
        params![order,key,service.service_id,service.source_instance_id],ProducerStatus::from_row).map_err(Into::into)
}

fn claim_in(
    tx: &Transaction<'_>,
    service: &ProductServiceContext,
    p: &ProducerStatus,
    now: DateTime<Utc>,
) -> Result<ProductWorkClaim, ProductError> {
    let (r, _) = load_task(tx, &p.order_id)?;
    if (
        r.source.as_str(),
        r.generation,
        r.instance_revision,
        r.source_revision,
    ) != (
        service.source_instance_id.as_str(),
        service.source_generation,
        service.source_instance_revision,
        service.connector_revision,
    ) || now >= p.deadline.parse::<DateTime<Utc>>().map_err(|_| denied())?
    {
        return Err(denied());
    }
    let (payload,lease):(String,String)=tx.query_row("SELECT o.payload_snapshot,c.lease_until FROM product_producers_v1 p
        JOIN product_orders_v1 o USING(order_id) JOIN product_work_claims_v1 c ON c.order_id=p.order_id AND c.lifecycle=o.lifecycle
        JOIN product_work_backlog_v1 b ON b.order_id=c.order_id AND b.lifecycle=c.lifecycle AND b.state=c.state
        WHERE p.order_id=?1 AND p.service_revision=?2 AND p.credential_revision=?3 AND c.owner_id=p.worker AND c.epoch=p.epoch AND c.fence=p.fence
        AND c.state='claimed' AND c.lease_until=p.deadline AND c.source_revision=?4 AND b.delivery_id=?5",
        params![p.order_id,service.service_revision,service.credential_revision,r.source_revision,r.delivery_id],|row|Ok((row.get(0)?,row.get(1)?)))?;
    let claim = ProductWorkClaim {
        order_id: p.order_id.clone(),
        lifecycle: r.lifecycle,
        delivery_id: r.delivery_id,
        worker_id: p.worker.clone(),
        epoch: p.epoch,
        fence: p.fence,
        source_revision: r.source_revision,
        payload_digest: payload_digest("github-issue", &payload),
        task_kind: "github-issue".into(),
        payload_snapshot: payload,
        lease_until: lease.parse().map_err(|_| denied())?,
        service_binding: Some(foundry_exec::ReviewServiceBinding {
            service_id: service.service_id.clone(),
            service_revision: service.service_revision,
            credential_revision: service.credential_revision,
            source_instance_id: r.source,
        }),
    };
    service::validate_review_work_claim_in(tx, &claim.review_binding(), now)?;
    Ok(claim)
}

fn event(
    tx: &Transaction<'_>,
    order: &str,
    state: &str,
    now: DateTime<Utc>,
) -> Result<(), ProductError> {
    tx.execute(
        "INSERT INTO product_producer_events_v1(order_id,state,at) VALUES(?1,?2,?3)",
        params![order, state, now.to_rfc3339()],
    )?;
    Ok(())
}

impl ProductStore {
    /// A successful return is the sole permission to start one producer. A
    /// lost reply must go to inspection/recovery, never a second reservation.
    pub fn reserve_code_change(
        &mut self,
        service: &ProductServiceContext,
        order: &str,
        key: &str,
    ) -> Result<ProductWorkClaim, ProductError> {
        if !w1_text(key) {
            return Err(denied());
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_product_pin_version(&tx)?;
        let now = self.clock.now();
        service::require_service_context_in(&tx, service, ProductServiceAction::DriveReview, now)?;
        let (r, t) = load_task(&tx, order)?;
        if r.source != service.source_instance_id
            || !registered_pack_snapshot(self.product_registry.as_ref(), &r.pack_digest)
        {
            return Err(denied());
        }
        let n=tx.execute("UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id=?1 AND lifecycle=?2 AND delivery_id=?3 AND state='waiting_unsupported' AND attempts=0",params![order,r.lifecycle,r.delivery_id])?;
        if n != 1 {
            return Err(denied());
        }
        let lease = chrono::Duration::seconds(i64::from(t.total_timeout_secs));
        let worker = format!("code-change-{}", sha(key.as_bytes()));
        let ProductWorkClaimResult::Claimed(claim) =
            Self::claim_scoped_work_in(&tx, service, &worker, Some(order), now, lease)?
        else {
            return Err(denied());
        };
        tx.execute("INSERT INTO product_producers_v1(order_id,operation_key,service,service_revision,credential_revision,worker,epoch,fence,deadline,state) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'reserved')",params![order,key,service.service_id,service.service_revision,service.credential_revision,worker,claim.epoch,claim.fence,claim.lease_until.to_rfc3339()])?;
        let p = status_in(&tx, service, order, key, now)?;
        claim_in(&tx, service, &p, now)?;
        event(&tx, order, "reserved", now)?;
        commit_review::append_execution_transition(
            &tx,
            order,
            r.lifecycle,
            OrderEvent::DieStarted,
            "code-change-reserved",
            &format!("Service/{}", service.service_id),
            &serde_json::to_string(&p).map_err(|_| denied())?,
            now,
        )?;
        tx.commit()?;
        Ok(claim)
    }

    pub fn code_change_status(
        &self,
        service: &ProductServiceContext,
        order: &str,
        key: &str,
    ) -> Result<ProducerStatus, ProductError> {
        let tx = self.conn.unchecked_transaction()?;
        status_in(&tx, service, order, key, self.clock.now())
    }

    pub fn seal_code_change(
        &mut self,
        service: &ProductServiceContext,
        order: &str,
        key: &str,
        repo: &Path,
        head: &str,
    ) -> Result<ProducerStatus, ProductError> {
        let (t, remaining) = {
            let tx = self.conn.unchecked_transaction()?;
            let now = self.clock.now();
            let p = status_in(&tx, service, order, key, now)?;
            if p.state != "reserved" {
                return Err(denied());
            }
            claim_in(&tx, service, &p, now)?;
            let deadline = p.deadline.parse::<DateTime<Utc>>().map_err(|_| denied())?;
            (
                load_task(&tx, order)?.1,
                (deadline - now).to_std().map_err(|_| denied())?,
            )
        };
        let artifact = retained_git::verify(repo, &t, head, remaining)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = self.clock.now();
        let p = status_in(&tx, service, order, key, now)?;
        claim_in(&tx, service, &p, now)?;
        if tx.execute("UPDATE product_producers_v1 SET state='sealed',result_commit=?2,artifact=?3,artifact_sha256=?4 WHERE order_id=?1 AND state='reserved'",params![order,head,&artifact,sha(&artifact)])?!=1{return Err(denied())}
        event(&tx, order, "sealed", now)?;
        let status = status_in(&tx, service, order, key, now)?;
        tx.commit()?;
        Ok(status)
    }

    pub fn collect_code_change(
        &mut self,
        service: &ProductServiceContext,
        order: &str,
        key: &str,
    ) -> Result<(ExecutionIntent, ProductWorkClaim, ProductSelectionBinding), ProductError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = self.clock.now();
        let p = status_in(&tx, service, order, key, now)?;
        let claim = claim_in(&tx, service, &p, now)?;
        let (r, _) = load_task(&tx, order)?;
        let head = p.result_commit.as_deref().ok_or_else(denied)?;
        let artifact: Vec<u8> = tx.query_row(
            "SELECT artifact FROM product_producers_v1 WHERE order_id=?1",
            [order],
            |row| row.get(0),
        )?;
        if p.artifact_sha256 != Some(sha(&artifact)) {
            return Err(denied());
        }
        if p.state == "sealed" {
            tx.execute(
                "UPDATE product_producers_v1 SET state='collected' WHERE order_id=?1",
                [order],
            )?;
            event(&tx, order, "collected", now)?;
            commit_review::append_execution_transition(
                &tx,
                order,
                r.lifecycle,
                OrderEvent::DieSucceeded { attempt: 1 },
                "code-change-collected",
                &format!("Service/{}", service.service_id),
                &serde_json::to_string(&p).map_err(|_| denied())?,
                now,
            )?;
        } else if p.state != "collected" {
            return Err(denied());
        }
        let recorded: String = tx.query_row(
            "SELECT at FROM product_producer_events_v1 WHERE order_id=?1 AND state='collected'",
            [order],
            |row| row.get(0),
        )?;
        let intent = ExecutionIntent {
            order: foundry_exec::OrderId(order.into()),
            attempt: foundry_exec::Attempt(1),
            run: foundry_exec::RunId(1),
            activation: 1,
            generation: foundry_exec::Generation(0),
            epoch: foundry_exec::Epoch(claim.fence as u64),
            recorded_at: recorded.parse().map_err(|_| denied())?,
            external_ref: format!("review:{order}:1:{}..{head}", r.base_commit),
        };
        tx.commit()?;
        Ok((intent, claim, r.selection()))
    }

    /// Reconcile a lost producer. A sealed artifact remains available for
    /// investigation. A concurrent collection and recovery serialize here.
    pub fn mark_code_change_uncertain(
        &mut self,
        service: &ProductServiceContext,
        order: &str,
        key: &str,
    ) -> Result<ProducerStatus, ProductError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = self.clock.now();
        let p = status_in(&tx, service, order, key, now)?;
        if matches!(p.state.as_str(), "reserved" | "sealed") {
            tx.execute(
                "UPDATE product_producers_v1 SET state='uncertain' WHERE order_id=?1",
                [order],
            )?;
            event(&tx, order, "uncertain", now)?;
        } else if p.state != "uncertain" {
            return Err(denied());
        }
        let p = status_in(&tx, service, order, key, now)?;
        tx.commit()?;
        Ok(p)
    }
}

pub(super) fn enabled_for(tx: &Transaction<'_>, order: &str) -> Result<bool, ProductError> {
    // The immutable admission kind and body identify the profile even if
    // someone removes the task sidecar. Missing collection then fails closed.
    let item:Option<String>=tx.query_row("SELECT payload_snapshot FROM product_orders_v1 WHERE order_id=?1 AND task_kind='github-issue' AND (SELECT schema_version FROM product_meta_v1 WHERE singleton=1)=4",[order],|r|r.get(0)).optional()?;
    Ok(item.is_some_and(|snapshot| normalize(&snapshot).is_ok()))
}

pub(super) fn collected(
    tx: &Transaction<'_>,
    order: &str,
) -> Result<(TaskReceipt, ProducerStatus), ProductError> {
    let (r, _) = load_task(tx, order)?;
    let p=tx.query_row("SELECT p.order_id,t.lifecycle,p.operation_key,p.state,p.worker,p.epoch,p.fence,p.deadline,p.result_commit,p.artifact_sha256,p.artifact
        FROM product_producers_v1 p JOIN product_code_tasks_v1 t USING(order_id)
        JOIN product_producer_events_v1 e ON e.order_id=p.order_id AND e.state='collected'
        JOIN product_work_claims_v1 c ON c.order_id=p.order_id AND c.lifecycle=t.lifecycle AND c.owner_id=p.worker AND c.epoch=p.epoch AND c.fence=p.fence
        JOIN product_service_work_bindings_v1 sb ON sb.order_id=c.order_id AND sb.lifecycle=c.lifecycle AND sb.service_id=p.service AND sb.service_revision=p.service_revision AND sb.credential_revision=p.credential_revision
        WHERE p.order_id=?1 AND p.state='collected' AND EXISTS(SELECT 1 FROM ledger l JOIN product_transition_receipts_v1 tr ON tr.seq=l.seq AND tr.event_hash=l.hash
         WHERE tr.order_id=p.order_id AND tr.lifecycle=t.lifecycle AND tr.operation='code-change-collected'
           AND json_extract(l.intent,'$.operation_key')=p.operation_key AND json_extract(l.intent,'$.fence')=p.fence
           AND json_extract(l.intent,'$.result_commit')=p.result_commit AND json_extract(l.intent,'$.artifact_sha256')=p.artifact_sha256)",[order],
        |row|Ok((ProducerStatus::from_row(row)?,row.get::<_,Vec<u8>>(10)?)))?;
    if p.0.artifact_sha256 != Some(sha(&p.1)) {
        return Err(denied());
    }
    Ok((r, p.0))
}

impl ProductStore {
    pub fn code_change_for_producer(
        &self,
        service: &ProductServiceContext,
        order: &str,
    ) -> Result<TaskReceipt, ProductError> {
        let tx = self.conn.unchecked_transaction()?;
        service::require_service_context_in(
            &tx,
            service,
            ProductServiceAction::DriveReview,
            self.clock.now(),
        )?;
        let (r, _) = load_task(&tx, order)?;
        if r.source != service.source_instance_id {
            return Err(denied());
        }
        Ok(r)
    }
}

pub(super) fn check_adapter_claim(
    tx: &Transaction<'_>,
    claim: &ProductWorkClaim,
    now: DateTime<Utc>,
) -> Result<(), ProductError> {
    load_task(tx, &claim.order_id)?;
    let exact: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM product_producers_v1 p
         JOIN product_work_claims_v1 c ON c.order_id=p.order_id AND c.lifecycle=?2
         WHERE p.order_id=?1 AND p.worker=?3 AND p.epoch=?4 AND p.fence=?5
           AND p.state='reserved' AND p.deadline=?6 AND c.lease_until=p.deadline AND p.deadline>?7)",
        params![claim.order_id, claim.lifecycle, claim.worker_id, claim.epoch, claim.fence,
                claim.lease_until.to_rfc3339(), now.to_rfc3339()],
        |row| row.get(0),
    )?;
    if !exact {
        return Err(denied());
    }
    Ok(())
}

/// Only fresh v4 approvals require a current originating task and producer.
/// Historical receipts are verified by the existing replay path.
pub(super) fn require_approval_collection(
    tx: &Transaction<'_>,
    intent: &ProductApprovalIntent,
    now: DateTime<Utc>,
) -> Result<(), ProductError> {
    let version: i64 = tx.query_row(
        "SELECT schema_version FROM product_meta_v1 WHERE singleton=1",
        [],
        |row| row.get(0),
    )?;
    if version != 4 {
        return Ok(());
    }
    let (task, producer) = collected(tx, &intent.order_id)?;
    if producer.result_commit.as_ref() != Some(&intent.artifact_digest) {
        return Err(denied());
    }
    let journal = format!(
        "{}:{}:{}:1",
        task.order_id, intent.expected_attempt, intent.run_id
    );
    let exact:bool=tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM product_w1_lifecycle_pins_v1 p
         JOIN product_review_runs_v1 r ON r.order_id=p.order_id AND r.product_lifecycle=p.lifecycle AND r.pack_digest=p.pack_digest
         JOIN product_review_authority_v1 a ON a.order_id=r.order_id AND a.journal_run=r.run_id AND a.service_id=p.service_id
         JOIN product_services_v1 s ON s.service_id=a.service_id AND s.revision=a.service_revision AND s.enabled=1
         JOIN product_service_credentials_v1 c ON c.service_id=s.service_id AND c.credential_revision=a.credential_revision
         JOIN product_service_grants_v1 g ON g.service_id=s.service_id AND g.source_instance_id=p.source_instance_id
             AND g.action='drive_review' AND g.enabled=1 AND g.grant_revision=a.drive_review_grant_revision
         WHERE p.order_id=?1 AND p.lifecycle=?2 AND p.request_key=?3 AND p.claim_epoch=?4 AND p.claim_fence=?5
           AND p.admission_hash=?6 AND p.selection_revision=?7 AND r.base_commit=?8 AND r.result_commit=?9
           AND r.attempt=?10 AND r.run_id=?11 AND r.status='passed' AND s.lease_until>?12 AND c.expires_at>?12)",
        params![task.order_id,task.lifecycle,producer.operation_key,producer.epoch,producer.fence,task.admission_hash,
            task.selection_revision,task.base_commit,intent.artifact_digest,intent.expected_attempt,journal,now.to_rfc3339()],|r|r.get(0))?;
    if !exact {
        return Err(denied());
    }
    Ok(())
}
