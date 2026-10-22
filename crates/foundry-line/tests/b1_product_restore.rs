use foundry_domain::Order;
use foundry_domain::OrderState;
use foundry_domain::ports::{PortError, PortResult, PublishOutcome, Publisher};
use foundry_ledger::{ProductApprovalReceipt, ProductConfig, ProductReviewConfig, ProductStore};
use foundry_line::{
    PackReviewHost, StageExecutor, StageOutput, StageTask, product::ProductProjection,
};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct CountingPublisher {
    calls: AtomicUsize,
    observed: Mutex<Vec<(String, String, String, String)>>,
}

impl Publisher for CountingPublisher {
    fn publish(
        &self,
        order: &Order,
        operation_id: &str,
        expected_digest: &str,
    ) -> impl std::future::Future<Output = PortResult<PublishOutcome>> + Send {
        assert_eq!(order.state(), OrderState::Publishing);
        let seen = (
            order.id.clone(),
            order.artifact_digest.clone().unwrap_or_default(),
            operation_id.to_owned(),
            expected_digest.to_owned(),
        );
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.observed.lock().unwrap().push(seen);
        async { Err(PortError::Permanent("publisher must remain unused".into())) }
    }
}

fn append_waiting_lifecycle(conn: &rusqlite::Connection, order_id: &str) {
    use foundry_domain::OrderEvent;
    for (event, from, to) in [
        (OrderEvent::DieStarted, "queued", "working"),
        (
            OrderEvent::DieSucceeded { attempt: 1 },
            "working",
            "inspecting",
        ),
        (OrderEvent::InspectionPassed, "inspecting", "waiting"),
    ] {
        use sha2::Digest as _;
        let event_repr = format!("{event:?}");
        let event_json = foundry_ledger::event_json::event_to_json(&event);
        let seq: i64 = conn
            .query_row("SELECT COALESCE(MAX(seq),0)+1 FROM ledger", [], |r| {
                r.get(0)
            })
            .unwrap();
        let previous: String = conn
            .query_row(
                "SELECT hash FROM ledger ORDER BY seq DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let predecessor: i64 = conn
            .query_row(
                "SELECT seq FROM ledger WHERE order_id=?1 ORDER BY seq DESC LIMIT 1",
                [order_id],
                |r| r.get(0),
            )
            .unwrap();
        let at = chrono::Utc::now().to_rfc3339();
        let actor = "Agent/github-intake";
        let base = format!("{seq}|{order_id}|{from}|{to}|{event_repr}|{actor}||{at}|{previous}");
        let hash: String = sha2::Sha256::digest(format!("{base}|{event_json}").as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        conn.execute("INSERT INTO product_transition_intents_v1(order_id,lifecycle,expected_predecessor,operation) VALUES(?1,1,?2,'test-lifecycle')", rusqlite::params![order_id, predecessor]).unwrap();
        conn.execute("INSERT INTO ledger(order_id,from_state,to_state,event,actor,intent,at,prev_hash,event_json,hash) VALUES(?1,?2,?3,?4,?5,'',?6,?7,?8,?9)", rusqlite::params![order_id,from,to,event_repr,actor,at,previous,event_json,hash]).unwrap();
    }
}

/// Test-only explicit local adapter selection for the synthetic host fixtures.
/// The claim itself is always issued by ProductStore's durable claim API.
fn synthetic_product_claim(
    store: &mut ProductStore,
    db: &std::path::Path,
    order: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> foundry_ledger::ProductWorkClaim {
    rusqlite::Connection::open(db)
        .unwrap()
        .execute(
            "UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id=?1 AND lifecycle=1 AND state='waiting_unsupported'",
            [order],
        )
        .unwrap();
    match store
        .claim_product_work(
            &format!("synthetic-test-{}", std::process::id()),
            now,
            chrono::Duration::minutes(5),
        )
        .unwrap()
    {
        foundry_ledger::ProductWorkClaimResult::Claimed(claim) if claim.order_id() == order => {
            claim
        }
        other => panic!("expected a durable local product claim, got {other:?}"),
    }
}

fn admit_local_delivery(
    store: &mut ProductStore,
    installation: &str,
    connector: &str,
    repository: &str,
    issue: &str,
    order: &str,
    task_kind: &str,
    payload: &str,
    now: chrono::DateTime<chrono::Utc>,
) {
    let updated = now.to_rfc3339();
    let version =
        foundry_ledger::product::delivery_version_digest(issue, &updated, task_kind, payload);
    let delivery = foundry_ledger::ProductDelivery {
        delivery_id: foundry_ledger::product::github_delivery_id(
            installation,
            connector,
            "https://api.github.com",
            repository,
            issue,
            &version,
        ),
        connector_id: connector.into(),
        repository_id: repository.into(),
        issue_id: issue.into(),
        observed_updated_at: updated.clone(),
        version_digest: version,
        task_kind: task_kind.into(),
        payload: payload.into(),
    };
    let cursor = foundry_ledger::ProductPollCursor {
        connector_id: connector.into(),
        query_fingerprint: format!("synthetic-{order}"),
        sweep_id: "synthetic-local".into(),
        next_page: 1,
        page_ceiling: 1,
        updated_at: updated,
    };
    store
        .persist_intake_page(&cursor, &[delivery.clone()])
        .unwrap();
    store
        .admit_intake_delivery(&delivery.delivery_id, order, now)
        .unwrap();
}

/// Build a real source/service/admission/work-claim chain for continuation
/// positives. Only the final local P2 adapter selection is synthetic; all
/// source, service, grant, credential, admission and activation authority is
/// captured through the ProductStore APIs.
fn service_backed_work(
    db: &std::path::Path,
    config: ProductConfig,
    order: &str,
    issue: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> (ProductStore, String, foundry_ledger::ProductWorkClaim) {
    use foundry_ledger::{
        ProductCapability, ProductPollCursor, ProductServiceAction, ProductServiceGrant,
        RepositoryIdentity, ScopeGrantManifest, ScopeSourceManifest,
    };

    let source_id = format!("{order}-source-v1");
    let source = ScopeSourceManifest {
        connector_id: "gh".into(),
        source_instance_id: source_id.clone(),
        generation: 1,
        identity: RepositoryIdentity::canonicalized(
            "acme",
            "github",
            "https://api.github.com",
            "repo:1",
        )
        .unwrap(),
        api_owner: Some("acme".into()),
        api_repo: Some("repo".into()),
        legacy_repository_locator: "repo:1".into(),
        scope_query: "open+foundry:make".into(),
        enabled: true,
    };
    let mut store = ProductStore::open(db, config).unwrap();
    let owner_phc =
        foundry_ledger::product_identity::ProductPasswordKdf::hash("synthetic-owner-password")
            .unwrap();
    store
        .bootstrap_owner_scoped(
            "owner",
            &owner_phc,
            std::slice::from_ref(&source),
            &[ScopeGrantManifest {
                actor_id: "owner".into(),
                source_instance_id: source_id.clone(),
                capabilities: vec![
                    ProductCapability::ReadReview,
                    ProductCapability::ResolveReviewHold,
                ],
            }],
        )
        .unwrap();
    let session = store
        .login("owner", "synthetic-owner-password", now)
        .unwrap()
        .digest()
        .to_owned();
    let actions = [
        ProductServiceAction::IntakeRead,
        ProductServiceAction::AdmitDelivery,
        ProductServiceAction::AckDelivery,
        ProductServiceAction::DriveReview,
    ]
    .into_iter()
    .map(|action| ProductServiceGrant {
        source_instance_id: source_id.clone(),
        action,
    })
    .collect::<Vec<_>>();
    let service_id = format!("{order}-service");
    store
        .manage_product_service(
            &session,
            &service_id,
            &actions,
            true,
            now + chrono::Duration::hours(3),
            0,
            &format!("{order}-service-config"),
            now,
        )
        .unwrap();
    let token = store
        .issue_product_service_credential(
            &session,
            &service_id,
            1,
            now + chrono::Duration::hours(2),
            &format!("{order}-service-credential"),
            now,
        )
        .unwrap()
        .expose_once_to_file_writer()
        .to_owned();
    let service = store
        .authenticate_product_service_for_action(
            &token,
            "gh",
            "https://api.github.com",
            "repo:1",
            ProductServiceAction::DriveReview,
            now,
        )
        .unwrap();
    let updated_at = now.to_rfc3339();
    let payload = "immutable local adapter task";
    let version =
        foundry_ledger::product::delivery_version_digest(issue, &updated_at, "dep-bump", payload);
    let delivery = foundry_ledger::ProductDelivery {
        delivery_id: foundry_ledger::product::github_delivery_id(
            store.installation_id(),
            "gh",
            "https://api.github.com",
            "repo:1",
            issue,
            &version,
        ),
        connector_id: "gh".into(),
        repository_id: "repo:1".into(),
        issue_id: issue.into(),
        observed_updated_at: updated_at.clone(),
        version_digest: version,
        task_kind: "dep-bump".into(),
        payload: payload.into(),
    };
    let cursor = ProductPollCursor {
        connector_id: "gh".into(),
        query_fingerprint: format!("{order}-query"),
        sweep_id: format!("{order}-sweep"),
        next_page: 1,
        page_ceiling: 1,
        updated_at,
    };
    store
        .persist_intake_page_for_service(&service, &cursor, &[delivery.clone()], &[], now)
        .unwrap();
    let admission = store
        .admit_intake_delivery_for_service(&service, &delivery.delivery_id, order, now)
        .unwrap();
    store
        .mark_intake_acked_for_service(&service, &delivery.delivery_id, &admission, now)
        .unwrap();

    // This explicitly selects the synthetic local adapter used by this test;
    // it does not create or modify any source/service/activation authority.
    rusqlite::Connection::open(db)
        .unwrap()
        .execute(
            "UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id=?1 AND lifecycle=1 AND state='waiting_unsupported'",
            [order],
        )
        .unwrap();
    let claim = match store
        .claim_product_work_for_service(
            &service,
            &format!("restore-test-worker-{}", std::process::id()),
            now,
            chrono::Duration::minutes(5),
        )
        .unwrap()
    {
        foundry_ledger::ProductWorkClaimResult::Claimed(claim) if claim.order_id() == order => {
            claim
        }
        other => panic!("expected a genuine service-authorized claim, got {other:?}"),
    };
    (store, session, claim)
}

fn assert_activation_authority_is_the_live_service_snapshot(
    db: &std::path::Path,
    order: &str,
    attempt: i64,
    run: i64,
    activation: i64,
) {
    let captured: bool = rusqlite::Connection::open(db)
        .unwrap()
        .query_row(
            "SELECT EXISTS(
               SELECT 1 FROM product_review_authority_v1 a
               JOIN product_services_v1 s ON s.service_id=a.service_id
               JOIN product_service_credentials_v1 c ON c.service_id=s.service_id
               JOIN product_service_grants_v1 g ON g.service_id=s.service_id
                 AND g.source_instance_id=a.source_instance_id AND g.action='drive_review'
               JOIN product_scope_source_instances_v1 i ON i.source_instance_id=a.source_instance_id
               JOIN product_sources_v1 ps ON ps.connector_id=i.connector_id
               WHERE a.order_id=?1 AND a.attempt=?2 AND a.run_id=?3 AND a.activation=?4
                 AND a.service_revision=s.revision
                 AND a.credential_revision=c.credential_revision
                 AND a.drive_review_grant_revision=g.grant_revision AND g.enabled=1
                 AND a.source_generation=i.generation AND a.source_instance_revision=i.revision
                 AND a.connector_revision=ps.revision AND ps.enabled=1 AND i.enabled=1
             )",
            rusqlite::params![order, attempt, run, activation],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        captured,
        "activation authority must match the real current service, credential, grant, and source rows"
    );
}

#[test]
fn product_restore_uses_verified_s1_anchor_and_keeps_memory_on_bad_mirror() {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "foundry-b1-product-restore-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("product.db");
    let mut store = ProductStore::open(
        &db,
        ProductConfig {
            installation_id: "restore-install".into(),
            database_id: "restore-db".into(),
        },
    )
    .unwrap();
    store
        .bind_source("gh", "github", "https://api.github.com", "repo:1", "repo:1")
        .unwrap();
    store
        .admit_order(
            "order-1",
            "gh",
            "repo:1",
            "issue:1",
            "dep-bump",
            "frozen payload",
            chrono::Utc::now(),
        )
        .unwrap();

    let mut projection = ProductProjection::default();
    projection.catch_up(&mut store).unwrap();
    let anchor = projection.anchor().unwrap().clone();
    assert_eq!(
        projection.get("order-1").unwrap().state(),
        OrderState::Queued
    );
    assert_eq!(anchor.sequence, 1);

    let mirror = foundry_ledger::Mirror::new(&db);
    std::fs::write(mirror.path(), "{torn").unwrap();
    assert!(projection.catch_up(&mut store).is_err());
    assert_eq!(projection.anchor(), Some(&anchor));
    assert_eq!(
        projection.get("order-1").unwrap().state(),
        OrderState::Queued
    );

    std::fs::remove_file(mirror.path()).unwrap();
    projection.catch_up(&mut store).unwrap();
    assert_eq!(projection.anchor(), Some(&anchor));
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn product_publish_composition_rejects_uncommitted_receipt_before_publisher() {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "foundry-b1-product-publish-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("product.db");
    let mut store = ProductStore::open(
        &db,
        ProductConfig {
            installation_id: "publish-install".into(),
            database_id: "publish-db".into(),
        },
    )
    .unwrap();
    store
        .bind_source("gh", "github", "https://api.github.com", "repo:1", "repo:1")
        .unwrap();
    store
        .admit_order(
            "order-1",
            "gh",
            "repo:1",
            "issue:1",
            "dep-bump",
            "frozen payload",
            chrono::Utc::now(),
        )
        .unwrap();
    let mut projection = ProductProjection::default();
    projection.catch_up(&mut store).unwrap();
    let forged = ProductApprovalReceipt {
        operation_id: "forged-operation".into(),
        actor_id: "owner".into(),
        order_id: "order-1".into(),
        lifecycle: 1,
        sequence: 2,
        event_hash: "a".repeat(64),
        artifact_digest: "b".repeat(40),
        attempt: 1,
        run_id: 1,
        generation: 1,
        tuple_digest: "c".repeat(64),
        source_revision: 1,
    };
    let publisher = CountingPublisher {
        calls: AtomicUsize::new(0),
        observed: Mutex::new(Vec::new()),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    assert!(
        runtime
            .block_on(projection.publish_approved(&store, &forged, &publisher))
            .is_err()
    );
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        projection.get("order-1").unwrap().state(),
        OrderState::Queued
    );
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

// The former SQL-seeded publish-positive block is superseded by the genuine
// host → strict result → human approval → explicit v3 legacy-publish rejection
// path in the required-feature `b1_product_results` target.

struct ProductHoldExecutor;
impl StageExecutor for ProductHoldExecutor {
    fn execute(&mut self, _task: &StageTask) -> Result<StageOutput, String> {
        Err("synthetic local stage failure".into())
    }
}

struct CountingHoldExecutor(std::sync::Arc<std::sync::atomic::AtomicUsize>);
impl StageExecutor for CountingHoldExecutor {
    fn execute(&mut self, _task: &StageTask) -> Result<StageOutput, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err("synthetic local stage failure".into())
    }
}

struct ProductContinuationExecutor {
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl StageExecutor for ProductContinuationExecutor {
    fn execute(&mut self, task: &StageTask) -> Result<StageOutput, String> {
        use std::sync::atomic::Ordering;
        self.calls.fetch_add(1, Ordering::SeqCst);
        let output = task.workdir.join("continued-stage-output.log");
        std::fs::write(&output, b"synthetic accepted continuation output\n")
            .map_err(|e| e.to_string())?;
        Ok(StageOutput {
            verdict_category: Some("patch".into()),
            findings: true,
            log_path: output.to_string_lossy().into_owned(),
            inspected_commit: (task.role == "verify").then(|| task.artifact_commit.clone()),
            verdict_categories: vec![],
            produced_commit: (task.role == "fix").then(|| task.artifact_commit.clone()),
        })
    }
    fn restore_session(
        &mut self,
        _session_id: &str,
        _workdir: &std::path::Path,
    ) -> Result<(), String> {
        Ok(())
    }
}

struct DisableSourceDuringExecution {
    db: std::path::PathBuf,
    session: String,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl StageExecutor for DisableSourceDuringExecution {
    fn execute(&mut self, task: &StageTask) -> Result<StageOutput, String> {
        use std::sync::atomic::Ordering;
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut store = ProductStore::open(
            &self.db,
            ProductConfig {
                installation_id: "host-e2e-install".into(),
                database_id: "host-e2e-db".into(),
            },
        )
        .map_err(|e| e.to_string())?;
        store
            .configure_source(
                &self.session,
                "gh",
                "github",
                "https://api.github.com",
                "repo:1",
                "repo:1",
                false,
                chrono::Utc::now(),
            )
            .map_err(|e| e.to_string())?;
        let output = task.workdir.join("synthetic-stage-output.log");
        std::fs::write(&output, b"synthetic stage output\n").map_err(|e| e.to_string())?;
        Ok(StageOutput {
            verdict_category: Some("pass".into()),
            findings: false,
            log_path: output.to_string_lossy().into_owned(),
            inspected_commit: (task.role == "verify").then(|| task.artifact_commit.clone()),
            verdict_categories: vec![],
            produced_commit: None,
        })
    }
}

#[test]
fn product_store_host_writes_real_frozen_hold_then_accepts_exact_snapshot() {
    use foundry_exec::{Epoch, ExecutionIntent, Generation, OrderId, ReviewHostStore, RunId};
    use foundry_ledger::{ProductHoldDecision, ProductHoldExpectation};

    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "foundry-b1-product-host-e2e-{}-{n}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("product.db");
    let config = ProductConfig {
        installation_id: "host-e2e-install".into(),
        database_id: "host-e2e-db".into(),
    };
    let now = chrono::Utc::now();
    let (store, session, work_claim) =
        service_backed_work(&db, config.clone(), "host-order", "issue:19", now);
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        append_waiting_lifecycle(&conn, "host-order");
    }
    let pack =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
    let mut host = PackReviewHost::with_product_store(
        pack,
        ProductHoldExecutor,
        store,
        root.join("artifacts"),
        root.join("sessions"),
        root.join("claims"),
    )
    .unwrap()
    .with_source_commit("67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0")
    .unwrap();
    let intent = ExecutionIntent {
        order: OrderId("host-order".into()),
        attempt: foundry_exec::Attempt(1),
        run: RunId(7),
        activation: 1,
        generation: Generation(0),
        epoch: Epoch(1),
        external_ref: "review:host-order:1:base..result".into(),
        recorded_at: now,
    };
    let held = host
        .try_dispatch_product_work(&intent, &work_claim)
        .unwrap();
    let hold = held
        .held
        .expect("the frozen-pack host must persist its hold outcome");
    drop(host);
    assert_activation_authority_is_the_live_service_snapshot(&db, "host-order", 1, 7, 1);
    let mut store = ProductStore::open(
        &db,
        ProductConfig {
            installation_id: "host-e2e-install".into(),
            database_id: "host-e2e-db".into(),
        },
    )
    .unwrap();
    let latest = store
        .latest_hold("host-order")
        .unwrap()
        .expect("product backend must read the persisted hold back");
    assert_eq!(latest.hold_id, hold.hold_instance_id);
    assert_eq!(latest.snapshot.intent, intent);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let expected = conn.query_row(
        "SELECT lifecycle,attempt,originating_run,activation,host_epoch,hold_revision,source_revision,expected_predecessor,snapshot_digest FROM product_review_holds_v1 WHERE order_id='host-order' AND hold_instance_id=?1",
        [&hold.hold_instance_id],
        |r| Ok(foundry_ledger::ProductHoldExpectation { lifecycle:r.get(0)?,attempt:r.get(1)?,originating_run:r.get(2)?,activation:r.get(3)?,host_epoch:r.get(4)?,hold_revision:r.get(5)?,source_revision:r.get(6)?,predecessor_seq:r.get(7)?,snapshot_digest:r.get(8)? }),
    ).unwrap();
    drop(conn);
    let target = hold
        .resume_targets
        .first()
        .cloned()
        .expect("frozen hold has a target");
    let receipt = store
        .accept_product_review_hold(
            &session,
            "host-order",
            &hold.hold_instance_id,
            &expected,
            "host-e2e-accept",
            ProductHoldDecision::Resume(target),
            now,
        )
        .unwrap();
    assert_eq!(receipt.hold_instance_id, hold.hold_instance_id);
    assert_eq!(receipt.originating_run, 7);
    assert_eq!(receipt.hold_revision, expected.hold_revision);
    let original_receipt_json: String = rusqlite::Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT receipt_json FROM product_review_hold_acceptances_v1 WHERE operation_id=?1",
            [&receipt.operation_id],
            |r| r.get(0),
        )
        .unwrap();
    let pack =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
    drop(store);
    let pinned_digest = foundry_pack::load_pack(&pack).unwrap().digest().to_owned();
    let mut stale_probe = ProductStore::open(
        &db,
        ProductConfig {
            installation_id: "host-e2e-install".into(),
            database_id: "host-e2e-db".into(),
        },
    )
    .unwrap();
    let (frozen_snapshot, hold_json_before, progress_before): (
        String,
        String,
        (String, i64, String, i64, i64, i64),
    ) = {
        let conn = rusqlite::Connection::open(&db).unwrap();
        let frozen = conn.query_row(
            "SELECT snapshot_json FROM product_review_holds_v1 WHERE order_id='host-order' AND hold_instance_id=?1",
            [&hold.hold_instance_id], |r| r.get(0),
        ).unwrap();
        let run = conn.query_row(
            "SELECT hold_json FROM product_review_runs_v1 WHERE order_id='host-order' AND hold_instance_id=?1",
            [&hold.hold_instance_id], |r| r.get(0),
        ).unwrap();
        let progress = conn.query_row(
            "SELECT phase,progress_revision,owner_id,epoch,fence,reserved_run_id FROM product_review_hold_progress_v1 WHERE operation_id=?1",
            [&receipt.operation_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
        ).unwrap();
        (frozen, run, progress)
    };
    // Simulate on-disk corruption beneath the immutable-history trigger. The
    // accepted continuation must validate the original frozen digest before
    // it mutates either the evolving run snapshot or its progress fence.
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("DROP TRIGGER product_review_hold_exact_cas;")
            .unwrap();
        conn.execute(
            "UPDATE product_review_holds_v1 SET snapshot_json='{}' WHERE order_id='host-order' AND hold_instance_id=?1",
            [&hold.hold_instance_id],
        ).unwrap();
    }
    assert!(
        stale_probe
            .claim_accepted_hold(&session, &receipt.operation_id, now)
            .is_err(),
        "corrupt immutable snapshot digest must reject continuation"
    );
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        let hold_json_after: String = conn.query_row(
            "SELECT hold_json FROM product_review_runs_v1 WHERE order_id='host-order' AND hold_instance_id=?1",
            [&hold.hold_instance_id], |r| r.get(0),
        ).unwrap();
        let progress_after: (String,i64,String,i64,i64,i64) = conn.query_row(
            "SELECT phase,progress_revision,owner_id,epoch,fence,reserved_run_id FROM product_review_hold_progress_v1 WHERE operation_id=?1",
            [&receipt.operation_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
        ).unwrap();
        assert_eq!(hold_json_after, hold_json_before);
        assert_eq!(progress_after, progress_before);
        conn.execute(
            "UPDATE product_review_holds_v1 SET snapshot_json=?1 WHERE order_id='host-order' AND hold_instance_id=?2",
            rusqlite::params![frozen_snapshot,hold.hold_instance_id],
        ).unwrap();
    }
    stale_probe
        .configure_review_backend(ProductReviewConfig {
            artifact_root: root.join("artifacts"),
            session_root: root.join("sessions"),
            claim_root: root.join("claims"),
            pack_digest: pinned_digest,
        })
        .unwrap();
    assert!(
        stale_probe
            .claim_accepted_hold("foreign-session-digest", &receipt.operation_id, now)
            .is_err(),
        "a foreign session cannot resume another actor's accepted decision"
    );
    drop(stale_probe);
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut continuation = PackReviewHost::with_product_store(
        &pack,
        ProductContinuationExecutor {
            calls: calls.clone(),
        },
        ProductStore::open(
            &db,
            ProductConfig {
                installation_id: "host-e2e-install".into(),
                database_id: "host-e2e-db".into(),
            },
        )
        .unwrap(),
        root.join("artifacts"),
        root.join("sessions"),
        root.join("claims"),
    )
    .unwrap()
    .with_source_commit("67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0")
    .unwrap();
    continuation
        .continue_accepted_product_hold(&session, &receipt.operation_id, now)
        .unwrap();
    assert!(
        calls.load(std::sync::atomic::Ordering::SeqCst) > 0,
        "accepted continuation must use the existing host stage executor"
    );
    drop(continuation);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let followup_journal = format!("host-order:1:{}:1", receipt.reserved_run_id);
    let (phase,status,result_count,stage_count,session_count,h2_id):(String,String,i64,i64,i64,String)=conn.query_row("SELECT (SELECT phase FROM product_review_hold_progress_v1 WHERE operation_id=?1),(SELECT status FROM product_review_runs_v1 WHERE order_id='host-order' AND run_id=?2),(SELECT count(*) FROM product_review_host_results_v1 WHERE order_id='host-order'),(SELECT count(*) FROM product_review_stages_v1 WHERE order_id='host-order' AND run_id=?2 AND status='complete'),(SELECT count(*) FROM product_review_sessions_v1 WHERE order_id='host-order' AND run_id=?2),(SELECT hold_instance_id FROM product_review_runs_v1 WHERE order_id='host-order' AND run_id=?2)",rusqlite::params![receipt.operation_id,followup_journal],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).unwrap();
    assert_eq!(phase, "completed");
    assert_eq!(
        status, "held",
        "the same host algorithm must persist a second hold on the reserved run"
    );
    assert_ne!(h2_id, hold.hold_instance_id);
    assert_eq!(
        result_count, 2,
        "originating and materialized followup results both remain recorded"
    );
    assert!(stage_count > 0);
    assert!(session_count > 0);
    drop(conn);
    let mut second_store = ProductStore::open(
        &db,
        ProductConfig {
            installation_id: "host-e2e-install".into(),
            database_id: "host-e2e-db".into(),
        },
    )
    .unwrap();
    let second_expectation = rusqlite::Connection::open(&db).unwrap().query_row(
        "SELECT lifecycle,attempt,originating_run,activation,host_epoch,hold_revision,source_revision,expected_predecessor,snapshot_digest FROM product_review_holds_v1 WHERE order_id='host-order' AND hold_instance_id=?1",
        [&h2_id],
        |r| Ok(ProductHoldExpectation { lifecycle:r.get(0)?,attempt:r.get(1)?,originating_run:r.get(2)?,activation:r.get(3)?,host_epoch:r.get(4)?,hold_revision:r.get(5)?,source_revision:r.get(6)?,predecessor_seq:r.get(7)?,snapshot_digest:r.get(8)? }),
    ).unwrap();
    let mut foreign_lifecycle = second_expectation.clone();
    foreign_lifecycle.lifecycle += 1;
    assert!(
        second_store
            .accept_product_review_hold(
                &session,
                "host-order",
                &h2_id,
                &foreign_lifecycle,
                "host-e2e-foreign-lifecycle",
                ProductHoldDecision::CommitPass,
                now,
            )
            .is_err(),
        "a followup hold cannot be accepted under a foreign lifecycle"
    );
    let mut changed_source = second_expectation.clone();
    changed_source.source_revision += 1;
    assert!(
        second_store
            .accept_product_review_hold(
                &session,
                "host-order",
                &h2_id,
                &changed_source,
                "host-e2e-changed-source",
                ProductHoldDecision::CommitPass,
                now,
            )
            .is_err(),
        "a followup hold cannot be accepted under a changed source revision"
    );
    let second_receipt = second_store
        .accept_product_review_hold(
            &session,
            "host-order",
            &h2_id,
            &second_expectation,
            "host-e2e-second-accept",
            ProductHoldDecision::CommitPass,
            now,
        )
        .unwrap();
    assert_eq!(second_receipt.originating_run, receipt.reserved_run_id);
    assert!(second_receipt.reserved_run_id > receipt.reserved_run_id);
    drop(second_store);
    let mut terminal_host = PackReviewHost::with_product_store(
        &pack,
        ProductContinuationExecutor {
            calls: calls.clone(),
        },
        ProductStore::open(
            &db,
            ProductConfig {
                installation_id: "host-e2e-install".into(),
                database_id: "host-e2e-db".into(),
            },
        )
        .unwrap(),
        root.join("artifacts"),
        root.join("sessions"),
        root.join("claims"),
    )
    .unwrap();
    terminal_host
        .continue_accepted_product_hold(&session, &second_receipt.operation_id, now)
        .unwrap();
    drop(terminal_host);
    let terminal: (String,String,String,i64,String,String,String) = rusqlite::Connection::open(&db).unwrap().query_row(
        "SELECT (SELECT phase FROM product_review_hold_progress_v1 WHERE operation_id=?1),(SELECT state FROM product_review_holds_v1 WHERE hold_instance_id=?2),(SELECT event FROM ledger WHERE order_id='host-order' ORDER BY seq DESC LIMIT 1),(SELECT count(*) FROM product_review_runs_v1 WHERE order_id='host-order'),(SELECT status FROM product_review_runs_v1 WHERE order_id='host-order' AND run_id='host-order:1:7:1'),(SELECT hold_json FROM product_review_runs_v1 WHERE order_id='host-order' AND run_id='host-order:1:7:1'),(SELECT receipt_json FROM product_review_hold_acceptances_v1 WHERE operation_id=?3)",
        rusqlite::params![second_receipt.operation_id,h2_id,receipt.operation_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)),
    ).unwrap();
    assert_eq!(terminal.0, "completed");
    assert_eq!(terminal.1, "accepted");
    assert_eq!(terminal.2, "InspectionPassed");
    assert_eq!(
        terminal.3, 2,
        "the originating run and its materialized followup remain distinct"
    );
    assert_eq!(
        terminal.4, "held",
        "the first run's history remains immutable after materialization"
    );
    assert_eq!(
        terminal.5, hold_json_before,
        "the original frozen snapshot bytes remain unchanged"
    );
    assert_eq!(
        terminal.6, original_receipt_json,
        "the first acceptance receipt remains unchanged"
    );
    let completed_calls = calls.load(std::sync::atomic::Ordering::SeqCst);
    let mut reopened = PackReviewHost::with_product_store(
        &pack,
        ProductContinuationExecutor {
            calls: calls.clone(),
        },
        ProductStore::open(
            &db,
            ProductConfig {
                installation_id: "host-e2e-install".into(),
                database_id: "host-e2e-db".into(),
            },
        )
        .unwrap(),
        root.join("artifacts"),
        root.join("sessions"),
        root.join("claims"),
    )
    .unwrap();
    assert!(
        reopened
            .continue_accepted_product_hold(&session, &receipt.operation_id, now)
            .is_err(),
        "completed receipt recovery cannot rerun external stages"
    );
    assert!(
        reopened
            .continue_accepted_product_hold(&session, &second_receipt.operation_id, now)
            .is_err(),
        "the second accepted hold's completed terminal decision cannot be replayed"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        completed_calls
    );
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn product_continuation_rejects_corrupt_provenance_before_terminal_result_or_s1_write() {
    use foundry_exec::{Epoch, ExecutionIntent, Generation, OrderId, RunId};
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "foundry-b1-product-continuation-provenance-{}-{n}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("product.db");
    let config = ProductConfig {
        installation_id: "continuation-proof-install".into(),
        database_id: "continuation-proof-db".into(),
    };
    let now = chrono::Utc::now();
    let (store, session, work_claim) =
        service_backed_work(&db, config.clone(), "proof-order", "issue:3", now);
    append_waiting_lifecycle(&rusqlite::Connection::open(&db).unwrap(), "proof-order");
    let pack =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
    let mut host = PackReviewHost::with_product_store(
        &pack,
        ProductHoldExecutor,
        store,
        root.join("artifacts"),
        root.join("sessions"),
        root.join("claims"),
    )
    .unwrap()
    .with_source_commit("67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0")
    .unwrap();
    let intent = ExecutionIntent {
        order: OrderId("proof-order".into()),
        attempt: foundry_exec::Attempt(1),
        run: RunId(5),
        activation: 1,
        generation: Generation(0),
        epoch: Epoch(1),
        external_ref: "review:proof-order:1:base..result".into(),
        recorded_at: now,
    };
    let hold = host
        .try_dispatch_product_work(&intent, &work_claim)
        .unwrap()
        .held
        .unwrap();
    drop(host);
    assert_activation_authority_is_the_live_service_snapshot(&db, "proof-order", 1, 5, 1);
    let mut store = ProductStore::open(&db, config.clone()).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    let expected = conn.query_row(
        "SELECT lifecycle,attempt,originating_run,activation,host_epoch,hold_revision,source_revision,expected_predecessor,snapshot_digest FROM product_review_holds_v1 WHERE order_id='proof-order' AND hold_instance_id=?1",
        [&hold.hold_instance_id],
        |r| Ok(foundry_ledger::ProductHoldExpectation { lifecycle:r.get(0)?,attempt:r.get(1)?,originating_run:r.get(2)?,activation:r.get(3)?,host_epoch:r.get(4)?,hold_revision:r.get(5)?,source_revision:r.get(6)?,predecessor_seq:r.get(7)?,snapshot_digest:r.get(8)? }),
    ).unwrap();
    drop(conn);
    let target = hold
        .resume_targets
        .first()
        .cloned()
        .expect("frozen hold has a resume target");
    let receipt = store
        .accept_product_review_hold(
            &session,
            "proof-order",
            &hold.hold_instance_id,
            &expected,
            "proof-resume",
            foundry_ledger::ProductHoldDecision::Resume(target),
            now,
        )
        .unwrap();
    drop(store);
    let before: (i64,String,String) = rusqlite::Connection::open(&db).unwrap().query_row(
        "SELECT (SELECT MAX(seq) FROM ledger WHERE order_id='proof-order'),(SELECT result_json FROM product_review_host_results_v1 WHERE order_id='proof-order' ORDER BY activation DESC LIMIT 1),(SELECT status FROM product_review_runs_v1 WHERE order_id='proof-order' AND run_id='proof-order:1:5:1')",
        [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).unwrap();
    rusqlite::Connection::open(&db).unwrap().execute(
        "UPDATE product_review_runs_v1 SET provenance_json='{}' WHERE order_id='proof-order' AND run_id='proof-order:1:5:1'",
        [],
    ).unwrap();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut continuation = PackReviewHost::with_product_store(
        &pack,
        ProductContinuationExecutor {
            calls: calls.clone(),
        },
        ProductStore::open(&db, config).unwrap(),
        root.join("artifacts"),
        root.join("sessions"),
        root.join("claims"),
    )
    .unwrap();
    assert!(
        continuation
            .continue_accepted_product_hold(&session, &receipt.operation_id, now)
            .is_err()
    );
    assert!(
        calls.load(Ordering::SeqCst) > 0,
        "the accepted Resume follows the existing host graph before terminal validation"
    );
    drop(continuation);
    let after: (i64,String,String,String) = rusqlite::Connection::open(&db).unwrap().query_row(
        "SELECT (SELECT MAX(seq) FROM ledger WHERE order_id='proof-order'),(SELECT result_json FROM product_review_host_results_v1 WHERE order_id='proof-order' ORDER BY activation DESC LIMIT 1),(SELECT status FROM product_review_runs_v1 WHERE order_id='proof-order' AND run_id='proof-order:1:5:1'),(SELECT to_state FROM ledger WHERE order_id='proof-order' ORDER BY seq DESC LIMIT 1)",
        [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
    ).unwrap();
    assert_eq!(
        (after.0, after.1, after.2),
        before,
        "corrupt continuation provenance cannot overwrite the stored result, terminal run, or S1 head"
    );
    assert_eq!(after.3, "waiting");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn accepted_commit_pass_finalizes_without_dispatching_another_stage() {
    use foundry_exec::{Epoch, ExecutionIntent, Generation, OrderId, RunId};
    use foundry_ledger::{ProductHoldDecision, ProductHoldExpectation};
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "foundry-b1-product-hold-finalize-{}-{n}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("product.db");
    let config = || ProductConfig {
        installation_id: "hold-finalize-install".into(),
        database_id: "hold-finalize-db".into(),
    };
    let mut store = ProductStore::open(&db, config()).unwrap();
    let phc =
        foundry_ledger::product_identity::ProductPasswordKdf::hash("synthetic-owner-password")
            .unwrap();
    store.bootstrap_owner("owner", &phc).unwrap();
    let now = chrono::Utc::now();
    let session = store
        .login("owner", "synthetic-owner-password", now)
        .unwrap()
        .digest()
        .to_owned();
    store
        .bind_source("gh", "github", "https://api.github.com", "repo:1", "repo:1")
        .unwrap();
    admit_local_delivery(
        &mut store,
        "hold-finalize-install",
        "gh",
        "repo:1",
        "issue:91",
        "finalize-order",
        "dep-bump",
        "immutable task",
        now,
    );
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        append_waiting_lifecycle(&conn, "finalize-order");
    }
    let work_claim = synthetic_product_claim(&mut store, &db, "finalize-order", now);
    let pack =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
    let mut host = PackReviewHost::with_product_store(
        &pack,
        ProductHoldExecutor,
        store,
        root.join("artifacts"),
        root.join("sessions"),
        root.join("claims"),
    )
    .unwrap()
    .with_source_commit("67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0")
    .unwrap();
    let intent = ExecutionIntent {
        order: OrderId("finalize-order".into()),
        attempt: foundry_exec::Attempt(1),
        run: RunId(3),
        activation: 1,
        generation: Generation(0),
        epoch: Epoch(1),
        external_ref: "review:finalize-order:1:base..result".into(),
        recorded_at: now,
    };
    let outcome = host
        .try_dispatch_product_work(&intent, &work_claim)
        .unwrap();
    let hold = outcome.held.unwrap();
    drop(host);
    let mut store = ProductStore::open(&db, config()).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    let expected=conn.query_row("SELECT lifecycle,attempt,originating_run,activation,host_epoch,hold_revision,source_revision,expected_predecessor,snapshot_digest FROM product_review_holds_v1 WHERE order_id='finalize-order' AND hold_instance_id=?1",[&hold.hold_instance_id],|r|Ok(ProductHoldExpectation{lifecycle:r.get(0)?,attempt:r.get(1)?,originating_run:r.get(2)?,activation:r.get(3)?,host_epoch:r.get(4)?,hold_revision:r.get(5)?,source_revision:r.get(6)?,predecessor_seq:r.get(7)?,snapshot_digest:r.get(8)?})).unwrap();
    drop(conn);
    let receipt = store
        .accept_product_review_hold(
            &session,
            "finalize-order",
            &hold.hold_instance_id,
            &expected,
            "commit-pass-key",
            ProductHoldDecision::CommitPass,
            now,
        )
        .unwrap();
    drop(store);
    let mut host = PackReviewHost::with_product_store(
        &pack,
        ProductHoldExecutor,
        ProductStore::open(&db, config()).unwrap(),
        root.join("artifacts"),
        root.join("sessions"),
        root.join("claims"),
    )
    .unwrap();
    host.continue_accepted_product_hold(&session, &receipt.operation_id, now)
        .unwrap();
    drop(host);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let (phase,status,exit_code,s1_event,s1_state):(String,String,i64,String,String)=conn.query_row("SELECT (SELECT phase FROM product_review_hold_progress_v1 WHERE operation_id=?1),(SELECT status FROM product_review_runs_v1 WHERE order_id='finalize-order' AND run_id='finalize-order:1:3:1'),json_extract(rr.result_json,'$.exit_code'),(SELECT event FROM ledger WHERE order_id='finalize-order' ORDER BY seq DESC LIMIT 1),(SELECT to_state FROM ledger WHERE order_id='finalize-order' ORDER BY seq DESC LIMIT 1) FROM product_review_runs_v1 rr WHERE order_id='finalize-order' AND run_id='finalize-order:1:3:1'",[&receipt.operation_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap();
    assert_eq!(phase, "completed");
    assert_eq!(status, "resolved");
    assert_eq!(exit_code, 0);
    assert_eq!(s1_event, "InspectionPassed");
    assert_eq!(s1_state, "waiting");
    drop(conn);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn accepted_commit_fail_and_abort_finalize_without_dispatching_another_stage() {
    use foundry_exec::{Epoch, ExecutionIntent, Generation, OrderId, RunId};
    use foundry_ledger::{ProductHoldDecision, ProductHoldExpectation};

    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "foundry-b1-product-terminal-holds-{}-{n}-{nonce}",
        std::process::id(),
    ));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("product.db");
    let config = || ProductConfig {
        installation_id: "terminal-holds-install".into(),
        database_id: "terminal-holds-db".into(),
    };
    let mut store = ProductStore::open(&db, config()).unwrap();
    let phc =
        foundry_ledger::product_identity::ProductPasswordKdf::hash("synthetic-owner-password")
            .unwrap();
    store.bootstrap_owner("owner", &phc).unwrap();
    let now = chrono::Utc::now();
    let session = store
        .login("owner", "synthetic-owner-password", now)
        .unwrap()
        .digest()
        .to_owned();
    store
        .bind_source("gh", "github", "https://api.github.com", "repo:1", "repo:1")
        .unwrap();
    let pack =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");

    for (index, decision, expected_status) in [
        (1_i64, ProductHoldDecision::CommitFail, "resolved"),
        (2_i64, ProductHoldDecision::Abort, "review_aborted"),
    ] {
        let order = format!("terminal-order-{index}");
        admit_local_delivery(
            &mut store,
            "terminal-holds-install",
            "gh",
            "repo:1",
            &format!("issue:{index}"),
            &order,
            "dep-bump",
            "immutable task",
            now,
        );
        append_waiting_lifecycle(&rusqlite::Connection::open(&db).unwrap(), &order);
        let work_claim = synthetic_product_claim(&mut store, &db, &order, now);
        let mut host = PackReviewHost::with_product_store(
            &pack,
            ProductHoldExecutor,
            store,
            root.join("artifacts"),
            root.join("sessions"),
            root.join("claims"),
        )
        .unwrap()
        .with_source_commit("67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0")
        .unwrap();
        let intent = ExecutionIntent {
            order: OrderId(order.clone()),
            attempt: foundry_exec::Attempt(1),
            run: RunId(index as u64 + 3),
            activation: 1,
            generation: Generation(0),
            epoch: Epoch(1),
            external_ref: format!("review:{order}:1:base..result"),
            recorded_at: now,
        };
        let hold = host
            .try_dispatch_product_work(&intent, &work_claim)
            .unwrap()
            .held
            .unwrap();
        drop(host);

        let mut store_again = ProductStore::open(&db, config()).unwrap();
        let expected = rusqlite::Connection::open(&db)
            .unwrap()
            .query_row(
                "SELECT lifecycle,attempt,originating_run,activation,host_epoch,hold_revision,source_revision,expected_predecessor,snapshot_digest FROM product_review_holds_v1 WHERE order_id=?1 AND hold_instance_id=?2",
                rusqlite::params![order,hold.hold_instance_id],
                |r| Ok(ProductHoldExpectation { lifecycle:r.get(0)?,attempt:r.get(1)?,originating_run:r.get(2)?,activation:r.get(3)?,host_epoch:r.get(4)?,hold_revision:r.get(5)?,source_revision:r.get(6)?,predecessor_seq:r.get(7)?,snapshot_digest:r.get(8)? }),
            )
            .unwrap();
        let receipt = store_again
            .accept_product_review_hold(
                &session,
                &order,
                &hold.hold_instance_id,
                &expected,
                &format!("terminal-key-{index}"),
                decision,
                now,
            )
            .unwrap();
        drop(store_again);

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut continuation = PackReviewHost::with_product_store(
            &pack,
            ProductContinuationExecutor {
                calls: calls.clone(),
            },
            ProductStore::open(&db, config()).unwrap(),
            root.join("artifacts"),
            root.join("sessions"),
            root.join("claims"),
        )
        .unwrap();
        continuation
            .continue_accepted_product_hold(&session, &receipt.operation_id, now)
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        drop(continuation);
        let run_id = format!("{order}:1:{}:1", index + 3);
        let (phase, status, exit_code, s1_event, s1_state): (String,String,i64,String,String) = rusqlite::Connection::open(&db)
            .unwrap()
            .query_row(
                "SELECT (SELECT phase FROM product_review_hold_progress_v1 WHERE operation_id=?1),(SELECT status FROM product_review_runs_v1 WHERE order_id=?2 AND run_id=?3),json_extract(rr.result_json,'$.exit_code'),(SELECT event FROM ledger WHERE order_id=?2 ORDER BY seq DESC LIMIT 1),(SELECT to_state FROM ledger WHERE order_id=?2 ORDER BY seq DESC LIMIT 1) FROM product_review_runs_v1 rr WHERE order_id=?2 AND run_id=?3",
                rusqlite::params![receipt.operation_id,order,run_id],
                |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
            )
            .unwrap();
        assert_eq!(phase, "completed");
        assert_eq!(status, expected_status);
        assert_eq!(exit_code, 1);
        assert_eq!(
            s1_event,
            "InspectionFailed { reason: \"product-review:abort\" }"
                .replace("abort", if index == 1 { "commit-fail" } else { "abort" })
        );
        assert_eq!(
            s1_state, "queued",
            "the existing InspectionFailed transition must apply its attempt policy"
        );
        store = ProductStore::open(&db, config()).unwrap();
    }
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn product_source_disable_during_accepted_stage_persists_cleanup_and_stops_next_stage() {
    use foundry_exec::{Epoch, ExecutionIntent, Generation, OrderId, RunId};
    use std::sync::{Arc, atomic::AtomicUsize};
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "foundry-b1-product-disable-cleanup-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("product.db");
    let mut store = ProductStore::open(
        &db,
        ProductConfig {
            installation_id: "host-e2e-install".into(),
            database_id: "host-e2e-db".into(),
        },
    )
    .unwrap();
    let phc =
        foundry_ledger::product_identity::ProductPasswordKdf::hash("synthetic-owner-password")
            .unwrap();
    store.bootstrap_owner("owner", &phc).unwrap();
    let now = chrono::Utc::now();
    let session = store
        .login("owner", "synthetic-owner-password", now)
        .unwrap()
        .digest()
        .to_owned();
    store
        .bind_source("gh", "github", "https://api.github.com", "repo:1", "repo:1")
        .unwrap();
    admit_local_delivery(
        &mut store,
        "host-e2e-install",
        "gh",
        "repo:1",
        "issue:20",
        "disable-order",
        "dep-bump",
        "immutable task",
        now,
    );
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        append_waiting_lifecycle(&conn, "disable-order");
    }
    let work_claim = synthetic_product_claim(&mut store, &db, "disable-order", now);
    let calls = Arc::new(AtomicUsize::new(0));
    let pack =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
    let mut host = PackReviewHost::with_product_store(
        pack,
        DisableSourceDuringExecution {
            db: db.clone(),
            session,
            calls: calls.clone(),
        },
        store,
        root.join("artifacts"),
        root.join("sessions"),
        root.join("claims"),
    )
    .unwrap()
    .with_source_commit("67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0")
    .unwrap();
    let intent = ExecutionIntent {
        order: OrderId("disable-order".into()),
        attempt: foundry_exec::Attempt(1),
        run: RunId(9),
        activation: 1,
        generation: Generation(0),
        epoch: Epoch(1),
        external_ref: "review:disable-order:1:base..result".into(),
        recorded_at: now,
    };
    assert!(
        host.try_dispatch_product_work(&intent, &work_claim)
            .is_err(),
        "the following new stage must be blocked after source disable"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "no second external stage may start"
    );
    drop(host);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let stage: (String, Option<String>) = conn.query_row(
        "SELECT status,output_json FROM product_review_stages_v1 WHERE order_id='disable-order' ORDER BY sequence LIMIT 1", [], |r| Ok((r.get(0)?,r.get(1)?)),
    ).unwrap();
    assert_eq!(stage.0, "complete");
    assert!(
        stage
            .1
            .as_deref()
            .is_some_and(|json| json.contains("accepted-output"))
    );
    let sessions: i64 = conn
        .query_row(
            "SELECT count(*) FROM product_review_sessions_v1 WHERE order_id='disable-order'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        sessions, 1,
        "accepted session evidence must be collected after source disable"
    );
    drop(conn);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn product_host_rejects_reclaimed_claim_then_dispatches_only_new_owner() {
    use foundry_exec::{Epoch, ExecutionIntent, Generation, OrderId, RunId};
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "foundry-b1-product-work-fence-{}-{n}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("product.db");
    let config = ProductConfig {
        installation_id: "work-fence-install".into(),
        database_id: "work-fence-db".into(),
    };
    let mut store = ProductStore::open(&db, config.clone()).unwrap();
    store
        .bind_source("gh", "github", "https://api.github.com", "repo:1", "repo:1")
        .unwrap();
    let now = chrono::Utc::now();
    admit_local_delivery(
        &mut store,
        "work-fence-install",
        "gh",
        "repo:1",
        "issue:1",
        "claim-order",
        "dep-bump",
        "immutable synthetic task",
        now,
    );
    append_waiting_lifecycle(&rusqlite::Connection::open(&db).unwrap(), "claim-order");
    let old = synthetic_product_claim(&mut store, &db, "claim-order", now);
    let reclaimed_at = now + chrono::Duration::minutes(6);
    let replacement = match store
        .claim_product_work("worker-new", reclaimed_at, chrono::Duration::minutes(5))
        .unwrap()
    {
        foundry_ledger::ProductWorkClaimResult::Claimed(claim) => claim,
        other => panic!("expired work claim was not reclaimed: {other:?}"),
    };
    assert_ne!(
        old.review_binding().epoch,
        replacement.review_binding().epoch
    );
    assert_ne!(
        old.review_binding().fence,
        replacement.review_binding().fence
    );

    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let pack =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
    let mut host = PackReviewHost::with_product_store(
        &pack,
        CountingHoldExecutor(calls.clone()),
        store,
        root.join("artifacts"),
        root.join("sessions"),
        root.join("claims"),
    )
    .unwrap()
    .with_source_commit("67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0")
    .unwrap();
    let intent = ExecutionIntent {
        order: OrderId("claim-order".into()),
        attempt: foundry_exec::Attempt(1),
        run: RunId(1),
        activation: 1,
        generation: Generation(0),
        epoch: Epoch(1),
        external_ref: "review:claim-order:1:base..result".into(),
        recorded_at: now,
    };
    assert!(
        foundry_exec::DispatchHost::try_dispatch(&mut host, &intent).is_err(),
        "product host cannot dispatch through the claimless compatibility API"
    );
    assert!(
        host.try_dispatch_product_work(&intent, &old).is_err(),
        "the stale owner cannot enter host admission"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        rusqlite::Connection::open(&db)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM product_review_runs_v1 WHERE order_id='claim-order'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let outcome = host
        .try_dispatch_product_work(&intent, &replacement)
        .unwrap();
    assert!(outcome.held.is_some());
    let accepted_calls = calls.load(Ordering::SeqCst);
    assert!(
        accepted_calls > 0 && accepted_calls <= 10,
        "the graph executes a bounded number of stages"
    );
    assert!(
        host.try_dispatch_product_work(&intent, &old).is_err(),
        "the previous owner cannot reuse the result through host dispatch"
    );
    assert_eq!(calls.load(Ordering::SeqCst), accepted_calls);
    drop(host);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let state: (String,String) = conn.query_row("SELECT b.state,c.state FROM product_work_backlog_v1 b JOIN product_work_claims_v1 c USING(order_id,lifecycle) WHERE b.order_id='claim-order'", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(
        state,
        ("claimed".into(), "claimed".into()),
        "a human review hold is not terminal product work; keep the service claim available for accepted Resume continuation"
    );
    drop(conn);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn product_terminal_trigger_failure_rolls_back_result_and_work_completion() {
    use foundry_exec::{Epoch, ExecutionIntent, Generation, OrderId, RunId};
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "foundry-b1-product-terminal-rollback-{}-{n}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("product.db");
    let config = ProductConfig {
        installation_id: "terminal-rollback-install".into(),
        database_id: "terminal-rollback-db".into(),
    };
    let mut store = ProductStore::open(&db, config).unwrap();
    store
        .bind_source("gh", "github", "https://api.github.com", "repo:1", "repo:1")
        .unwrap();
    let now = chrono::Utc::now();
    admit_local_delivery(
        &mut store,
        "terminal-rollback-install",
        "gh",
        "repo:1",
        "issue:1",
        "rollback-order",
        "dep-bump",
        "immutable synthetic task",
        now,
    );
    append_waiting_lifecycle(&rusqlite::Connection::open(&db).unwrap(), "rollback-order");
    let claim = synthetic_product_claim(&mut store, &db, "rollback-order", now);
    rusqlite::Connection::open(&db).unwrap().execute_batch(
        "CREATE TRIGGER reject_product_host_result BEFORE INSERT ON product_review_host_results_v1 BEGIN SELECT RAISE(ABORT,'injected terminal failure'); END;",
    ).unwrap();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let pack =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
    let mut host = PackReviewHost::with_product_store(
        &pack,
        CountingHoldExecutor(calls.clone()),
        ProductStore::open(
            &db,
            ProductConfig {
                installation_id: "terminal-rollback-install".into(),
                database_id: "terminal-rollback-db".into(),
            },
        )
        .unwrap(),
        root.join("artifacts"),
        root.join("sessions"),
        root.join("claims"),
    )
    .unwrap()
    .with_source_commit("67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0")
    .unwrap();
    let intent = ExecutionIntent {
        order: OrderId("rollback-order".into()),
        attempt: foundry_exec::Attempt(1),
        run: RunId(1),
        activation: 1,
        generation: Generation(0),
        epoch: Epoch(1),
        external_ref: "review:rollback-order:1:base..result".into(),
        recorded_at: now,
    };
    assert!(host.try_dispatch_product_work(&intent, &claim).is_err());
    let first_calls = calls.load(Ordering::SeqCst);
    assert!(
        first_calls > 0 && first_calls <= 10,
        "the graph stage budget bounds work"
    );
    let conn = rusqlite::Connection::open(&db).unwrap();
    let (run_status,results,backlog,claim_state,hold_rows,latest_state):(String,i64,String,String,i64,String) = conn.query_row(
        "SELECT (SELECT status FROM product_review_runs_v1 WHERE order_id='rollback-order'),(SELECT count(*) FROM product_review_host_results_v1 WHERE order_id='rollback-order'),(SELECT state FROM product_work_backlog_v1 WHERE order_id='rollback-order'),(SELECT state FROM product_work_claims_v1 WHERE order_id='rollback-order'),(SELECT count(*) FROM product_review_holds_v1 WHERE order_id='rollback-order'),(SELECT to_state FROM ledger WHERE order_id='rollback-order' ORDER BY seq DESC LIMIT 1)", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
    ).unwrap();
    assert_eq!(
        (
            run_status,
            results,
            backlog,
            claim_state,
            hold_rows,
            latest_state
        ),
        (
            "running".into(),
            0,
            "claimed".into(),
            "claimed".into(),
            0,
            "waiting".into()
        )
    );
    drop(conn);
    assert!(
        host.try_dispatch_product_work(&intent, &claim).is_err(),
        "an ambiguous terminal failure is not replayed"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        first_calls,
        "the accepted stage is never executed twice"
    );
    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn standalone_review_host_refuses_product_contract_before_creating_host_schema() {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "foundry-b1-product-host-guard-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("product.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE foundry_contract(singleton INTEGER PRIMARY KEY, contract TEXT NOT NULL);
         INSERT INTO foundry_contract(singleton,contract) VALUES(1,'ProductAtomicV1');",
    )
    .unwrap();
    drop(conn);
    let pack =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
    assert!(
        foundry_line::PackReviewHost::new(pack, foundry_line::UnavailableStageExecutor, &db)
            .is_err()
    );
    let conn = rusqlite::Connection::open(&db).unwrap();
    let host_schema: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='review_runs')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!host_schema);
    drop(conn);
    std::fs::remove_dir_all(root).unwrap();
}
