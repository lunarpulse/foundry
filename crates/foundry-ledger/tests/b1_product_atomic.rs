#![allow(clippy::cloned_ref_to_slice_refs)] // W6a clippy debt: preserve this existing test/example logic, assertions, and cell sequence

use foundry_domain::ports::Ledger;
use foundry_domain::{ActorId, ActorKind, OrderEvent, OrderState};
use foundry_exec::{
    Attempt, Epoch, ExecutionIntent, Generation, OrderId, ReviewAdmissionRequest,
    ReviewAdmissionStore, ReviewBinaryProvenance, ReviewDispatchCompletion, ReviewDispatchStore,
    ReviewTerminalStatus, RunId,
};
use foundry_ledger::{
    ProductApprovalIntent, ProductConfig, ProductError, ProductHoldDecision,
    ProductHoldExpectation, ProductStore, SqliteLedger,
};

fn scratch() -> (std::path::PathBuf, std::path::PathBuf) {
    let p = std::env::temp_dir().join(format!(
        "foundry-b1-product-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    (p.clone(), p.join("product.db"))
}
fn config() -> ProductConfig {
    ProductConfig {
        installation_id: "install-test".into(),
        database_id: "db-test".into(),
    }
}

fn append_product_state(
    conn: &rusqlite::Connection,
    order: &str,
    from: &str,
    to: &str,
    event: OrderEvent,
) {
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
            "SELECT COALESCE(MAX(seq),0) FROM ledger WHERE order_id=?1",
            [order],
            |r| r.get(0),
        )
        .unwrap();
    conn.execute("INSERT INTO product_transition_intents_v1(order_id,lifecycle,expected_predecessor,operation) VALUES(?1,1,?2,'fixture')", rusqlite::params![order,predecessor]).unwrap();
    let event_text = format!("{event:?}");
    let event_json = foundry_ledger::event_json::event_to_json(&event);
    let at = chrono::Utc::now().to_rfc3339();
    let v1 = format!("{seq}|{order}|{from}|{to}|{event_text}|Agent/github-intake||{at}|{prev}");
    let hash = hex::encode(sha2::Sha256::digest(
        format!("{v1}|{event_json}").as_bytes(),
    ));
    conn.execute("INSERT INTO ledger(order_id,from_state,to_state,event,actor,intent,at,prev_hash,event_json,hash) VALUES(?1,?2,?3,?4,'Agent/github-intake','',?5,?6,?7,?8)", rusqlite::params![order,from,to,event_text,at,prev,event_json,hash]).unwrap();
}

fn seed_product_hold(path: &std::path::Path) -> (String, String) {
    let mut store = ProductStore::open(path, config()).unwrap();
    store
        .bind_source(
            "gh1",
            "github",
            "https://api.github.com",
            "repo:42",
            "repo:42",
        )
        .unwrap();
    let admission = store
        .admit_order(
            "hold-order",
            "gh1",
            "repo:42",
            "issue:7",
            "dep-bump",
            "immutable snapshot",
            chrono::Utc::now(),
        )
        .unwrap();
    let session = "f".repeat(64);
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute("INSERT INTO product_accounts_v1(actor_id,role,enabled,revision) VALUES('owner','owner',1,1)", []).unwrap();
    conn.execute("INSERT INTO product_sessions_v1(session_digest,actor_id,authority_revision,expires_at,revoked) VALUES(?1,'owner',1,?2,0)", rusqlite::params![session,(chrono::Utc::now()+chrono::Duration::hours(1)).to_rfc3339()]).unwrap();
    append_product_state(
        &conn,
        "hold-order",
        "queued",
        "working",
        OrderEvent::DieStarted,
    );
    append_product_state(
        &conn,
        "hold-order",
        "working",
        "review",
        OrderEvent::DieSucceeded { attempt: 1 },
    );
    append_product_state(
        &conn,
        "hold-order",
        "review",
        "waiting",
        OrderEvent::InspectionPassed,
    );
    let snapshot = "{\"snapshot\":\"frozen\"}";
    let snapshot_digest = {
        use sha2::Digest as _;
        let mut hash = sha2::Sha256::new();
        for field in ["foundry-product-review-snapshot-v1", snapshot] {
            hash.update((field.len() as u64).to_be_bytes());
            hash.update(field.as_bytes());
        }
        hex::encode(hash.finalize())
    };
    let expected_predecessor: i64 = conn
        .query_row(
            "SELECT max(seq) FROM ledger WHERE order_id='hold-order'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    conn.execute("INSERT INTO product_review_holds_v1(order_id,lifecycle,attempt,originating_run,activation,host_epoch,hold_instance_id,hold_revision,expected_predecessor,snapshot_digest,snapshot_json,allowed_targets_json,deadline,source_revision,admission_seq,admission_hash,state) VALUES('hold-order',1,1,7,1,7,'hold-H1',2,?1,?2,?3,'[\"lint\"]',?4,1,?5,?6,'active')", rusqlite::params![expected_predecessor,snapshot_digest,snapshot,(chrono::Utc::now()+chrono::Duration::hours(1)).to_rfc3339(),admission.sequence,admission.event_hash]).unwrap();
    drop(conn);
    (session, "hold-H1".into())
}

fn hold_expectation(path: &std::path::Path, hold_id: &str) -> ProductHoldExpectation {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.query_row(
        "SELECT lifecycle,attempt,originating_run,activation,host_epoch,hold_revision,source_revision,expected_predecessor,snapshot_digest FROM product_review_holds_v1 WHERE hold_instance_id=?1",
        [hold_id],
        |r| Ok(ProductHoldExpectation { lifecycle:r.get(0)?, attempt:r.get(1)?, originating_run:r.get(2)?, activation:r.get(3)?, host_epoch:r.get(4)?, hold_revision:r.get(5)?, source_revision:r.get(6)?, predecessor_seq:r.get(7)?, snapshot_digest:r.get(8)? }),
    ).unwrap()
}

#[test]
fn product_admission_is_durable_idempotent_and_scope_checked_atomically() {
    let (dir, path) = scratch();
    let mut store = ProductStore::open(&path, config()).unwrap();
    store
        .bind_source(
            "gh1",
            "github",
            "https://api.github.com",
            "repo:42",
            "repo:42",
        )
        .unwrap();
    assert!(
        store
            .admit_order(
                "o-denied",
                "gh1",
                "repo:99",
                "issue:1",
                "dep-bump",
                "payload",
                chrono::Utc::now()
            )
            .is_err()
    );
    let first = store
        .admit_order(
            "o-1",
            "gh1",
            "repo:42",
            "issue:1",
            "dep-bump",
            "snapshot",
            chrono::Utc::now(),
        )
        .unwrap();
    let replay = store
        .admit_order(
            "o-ignored",
            "gh1",
            "repo:42",
            "issue:1",
            "dep-bump",
            "snapshot",
            chrono::Utc::now(),
        )
        .unwrap();
    assert_eq!(first, replay);
    assert!(
        store
            .admit_order(
                "o-1",
                "gh1",
                "repo:42",
                "issue:1",
                "dep-bump",
                "changed",
                chrono::Utc::now()
            )
            .is_err()
    );
    drop(store);

    // The product contract blocks legacy append/open before any migration or write.
    assert!(SqliteLedger::open(&path).is_err());
    let conn = rusqlite::Connection::open(&path).unwrap();
    let counts: (i64,i64,i64) = conn.query_row("SELECT (SELECT count(*) FROM ledger),(SELECT count(*) FROM product_orders_v1),(SELECT count(*) FROM product_transition_receipts_v1)", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(counts, (1, 1, 1));
    assert!(conn.execute("INSERT INTO ledger(order_id,from_state,to_state,event,actor,intent,at,prev_hash,event_json,hash) VALUES('raw','received','queued','Queued','Agent/x','','now','GENESIS',NULL,'x')", []).is_err());
    let mut reopened = ProductStore::open(&path, config()).unwrap();
    reopened
        .bind_source(
            "gh1",
            "github",
            "https://api.github.com",
            "repo:42",
            "repo:42",
        )
        .unwrap();
    reopened
        .admit_order(
            "o-2",
            "gh1",
            "repo:42",
            "issue:2",
            "dep-bump",
            "next",
            chrono::Utc::now(),
        )
        .unwrap();
    drop(reopened);
    assert!(matches!(
        ProductStore::open(
            &path,
            ProductConfig {
                installation_id: "other".into(),
                database_id: "db-test".into()
            }
        ),
        Err(ProductError::ContractMismatch)
    ));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn product_terminal_result_binding_rejects_malformed_rows_before_any_write() {
    let (dir, path) = scratch();
    let mut store = ProductStore::open(&path, config()).unwrap();
    store
        .bind_source(
            "gh",
            "github",
            "https://api.github.com",
            "repo:terminal",
            "repo:terminal",
        )
        .unwrap();
    let now = chrono::Utc::now();
    let payload = "immutable task";
    let updated = now.to_rfc3339();
    let version =
        foundry_ledger::product::delivery_version_digest("issue:8", &updated, "dep-bump", payload);
    let delivery = foundry_ledger::ProductDelivery {
        delivery_id: foundry_ledger::product::github_delivery_id(
            "install-test",
            "gh",
            "https://api.github.com",
            "repo:terminal",
            "issue:8",
            &version,
        ),
        connector_id: "gh".into(),
        repository_id: "repo:terminal".into(),
        issue_id: "issue:8".into(),
        observed_updated_at: updated.clone(),
        version_digest: version,
        task_kind: "dep-bump".into(),
        payload: payload.into(),
    };
    store
        .persist_intake_page(
            &foundry_ledger::ProductPollCursor {
                connector_id: "gh".into(),
                query_fingerprint: "terminal-test".into(),
                sweep_id: "sweep".into(),
                next_page: 1,
                page_ceiling: 1,
                updated_at: updated,
            },
            &[delivery.clone()],
        )
        .unwrap();
    store
        .admit_intake_delivery(&delivery.delivery_id, "terminal-order", now)
        .unwrap();
    // Explicit test adapter selection; the durable claim is issued by the
    // real ProductStore claim path from the intake-created backlog row.
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute(
            "UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id='terminal-order'",
            [],
        )
        .unwrap();
    let work_claim = match store
        .claim_product_work("atomic-test-worker", now, chrono::Duration::minutes(5))
        .unwrap()
    {
        foundry_ledger::ProductWorkClaimResult::Claimed(claim) => claim,
        other => panic!("expected test work claim, got {other:?}"),
    };
    let pack = "a".repeat(64);
    store
        .configure_review_backend(foundry_ledger::ProductReviewConfig {
            artifact_root: dir.join("artifacts"),
            session_root: dir.join("sessions"),
            claim_root: dir.join("claims"),
            pack_digest: pack.clone(),
        })
        .unwrap();
    let intent = ExecutionIntent {
        order: OrderId("terminal-order".into()),
        attempt: Attempt(1),
        run: RunId(4),
        activation: 1,
        generation: Generation(0),
        epoch: Epoch(1),
        external_ref: "review:terminal-order:1:base..target".into(),
        recorded_at: chrono::Utc::now(),
    };
    let admission = store
        .admit_review_activation(ReviewAdmissionRequest {
            intent: intent.clone(),
            base_commit: "base".into(),
            result_commit: "target".into(),
            pack_digest: pack.clone(),
            product_work: Some(work_claim.review_binding()),
        })
        .unwrap();
    let mut stale_intent = intent.clone();
    stale_intent.epoch = Epoch(0);
    assert!(
        store
            .start_review_stage(foundry_exec::ReviewStageStart {
                intent: stale_intent,
                journal_run: admission.journal_run.clone(),
                sequence: 1,
                stage_id: "stale-stage".into(),
                budget_limit: 10,
            })
            .is_err(),
        "a stale epoch cannot claim a new external stage"
    );
    let provenance = ReviewBinaryProvenance {
        executable: "/synthetic/runner".into(),
        sha256: "b".repeat(64),
        source_commit: "c".repeat(40),
        pack_digest: pack,
    };
    let malformed = [
        ReviewDispatchCompletion {
            intent: intent.clone(),
            journal_run: admission.journal_run.clone(),
            status: ReviewTerminalStatus::Passed,
            counters: Default::default(),
            result: foundry_exec::HostResult {
                base_commit: "foreign-base".into(),
                result_commit: "foreign-final".into(),
                log_path: "synthetic.log".into(),
                exit_code: 0,
                generations: vec![],
                held: None,
            },
            held_snapshot: None,
            provenance: provenance.clone(),
            deadline: None,
        },
        ReviewDispatchCompletion {
            intent: intent.clone(),
            journal_run: admission.journal_run.clone(),
            status: ReviewTerminalStatus::Failed,
            counters: Default::default(),
            result: foundry_exec::HostResult {
                base_commit: "base".into(),
                result_commit: "base".into(),
                log_path: "synthetic.log".into(),
                exit_code: 0,
                generations: vec![],
                held: None,
            },
            held_snapshot: None,
            provenance: provenance.clone(),
            deadline: None,
        },
        ReviewDispatchCompletion {
            intent: intent.clone(),
            journal_run: admission.journal_run.clone(),
            status: ReviewTerminalStatus::Passed,
            counters: Default::default(),
            result: foundry_exec::HostResult {
                base_commit: "base".into(),
                result_commit: "base".into(),
                log_path: "synthetic.log".into(),
                exit_code: 0,
                generations: vec![],
                held: None,
            },
            held_snapshot: None,
            provenance: ReviewBinaryProvenance {
                pack_digest: "d".repeat(64),
                ..provenance.clone()
            },
            deadline: None,
        },
    ];
    for request in malformed {
        assert!(store.complete_review_dispatch(request).is_err());
    }
    drop(store);
    let conn = rusqlite::Connection::open(&path).unwrap();
    let run: (String, Option<String>) = conn.query_row(
        "SELECT status,result_json FROM product_review_runs_v1 WHERE order_id='terminal-order' AND run_id=?1",
        [&admission.journal_run], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!(run, ("running".into(), None));
    let budget: i64 = conn.query_row("SELECT COALESCE((SELECT used FROM product_review_budget_v1 WHERE order_id='terminal-order'),0)", [], |r| r.get(0)).unwrap();
    assert_eq!(budget, 0);
    let results: i64 = conn
        .query_row(
            "SELECT count(*) FROM product_review_host_results_v1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(results, 0);
    drop(conn);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn standalone_ledger_contract_is_preserved() {
    let (dir, path) = scratch();
    let mut ledger = SqliteLedger::open(&path).unwrap();
    let actor = ActorId {
        kind: ActorKind::Human,
        source: "local".into(),
        id: "owner".into(),
    };
    ledger
        .append(
            "o",
            &OrderEvent::Queued,
            OrderState::Received,
            OrderState::Queued,
            &actor,
            chrono::Utc::now(),
        )
        .unwrap();
    assert!(ProductStore::open(&path, config()).is_err());
    assert_eq!(ledger.read_all().unwrap().len(), 1);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn product_opener_refuses_uncontracted_review_database_without_adopting_it() {
    let (dir, path) = scratch();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE review_runs(order_id TEXT, status TEXT);")
        .unwrap();
    drop(conn);
    assert!(matches!(
        ProductStore::open(&path, config()),
        Err(ProductError::ContractMismatch)
    ));
    let conn = rusqlite::Connection::open(&path).unwrap();
    let product_contract: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='foundry_contract')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!product_contract);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn product_opener_rejects_unsupported_schema_before_additive_upgrade() {
    let (dir, path) = scratch();
    drop(ProductStore::open(&path, config()).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE product_meta_v1 SET schema_version=999 WHERE singleton=1",
        [],
    )
    .unwrap();
    drop(conn);
    assert!(matches!(
        ProductStore::open(&path, config()),
        Err(ProductError::ContractMismatch)
    ));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn product_hold_acceptance_is_atomic_replayable_and_current_authority_gated() {
    let (dir, path) = scratch();
    let (session, hold) = seed_product_hold(&path);
    let expected = hold_expectation(&path, &hold);
    let now = chrono::Utc::now();
    let mut store = ProductStore::open(&path, config()).unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    let deadline: String = conn
        .query_row(
            "SELECT deadline FROM product_review_holds_v1 WHERE hold_instance_id='hold-H1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn);
    let deadline = chrono::DateTime::parse_from_rfc3339(&deadline)
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert!(
        store
            .accept_product_review_hold(
                &session,
                "hold-order",
                &hold,
                &expected,
                "expired-key",
                ProductHoldDecision::CommitPass,
                deadline
            )
            .is_err()
    );
    let accepted = store
        .accept_product_review_hold(
            &session,
            "hold-order",
            &hold,
            &expected,
            "hold-key-1",
            ProductHoldDecision::Resume("lint".into()),
            now,
        )
        .unwrap();
    assert_eq!(accepted.actor_id, "owner");
    assert_eq!(accepted.reserved_run_id, 8);
    let allocated: (i64,i64) = rusqlite::Connection::open(&path).unwrap().query_row(
        "SELECT reservation_id,followup_run_id FROM product_review_run_reservations_v1 WHERE operation_id=?1",
        [&accepted.operation_id], |r| Ok((r.get(0)?,r.get(1)?)),
    ).unwrap();
    assert_eq!(allocated, (1, 8));
    assert_eq!(
        accepted.decision,
        ProductHoldDecision::Resume("lint".into())
    );
    drop(store);
    let mut reopened = ProductStore::open(&path, config()).unwrap();
    let replay = reopened
        .accept_product_review_hold(
            &session,
            "hold-order",
            &hold,
            &expected,
            "hold-key-1",
            ProductHoldDecision::Resume("lint".into()),
            now + chrono::Duration::minutes(1),
        )
        .unwrap();
    assert_eq!(accepted, replay);
    assert!(
        reopened
            .accept_product_review_hold(
                &session,
                "hold-order",
                &hold,
                &expected,
                "hold-key-1",
                ProductHoldDecision::Abort,
                now
            )
            .is_err()
    );
    assert!(
        reopened
            .accept_product_review_hold(
                &session,
                "hold-order",
                &hold,
                &expected,
                "hold-key-2",
                ProductHoldDecision::Resume("lint".into()),
                now
            )
            .is_err()
    );
    reopened
        .configure_source(
            &session,
            "gh1",
            "github",
            "https://api.github.com",
            "repo:42",
            "repo:42",
            false,
            now,
        )
        .unwrap();
    assert!(
        reopened
            .accept_product_review_hold(
                &session,
                "hold-order",
                &hold,
                &expected,
                "hold-key-1",
                ProductHoldDecision::Resume("lint".into()),
                now
            )
            .is_err()
    );
    let conn = rusqlite::Connection::open(&path).unwrap();
    let counts:(i64,i64,i64,String) = conn.query_row("SELECT (SELECT count(*) FROM product_review_hold_acceptances_v1),(SELECT count(*) FROM product_review_run_reservations_v1),(SELECT count(*) FROM product_review_hold_progress_v1),(SELECT state FROM product_review_holds_v1 WHERE hold_instance_id='hold-H1')",[],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
    assert_eq!(counts, (1, 1, 1, "accepted".into()));
    drop(conn);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn product_hold_acceptance_rejects_stale_client_snapshot_binding() {
    let (dir, path) = scratch();
    let (session, hold) = seed_product_hold(&path);
    let mut expected = hold_expectation(&path, &hold);
    expected.snapshot_digest = "0".repeat(64);
    let mut store = ProductStore::open(&path, config()).unwrap();
    assert!(
        store
            .accept_product_review_hold(
                &session,
                "hold-order",
                &hold,
                &expected,
                "stale-key",
                ProductHoldDecision::CommitPass,
                chrono::Utc::now()
            )
            .is_err()
    );
    drop(store);
    let conn = rusqlite::Connection::open(&path).unwrap();
    let state: String = conn
        .query_row(
            "SELECT state FROM product_review_holds_v1 WHERE hold_instance_id=?1",
            [&hold],
            |r| r.get(0),
        )
        .unwrap();
    let accepted: i64 = conn
        .query_row(
            "SELECT count(*) FROM product_review_hold_acceptances_v1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!((state, accepted), ("active".into(), 0));
    drop(conn);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn product_hold_retry_rejects_authority_revision_change_even_after_role_is_restored() {
    let (dir, path) = scratch();
    let (old_session, hold) = seed_product_hold(&path);
    let expected = hold_expectation(&path, &hold);
    let now = chrono::Utc::now();
    let accepted = ProductStore::open(&path, config())
        .unwrap()
        .accept_product_review_hold(
            &old_session,
            "hold-order",
            &hold,
            &expected,
            "authority-bound-key",
            ProductHoldDecision::CommitPass,
            now,
        )
        .unwrap();

    // Test-only account setup. Authority changes themselves use the public
    // administrative API; the fresh owner session comes through login.
    let phc = foundry_ledger::product_identity::ProductPasswordKdf::hash("admin-password").unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute("INSERT INTO product_accounts_v1(actor_id,role,enabled,revision) VALUES('admin','owner',1,1)", []).unwrap();
    conn.execute(
        "INSERT INTO product_credentials_v1(actor_id,argon2id_phc,revision) VALUES('owner',?1,1)",
        [&phc],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO product_credentials_v1(actor_id,argon2id_phc,revision) VALUES('admin',?1,1)",
        [&phc],
    )
    .unwrap();
    drop(conn);
    let mut admin = ProductStore::open(&path, config()).unwrap();
    let admin_session = admin
        .login("admin", "admin-password", now)
        .unwrap()
        .digest()
        .to_owned();
    admin
        .set_account_authority(&admin_session, "owner", "viewer", true, now)
        .unwrap();
    admin
        .set_account_authority(&admin_session, "owner", "approver", true, now)
        .unwrap();
    let fresh_owner_session = admin
        .login(
            "owner",
            "admin-password",
            now + chrono::Duration::seconds(1),
        )
        .unwrap()
        .digest()
        .to_owned();
    let replay = admin.accept_product_review_hold(
        &fresh_owner_session,
        "hold-order",
        &hold,
        &expected,
        "authority-bound-key",
        ProductHoldDecision::CommitPass,
        now + chrono::Duration::seconds(1),
    );
    assert!(
        replay.is_err(),
        "a changed authority revision must invalidate mutation replay"
    );
    let conn = rusqlite::Connection::open(&path).unwrap();
    let stored: String = conn
        .query_row(
            "SELECT receipt_json FROM product_review_hold_acceptances_v1 WHERE operation_id=?1",
            [&accepted.operation_id],
            |r| r.get(0),
        )
        .unwrap();
    let receipt: foundry_ledger::ProductHoldAcceptance = serde_json::from_str(&stored).unwrap();
    assert_eq!(receipt, accepted);
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM product_review_run_reservations_v1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    drop(conn);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn product_hold_receipt_insert_failure_rolls_back_cas_and_run_reservation() {
    let (dir, path) = scratch();
    let (session, hold) = seed_product_hold(&path);
    let expected = hold_expectation(&path, &hold);
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TRIGGER fail_product_hold_receipt BEFORE INSERT ON product_review_hold_acceptances_v1 BEGIN SELECT RAISE(ABORT,'injected receipt failure'); END;").unwrap();
    drop(conn);
    let mut store = ProductStore::open(&path, config()).unwrap();
    assert!(
        store
            .accept_product_review_hold(
                &session,
                "hold-order",
                &hold,
                &expected,
                "hold-key",
                ProductHoldDecision::CommitPass,
                chrono::Utc::now()
            )
            .is_err()
    );
    drop(store);
    let conn = rusqlite::Connection::open(&path).unwrap();
    let state: String = conn
        .query_row(
            "SELECT state FROM product_review_holds_v1 WHERE hold_instance_id='hold-H1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let counts:(i64,i64,i64) = conn.query_row("SELECT (SELECT count(*) FROM product_review_hold_acceptances_v1),(SELECT count(*) FROM product_review_run_reservations_v1),(SELECT count(*) FROM product_review_hold_progress_v1)",[],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(state, "active");
    assert_eq!(counts, (0, 0, 0));
    conn.execute_batch("DROP TRIGGER fail_product_hold_receipt;")
        .unwrap();
    drop(conn);
    let mut reopened = ProductStore::open(&path, config()).unwrap();
    let accepted = reopened
        .accept_product_review_hold(
            &session,
            "hold-order",
            &hold,
            &expected,
            "hold-key",
            ProductHoldDecision::CommitPass,
            chrono::Utc::now(),
        )
        .unwrap();
    assert_eq!(accepted.decision, ProductHoldDecision::CommitPass);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn product_hold_acceptance_rechecks_revoked_session_and_rebound_source() {
    for revoke_session in [true, false] {
        let (dir, path) = scratch();
        let (session, hold) = seed_product_hold(&path);
        let expected = hold_expectation(&path, &hold);
        let now = chrono::Utc::now();
        if revoke_session {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute(
                "UPDATE product_sessions_v1 SET revoked=1 WHERE session_digest=?1",
                [&session],
            )
            .unwrap();
        } else {
            let mut owner = ProductStore::open(&path, config()).unwrap();
            owner
                .configure_source(
                    &session,
                    "gh1",
                    "github",
                    "https://api.github.com",
                    "repo:99",
                    "repo:99",
                    true,
                    now,
                )
                .unwrap();
        }
        let mut store = ProductStore::open(&path, config()).unwrap();
        assert!(
            store
                .accept_product_review_hold(
                    &session,
                    "hold-order",
                    &hold,
                    &expected,
                    "denied-key",
                    ProductHoldDecision::CommitFail,
                    now
                )
                .is_err()
        );
        let conn = rusqlite::Connection::open(&path).unwrap();
        let counts:(i64,i64,String) = conn.query_row("SELECT (SELECT count(*) FROM product_review_hold_acceptances_v1),(SELECT count(*) FROM product_review_run_reservations_v1),(SELECT state FROM product_review_holds_v1 WHERE hold_instance_id='hold-H1')",[],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!(counts, (0, 0, "active".into()));
        drop(conn);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn product_owner_bootstrap_and_session_revision_are_durable_and_fail_closed() {
    let (dir, path) = scratch();
    let mut store = ProductStore::open(&path, config()).unwrap();
    let phc = foundry_ledger::product_identity::ProductPasswordKdf::hash("pw").unwrap();
    store.bootstrap_owner("owner-1", &phc).unwrap();
    assert!(store.bootstrap_owner("owner-2", &phc).is_err());
    assert!(store.login("owner-1", "wrong", chrono::Utc::now()).is_err());
    let login_at = chrono::Utc::now();
    let expiry = login_at + chrono::Duration::hours(8);
    let session = store.login("owner-1", "pw", login_at).unwrap();
    assert_eq!(session.bearer().len(), 64);
    assert_ne!(session.bearer(), session.digest());
    let session_digest = session.digest().to_owned();
    assert_eq!(
        store
            .resolve_session(&session_digest, chrono::Utc::now())
            .unwrap()
            .actor_id,
        "owner-1"
    );
    drop(store);
    let reopened = ProductStore::open(&path, config()).unwrap();
    assert_eq!(
        reopened
            .resolve_session(&session_digest, chrono::Utc::now())
            .unwrap()
            .role,
        "owner"
    );
    assert!(reopened.resolve_session(&session_digest, expiry).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn approval_prepare_is_durable_and_request_key_is_actor_bound() {
    let (dir, path) = scratch();
    let mut store = ProductStore::open(&path, config()).unwrap();
    let phc = foundry_ledger::product_identity::ProductPasswordKdf::hash("pw").unwrap();
    store.bootstrap_owner("owner-1", &phc).unwrap();
    let session = store
        .login("owner-1", "pw", chrono::Utc::now())
        .unwrap()
        .digest()
        .to_owned();
    store
        .bind_source(
            "gh1",
            "github",
            "https://api.github.com",
            "repo:42",
            "repo:42",
        )
        .unwrap();
    store
        .admit_order(
            "o-1",
            "gh1",
            "repo:42",
            "issue:1",
            "dep-bump",
            "snapshot",
            chrono::Utc::now(),
        )
        .unwrap();
    let token_digest = "c".repeat(64);
    store
        .issue_approval_grant(
            &session,
            "grant-1",
            &token_digest,
            "o-1",
            chrono::Utc::now(),
        )
        .unwrap();
    let request = ProductApprovalIntent {
        request_key: "request-1".into(),
        grant_id: "grant-1".into(),
        token_digest,
        order_id: "o-1".into(),
        expected_source_revision: 1,
        expected_attempt: 1,
        run_id: 1,
        generation: 1,
        artifact_digest: "d".repeat(40),
        tuple_digest: "e".repeat(64),
        intent: "approve review".into(),
        now: chrono::Utc::now(),
    };
    let op = store.prepare_approval(&session, &request).unwrap();
    let retry = store.prepare_approval(&session, &request).unwrap();
    assert_eq!(op, retry);
    let mut changed = request.clone();
    changed.artifact_digest = "f".repeat(40);
    assert!(store.prepare_approval(&session, &changed).is_err());
    drop(store);
    let conn = rusqlite::Connection::open(&path).unwrap();
    let (phase, grant_state, consumed): (String,String,i64) = conn.query_row(
        "SELECT o.phase,g.state,(SELECT count(*) FROM product_token_consumptions_v1) FROM product_operations_v1 o JOIN product_grants_v1 g ON g.grant_id='grant-1' WHERE o.operation_id=?1",
        [op.operation_id()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).unwrap();
    assert_eq!(
        (phase.as_str(), grant_state.as_str(), consumed),
        ("prepared", "reserved", 0)
    );
    std::fs::remove_dir_all(dir).unwrap();
}

// The former SQL-seeded promoted-tuple acceptance test is replaced by the
// genuine-host, strict-result approval coverage in foundry-line's required-
// feature `b1_product_results` target. Ledger-only tests remain schema and
// transaction-participant tests; they do not synthesize accepted tuples.

#[test]
fn prepared_approval_expires_and_recovery_fences_reservations() {
    let (dir, path) = scratch();
    let mut store = ProductStore::open(&path, config()).unwrap();
    let phc = foundry_ledger::product_identity::ProductPasswordKdf::hash("pw").unwrap();
    store.bootstrap_owner("owner-1", &phc).unwrap();
    let base = chrono::Utc::now();
    let session = store
        .login("owner-1", "pw", base)
        .unwrap()
        .digest()
        .to_owned();
    store
        .bind_source(
            "gh1",
            "github",
            "https://api.github.com",
            "repo:42",
            "repo:42",
        )
        .unwrap();
    store
        .admit_order(
            "o-1", "gh1", "repo:42", "issue:1", "dep-bump", "snapshot", base,
        )
        .unwrap();
    let token_digest = "c".repeat(64);
    store
        .issue_approval_grant(&session, "grant-1", &token_digest, "o-1", base)
        .unwrap();
    let request = ProductApprovalIntent {
        request_key: "expire-1".into(),
        grant_id: "grant-1".into(),
        token_digest,
        order_id: "o-1".into(),
        expected_source_revision: 1,
        expected_attempt: 1,
        run_id: 1,
        generation: 1,
        artifact_digest: "d".repeat(40),
        tuple_digest: "e".repeat(64),
        intent: "approve".into(),
        now: base,
    };
    let operation = store.prepare_approval(&session, &request).unwrap();
    let exact_expiry = base + chrono::Duration::minutes(5);
    let mut at_expiry = request.clone();
    at_expiry.now = exact_expiry;
    let expired_claim = store.prepare_approval(&session, &at_expiry).unwrap();
    assert_ne!(expired_claim, operation);
    assert!(
        store
            .approve_product(&session, &at_expiry, &operation)
            .is_err(),
        "the old claim must fail after expiry reclaim"
    );
    assert!(
        store
            .approve_product(&session, &at_expiry, &expired_claim)
            .is_err(),
        "the fresh reservation is also invalid at the exact expiry boundary"
    );
    store
        .issue_approval_grant(
            &session,
            "unrelated-grant",
            &"a".repeat(64),
            "o-1",
            exact_expiry,
        )
        .unwrap();
    let fixture = rusqlite::Connection::open(&path).unwrap();
    fixture.execute("UPDATE product_grants_v1 SET state='reserved',fence=999 WHERE grant_id='unrelated-grant'", []).unwrap();
    drop(fixture);
    assert!(
        store
            .approve_product(&session, &at_expiry, &operation)
            .is_err()
    );
    let epoch = store.fence_prepared_approvals().unwrap();
    assert_eq!(epoch, 1);
    let fixture = rusqlite::Connection::open(&path).unwrap();
    let unrelated_state: String = fixture
        .query_row(
            "SELECT state FROM product_grants_v1 WHERE grant_id='unrelated-grant'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        unrelated_state, "reserved",
        "recovery may release only grants explicitly bound to prepared operations"
    );
    drop(fixture);
    assert!(
        store
            .approve_product(&session, &at_expiry, &operation)
            .is_err()
    );
    let mut retry = at_expiry;
    retry.now += chrono::Duration::seconds(1);
    let reclaimed = store.prepare_approval(&session, &retry).unwrap();
    assert_ne!(reclaimed, operation);
    assert!(
        store.approve_product(&session, &retry, &operation).is_err(),
        "the prior claim must stay fenced after a new claim exists"
    );
    let conn = rusqlite::Connection::open(&path).unwrap();
    let (phase, epoch, fence): (String, i64, i64) = conn
        .query_row(
            "SELECT phase,epoch,fence FROM product_operations_v1 WHERE operation_id=?1",
            [reclaimed.operation_id()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(phase, "prepared");
    assert_eq!(epoch, 1);
    assert!(fence >= 2);
    drop(conn);
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn account_disable_and_source_rebind_are_serialized_authority_changes() {
    let (dir, path) = scratch();
    let mut store = ProductStore::open(&path, config()).unwrap();
    let phc = foundry_ledger::product_identity::ProductPasswordKdf::hash("pw").unwrap();
    store.bootstrap_owner("owner-1", &phc).unwrap();
    let now = chrono::Utc::now();
    let session = store
        .login("owner-1", "pw", now)
        .unwrap()
        .digest()
        .to_owned();
    store
        .bind_source(
            "gh1",
            "github",
            "https://api.github.com",
            "repo:42",
            "repo:42",
        )
        .unwrap();
    assert!(
        store
            .set_account_authority(&session, "owner-1", "viewer", true, now)
            .is_err()
    );
    let revision = store
        .configure_source(
            &session,
            "gh1",
            "github",
            "https://api.github.com",
            "repo:43",
            "repo:43",
            true,
            now,
        )
        .unwrap();
    assert_eq!(revision, 2);
    assert!(
        store
            .admit_order(
                "o-1", "gh1", "repo:42", "issue:1", "dep-bump", "snapshot", now
            )
            .is_err()
    );
    store
        .set_account_authority(&session, "owner-1", "owner", false, now)
        .unwrap_err();
    assert!(store.resolve_session(&session, now).is_ok());
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn product_login_throttle_persists_and_has_exact_five_minute_boundary() {
    let (dir, path) = scratch();
    let mut store = ProductStore::open(&path, config()).unwrap();
    let phc =
        foundry_ledger::product_identity::ProductPasswordKdf::hash("correct-password").unwrap();
    store.bootstrap_owner("owner-throttle", &phc).unwrap();
    let start = chrono::DateTime::parse_from_rfc3339("2026-10-16T00:00:00Z")
        .unwrap()
        .to_utc();
    for _ in 0..5 {
        assert!(
            store
                .login("owner-throttle", "wrong-password", start)
                .is_err()
        );
    }
    drop(store);
    let mut reopened = ProductStore::open(&path, config()).unwrap();
    assert!(
        reopened
            .login(
                "owner-throttle",
                "correct-password",
                start + chrono::Duration::minutes(4) + chrono::Duration::seconds(59)
            )
            .is_err()
    );
    let session = reopened
        .login(
            "owner-throttle",
            "correct-password",
            start + chrono::Duration::minutes(5),
        )
        .unwrap();
    assert!(
        reopened
            .resolve_session(session.digest(), start + chrono::Duration::minutes(5))
            .is_ok()
    );
    drop(reopened);
    std::fs::remove_dir_all(dir).unwrap();
}
