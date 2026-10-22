use chrono::Duration;
use foundry_exec::{DispatchHost, ReviewHostStore};
use foundry_ledger::{
    ProductConfig, ProductDelivery, ProductHoldDecision, ProductPollCursor,
    ProductResultExpectation, ProductStore,
};
use rusqlite::Connection;
use sha2::Digest;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

#[path = "support/product_result_fixture.rs"]
mod product_result_fixture;
use product_result_fixture::{
    AdjustableFixtureClock, FixtureClock, FixtureIdentity, ReviewScript, genuine_host_done,
    genuine_host_done_with_stage_start_observer, genuine_host_inflight, grant_review_hold_scope,
};

#[test]
fn initial_redrive_remains_unsupported_and_cannot_finalize() {
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: "failed-result-install".into(),
            database_id: "failed-result-db".into(),
        },
        "failed-result-source",
        "failed-result:order",
    )
    .with_script(ReviewScript::InitialRedrive);
    let mut fixture = genuine_host_done(identity);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 5);
    assert_eq!(fixture.restores.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.host.as_ref().unwrap().dispatch_count(), 1);
    assert_eq!(
        fixture.dispatch_error.as_deref(),
        Some(
            "Worker(\"E_HOST_STAGE: Worker(\\\"product review recovery operation unavailable: product redrive persistence is not configured\\\")\")"
        ),
        "the fifth dispatched stage must fail at the unsupported save operation"
    );
    let failed: (String, i64, i64, i64, i64, i64, i64, i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT r.status,
                    (SELECT count(*) FROM product_review_stages_v1 s WHERE s.order_id=r.order_id AND s.status='complete'),
                    (SELECT count(*) FROM product_artifacts_v1 a WHERE a.order_id=r.order_id),
                    (SELECT count(*) FROM product_review_sessions_v1 s WHERE s.order_id=r.order_id),
                    (SELECT used FROM product_review_budget_v1 b WHERE b.order_id=r.order_id),
                    (SELECT count(*) FROM product_review_failures_v1 f WHERE f.order_id=r.order_id),
                    (SELECT count(*) FROM product_review_host_results_v1 h WHERE h.order_id=r.order_id),
                    (SELECT count(*) FROM artifact_tuples t WHERE t.order_id=r.order_id),
                    (SELECT count(*) FROM product_result_bindings_v1 b WHERE b.order_id=r.order_id)
             FROM product_review_runs_v1 r WHERE r.order_id=?1",
            [&fixture.identity.order_id],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?)),
        )
        .unwrap();
    assert_eq!(failed, ("failed".into(), 4, 4, 3, 5, 1, 0, 0, 0));
    let work: (String, String, i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT b.state,c.state,
                    (SELECT count(*) FROM product_review_stages_v1 s WHERE s.order_id=b.order_id AND s.stage_id='fixdie-g1'),
                    (SELECT count(*) FROM product_artifacts_v1 a WHERE a.order_id=b.order_id AND a.relative_path LIKE '%fixdie-g1%')
             FROM product_work_backlog_v1 b JOIN product_work_claims_v1 c USING(order_id,lifecycle)
             WHERE b.order_id=?1",
            [&fixture.identity.order_id],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
        )
        .unwrap();
    assert_eq!(work, ("claimed".into(), "claimed".into(), 0, 0));
    let s1: (i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT count(*),COALESCE(max(seq),0) FROM ledger WHERE order_id=?1",
            [&fixture.identity.order_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(s1, (4, 4));
    let mut result_store = ProductStore::open_with_clock(
        &fixture.db_path,
        fixture.config.clone(),
        std::sync::Arc::new(FixtureClock(fixture.now)),
    )
    .unwrap();
    result_store
        .configure_product_artifact_root(&fixture.artifact_root)
        .unwrap();
    assert_eq!(
        result_store.finalize_product_review_result(&fixture.service, &fixture.expected),
        Err(foundry_ledger::ProductResultError::Ineligible)
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 5);
    assert_eq!(fixture.restores.load(Ordering::SeqCst), 0);
    drop(result_store);
    drop(fixture.host.take());
    std::fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn verified_replay_actual_attempt_mismatch_is_stale_binding() {
    // G05 (rubric v1 line 132): a chain-consistent retry ledger — the actual
    // attempt genuinely advanced to 2 through DieFailed/DieStarted while every
    // stored copy (event repr, canonical JSON, chain hash) stayed consistent —
    // must reject an attempt-1 genuine caller coordinate through the
    // finalizer's actual-attempt comparison (result.rs:695), NOT through
    // stored-set contamination or CorruptEvidence hash-chain rejection.
    let order = "g05-retry-actual-attempt";
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: format!("{order}-install"),
            database_id: format!("{order}-db"),
        },
        format!("{order}-source"),
        order,
    )
    .with_preparation(foundry_ledger::P2ReviewPreparation::Queued)
    .with_g05_retry_ledger();
    let mut fixture = genuine_host_inflight(identity);
    // The retry history is present and consistent: six rows, the terminal
    // success carried attempt 2, and the whole chain re-verifies through the
    // ledger's own verifier (no corruption evidence anywhere).
    let history: (i64, String) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT count(*), (SELECT event FROM ledger WHERE order_id=?1 AND event LIKE 'DieSucceeded%') FROM ledger WHERE order_id=?1",
            [&fixture.identity.order_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(history, (6, "DieSucceeded { attempt: 2 }".into()));
    let chain_rows: Vec<foundry_ledger::LedgerRow> = Connection::open(&fixture.db_path)
        .unwrap()
        .prepare("SELECT seq, order_id, from_state, to_state, event, actor, intent, at, prev_hash, hash, event_json FROM ledger ORDER BY seq")
        .unwrap()
        .query_map([], |r| {
            Ok(foundry_ledger::LedgerRow {
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
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(
        foundry_ledger::SqliteLedger::verify_rows(&chain_rows),
        "the retry history is chain-consistent, so corruption checks cannot fire"
    );
    // No stored result exists yet: the first finalization reaches the
    // actual-attempt comparison through the clean lookup-miss path.
    let bindings_before: i64 = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1",
            [&fixture.identity.order_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(bindings_before, 0);
    assert_eq!(fixture.expected.host_key.attempt, 1);
    let mut result_store = fixture.result_store.take().unwrap();
    assert_eq!(
        result_store.finalize_product_review_result(&fixture.service, &fixture.expected),
        Err(foundry_ledger::ProductResultError::StaleBinding),
        "the replayed actual attempt (2) must reject the attempt-1 caller coordinate itself"
    );
    let bindings_after: i64 = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1",
            [&fixture.identity.order_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        bindings_after, 0,
        "rejection happened at the comparison, before any binding write"
    );
    drop(result_store);
    drop(fixture.host.take());
    std::fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn p2_preparation_walks_s1_without_result_seeding() {
    let order = "p2-preparation-duplicate-after-claim";
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: format!("{order}-install"),
            database_id: format!("{order}-db"),
        },
        format!("{order}-source"),
        order,
    )
    .with_stale_admission_preparation_probe();
    // Leave work fresh and the host untouched so stale admission/source
    // negatives cannot be rejected by completed-work eligibility.
    let mut fixture = genuine_host_inflight(identity);
    let before: (i64, i64, i64, i64, i64, i64, i64, i64, i64) =
        Connection::open(&fixture.db_path)
            .unwrap()
            .query_row(
                "SELECT (SELECT count(*) FROM ledger WHERE order_id=?1),
                        (SELECT count(*) FROM product_transition_receipts_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_transition_intents_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_work_claims_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_review_host_accept_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_review_runs_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                        (SELECT count(*) FROM sqlite_master WHERE type='table' AND name='tuple_promotions')",
                [&order],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?)),
            )
            .unwrap();
    assert_eq!(before, (4, 4, 0, 1, 0, 0, 0, 0, 0));
    let result = fixture
        .result_store
        .as_mut()
        .unwrap()
        .prepare_p2_review_fixture(
            &fixture.service,
            &fixture.admission,
            &fixture.delivery_id,
            foundry_ledger::P2ReviewPreparation::Waiting,
        );
    assert!(matches!(
        result,
        Err(foundry_ledger::ProductError::Invariant(
            "P2 preparation requires exact acknowledged admission and fresh work"
        ))
    ));
    let mut stale_admission = fixture.admission.clone();
    stale_admission.sequence += 1;
    assert!(matches!(
        fixture.result_store.as_mut().unwrap().prepare_p2_review_fixture(
            &fixture.service, &stale_admission, &fixture.delivery_id,
            foundry_ledger::P2ReviewPreparation::Waiting,
        ),
        Err(foundry_ledger::ProductError::Invariant(
            "P2 preparation requires exact acknowledged admission and fresh work"
        ))
    ), "stale admission sequence is rejected before any work preparation");
    // Probe the stale source binding on the fixture's independent, fresh
    // admission snapshot. It has no claim, preparation, or host completion,
    // so none of those predicates can explain the rejection.
    let source_probe_baseline: (i64, i64, i64, i64, i64, String) =
        Connection::open(&fixture.preclaim_db_path)
            .unwrap()
            .query_row(
                "SELECT (SELECT count(*) FROM product_work_claims_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_review_host_accept_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_review_runs_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                        (SELECT state FROM product_work_backlog_v1 WHERE order_id=?1 AND lifecycle=1)",
                [&order],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .unwrap();
    assert_eq!(source_probe_baseline, (0, 0, 0, 0, 0, "waiting_unsupported".into()));
    let mut owner = ProductStore::open(&fixture.preclaim_db_path, fixture.config.clone()).unwrap();
    let current_source_revision: i64 = Connection::open(&fixture.preclaim_db_path)
        .unwrap()
        .query_row(
            "SELECT revision FROM product_sources_v1 WHERE connector_id=?1",
            [&fixture.identity.connector_id],
            |row| row.get(0),
        )
        .unwrap();
    let current_service_revision: i64 = Connection::open(&fixture.preclaim_db_path)
        .unwrap()
        .query_row(
            "SELECT revision FROM product_services_v1 WHERE service_id=?1",
            [&fixture.identity.service_id],
            |row| row.get(0),
        )
        .unwrap();
    let rebound_identity = foundry_ledger::RepositoryIdentity::canonicalized(
        fixture.identity.repository_owner.clone(),
        "github",
        &fixture.identity.api_origin,
        "repo:stale-binding",
    )
    .unwrap();
    let rebound = owner
        .configure_scoped_source(
            &fixture.owner_session_digest,
            &fixture.identity.connector_id,
            rebound_identity,
            &fixture.identity.repository_owner,
            "renamed-repo",
            "repo:stale-binding",
            "open+foundry:make",
            true,
            current_source_revision,
            "p2-preparation-stale-source-rebind",
            fixture.now,
        )
        .unwrap();
    let grants = [
        foundry_ledger::ProductServiceAction::IntakeRead,
        foundry_ledger::ProductServiceAction::AdmitDelivery,
        foundry_ledger::ProductServiceAction::AckDelivery,
        foundry_ledger::ProductServiceAction::DriveReview,
    ]
    .into_iter()
    .map(|action| foundry_ledger::ProductServiceGrant {
        source_instance_id: rebound.source_instance_id.clone(),
        action,
    })
    .collect::<Vec<_>>();
    let service_revision = owner
        .manage_product_service(
            &fixture.owner_session_digest,
            &fixture.identity.service_id,
            &grants,
            true,
            fixture.now + Duration::hours(3),
            current_service_revision,
            "p2-preparation-stale-source-service-rebind",
            fixture.now,
        )
        .unwrap()
        .revision;
    let rebound_credential = owner
        .issue_product_service_credential(
            &fixture.owner_session_digest,
            &fixture.identity.service_id,
            service_revision,
            fixture.now + Duration::hours(1),
            "p2-preparation-stale-source-credential",
            fixture.now,
        )
        .unwrap()
        .expose_once_to_file_writer()
        .to_owned();
    let rebound_service = owner
        .authenticate_product_service_for_action(
            &rebound_credential,
            &fixture.identity.connector_id,
            &fixture.identity.api_origin,
            "repo:stale-binding",
            foundry_ledger::ProductServiceAction::DriveReview,
            fixture.now,
        )
        .unwrap();
    assert!(
        matches!(
            owner.prepare_p2_review_fixture(
            &rebound_service,
            &fixture.admission,
            &fixture.delivery_id,
            foundry_ledger::P2ReviewPreparation::Waiting,
            ),
            Err(foundry_ledger::ProductError::Invariant(
                "P2 preparation requires exact acknowledged admission and fresh work"
            ))
        ),
        "a valid service on the rebound source cannot prepare the stale admission binding"
    );
    let source_probe_after: (i64, i64, i64, i64, i64, String) =
        Connection::open(&fixture.preclaim_db_path)
            .unwrap()
            .query_row(
                "SELECT (SELECT count(*) FROM product_work_claims_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_review_host_accept_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_review_runs_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                        (SELECT state FROM product_work_backlog_v1 WHERE order_id=?1 AND lifecycle=1)",
                [&order],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .unwrap();
    assert_eq!(source_probe_after, source_probe_baseline, "stale source rejection preserves the fresh unclaimed baseline");
    let after: (i64, i64, i64, i64, i64, i64, i64, i64, i64) =
        Connection::open(&fixture.db_path)
            .unwrap()
            .query_row(
                "SELECT (SELECT count(*) FROM ledger WHERE order_id=?1),
                        (SELECT count(*) FROM product_transition_receipts_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_transition_intents_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_work_claims_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_review_host_accept_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_review_runs_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                        (SELECT count(*) FROM sqlite_master WHERE type='table' AND name='tuple_promotions')",
                [&order],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?)),
            )
            .unwrap();
    assert_eq!(after, before, "duplicate preparation has no durable side effects");
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.restores.load(Ordering::SeqCst), 0);
    drop(owner);
    drop(fixture.result_store.take());
    drop(fixture.host.take());
    std::fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn activation_authority_captures_connector_revision_independently() {
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: "revision-projection-install".into(),
            database_id: "revision-projection-db".into(),
        },
        "revision-projection-source",
        "revision:projection-order",
    )
    .with_unequal_service_and_connector_revisions();
    let mut fixture = genuine_host_done(identity);
    let current: (i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT revision FROM product_services_v1 WHERE service_id=?1),
                    (SELECT revision FROM product_sources_v1 WHERE connector_id=?2)",
            rusqlite::params![fixture.identity.service_id, fixture.identity.connector_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(current, (2, 1), "service and connector revisions differ");
    let captured: (i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT service_revision,connector_revision FROM product_review_authority_v1 WHERE order_id=?1",
            [&fixture.identity.order_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(captured, current);
    fixture
        .result_store
        .as_mut()
        .unwrap()
        .finalize_product_review_result(&fixture.service, &fixture.expected)
        .unwrap();
    drop(fixture.result_store.take());
    drop(fixture.host.take());
    std::fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn activation_authority_insert_failure_rolls_back_the_complete_admission_set() {
    let order = "activation-authority-insert-failure";
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: format!("{order}-install"),
            database_id: format!("{order}-db"),
        },
        format!("{order}-source"),
        order,
    );
    let mut fixture = genuine_host_inflight(identity);
    let admission_anchor_before: (i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM ledger WHERE order_id=?1),
                    (SELECT count(*) FROM product_transition_receipts_v1 WHERE order_id=?1)",
            [&fixture.identity.order_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    Connection::open(&fixture.db_path)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER fail_initial_authority BEFORE INSERT ON product_review_authority_v1
             WHEN NEW.run_id=7
             BEGIN SELECT RAISE(ABORT,'injected activation authority insert failure'); END;",
        )
        .unwrap();
    let dispatch = fixture
        .host
        .as_mut()
        .unwrap()
        .try_dispatch_product_work(&fixture.intent, &fixture.claim);
    assert!(fixture.terminal.is_none());
    let error = format!("{dispatch:?}");
    assert!(error.contains("injected activation authority insert failure"), "{error}");
    let conn = Connection::open(&fixture.db_path).unwrap();
    let activation_rows: (i64, i64, i64, i64, i64, i64, i64) = conn
        .query_row(
            "SELECT (SELECT count(*) FROM product_review_host_accept_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_runs_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_work_bindings_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_bindings_v1),
                    (SELECT count(*) FROM product_review_active_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_authority_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_run_reservations_v1 WHERE order_id=?1)",
            [&fixture.identity.order_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
        )
        .unwrap();
    assert_eq!(activation_rows, (0, 0, 0, 0, 0, 0, 0));
    let budget_and_result: (i64, i64, i64, i64, i64, i64) = conn
        .query_row(
            "SELECT (SELECT count(*) FROM product_review_budget_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                    (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_hold_progress_v1)",
            [&fixture.identity.order_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        )
        .unwrap();
    assert_eq!(budget_and_result, (0, 0, 0, 0, 0, 0));
    let admission_anchor_after: (i64, i64) = conn
        .query_row(
            "SELECT (SELECT count(*) FROM ledger WHERE order_id=?1),
                    (SELECT count(*) FROM product_transition_receipts_v1 WHERE order_id=?1)",
            [&fixture.identity.order_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(admission_anchor_after, admission_anchor_before);
    drop(conn);
    drop(fixture.result_store.take());
    drop(fixture.host.take());
    std::fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn genuine_host_result_finalizes_replays_and_preserves_selected_s1_anchor() {
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: "result-host-install".into(),
            database_id: "result-host-db".into(),
        },
        "result-source-v1",
        "result:order",
    );
    let mut fixture = genuine_host_done(identity);
    // Initial automatic finalization has no human acceptance lineage. A
    // later, unrelated human-scope revision must not become its authority.
    grant_review_hold_scope(&fixture);
    fixture.assert_genuine_private_handoff();
    let root = fixture.root.clone();
    let db = fixture.db_path.clone();
    let now = fixture.now;
    let service = fixture.service.clone();
    let reader = fixture.human_session_digest.clone();
    let terminal = fixture.terminal.as_ref().unwrap().clone();
    let expected = fixture.expected.clone();
    let calls = fixture.calls.clone();
    let overflow = ProductResultExpectation {
        host_key: foundry_exec::HostKey {
            order: expected.host_key.order.clone(),
            attempt: (i64::MAX as u64) + 1,
            run: expected.host_key.run,
            activation: expected.host_key.activation,
        },
        host_result_digest: expected.host_result_digest.clone(),
    };
    assert_eq!(
        fixture
            .result_store
            .as_mut()
            .unwrap()
            .finalize_product_review_result(&service, &overflow),
        Err(foundry_ledger::ProductResultError::InvalidInput),
        "u64 coordinates must fail checked persisted-width conversion"
    );
    let wrong_attempt = ProductResultExpectation {
        host_key: foundry_exec::HostKey {
            order: expected.host_key.order.clone(),
            attempt: expected.host_key.attempt + 1,
            run: expected.host_key.run,
            activation: expected.host_key.activation,
        },
        host_result_digest: expected.host_result_digest.clone(),
    };
    assert_eq!(
        fixture
            .result_store
            .as_mut()
            .unwrap()
            .finalize_product_review_result(&service, &wrong_attempt),
        Err(foundry_ledger::ProductResultError::StaleBinding),
        "wrong persisted attempt must not fall back to a latest host run"
    );
    let wrong_activation = ProductResultExpectation {
        host_key: foundry_exec::HostKey {
            order: expected.host_key.order.clone(),
            attempt: expected.host_key.attempt,
            run: expected.host_key.run,
            activation: expected.host_key.activation + 1,
        },
        host_result_digest: expected.host_result_digest.clone(),
    };
    assert_eq!(
        fixture
            .result_store
            .as_mut()
            .unwrap()
            .finalize_product_review_result(&service, &wrong_activation),
        Err(foundry_ledger::ProductResultError::Ineligible),
        "a different activation sharing order/run coordinates must not alias the accepted host"
    );
    let untouched: (i64, i64, i64, i64) = Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM artifact_tuples WHERE order_id='result:order'),
                    (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id='result:order'),
                    (SELECT count(*) FROM product_review_stages_v1 WHERE order_id='result:order' AND status='complete'),
                    (SELECT count(*) FROM product_review_sessions_v1 WHERE order_id='result:order')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(untouched, (0, 0, 4, 3));
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    let host = fixture.host.take().unwrap();
    let mut result_store = fixture.result_store.take().unwrap();
    let mut store = ProductStore::open(&db, fixture.config.clone()).unwrap();
    // Interleave a different order after the selected order's last event.
    // Result anchors must pair the selected order's sequence with that row's
    // hash, not the global chain head hash.
    let foreign_updated = (now + Duration::seconds(1)).to_rfc3339();
    let foreign_payload = "second synthetic order";
    let foreign_version = foundry_ledger::product::delivery_version_digest(
        "issue:2",
        &foreign_updated,
        "unsupported-test",
        foreign_payload,
    );
    let foreign_delivery = ProductDelivery {
        delivery_id: foundry_ledger::product::github_delivery_id(
            store.installation_id(),
            "github-main",
            "https://api.github.com",
            "repo:1",
            "issue:2",
            &foreign_version,
        ),
        connector_id: "github-main".into(),
        repository_id: "repo:1".into(),
        issue_id: "issue:2".into(),
        observed_updated_at: foreign_updated.clone(),
        version_digest: foreign_version,
        task_kind: "unsupported-test".into(),
        payload: foreign_payload.into(),
    };
    let foreign_cursor = ProductPollCursor {
        connector_id: "github-main".into(),
        query_fingerprint: "result-query-foreign".into(),
        sweep_id: "result-sweep-foreign".into(),
        next_page: 1,
        page_ceiling: 1,
        updated_at: foreign_updated,
    };
    store
        .persist_intake_page_for_service(
            &service,
            &foreign_cursor,
            std::slice::from_ref(&foreign_delivery),
            &[],
            now + Duration::seconds(1),
        )
        .unwrap();
    let foreign_admission = store
        .admit_intake_delivery_for_service(
            &service,
            &foreign_delivery.delivery_id,
            "foreign-order",
            now + Duration::seconds(1),
        )
        .unwrap();
    store
        .mark_intake_acked_for_service(
            &service,
            &foreign_delivery.delivery_id,
            &foreign_admission,
            now + Duration::seconds(1),
        )
        .unwrap();
    let stage_one_relative: String = Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT relative_path FROM product_artifacts_v1
             WHERE order_id='result:order' AND run_id=?1 ORDER BY stage_sequence LIMIT 1",
            [format!("{}:1:7:3", expected.host_key.order)],
            |row| row.get(0),
        )
        .unwrap();
    let foreign_stage_path = fixture.artifact_root.join(stage_one_relative);
    let mut mutations = Vec::new();
    let mut fallback = terminal.clone();
    fallback.log_path = format!("pack#{}", expected.host_key.run);
    mutations.push((
        fallback,
        foundry_ledger::ProductResultError::InvalidEvidence,
    ));
    let mut external = terminal.clone();
    external.log_path = "/outside/unregistered/review.json".into();
    mutations.push((
        external,
        foundry_ledger::ProductResultError::InvalidEvidence,
    ));
    let mut wrong_stage = terminal.clone();
    wrong_stage.log_path = foreign_stage_path.to_string_lossy().into_owned();
    mutations.push((
        wrong_stage,
        foundry_ledger::ProductResultError::InvalidEvidence,
    ));
    let mut empty_chain = terminal.clone();
    empty_chain.generations.clear();
    mutations.push((
        empty_chain,
        foundry_ledger::ProductResultError::InvalidEvidence,
    ));
    let mut wrong_root = terminal.clone();
    wrong_root.generations[0].commit = "f".repeat(40);
    mutations.push((
        wrong_root,
        foundry_ledger::ProductResultError::InvalidEvidence,
    ));
    let mut wrong_terminal = terminal.clone();
    wrong_terminal.generations.last_mut().unwrap().commit = "e".repeat(40);
    mutations.push((
        wrong_terminal,
        foundry_ledger::ProductResultError::InvalidEvidence,
    ));
    let mut wrong_index = terminal.clone();
    wrong_index.generations[0].generation = 1;
    mutations.push((
        wrong_index,
        foundry_ledger::ProductResultError::InvalidEvidence,
    ));
    let mut wrong_initial_fix_log = terminal.clone();
    wrong_initial_fix_log.generations[0].fix_log_path = "/outside/initial-fix.log".into();
    mutations.push((
        wrong_initial_fix_log,
        foundry_ledger::ProductResultError::InvalidEvidence,
    ));
    for (mutated, expected_error) in mutations {
        replace_host_result_evidence(&db, &expected.host_key, &mutated);
        let changed = ProductResultExpectation {
            host_key: expected.host_key.clone(),
            host_result_digest: foundry_ledger::product_host_result_digest(&mutated).unwrap(),
        };
        assert_eq!(
            result_store.finalize_product_review_result(&service, &changed),
            Err(expected_error),
            "invalid generation or nonterminal evidence location must be rejected"
        );
        let untouched: (i64, i64) = Connection::open(&db)
            .unwrap()
            .query_row(
                "SELECT (SELECT count(*) FROM artifact_tuples WHERE order_id='result:order'),
                        (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id='result:order')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(untouched, (0, 0));
        replace_host_result_evidence(&db, &expected.host_key, &terminal);
    }
    let fault = Connection::open(&db).unwrap();
    for (create, drop) in [
        (
            "CREATE TRIGGER result_fail_tuple BEFORE INSERT ON artifact_tuples BEGIN SELECT RAISE(ABORT,'injected tuple write failure'); END;",
            "DROP TRIGGER result_fail_tuple;",
        ),
        (
            "CREATE TRIGGER result_fail_binding BEFORE INSERT ON product_result_bindings_v1 BEGIN SELECT RAISE(ABORT,'injected result binding failure'); END;",
            "DROP TRIGGER result_fail_binding;",
        ),
    ] {
        fault.execute_batch(create).unwrap();
        assert!(
            result_store
                .finalize_product_review_result(&service, &expected)
                .is_err()
        );
        let rows: (i64, i64, i64, i64) = fault
            .query_row(
                "SELECT (SELECT count(*) FROM artifact_tuples WHERE order_id='result:order'),
                        (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id='result:order'),
                        (SELECT count(*) FROM product_review_stages_v1 WHERE order_id='result:order' AND status='complete'),
                        (SELECT count(*) FROM product_review_sessions_v1 WHERE order_id='result:order')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(rows, (0, 0, 4, 3));
        fault.execute_batch(drop).unwrap();
    }
    drop(fault);
    let claim_before_finalize: (String, i64, i64, String, String, String) =
        Connection::open(&db).unwrap().query_row(
            "SELECT c.owner_id,c.epoch,c.fence,c.state,c.lease_until,b.state
             FROM product_work_claims_v1 c JOIN product_work_backlog_v1 b USING(order_id,lifecycle)
             WHERE c.order_id='result:order' AND c.lifecycle=1",
            [],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
        ).unwrap();
    let pre_finalize: (i64, i64, i64, i64, i64) = Connection::open(&db).unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM product_review_host_accept_v1 WHERE order_id='result:order'),
                    (SELECT count(*) FROM product_review_host_results_v1 WHERE order_id='result:order'),
                    (SELECT count(*) FROM product_review_runs_v1 WHERE order_id='result:order'),
                    (SELECT count(*) FROM artifact_tuples WHERE order_id='result:order'),
                    (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id='result:order')",
            [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
        ).unwrap();
    assert_eq!(pre_finalize, (1, 1, 1, 0, 0));
    let persisted_coordinates: (String, String, i64, i64, i64, i64) =
        Connection::open(&db).unwrap().query_row(
            "SELECT r.run_id,a.journal_run,json_extract(a.key_json,'$.attempt'),
                    json_extract(a.key_json,'$.run'),json_extract(a.key_json,'$.activation'),
                    (SELECT run_id FROM product_review_host_accept_v1 WHERE order_id=r.order_id)
             FROM product_review_runs_v1 r JOIN product_review_active_v1 a USING(order_id)
             WHERE r.order_id='result:order'",
            [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
        ).unwrap();
    assert_eq!(persisted_coordinates, ("result:order:1:7:3".into(), "result:order:1:7:3".into(), 1, 7, 3, 7));
    let missing_initial = Connection::open(&db).unwrap();
    missing_initial
        .execute_batch(
            "CREATE TEMP TABLE saved_initial_authority AS
               SELECT * FROM product_review_authority_v1 WHERE order_id='result:order' AND run_id=7;
             DROP TRIGGER product_review_authority_no_delete;",
        )
        .unwrap();
    assert_eq!(
        missing_initial
            .execute(
                "DELETE FROM product_review_authority_v1 WHERE order_id='result:order' AND run_id=7",
                [],
            )
            .unwrap(),
        1,
        "the modern initial authority snapshot is actually absent for this negative vector"
    );
    assert_eq!(
        result_store.finalize_product_review_result(&service, &expected),
        Err(foundry_ledger::ProductResultError::CorruptEvidence),
        "a modern initial activation missing its required authority snapshot is corrupt evidence"
    );
    let missing_initial_no_effect: (i64, i64, i64) = missing_initial
        .query_row(
            "SELECT (SELECT count(*) FROM artifact_tuples WHERE order_id='result:order'),
                    (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id='result:order'),
                    (SELECT count(*) FROM product_review_authority_v1 WHERE order_id='result:order')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(missing_initial_no_effect, (0, 0, 0));
    missing_initial
        .execute(
            "INSERT INTO product_review_authority_v1 SELECT * FROM saved_initial_authority",
            [],
        )
        .unwrap();
    missing_initial
        .execute_batch(
            "CREATE TRIGGER product_review_authority_no_delete BEFORE DELETE ON product_review_authority_v1 BEGIN SELECT RAISE(ABORT,'review authority is immutable'); END;",
        )
        .unwrap();
    assert_eq!(
        missing_initial
            .query_row::<i64, _, _>(
                "SELECT count(*) FROM product_review_authority_v1 WHERE order_id='result:order' AND run_id=7",
                [],
                |r| r.get(0),
            )
            .unwrap(),
        1,
        "exact historical authority bytes are restored before the positive control"
    );
    drop(missing_initial);
    let info = result_store
        .finalize_product_review_result(&service, &expected)
        .unwrap();
    // This preserved negative changes the caller's requested attempt/run to an
    // unoccupied pair. It is deliberately distinct from G05's remaining
    // stored actual-attempt vector and must not be counted as that vector.
    let replay_requested_coordinates_mismatch = ProductResultExpectation {
        host_key: foundry_exec::HostKey {
            // Use an unoccupied attempt/run pair so winner-alias conflict
            // classification cannot hide the verified current-attempt check.
            attempt: expected.host_key.attempt + 1,
            run: expected.host_key.run + 1,
            ..expected.host_key.clone()
        },
        host_result_digest: expected.host_result_digest.clone(),
    };
    let winner_bytes_before_attempt_probe: (String, String) = Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT payload_json,receipt_json FROM product_result_bindings_v1 WHERE order_id='result:order'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        result_store.finalize_product_review_result(&service, &replay_requested_coordinates_mismatch),
        Err(foundry_ledger::ProductResultError::StaleBinding),
        "verified-order replay with unoccupied caller attempt/run coordinates is stale while the stored winner remains valid"
    );
    assert_eq!(
        Connection::open(&db)
            .unwrap()
            .query_row(
                "SELECT payload_json,receipt_json FROM product_result_bindings_v1 WHERE order_id='result:order'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap(),
        winner_bytes_before_attempt_probe,
        "the actual-attempt mismatch leaves the internally consistent winner bytes unchanged"
    );
    let claim_after_first_finalize: (String, i64, i64, String, String, String) =
        Connection::open(&db).unwrap().query_row(
            "SELECT c.owner_id,c.epoch,c.fence,c.state,c.lease_until,b.state
             FROM product_work_claims_v1 c JOIN product_work_backlog_v1 b USING(order_id,lifecycle)
             WHERE c.order_id='result:order' AND c.lifecycle=1",
            [],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
        ).unwrap();
    assert_eq!(
        claim_after_first_finalize, claim_before_finalize,
        "first successful finalization leaves the claim and backlog unchanged"
    );
    let after_first_finalize: (i64, i64, i64, i64, i64) = Connection::open(&db).unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM product_review_host_accept_v1 WHERE order_id='result:order'),
                    (SELECT count(*) FROM product_review_host_results_v1 WHERE order_id='result:order'),
                    (SELECT count(*) FROM product_review_runs_v1 WHERE order_id='result:order'),
                    (SELECT count(*) FROM artifact_tuples WHERE order_id='result:order'),
                    (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id='result:order')",
            [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
        ).unwrap();
    assert_eq!(after_first_finalize, (1, 1, 1, 1, 1));
    let persisted_intent_json: String = Connection::open(&db).unwrap().query_row(
        "SELECT intent_json FROM product_review_bindings_v1 WHERE key_json=?1",
        [serde_json::to_string(&expected.host_key).unwrap()],
        |r| r.get(0),
    ).unwrap();
    assert_eq!(
        serde_json::from_str::<foundry_exec::ExecutionIntent>(&persisted_intent_json).unwrap(),
        fixture.intent,
        "the persisted host binding contains the exact expected intent_json"
    );
    assert_eq!(terminal.exit_code, 0);
    let activation_alias = ProductResultExpectation {
        host_key: foundry_exec::HostKey {
            activation: expected.host_key.activation + 1,
            ..expected.host_key.clone()
        },
        host_result_digest: expected.host_result_digest.clone(),
    };
    assert_eq!(
        result_store.finalize_product_review_result(&service, &activation_alias),
        Err(foundry_ledger::ProductResultError::Conflict),
        "a valid finalized winner makes another activation an alias conflict"
    );
    let lifecycle_alias = ProductResultExpectation {
        host_key: foundry_exec::HostKey {
            attempt: expected.host_key.attempt + 1,
            ..expected.host_key.clone()
        },
        host_result_digest: expected.host_result_digest.clone(),
    };
    assert_eq!(
        result_store.finalize_product_review_result(&service, &lifecycle_alias),
        Err(foundry_ledger::ProductResultError::Conflict),
        "a valid finalized winner makes another lifecycle an alias conflict"
    );
    let expected = ProductResultExpectation {
        host_key: foundry_exec::HostKey {
            order: "result:order".into(),
            attempt: 1,
            run: 7,
            activation: 3,
        },
        host_result_digest: foundry_ledger::product_host_result_digest(&terminal).unwrap(),
    };
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert_eq!(info.run_id, 7);
    assert_eq!(info.activation, 3);
    assert_eq!(info.generation, 0);
    let before:(i64,i64,i64,i64,i64)=Connection::open(&db).unwrap().query_row("SELECT (SELECT count(*) FROM product_review_stages_v1 WHERE order_id='result:order' AND status='complete'),(SELECT count(*) FROM product_artifacts_v1 WHERE order_id='result:order'),(SELECT count(*) FROM product_review_sessions_v1 WHERE order_id='result:order'),(SELECT count(*) FROM artifact_tuples WHERE order_id='result:order'),(SELECT count(*) FROM product_result_bindings_v1 WHERE order_id='result:order')",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap();
    assert_eq!(before, (4, 4, 3, 1, 1));
    let receipt_before_replay: (String, String) = Connection::open(&db).unwrap()
        .query_row(
            "SELECT payload_json,receipt_json FROM product_result_bindings_v1 WHERE order_id='result:order'",
            [], |r| Ok((r.get(0)?,r.get(1)?)),
        ).unwrap();
    let mut malformed_stored_rows = vec![
        (
            receipt_before_replay.0.clone(),
            receipt_before_replay.1.replacen("\"version\":1", "\"version\":2", 1),
        ),
        (
            receipt_before_replay.0.clone(),
            receipt_before_replay.1.replacen(
                "local_review_result",
                "execution_emit_receipt",
                1,
            ),
        ),
        (
            receipt_before_replay.0.clone(),
            receipt_before_replay.1.replacen(
                "{\"version\":1,\"kind\":\"local_review_result\",",
                "{\"kind\":\"local_review_result\",\"version\":1,",
                1,
            ),
        ),
        (
            receipt_before_replay.0.replacen("\"version\":1", "\"version\":2", 1),
            receipt_before_replay.1.clone(),
        ),
        (
            receipt_before_replay.0.replacen(
                "{\"version\":1,\"installation_id\":\"result-host-install\",",
                "{\"installation_id\":\"result-host-install\",\"version\":1,",
                1,
            ),
            receipt_before_replay.1.clone(),
        ),
        (
            receipt_before_replay.0.replacen(
                &format!("\"intent\":{{\"order\":\"{}\",\"attempt\":1", expected.host_key.order),
                &format!("\"intent\":{{\"order\":\"{}\",\"attempt\":-1", expected.host_key.order),
                1,
            ),
            receipt_before_replay.1.clone(),
        ),
        (
            receipt_before_replay.0.replacen(
                &format!("\"intent\":{{\"order\":\"{}\",\"attempt\":1,\"run\":7", expected.host_key.order),
                &format!("\"intent\":{{\"order\":\"{}\",\"attempt\":1,\"run\":-1", expected.host_key.order),
                1,
            ),
            receipt_before_replay.1.clone(),
        ),
    ];
    let stored_payload: serde_json::Value =
        serde_json::from_str(&receipt_before_replay.0).unwrap();
    // Each signed persisted coordinate is changed independently on the exact
    // committed winner. A reader must reject negative coordinates as corrupt
    // stored evidence without changing the winner or its participant set.
    let negative_coordinate_paths = [
        "/tuple/attempt",
        "/tuple/run",
        "/tuple/generation",
        "/artifacts/0/lifecycle",
        "/artifacts/0/admission_seq",
        "/artifacts/0/attempt",
        "/artifacts/0/generation",
        "/artifacts/0/stage_sequence",
    ];
    for pointer in negative_coordinate_paths {
        let mut changed: serde_json::Value = serde_json::from_str(&receipt_before_replay.0).unwrap();
        assert!(changed.pointer(pointer).unwrap().as_i64().unwrap() >= 0, "coordinate exists: {pointer}");
        *changed.pointer_mut(pointer).unwrap() = serde_json::json!(-1);
        let changed = serde_json::to_string(&changed).unwrap();
        let corruption = Connection::open(&db).unwrap();
        corruption.execute_batch("DROP TRIGGER product_result_bindings_no_update;").unwrap();
        assert_eq!(corruption.execute(
            "UPDATE product_result_bindings_v1 SET payload_json=?1 WHERE order_id='result:order'",
            [&changed],
        ).unwrap(), 1);
        assert_eq!(
            result_store.lookup_product_review_result(&service, &expected),
            Err(foundry_ledger::ProductResultError::CorruptEvidence),
            "negative persisted coordinate is corrupt stored evidence: {pointer}"
        );
        if pointer == "/tuple/attempt" {
            assert_eq!(
                result_store.finalize_product_review_result(&service, &expected),
                Err(foundry_ledger::ProductResultError::CorruptEvidence),
                "verified replay keeps the caller's actual attempt fixed and rejects a separately mutated stored winner attempt"
            );
        }
        corruption.execute(
            "UPDATE product_result_bindings_v1 SET payload_json=?1 WHERE order_id='result:order'",
            [&receipt_before_replay.0],
        ).unwrap();
        corruption.execute_batch("CREATE TRIGGER product_result_bindings_no_update BEFORE UPDATE ON product_result_bindings_v1 BEGIN SELECT RAISE(ABORT,'product result is immutable'); END;").unwrap();
    }
    let service_revision = stored_payload["work"]["authority"]["service_revision"]
        .as_i64()
        .unwrap();
    malformed_stored_rows.push((
        receipt_before_replay.0.replacen(
            &format!("\"service_revision\":{service_revision}"),
            &format!("\"service_revision\":{}", service_revision + 1),
            1,
        ),
        receipt_before_replay.1.clone(),
    ));
    let artifact_digest = stored_payload["artifacts"][0]["artifact_sha256"]
        .as_str()
        .unwrap();
    malformed_stored_rows.push((
        receipt_before_replay.0.replacen(
            &format!("\"artifact_sha256\":{}", serde_json::to_string(artifact_digest).unwrap()),
            &format!("\"artifact_sha256\":{}", serde_json::to_string(&"0".repeat(64)).unwrap()),
            1,
        ),
        receipt_before_replay.1.clone(),
    ));
    assert_eq!(malformed_stored_rows.len(), 9);
    let member_paths = [
        "/work/authority/service_id",
        "/work/authority/service_revision",
        "/work/authority/credential_revision",
        "/work/authority/drive_review_grant_revision",
        "/work/authority/source_instance_id",
        "/work/authority/source_generation",
        "/work/authority/source_instance_revision",
        "/work/authority/connector_revision",
        "/artifacts/0/artifact_id",
        "/artifacts/0/provenance_digest",
        "/artifacts/0/artifact_sha256",
        "/artifacts/0/order_id",
        "/artifacts/0/lifecycle",
        "/artifacts/0/source_instance_id",
        "/artifacts/0/repository_key",
        "/artifacts/0/admission_seq",
        "/artifacts/0/admission_hash",
        "/artifacts/0/attempt",
        "/artifacts/0/journal_run",
        "/artifacts/0/generation",
        "/artifacts/0/stage_sequence",
        "/artifacts/0/stage_id",
        "/artifacts/0/pack_digest",
        "/artifacts/0/relative_path",
        "/artifacts/0/output_json",
    ];
    for pointer in member_paths {
        let mut value: serde_json::Value = serde_json::from_str(&receipt_before_replay.0).unwrap();
        let current = value
            .pointer(pointer)
            .unwrap_or_else(|| panic!("G08 wire member exists: {pointer}"))
            .clone();
        let changed = match current {
            serde_json::Value::String(text) => serde_json::Value::String(format!("{text}-changed")),
            serde_json::Value::Number(number) => serde_json::Value::Number(
                serde_json::Number::from(number.as_i64().unwrap() + 1),
            ),
            other => panic!("unexpected G08 member kind at {pointer}: {other}"),
        };
        *value.pointer_mut(pointer).unwrap() = changed;
        let payload = serde_json::to_string(&value).unwrap();
        assert_ne!(payload, receipt_before_replay.0, "G08 member mutation is effective: {pointer}");
        malformed_stored_rows.push((payload, receipt_before_replay.1.clone()));
    }
    for (payload, receipt) in malformed_stored_rows.drain(..) {
        let corruption = Connection::open(&db).unwrap();
        corruption
            .execute_batch("DROP TRIGGER product_result_bindings_no_update;")
            .unwrap();
        corruption
            .execute(
                "UPDATE product_result_bindings_v1 SET payload_json=?1,receipt_json=?2 WHERE order_id='result:order'",
                rusqlite::params![payload, receipt],
            )
            .unwrap();
        assert_eq!(
            result_store.lookup_product_review_result(&service, &expected),
            Err(foundry_ledger::ProductResultError::CorruptEvidence),
            "stored payload/receipt version, kind, and canonical bytes are validated"
        );
        corruption
            .execute(
                "UPDATE product_result_bindings_v1 SET payload_json=?1,receipt_json=?2 WHERE order_id='result:order'",
                rusqlite::params![receipt_before_replay.0, receipt_before_replay.1],
            )
            .unwrap();
        corruption
            .execute_batch(
                "CREATE TRIGGER product_result_bindings_no_update BEFORE UPDATE ON product_result_bindings_v1 BEGIN SELECT RAISE(ABORT,'product result is immutable'); END;",
            )
            .unwrap();
    }
    let key = foundry_exec::ArtifactTupleKey {
        order: foundry_exec::OrderId(expected.host_key.order.clone()),
        attempt: foundry_exec::Attempt(expected.host_key.attempt),
        run: foundry_exec::RunId(expected.host_key.run),
        generation: foundry_exec::Generation(0),
    };
    let incompatible_wires = [
        serde_json::to_string(&foundry_exec::EmitReceipt {
            key: key.clone(),
            event: "stored-result-negative".into(),
            at: now,
        })
        .unwrap(),
        serde_json::to_string(&foundry_exec::PromotionReceipt {
            tuple: key,
            promoted_at: now,
        })
        .unwrap(),
    ];
    for wire in incompatible_wires {
        let corruption = Connection::open(&db).unwrap();
        corruption
            .execute_batch("DROP TRIGGER product_result_bindings_no_update;")
            .unwrap();
        let before_wire_rejection: (i64, i64, i64, i64, i64, String, String) = corruption
            .query_row(
                "SELECT (SELECT count(*) FROM artifact_tuples WHERE order_id='result:order'),
                        (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id='result:order'),
                        (SELECT count(*) FROM product_outcomes_v1),
                        (SELECT count(*) FROM product_token_consumptions_v1),
                        (SELECT count(*) FROM product_review_stages_v1 WHERE order_id='result:order'),
                        (SELECT payload_json FROM product_result_bindings_v1 WHERE order_id='result:order'),
                        (SELECT receipt_json FROM product_result_bindings_v1 WHERE order_id='result:order')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
            )
            .unwrap();
        assert_eq!(
            corruption
                .execute(
                    "UPDATE product_result_bindings_v1 SET receipt_json=?1 WHERE order_id='result:order'",
                    [&wire],
                )
                .unwrap(),
            1,
            "the genuine stored receipt is independently replaced by a real exec wire"
        );
        let corrupted_receipt: String = corruption
            .query_row(
                "SELECT receipt_json FROM product_result_bindings_v1 WHERE order_id='result:order'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(corrupted_receipt, wire, "the wire mutation must take effect");
        assert_eq!(
            result_store.lookup_product_review_result(&service, &expected),
            Err(foundry_ledger::ProductResultError::CorruptEvidence),
            "EmitReceipt and PromotionReceipt cannot be accepted as local result receipts"
        );
        assert_eq!(
            result_store.finalize_product_review_result(&service, &expected),
            Err(foundry_ledger::ProductResultError::CorruptEvidence),
            "the strict finalizer rejects a genuine stored result whose receipt is an exec wire"
        );
        let after_wire_rejection: (i64, i64, i64, i64, i64, String) = corruption
            .query_row(
                "SELECT (SELECT count(*) FROM artifact_tuples WHERE order_id='result:order'),
                        (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id='result:order'),
                        (SELECT count(*) FROM product_outcomes_v1),
                        (SELECT count(*) FROM product_token_consumptions_v1),
                        (SELECT count(*) FROM product_review_stages_v1 WHERE order_id='result:order'),
                        (SELECT payload_json FROM product_result_bindings_v1 WHERE order_id='result:order')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .unwrap();
        assert_eq!(
            after_wire_rejection,
            (
                before_wire_rejection.0,
                before_wire_rejection.1,
                before_wire_rejection.2,
                before_wire_rejection.3,
                before_wire_rejection.4,
                before_wire_rejection.5.clone(),
            ),
            "strict lookup/finalizer rejection causes no tuple, outcome, consumption, stage, or payload effect"
        );
        corruption
            .execute(
                "UPDATE product_result_bindings_v1 SET receipt_json=?1 WHERE order_id='result:order'",
                [&receipt_before_replay.1],
            )
            .unwrap();
        corruption
            .execute_batch(
                "CREATE TRIGGER product_result_bindings_no_update BEFORE UPDATE ON product_result_bindings_v1 BEGIN SELECT RAISE(ABORT,'product result is immutable'); END;",
            )
            .unwrap();
    }
    let replay = result_store
        .finalize_product_review_result(&service, &expected)
        .unwrap();
    assert_eq!(replay, info);
    let conn = Connection::open(&db).unwrap();
    let after: (i64, i64, i64, i64) = conn
        .query_row(
            "SELECT (SELECT count(*) FROM ledger),(SELECT count(*) FROM artifact_tuples WHERE order_id='result:order'),(SELECT count(*) FROM product_result_bindings_v1 WHERE order_id='result:order'),(SELECT count(*) FROM sqlite_master WHERE type='table' AND name='tuple_promotions')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(after, (5, 1, 1, 0));
    let claim_after_replay: (String, i64, i64, String, String, String) = conn.query_row(
        "SELECT c.owner_id,c.epoch,c.fence,c.state,c.lease_until,b.state
         FROM product_work_claims_v1 c JOIN product_work_backlog_v1 b USING(order_id,lifecycle)
         WHERE c.order_id='result:order' AND c.lifecycle=1",
        [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
    ).unwrap();
    assert_eq!(claim_after_replay, claim_before_finalize, "finalization and replay preserve the completed claim");
    let receipt_after_replay: (String, String) = conn.query_row(
        "SELECT payload_json,receipt_json FROM product_result_bindings_v1 WHERE order_id='result:order'",
        [], |r| Ok((r.get(0)?,r.get(1)?)),
    ).unwrap();
    assert_eq!(receipt_after_replay, receipt_before_replay, "replay preserves the exact stored payload and receipt bytes");
    let finalized_counts: (i64, i64, i64, i64, i64) = conn.query_row(
        "SELECT (SELECT count(*) FROM product_review_host_accept_v1 WHERE order_id='result:order'),
                (SELECT count(*) FROM product_review_host_results_v1 WHERE order_id='result:order'),
                (SELECT count(*) FROM product_review_runs_v1 WHERE order_id='result:order'),
                (SELECT count(*) FROM artifact_tuples WHERE order_id='result:order'),
                (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id='result:order')",
        [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
    ).unwrap();
    assert_eq!(finalized_counts, (1, 1, 1, 1, 1));
    let (payload, selected_hash, global_hash): (String, String, String) = conn
        .query_row(
            "SELECT b.payload_json,l.hash,(SELECT hash FROM ledger ORDER BY seq DESC LIMIT 1)
             FROM product_result_bindings_v1 b
             JOIN ledger l ON l.seq=json_extract(b.payload_json,'$.s1_sequence')
             WHERE b.order_id='result:order'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload["s1_sequence"], 4);
    assert_eq!(payload["s1_event_hash"], selected_hash);
    assert_ne!(selected_hash, global_hash);
    let wrong = ProductResultExpectation {
        host_key: expected.host_key.clone(),
        host_result_digest: "f".repeat(64),
    };
    assert_eq!(
        result_store.finalize_product_review_result(&service, &wrong),
        Err(foundry_ledger::ProductResultError::Conflict)
    );
    let token_digest = "c".repeat(64);
    result_store
        .issue_approval_grant(
            &reader,
            "result-approval-grant",
            &token_digest,
            "result:order",
            now,
        )
        .unwrap();
    let mut approval = fixture.approval.clone();
    approval.token_digest = token_digest;
    approval.artifact_digest = terminal.result_commit.clone();
    approval.tuple_digest = info.tuple_digest.clone();
    let stale_reservation = result_store.prepare_approval(&reader, &approval).unwrap();
    let mut reclaimed = approval.clone();
    reclaimed.now += Duration::minutes(5);
    let reservation = result_store.prepare_approval(&reader, &reclaimed).unwrap();
    assert_ne!(stale_reservation, reservation);
    assert!(
        result_store
            .approve_product(&reader, &reclaimed, &stale_reservation)
            .is_err()
    );
    let wrong_token = "d".repeat(64);
    result_store
        .issue_approval_grant(
            &reader,
            "wrong-result-grant",
            &wrong_token,
            "result:order",
            now,
        )
        .unwrap();
    let mut wrong_result = reclaimed.clone();
    wrong_result.request_key = "wrong-result-binding".into();
    wrong_result.grant_id = "wrong-result-grant".into();
    wrong_result.token_digest = wrong_token;
    wrong_result.tuple_digest = "f".repeat(64);
    let wrong_proof = result_store
        .prepare_approval(&reader, &wrong_result)
        .unwrap();
    assert!(
        result_store
            .approve_product(&reader, &wrong_result, &wrong_proof)
            .is_err()
    );
    let denied: (i64, i64, i64) = Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM ledger WHERE order_id='result:order' AND to_state='publishing'),
                    (SELECT count(*) FROM product_token_consumptions_v1),
                    (SELECT count(*) FROM product_outcomes_v1)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(denied, (0, 0, 0));

    let fault = Connection::open(&db).unwrap();
    let boundaries = [
        (
            "result_fail_s1",
            "CREATE TRIGGER result_fail_s1 BEFORE INSERT ON ledger BEGIN SELECT RAISE(ABORT,'injected result S1 fault'); END;",
            "DROP TRIGGER result_fail_s1;",
        ),
        (
            "result_fail_token",
            "CREATE TRIGGER result_fail_token BEFORE INSERT ON product_token_consumptions_v1 BEGIN SELECT RAISE(ABORT,'injected result token fault'); END;",
            "DROP TRIGGER result_fail_token;",
        ),
        (
            "result_fail_grant",
            "CREATE TRIGGER result_fail_grant BEFORE UPDATE ON product_grants_v1 BEGIN SELECT RAISE(ABORT,'injected result grant fault'); END;",
            "DROP TRIGGER result_fail_grant;",
        ),
        (
            "result_fail_outcome",
            "CREATE TRIGGER result_fail_outcome BEFORE INSERT ON product_outcomes_v1 BEGIN SELECT RAISE(ABORT,'injected result outcome fault'); END;",
            "DROP TRIGGER result_fail_outcome;",
        ),
        (
            "result_fail_sidebinding",
            "CREATE TRIGGER result_fail_sidebinding BEFORE INSERT ON product_approval_result_bindings_v1 BEGIN SELECT RAISE(ABORT,'injected result sidebinding fault'); END;",
            "DROP TRIGGER result_fail_sidebinding;",
        ),
    ];
    for (name, create, drop) in boundaries {
        fault.execute_batch(create).unwrap();
        assert!(
            result_store
                .approve_product(&reader, &reclaimed, &reservation)
                .is_err()
        );
        let partial: (i64, i64, i64, i64, i64) = fault
            .query_row(
                "SELECT
                   (SELECT count(*) FROM ledger WHERE order_id='result:order' AND to_state='publishing'),
                   (SELECT count(*) FROM product_token_consumptions_v1),
                   (SELECT count(*) FROM product_grants_v1 WHERE state='consumed'),
                   (SELECT count(*) FROM product_outcomes_v1),
                   (SELECT count(*) FROM product_approval_result_bindings_v1)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(partial, (0, 0, 0, 0, 0), "partial state after {name}");
        fault.execute_batch(drop).unwrap();
    }
    drop(fault);
    let committed = result_store
        .approve_product(&reader, &reclaimed, &reservation)
        .unwrap();
    assert_eq!(committed.sequence, 6);
    let approvals: (i64, i64, i64, i64, i64) = Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT
               (SELECT count(*) FROM ledger WHERE order_id='result:order' AND to_state='publishing'),
               (SELECT count(*) FROM product_transition_receipts_v1 WHERE operation='human-approval'),
               (SELECT count(*) FROM product_token_consumptions_v1),
               (SELECT count(*) FROM product_outcomes_v1),
               (SELECT count(*) FROM product_approval_result_bindings_v1)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(approvals, (1, 1, 1, 1, 1));
    assert!(result_store.authorize_product_publish(&committed).is_err());
    let immutable_tuple = Connection::open(&db).unwrap();
    assert!(immutable_tuple
        .execute(
            "UPDATE artifact_tuples SET immutable_commit=?1 WHERE order_id='result:order' AND attempt=1 AND run_id=7 AND generation=0",
            ["f".repeat(40)],
        )
        .is_err());
    drop(immutable_tuple);
    let after_approval = result_store
        .finalize_product_review_result(&service, &expected)
        .unwrap();
    assert_eq!(
        after_approval, info,
        "stored result replay must survive approval"
    );
    assert_eq!(host.dispatch_count(), 1);
    drop(host);
    drop(result_store);
    let mut reopened = ProductStore::open(&db, fixture.config.clone()).unwrap();
    let replay_proof = reopened.prepare_approval(&reader, &reclaimed).unwrap();
    assert_eq!(
        reopened
            .approve_product(&reader, &reclaimed, &replay_proof)
            .unwrap(),
        committed,
        "approval replay after reopen must return the exact stored receipt"
    );
    let deliberate_corruption = Connection::open(&db).unwrap();
    deliberate_corruption
        .execute_batch(
            "DROP TRIGGER product_result_bindings_no_update;
             UPDATE product_result_bindings_v1 SET receipt_json=receipt_json || ' '
             WHERE order_id='result:order';",
        )
        .unwrap();
    assert_eq!(
        reopened.finalize_product_review_result(&service, &activation_alias),
        Err(foundry_ledger::ProductResultError::CorruptEvidence),
        "a corrupt stored winner must not be hidden by an alias Conflict"
    );
    assert_eq!(
        reopened.finalize_product_review_result(&service, &lifecycle_alias),
        Err(foundry_ledger::ProductResultError::CorruptEvidence),
        "a corrupt stored winner must not be hidden by a lifecycle alias Conflict"
    );
    drop(deliberate_corruption);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert_eq!(fixture.restores.load(Ordering::SeqCst), 0);
    drop(reopened);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn result_location_must_resolve_registered_terminal_evidence() {
    for (case, mutation) in [
        ("fallback", 0),
        ("foreign-stage", 1),
        ("external", 2),
        ("malformed-terminal", 3),
    ] {
        let order = format!("result-location-{case}");
        let identity = FixtureIdentity::new(
            ProductConfig {
                installation_id: format!("{order}-install"),
                database_id: format!("{order}-db"),
            },
            format!("{order}-source"),
            order.clone(),
        );

        // A separate unmodified G-pre control proves this exact producer and
        // expectation can be finalized before the independent corruption.
        let mut control = genuine_host_done(identity.clone());
        control
            .result_store
            .as_mut()
            .unwrap()
            .finalize_product_review_result(&control.service, &control.expected)
            .expect("unmodified terminal evidence finalizes");
        assert_eq!(control.calls.load(Ordering::SeqCst), 4);
        drop(control.result_store.take());
        drop(control.host.take());
        std::fs::remove_dir_all(control.root).unwrap();

        let mut fixture = genuine_host_done(identity);
        let terminal = fixture.terminal.as_ref().unwrap().clone();
        let mut invalid = terminal.clone();
        match mutation {
            0 => invalid.log_path = format!("pack#{}", fixture.expected.host_key.run),
            1 => {
                let relative: String = Connection::open(&fixture.db_path)
                    .unwrap()
                    .query_row(
                        "SELECT relative_path FROM product_artifacts_v1
                         WHERE order_id=?1 ORDER BY stage_sequence LIMIT 1",
                        [&order],
                        |row| row.get(0),
                    )
                    .unwrap();
                invalid.log_path = fixture.artifact_root.join(relative).to_string_lossy().into();
            }
            2 => invalid.log_path = "/outside/unregistered/review.json".into(),
            3 => invalid.log_path = "../registered/../../malformed-terminal.log".into(),
            _ => unreachable!(),
        }
        assert_ne!(invalid.log_path, terminal.log_path, "the mutation is effective");
        replace_host_result_evidence(&fixture.db_path, &fixture.expected.host_key, &invalid);
        let invalid_expectation = ProductResultExpectation {
            host_key: fixture.expected.host_key.clone(),
            host_result_digest: foundry_ledger::product_host_result_digest(&invalid).unwrap(),
        };
        assert_eq!(
            fixture
                .result_store
                .as_mut()
                .unwrap()
                .finalize_product_review_result(&fixture.service, &invalid_expectation),
            Err(foundry_ledger::ProductResultError::InvalidEvidence),
            "{case} path does not resolve to registered terminal evidence"
        );
        let no_effect: (i64, i64, i64, i64, i64) = Connection::open(&fixture.db_path)
            .unwrap()
            .query_row(
                "SELECT (SELECT count(*) FROM product_review_host_accept_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_review_host_results_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                        (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_outcomes_v1)",
                [&order],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .unwrap();
        assert_eq!(no_effect, (1, 1, 0, 0, 0));
        replace_host_result_evidence(&fixture.db_path, &fixture.expected.host_key, &terminal);
        let restored = fixture
            .result_store
            .as_mut()
            .unwrap()
            .finalize_product_review_result(&fixture.service, &fixture.expected)
            .expect("restoring the exact producer bytes restores successful finalization");
        assert_eq!(restored.host_result_digest, fixture.expected.host_result_digest);
        let committed: (i64, i64) = Connection::open(&fixture.db_path)
            .unwrap()
            .query_row(
                "SELECT (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                        (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1)",
                [&order],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(committed, (1, 1));
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 4);
        assert_eq!(fixture.restores.load(Ordering::SeqCst), 0);
        drop(fixture.result_store.take());
        drop(fixture.host.take());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }
}

#[test]
fn accepted_resume_binds_three_generations_and_fourteen_artifacts() {
    assert_continuation_result(ReviewScript::ResumePass, "p2:A", false);
    assert_continuation_result(ReviewScript::ResumeCommitPass, "p2:B", false);
}

#[test]
fn completed_pending_a_b_reject_each_current_service_authority_change() {
    for (script, order) in [
        (ReviewScript::ResumePass, "g19-a"),
        (ReviewScript::ResumeCommitPass, "g19-b"),
    ] {
        let (fixture, expected) = completed_pending_resume_fixture(script, order);
        assert_pending_result_authority_matrix(&fixture, &expected);
        drop(fixture.result_store);
        std::fs::remove_dir_all(fixture.root).unwrap();
    }
}

#[test]
fn initial_completed_pending_result_rejects_each_lineage_human_authority_change() {
    let (mut fixture, accepted) = accepted_hold(
        "g19-initial-lineage-authority",
        ReviewScript::DirectCommitPass,
        ProductHoldDecision::CommitPass,
    );
    let mut host = fixture.host.take().unwrap();
    host.continue_accepted_product_hold(
        &fixture.human_session_digest,
        &accepted.operation_id,
        fixture.now,
    )
    .unwrap();
    drop(host);
    let db = Connection::open(&fixture.db_path).unwrap();
    let terminal: foundry_exec::HostResult = db
        .query_row(
            "SELECT result_json FROM product_review_host_results_v1 WHERE order_id=?1 AND attempt=1 AND run_id=7 AND activation=3",
            [&fixture.identity.order_id],
            |row| {
                serde_json::from_str(&row.get::<_, String>(0)?).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })
            },
        )
        .unwrap();
    fixture.expected = ProductResultExpectation {
        host_key: foundry_exec::HostKey {
            order: fixture.identity.order_id.clone(),
            attempt: 1,
            run: 7,
            activation: 3,
        },
        host_result_digest: foundry_ledger::product_host_result_digest(&terminal).unwrap(),
    };
    let mut store = ProductStore::open(&fixture.db_path, fixture.config.clone()).unwrap();
    let mut result_store = fixture.result_store.take().unwrap();
    let db = Connection::open(&fixture.db_path).unwrap();

    let account_before: (String, i64, i64) = db
        .query_row(
            "SELECT role,revision,enabled FROM product_accounts_v1 WHERE actor_id=?1",
            [&accepted.actor_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        db.execute(
            "UPDATE product_accounts_v1 SET revision=revision+1 WHERE actor_id=?1",
            [&accepted.actor_id],
        )
        .unwrap(),
        1,
        "the lineage account revision changes independently of role, enabled state, and scope grants"
    );
    assert_eq!(
        result_store.finalize_product_review_result(&fixture.service, &fixture.expected),
        Err(foundry_ledger::ProductResultError::StaleBinding),
        "initial completed-pending finalizer rejects lineage human-account revision ABA"
    );
    assert_eq!(result_store.lookup_product_review_result(&fixture.service, &fixture.expected), Ok(None));
    db.execute(
        "UPDATE product_accounts_v1 SET role=?1,revision=?2,enabled=?3 WHERE actor_id=?4",
        rusqlite::params![account_before.0, account_before.1, account_before.2, accepted.actor_id],
    )
    .unwrap();
    db.execute(
        "UPDATE product_scope_subject_revisions_v1 SET revision=?1
         WHERE subject_id=?2 AND source_instance_id=?3",
        rusqlite::params![accepted.scope_revision + 1, accepted.actor_id, accepted.source_instance_id],
    )
    .unwrap();
    assert_eq!(
        result_store.finalize_product_review_result(&fixture.service, &fixture.expected),
        Err(foundry_ledger::ProductResultError::StaleBinding),
        "initial completed-pending finalizer independently rejects lineage human-scope revision"
    );
    assert_eq!(result_store.lookup_product_review_result(&fixture.service, &fixture.expected), Ok(None));
    db.execute(
        "UPDATE product_scope_subject_revisions_v1 SET revision=?1
         WHERE subject_id=?2 AND source_instance_id=?3",
        rusqlite::params![accepted.scope_revision, accepted.actor_id, accepted.source_instance_id],
    )
    .unwrap();

    db.execute(
        "UPDATE product_scope_grants_v1 SET grant_revision=?1
         WHERE subject_id=?2 AND source_instance_id=?3 AND capability='resolve_review_hold'",
        rusqlite::params![accepted.grant_revision + 1, accepted.actor_id, accepted.source_instance_id],
    )
    .unwrap();
    assert_eq!(
        result_store.finalize_product_review_result(&fixture.service, &fixture.expected),
        Err(foundry_ledger::ProductResultError::StaleBinding),
        "initial completed-pending finalizer independently rejects ResolveReviewHold grant revision"
    );
    assert_eq!(result_store.lookup_product_review_result(&fixture.service, &fixture.expected), Ok(None));
    db.execute(
        "UPDATE product_scope_grants_v1 SET grant_revision=?1
         WHERE subject_id=?2 AND source_instance_id=?3 AND capability='resolve_review_hold'",
        rusqlite::params![accepted.grant_revision, accepted.actor_id, accepted.source_instance_id],
    )
    .unwrap();

    assert_pending_result_authority_matrix(&fixture, &fixture.expected);

    // Revisions on a human outside the accepted lineage are immaterial.
    store
        .set_account_authority(
            &fixture.owner_session_digest,
            "owner",
            "owner",
            true,
            fixture.now,
        )
        .unwrap();
    result_store
        .finalize_product_review_result(&fixture.service, &fixture.expected)
        .expect("unrelated human authority changes remain allowed for initial finalization");
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 4);
    assert_eq!(fixture.restores.load(Ordering::SeqCst), 0);
    drop(db);
    drop(store);
    drop(result_store);
    std::fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn queued_success_is_not_a_final_result_and_does_not_change_s1() {
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: "queued-result-install".into(),
            database_id: "queued-result-db".into(),
        },
        "queued-result-source",
        "queued-result:order",
    )
    .with_preparation(foundry_ledger::P2ReviewPreparation::Queued);
    let mut fixture = genuine_host_done(identity);
    assert_eq!(fixture.terminal.as_ref().unwrap().exit_code, 0);
    assert!(fixture.terminal.as_ref().unwrap().held.is_none());
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 4);
    let mut store = fixture.result_store.take().unwrap();
    assert_eq!(
        store.finalize_product_review_result(&fixture.service, &fixture.expected),
        Err(foundry_ledger::ProductResultError::StaleBinding)
    );
    let rows: (String, i64, i64, i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT
               (SELECT to_state FROM ledger WHERE order_id=?1 ORDER BY seq DESC LIMIT 1),
               (SELECT count(*) FROM ledger WHERE order_id=?1),
               (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
               (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1),
               (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND status='complete')",
            [&fixture.identity.order_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .unwrap();
    assert_eq!(rows, ("queued".into(), 1, 0, 0, 4));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 4);
    assert_eq!(fixture.restores.load(Ordering::SeqCst), 0);
    drop(store);
    drop(fixture.host.take());
    std::fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn completed_initial_result_finalizes_after_worker_lease_expiry_without_renewal() {
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: "expired-worker-install".into(),
            database_id: "expired-worker-db".into(),
        },
        "expired-worker-source",
        "expired-worker-order",
    );
    let mut fixture = genuine_host_done(identity);
    let claim_before: (String, i64, i64, String, String, String) =
        Connection::open(&fixture.db_path)
            .unwrap()
            .query_row(
                "SELECT c.owner_id,c.epoch,c.fence,c.state,c.lease_until,b.state
             FROM product_work_claims_v1 c JOIN product_work_backlog_v1 b USING(order_id,lifecycle)
             WHERE c.order_id=?1 AND c.lifecycle=1",
                [&fixture.identity.order_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
    assert_eq!(claim_before.3, "completed");
    assert_eq!(claim_before.5, "completed");
    let mut expired = ProductStore::open_with_clock(
        &fixture.db_path,
        fixture.config.clone(),
        std::sync::Arc::new(FixtureClock(fixture.now + Duration::minutes(6))),
    )
    .unwrap();
    expired
        .configure_product_artifact_root(&fixture.artifact_root)
        .unwrap();
    let result = expired
        .finalize_product_review_result(&fixture.service, &fixture.expected)
        .unwrap();
    assert_eq!(result.run_id, 7);
    let claim_after: (String, i64, i64, String, String, String) =
        Connection::open(&fixture.db_path)
            .unwrap()
            .query_row(
                "SELECT c.owner_id,c.epoch,c.fence,c.state,c.lease_until,b.state
             FROM product_work_claims_v1 c JOIN product_work_backlog_v1 b USING(order_id,lifecycle)
             WHERE c.order_id=?1 AND c.lifecycle=1",
                [&fixture.identity.order_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
    assert_eq!(
        claim_after, claim_before,
        "finalization must not renew or rewrite the completed worker claim"
    );
    let receipt_before_replay: (String, String) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT payload_json,receipt_json FROM product_result_bindings_v1 WHERE order_id=?1",
            [&fixture.identity.order_id],
            |r| Ok((r.get(0)?,r.get(1)?)),
        )
        .unwrap();
    let replay = expired
        .finalize_product_review_result(&fixture.service, &fixture.expected)
        .unwrap();
    assert_eq!(replay, result, "same expired-worker condition replays the stored receipt");
    let claim_after_replay: (String, i64, i64, String, String, String) =
        Connection::open(&fixture.db_path)
            .unwrap()
            .query_row(
                "SELECT c.owner_id,c.epoch,c.fence,c.state,c.lease_until,b.state
                 FROM product_work_claims_v1 c JOIN product_work_backlog_v1 b USING(order_id,lifecycle)
                 WHERE c.order_id=?1 AND c.lifecycle=1",
                [&fixture.identity.order_id],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
            )
            .unwrap();
    assert_eq!(claim_after_replay, claim_before, "replay leaves expired claim and lease untouched");
    let receipt_after_replay: (String, String) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT payload_json,receipt_json FROM product_result_bindings_v1 WHERE order_id=?1",
            [&fixture.identity.order_id], |r| Ok((r.get(0)?,r.get(1)?)),
        ).unwrap();
    assert_eq!(receipt_after_replay, receipt_before_replay);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 4);
    assert_eq!(fixture.restores.load(Ordering::SeqCst), 0);
    let counts: (i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                    (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1)",
            [&fixture.identity.order_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(counts, (1, 1));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 4);
    assert_eq!(fixture.restores.load(Ordering::SeqCst), 0);
    drop(expired);
    drop(fixture.result_store.take());
    drop(fixture.host.take());
    std::fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn held_failed_and_materialized_continuation_are_not_final_results() {
    let order = "materialized-resuming-is-not-final";
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: format!("{order}-install"),
            database_id: format!("{order}-db"),
        },
        format!("{order}-source"),
        order,
    )
    .with_script(ReviewScript::ResumePass);
    let (mut fixture, accepted) = accepted_hold_with_identity(
        identity,
        order,
        ProductHoldDecision::Resume("replan_full".into()),
    );
    let original_host_results: i64 = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM product_review_host_results_v1 WHERE order_id=?1",
            [&order],
            |row| row.get(0),
        )
        .unwrap();
    let pause = fixture.stage_pause.clone();
    pause.arm("replan_full-g1");
    let mut host = fixture.host.take().unwrap();
    let session = fixture.human_session_digest.clone();
    let operation = accepted.operation_id.clone();
    let now = fixture.now;
    let worker = std::thread::spawn(move || {
        let result = host.continue_accepted_product_hold(&session, &operation, now);
        (host, result)
    });
    pause.wait_until_reached();
    let run = format!("{order}:1:8:3");
    let live: (String, i64, i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT r.status,
                    (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND run_id=?2 AND stage_id='replan_full-g1' AND status='accepted'),
                    (SELECT count(*) FROM product_review_authority_v1 WHERE order_id=?1 AND run_id=8),
                    (SELECT used FROM product_review_budget_v1 WHERE order_id=?1)
             FROM product_review_runs_v1 r WHERE r.order_id=?1 AND r.run_id=?2",
            rusqlite::params![order, run],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(live, ("resuming".into(), 1, 1, 5));
    let before: (i64, i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                    (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_host_results_v1 WHERE order_id=?1)",
            [&order],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((before.0, before.1), (0, 0));
    assert!(before.2 >= original_host_results);
    let followup = ProductResultExpectation {
        host_key: foundry_exec::HostKey {
            run: 8,
            ..fixture.expected.host_key.clone()
        },
        host_result_digest: fixture.expected.host_result_digest.clone(),
    };
    assert_eq!(
        fixture
            .result_store
            .as_mut()
            .unwrap()
            .finalize_product_review_result(&fixture.service, &followup),
        Err(foundry_ledger::ProductResultError::Ineligible),
        "valid service authority does not turn a genuinely materialized resuming run into a final result"
    );
    let after: (i64, i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                    (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_host_results_v1 WHERE order_id=?1)",
            [&order],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(after, before);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 5);
    assert_eq!(fixture.restores.load(Ordering::SeqCst), 0);
    pause.release();
    let (host, continuation) = worker.join().unwrap();
    assert!(continuation.is_ok());
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 14);
    assert_eq!(fixture.restores.load(Ordering::SeqCst), 3);
    assert_eq!(host.dispatch_count(), 1);
    drop(host);
    drop(fixture.result_store.take());
    std::fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn pending_result_rejects_service_grant_and_credential_rotation_without_rewriting_history() {
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: "rotated-credential-install".into(),
            database_id: "rotated-credential-db".into(),
        },
        "rotated-credential-source",
        "rotated-credential-order",
    );
    let mut fixture = genuine_host_done(identity);
    let original_claim: (String, i64, i64, String, String) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT owner_id,epoch,fence,state,lease_until FROM product_work_claims_v1
             WHERE order_id=?1 AND lifecycle=1",
            [&fixture.identity.order_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    let owner_mutation = Connection::open(&fixture.db_path).unwrap();
    owner_mutation
        .execute(
            "UPDATE product_work_claims_v1
             SET owner_id='replacement-worker',epoch=epoch+1,fence=fence+1
             WHERE order_id=?1 AND lifecycle=1",
            [&fixture.identity.order_id],
        )
        .unwrap();
    assert_eq!(
        fixture
            .result_store
            .as_mut()
            .unwrap()
            .finalize_product_review_result(&fixture.service, &fixture.expected),
        Err(foundry_ledger::ProductResultError::StaleBinding),
        "a completed result cannot be finalized after worker owner reassignment"
    );
    owner_mutation
        .execute(
            "UPDATE product_work_claims_v1 SET owner_id=?1,epoch=?2,fence=?3,state=?4,lease_until=?5
             WHERE order_id=?6 AND lifecycle=1",
            rusqlite::params![
                original_claim.0,
                original_claim.1,
                original_claim.2,
                original_claim.3,
                original_claim.4,
                fixture.identity.order_id
            ],
        )
        .unwrap();
    owner_mutation
        .execute(
            "UPDATE product_work_claims_v1 SET epoch=epoch+1,fence=fence+1
             WHERE order_id=?1 AND lifecycle=1",
            [&fixture.identity.order_id],
        )
        .unwrap();
    assert_eq!(
        fixture
            .result_store
            .as_mut()
            .unwrap()
            .finalize_product_review_result(&fixture.service, &fixture.expected),
        Err(foundry_ledger::ProductResultError::StaleBinding),
        "same-owner ABA with a newer epoch/fence cannot finalize the old result"
    );
    owner_mutation
        .execute(
            "UPDATE product_work_claims_v1 SET owner_id=?1,epoch=?2,fence=?3,state=?4,lease_until=?5
             WHERE order_id=?6 AND lifecycle=1",
            rusqlite::params![
                original_claim.0,
                original_claim.1,
                original_claim.2,
                original_claim.3,
                original_claim.4,
                fixture.identity.order_id
            ],
        )
        .unwrap();
    drop(owner_mutation);
    let mut owner_store = ProductStore::open(&fixture.db_path, fixture.config.clone()).unwrap();
    let service_revision: i64 = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT revision FROM product_services_v1 WHERE service_id=?1",
            [&fixture.identity.service_id],
            |row| row.get(0),
        )
        .unwrap();
    let service_grants = [foundry_ledger::ProductServiceGrant {
        source_instance_id: fixture.identity.source_instance_id.clone(),
        action: foundry_ledger::ProductServiceAction::DriveReview,
    }];
    let configured = owner_store
        .manage_product_service(
            &fixture.owner_session_digest,
            &fixture.identity.service_id,
            &service_grants,
            true,
            fixture.now + Duration::hours(2),
            service_revision,
            "rotate-service-grant-request",
            fixture.now,
        )
        .unwrap();
    let replacement = owner_store
        .issue_product_service_credential(
            &fixture.owner_session_digest,
            &fixture.identity.service_id,
            configured.revision,
            fixture.now + Duration::hours(1),
            "rotated-credential-request",
            fixture.now,
        )
        .unwrap();
    let replacement_token = replacement.expose_once_to_file_writer().to_owned();
    let current_service = owner_store
        .authenticate_product_service_for_action(
            &replacement_token,
            &fixture.identity.connector_id,
            &fixture.identity.api_origin,
            &fixture.identity.repository_id,
            foundry_ledger::ProductServiceAction::DriveReview,
            fixture.now,
        )
        .unwrap();
    let revisions: (i64, i64, i64, i64, i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT c.credential_revision,a.credential_revision,
                    s.revision,a.service_revision,g.grant_revision,
                    a.drive_review_grant_revision
             FROM product_service_credentials_v1 c
             JOIN product_services_v1 s USING(service_id)
             JOIN product_service_grants_v1 g
               ON g.service_id=c.service_id AND g.source_instance_id=?2
                  AND g.action='drive_review' AND g.enabled=1
             JOIN product_review_authority_v1 a ON a.service_id=c.service_id
             WHERE c.service_id=?1 AND a.order_id=?3 AND a.run_id=7",
            rusqlite::params![
                fixture.identity.service_id,
                fixture.identity.source_instance_id,
                fixture.identity.order_id
            ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_ne!(revisions.0, revisions.1);
    assert_ne!(revisions.2, revisions.3);
    assert_ne!(revisions.4, revisions.5);
    assert_eq!(
        owner_store.finalize_product_review_result(&current_service, &fixture.expected),
        Err(foundry_ledger::ProductResultError::StaleBinding)
    );
    // No finalized binding exists in this pending-result fixture, so lookup is
    // intentionally covered by the completed-result rotation fixture below.
    let counts: (i64, i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                    (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND status='complete')",
            [&fixture.identity.order_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(counts, (0, 0, 4));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 4);
    assert_eq!(fixture.restores.load(Ordering::SeqCst), 0);
    drop(owner_store);
    drop(fixture.result_store.take());
    drop(fixture.host.take());
    std::fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn pending_result_rejects_source_rebind_without_writing_history() {
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: "rebound-source-install".into(),
            database_id: "rebound-source-db".into(),
        },
        "rebound-source-instance",
        "rebound-source-order",
    );
    let mut fixture = genuine_host_done(identity);
    let mut owner_store = ProductStore::open(&fixture.db_path, fixture.config.clone()).unwrap();
    let current = owner_store
        .list_scoped_sources(&fixture.owner_session_digest, fixture.now)
        .unwrap()
        .into_iter()
        .find(|view| view.current)
        .expect("fixture source is current");
    let source = current.source;
    let rebound = owner_store
        .configure_scoped_source(
            &fixture.owner_session_digest,
            &source.connector_id,
            source.identity.clone(),
            source.api_owner.as_deref().unwrap(),
            source.api_repo.as_deref().unwrap(),
            &source.legacy_repository_locator,
            &format!("{} and updated:>=2000-01-01", source.scope_query),
            true,
            current.connector_revision,
            "rebind-after-result-pending",
            fixture.now,
        )
        .unwrap();
    assert_ne!(
        rebound.source_instance_id,
        fixture.identity.source_instance_id
    );
    assert_eq!(
        fixture
            .result_store
            .as_mut()
            .unwrap()
            .finalize_product_review_result(&fixture.service, &fixture.expected),
        Err(foundry_ledger::ProductResultError::Unauthorized),
        "a service context bound to a retired source cannot disclose or finalize its pending result"
    );
    assert_eq!(
        fixture.result_store.as_mut().unwrap()
            .lookup_product_review_result(&fixture.service, &fixture.expected),
        Err(foundry_ledger::ProductResultError::Unauthorized),
        "pending-result lookup denies the retired source context"
    );
    let unchanged: (i64, i64, i64, i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                    (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND status='complete'),
                    (SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM ledger WHERE order_id=?1)",
            [&fixture.identity.order_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .unwrap();
    assert_eq!(unchanged, (0, 0, 4, 3, 4));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 4);
    assert_eq!(fixture.restores.load(Ordering::SeqCst), 0);
    drop(owner_store);
    drop(fixture.result_store.take());
    drop(fixture.host.take());
    std::fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn lookup_denies_wrong_source_before_inspecting_a_corrupt_winner() {
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: "wrong-source-lookup-install".into(),
            database_id: "wrong-source-lookup-db".into(),
        },
        "wrong-source-lookup-source",
        "wrong-source-lookup-order",
    );
    let mut fixture = genuine_host_done(identity);
    let result = fixture
        .result_store
        .as_mut()
        .unwrap()
        .finalize_product_review_result(&fixture.service, &fixture.expected)
        .unwrap();

    let mut owner = ProductStore::open(&fixture.db_path, fixture.config.clone()).unwrap();
    let other_source = owner
        .configure_scoped_source(
            &fixture.owner_session_digest,
            "github-other",
            foundry_ledger::RepositoryIdentity::canonicalized(
                "acme",
                "github",
                "https://api.github.com",
                "repo:other",
            )
            .unwrap(),
            "acme",
            "other",
            "repo:other",
            "open+foundry:make",
            true,
            0,
            "wrong-source-configure",
            fixture.now,
        )
        .unwrap();
    let service_revision: i64 = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT revision FROM product_services_v1 WHERE service_id=?1",
            [&fixture.identity.service_id],
            |row| row.get(0),
        )
        .unwrap();
    let mut grants = Vec::new();
    for source_instance_id in [
        fixture.identity.source_instance_id.as_str(),
        other_source.source_instance_id.as_str(),
    ] {
        grants.extend(
            [
                foundry_ledger::ProductServiceAction::IntakeRead,
                foundry_ledger::ProductServiceAction::AdmitDelivery,
                foundry_ledger::ProductServiceAction::AckDelivery,
                foundry_ledger::ProductServiceAction::DriveReview,
            ]
            .into_iter()
            .map(|action| foundry_ledger::ProductServiceGrant {
                source_instance_id: source_instance_id.to_owned(),
                action,
            }),
        );
    }
    owner
        .manage_product_service(
            &fixture.owner_session_digest,
            &fixture.identity.service_id,
            &grants,
            true,
            fixture.now + Duration::hours(2),
            service_revision,
            "wrong-source-service-revision",
            fixture.now,
        )
        .unwrap();
    let revised_service = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT revision FROM product_services_v1 WHERE service_id=?1",
            [&fixture.identity.service_id],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    let replacement = owner
        .issue_product_service_credential(
            &fixture.owner_session_digest,
            &fixture.identity.service_id,
            revised_service,
            fixture.now + Duration::hours(1),
            "wrong-source-revision-credential",
            fixture.now,
        )
        .unwrap();
    let replacement_token = replacement.expose_once_to_file_writer().to_owned();
    let wrong_source = owner
        .authenticate_product_service_for_action(
            &replacement_token,
            "github-other",
            "https://api.github.com",
            "repo:other",
            foundry_ledger::ProductServiceAction::DriveReview,
            fixture.now,
        )
        .unwrap();
    let current_original_source = owner
        .authenticate_product_service_for_action(
            &replacement_token,
            &fixture.identity.connector_id,
            &fixture.identity.api_origin,
            &fixture.identity.repository_id,
            foundry_ledger::ProductServiceAction::DriveReview,
            fixture.now,
        )
        .unwrap();
    let lookup = fixture
        .result_store
        .as_mut()
        .unwrap()
        .lookup_product_review_result(&wrong_source, &fixture.expected);
    assert_eq!(
        lookup,
        Err(foundry_ledger::ProductResultError::Unauthorized)
    );

    let corrupt = Connection::open(&fixture.db_path).unwrap();
    corrupt
        .execute_batch(
            "DROP TRIGGER product_result_bindings_no_update;
             UPDATE product_result_bindings_v1 SET receipt_json=receipt_json || ' '
             WHERE result_id=(SELECT result_id FROM product_result_bindings_v1 WHERE order_id='wrong-source-lookup-order');",
        )
        .unwrap();
    assert_eq!(
        fixture
            .result_store
            .as_mut()
            .unwrap()
            .lookup_product_review_result(&wrong_source, &fixture.expected),
        Err(foundry_ledger::ProductResultError::Unauthorized),
        "the original-source grant denial must precede corrupt stored-winner inspection"
    );
    let current_source_lookup = fixture
        .result_store
        .as_mut()
        .unwrap()
        .lookup_product_review_result(&current_original_source, &fixture.expected);
    assert_eq!(
        current_source_lookup,
        Err(foundry_ledger::ProductResultError::CorruptEvidence),
        "the same stored winner is classified as corrupt for an authorized original-source caller"
    );
    assert_eq!(result.order_id, fixture.identity.order_id);
    drop(corrupt);
    drop(owner);
    drop(fixture.result_store.take());
    drop(fixture.host.take());
    std::fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn isolated_service_revision_and_credential_rotations_reject_the_captured_initial_authority() {
    for rotate_credential in [false, true] {
        let label = if rotate_credential {
            "credential"
        } else {
            "service"
        };
        let identity = FixtureIdentity::new(
            ProductConfig {
                installation_id: format!("isolated-{label}-rotation-install"),
                database_id: format!("isolated-{label}-rotation-db"),
            },
            format!("isolated-{label}-rotation-source"),
            format!("isolated-{label}-rotation-order"),
        );
        let mut fixture = genuine_host_done(identity);
        let mut owner = ProductStore::open(&fixture.db_path, fixture.config.clone()).unwrap();
        let current_context = if rotate_credential {
            let service = owner
                .issue_product_service_credential(
                    &fixture.owner_session_digest,
                    &fixture.identity.service_id,
                    1,
                    fixture.now + Duration::hours(1),
                    "isolated-credential-rotation",
                    fixture.now,
                )
                .unwrap()
                .expose_once_to_file_writer()
                .to_owned();
            owner
                .authenticate_product_service_for_action(
                    &service,
                    &fixture.identity.connector_id,
                    &fixture.identity.api_origin,
                    &fixture.identity.repository_id,
                    foundry_ledger::ProductServiceAction::DriveReview,
                    fixture.now,
                )
                .unwrap()
        } else {
            let grants = [
                foundry_ledger::ProductServiceAction::IntakeRead,
                foundry_ledger::ProductServiceAction::AdmitDelivery,
                foundry_ledger::ProductServiceAction::AckDelivery,
                foundry_ledger::ProductServiceAction::DriveReview,
            ]
            .into_iter()
            .map(|action| foundry_ledger::ProductServiceGrant {
                source_instance_id: fixture.identity.source_instance_id.clone(),
                action,
            })
            .collect::<Vec<_>>();
            owner
                .manage_product_service(
                    &fixture.owner_session_digest,
                    &fixture.identity.service_id,
                    &grants,
                    true,
                    fixture.now + Duration::hours(2),
                    1,
                    "isolated-service-rotation",
                    fixture.now,
                )
                .unwrap();
            fixture.service.clone()
        };
        let expected_error = if rotate_credential {
            foundry_ledger::ProductResultError::StaleBinding
        } else {
            foundry_ledger::ProductResultError::Unauthorized
        };
        assert_eq!(
            owner.finalize_product_review_result(&current_context, &fixture.expected),
            Err(expected_error),
            "an isolated {label} rotation must invalidate the activation's captured authority"
        );
        let writes: (i64, i64) = Connection::open(&fixture.db_path)
            .unwrap()
            .query_row(
                "SELECT (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                        (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1)",
                [&fixture.identity.order_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(writes, (0, 0));
        drop(owner);
        drop(fixture.result_store.take());
        drop(fixture.host.take());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }
}

#[test]
fn disabled_then_reenabled_service_rejects_its_old_captured_context() {
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: "disable-reenable-context-install".into(),
            database_id: "disable-reenable-context-db".into(),
        },
        "disable-reenable-context-source",
        "disable-reenable-context-order",
    );
    let mut fixture = genuine_host_done(identity);
    let mut owner = ProductStore::open(&fixture.db_path, fixture.config.clone()).unwrap();
    let grants = [
        foundry_ledger::ProductServiceAction::IntakeRead,
        foundry_ledger::ProductServiceAction::AdmitDelivery,
        foundry_ledger::ProductServiceAction::AckDelivery,
        foundry_ledger::ProductServiceAction::DriveReview,
    ]
    .into_iter()
    .map(|action| foundry_ledger::ProductServiceGrant {
        source_instance_id: fixture.identity.source_instance_id.clone(),
        action,
    })
    .collect::<Vec<_>>();
    let disabled = owner
        .manage_product_service(
            &fixture.owner_session_digest,
            &fixture.identity.service_id,
            &[],
            false,
            fixture.now + Duration::hours(2),
            1,
            "disable-before-reenable",
            fixture.now,
        )
        .unwrap();
    owner
        .manage_product_service(
            &fixture.owner_session_digest,
            &fixture.identity.service_id,
            &grants,
            true,
            fixture.now + Duration::hours(2),
            disabled.revision,
            "reenable-with-new-revision",
            fixture.now,
        )
        .unwrap();
    assert_eq!(
        owner.finalize_product_review_result(&fixture.service, &fixture.expected),
        Err(foundry_ledger::ProductResultError::Unauthorized),
        "an old context cannot become valid again when the service is re-enabled"
    );
    let unchanged: (i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                    (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1)",
            [&fixture.identity.order_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(unchanged, (0, 0));
    drop(owner);
    drop(fixture.result_store.take());
    drop(fixture.host.take());
    std::fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn redrive_charge_rejects_wrong_order_and_stale_run_without_durable_changes() {
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: "redrive-charge-negative-install".into(),
            database_id: "redrive-charge-negative-db".into(),
        },
        "redrive-charge-negative-source",
        "redrive-charge-negative-order",
    );
    let mut fixture = genuine_host_inflight(identity);
    let worker = spawn_initial_dispatch(&mut fixture);
    fixture.stage_pause.wait_until_reached();
    let before: (i64, i64, i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT used FROM product_review_budget_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_artifacts_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?1)",
            [&fixture.identity.order_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    let mut store = ProductStore::open(&fixture.db_path, fixture.config.clone()).unwrap();
    store
        .configure_review_backend(foundry_ledger::ProductReviewConfig {
            artifact_root: fixture.artifact_root.clone(),
            session_root: fixture.session_root.clone(),
            claim_root: fixture.claim_root.clone(),
            pack_digest: "e925fd014d367c4c7f3d83fe92d03cd4bc4d3b7cbc1ecdf0d4e77320acab1db8".into(),
        })
        .unwrap();
    assert_eq!(
        ReviewHostStore::charge_budget(&mut store, "different-order", &fixture.intent),
        Err(foundry_exec::HostError::Acceptance(
            "redrive budget order does not match execution intent".into()
        )),
        "wrong-order charge returns its exact acceptance error"
    );
    let mut stale_run = fixture.intent.clone();
    stale_run.run = foundry_exec::RunId(fixture.intent.run.0 + 1);
    assert_eq!(
        ReviewHostStore::charge_budget(&mut store, &fixture.identity.order_id, &stale_run),
        Err(foundry_exec::HostError::Worker("inactive product review run".into())),
        "stale-run charge returns its exact inactive-run error while a different run is actively executing"
    );
    let after: (i64, i64, i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT used FROM product_review_budget_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_artifacts_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?1)",
            [&fixture.identity.order_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        after, before,
        "rejected charges leave all durable work evidence unchanged"
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.restores.load(Ordering::SeqCst), 0);
    drop(store);
    fixture.stage_pause.release();
    let (host, outcome) = worker.join().unwrap();
    assert!(outcome.is_ok(), "active-running control completes after the stale-run probe");
    drop(host);
    drop(fixture.result_store.take());
    drop(fixture.host.take());
    std::fs::remove_dir_all(fixture.root).unwrap();
}

fn accepted_resume(order: &str) -> (product_result_fixture::GenuineResultFixture, foundry_ledger::ProductHoldAcceptance) {
    accepted_hold(order, ReviewScript::ResumePass, ProductHoldDecision::Resume("replan_full".into()))
}

fn completed_pending_resume_fixture(
    script: ReviewScript,
    order: &str,
) -> (product_result_fixture::GenuineResultFixture, ProductResultExpectation) {
    let (mut fixture, first) = accepted_hold(
        order,
        script,
        ProductHoldDecision::Resume("replan_full".into()),
    );
    let mut host = fixture.host.take().unwrap();
    host.continue_accepted_product_hold(
        &fixture.human_session_digest,
        &first.operation_id,
        fixture.now,
    )
    .unwrap();
    if script == ReviewScript::ResumeCommitPass {
        let mut store = ProductStore::open(&fixture.db_path, fixture.config.clone()).unwrap();
        let second = store
            .list_product_review_holds(&fixture.human_session_digest, fixture.now)
            .unwrap()
            .into_iter()
            .find(|hold| hold.order_id == order)
            .expect("B completed-pending baseline reaches its second hold");
        let receipt = store
            .accept_product_review_hold(
                &fixture.human_session_digest,
                order,
                &second.hold_instance_id,
                &second.expectation,
                &format!("{order}-g19-hold-2"),
                ProductHoldDecision::CommitPass,
                fixture.now,
            )
            .unwrap();
        host.continue_accepted_product_hold(
            &fixture.human_session_digest,
            &receipt.operation_id,
            fixture.now,
        )
        .unwrap();
    }
    drop(host);
    let run = 8;
    let terminal: foundry_exec::HostResult = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT result_json FROM product_review_host_results_v1
             WHERE order_id=?1 AND attempt=1 AND run_id=?2 AND activation=3",
            rusqlite::params![order, run],
            |row| {
                serde_json::from_str(&row.get::<_, String>(0)?).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })
            },
        )
        .unwrap();
    let expected = ProductResultExpectation {
        host_key: foundry_exec::HostKey {
            order: order.into(),
            attempt: 1,
            run,
            activation: 3,
        },
        host_result_digest: foundry_ledger::product_host_result_digest(&terminal).unwrap(),
    };
    (fixture, expected)
}

fn spawn_initial_dispatch(
    fixture: &mut product_result_fixture::GenuineResultFixture,
) -> std::thread::JoinHandle<(
    foundry_line::PackReviewHost,
    Result<foundry_exec::HostResult, foundry_exec::HostError>,
)> {
    fixture.stage_pause.arm("*");
    let mut host = fixture.host.take().unwrap();
    let intent = fixture.intent.clone();
    let claim = fixture.claim.clone();
    std::thread::spawn(move || {
        let result = host.try_dispatch_product_work(&intent, &claim);
        (host, result)
    })
}

fn accepted_hold(
    order: &str,
    script: ReviewScript,
    decision: ProductHoldDecision,
) -> (product_result_fixture::GenuineResultFixture, foundry_ledger::ProductHoldAcceptance) {
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: format!("{order}-pause-install"),
            database_id: format!("{order}-pause-db"),
        },
        format!("{order}-pause-source"),
        order,
    )
    .with_script(script);
    accepted_hold_with_identity(identity, order, decision)
}

fn accepted_hold_with_identity(
    identity: FixtureIdentity,
    order: &str,
    decision: ProductHoldDecision,
) -> (product_result_fixture::GenuineResultFixture, foundry_ledger::ProductHoldAcceptance) {
    let fixture = genuine_host_done(identity);
    grant_review_hold_scope(&fixture);
    let mut store = ProductStore::open(&fixture.db_path, fixture.config.clone()).unwrap();
    let hold = store
        .list_product_review_holds(&fixture.human_session_digest, fixture.now)
        .unwrap()
        .into_iter()
        .find(|hold| hold.order_id == order)
        .expect("the genuine initial run created a listed hold");
    let accepted = store
        .accept_product_review_hold(
            &fixture.human_session_digest,
            order,
            &hold.hold_instance_id,
            &hold.expectation,
            &format!("{order}-pause-accept"),
            decision,
            fixture.now,
        )
        .unwrap();
    (fixture, accepted)
}

fn configured_charge_store(
    fixture: &product_result_fixture::GenuineResultFixture,
    clock: std::sync::Arc<dyn foundry_ledger::ProductClock>,
) -> ProductStore {
    let mut store =
        ProductStore::open_with_clock(&fixture.db_path, fixture.config.clone(), clock).unwrap();
    store
        .configure_review_backend(foundry_ledger::ProductReviewConfig {
            artifact_root: fixture.artifact_root.clone(),
            session_root: fixture.session_root.clone(),
            claim_root: fixture.claim_root.clone(),
            pack_digest: "e925fd014d367c4c7f3d83fe92d03cd4bc4d3b7cbc1ecdf0d4e77320acab1db8".into(),
        })
        .unwrap();
    store
}

fn charge_durable_state(
    fixture: &product_result_fixture::GenuineResultFixture,
    run: &str,
) -> (String, String, i64, i64, i64, i64, i64) {
    Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT status FROM product_review_runs_v1 WHERE order_id=?1 AND run_id=?2),
                    (SELECT journal_run FROM product_review_active_v1 WHERE order_id=?1),
                    (SELECT used FROM product_review_budget_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND run_id=?2 AND status='accepted'),
                    (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND run_id=?2 AND status='complete'),
                    (SELECT count(*) FROM product_artifacts_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?1)",
            rusqlite::params![fixture.identity.order_id, run],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .unwrap()
}

fn disable_fixture_service(fixture: &product_result_fixture::GenuineResultFixture) {
    let mut owner = ProductStore::open(&fixture.db_path, fixture.config.clone()).unwrap();
    let revision: i64 = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT revision FROM product_services_v1 WHERE service_id=?1",
            [&fixture.identity.service_id],
            |row| row.get(0),
        )
        .unwrap();
    owner
        .manage_product_service(
            &fixture.owner_session_digest,
            &fixture.identity.service_id,
            &[],
            false,
            fixture.now + Duration::hours(2),
            revision,
            "pause-revoke-service",
            fixture.now,
        )
        .unwrap();
}

#[test]
fn charge_budget_rejects_revoked_authority_for_the_active_running_run() {
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: "charge-revoked-mid-run-install".into(),
            database_id: "charge-revoked-mid-run-db".into(),
        },
        "charge-revoked-mid-run-source",
        "charge-revoked-mid-run-order",
    );
    let mut fixture = genuine_host_inflight(identity);
    let worker = spawn_initial_dispatch(&mut fixture);
    let pause = fixture.stage_pause.clone();
    pause.wait_until_reached();
    let active: (String, String, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT status FROM product_review_runs_v1 WHERE order_id=?1 AND run_id=?2),
                    (SELECT state FROM product_work_backlog_v1 WHERE order_id=?1 AND lifecycle=1),
                    (SELECT used FROM product_review_budget_v1 WHERE order_id=?1)",
            rusqlite::params![
                fixture.identity.order_id,
                format!("{}:1:7:3", fixture.identity.order_id)
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(active.0, "running");
    assert_eq!(active.1, "claimed");
    disable_fixture_service(&fixture);
    let run = format!("{}:1:7:3", fixture.identity.order_id);
    let before_charge = charge_durable_state(&fixture, &run);
    assert_eq!(before_charge.0, "running");
    assert_eq!(before_charge.1, run);
    let mut charge_store =
        configured_charge_store(&fixture, std::sync::Arc::new(FixtureClock(fixture.now)));
    let charge = ReviewHostStore::charge_budget(
        &mut charge_store,
        &fixture.identity.order_id,
        &fixture.intent,
    );
    assert_eq!(
        charge,
        Err(foundry_exec::HostError::Acceptance(
            "review service authority is absent or expired".into()
        )),
        "charge_budget returns the exact revoked-service error"
    );
    assert_eq!(charge_durable_state(&fixture, &run), before_charge);
    pause.release();
    let (host, outcome) = worker.join().unwrap();
    assert!(
        outcome.is_err(),
        "the genuine initial flow observes revoked authority"
    );
    let durable: (String, i64, i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT status FROM product_review_runs_v1 WHERE order_id=?1 AND run_id=?2),
                    (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND run_id=?2 AND status='complete'),
                    (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND run_id=?2 AND status='accepted'),
                    (SELECT used FROM product_review_budget_v1 WHERE order_id=?1)",
            rusqlite::params![fixture.identity.order_id, run],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(durable.1, 1, "the already accepted stage is preserved");
    assert_eq!(
        durable.2, 0,
        "revocation prevents a later stage from starting"
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    drop(host);
    drop(fixture.result_store.take());
    std::fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn charge_budget_rejects_exhausted_budget_without_charging() {
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: "charge-exhausted-mid-run-install".into(),
            database_id: "charge-exhausted-mid-run-db".into(),
        },
        "charge-exhausted-mid-run-source",
        "charge-exhausted-mid-run-order",
    );
    let mut fixture = genuine_host_inflight(identity);
    let worker = spawn_initial_dispatch(&mut fixture);
    let pause = fixture.stage_pause.clone();
    pause.wait_until_reached();
    let db = Connection::open(&fixture.db_path).unwrap();
    db.execute(
        "UPDATE product_review_budget_v1 SET used=10000 WHERE order_id=?1",
        [&fixture.identity.order_id],
    )
    .unwrap();
    let run = format!("{}:1:7:3", fixture.identity.order_id);
    let before_charge = charge_durable_state(&fixture, &run);
    assert_eq!(before_charge.0, "running");
    assert_eq!(before_charge.1, run);
    let mut charge_store =
        configured_charge_store(&fixture, std::sync::Arc::new(FixtureClock(fixture.now)));
    let charge = ReviewHostStore::charge_budget(
        &mut charge_store,
        &fixture.identity.order_id,
        &fixture.intent,
    );
    assert_eq!(
        charge,
        Err(foundry_exec::HostError::Worker(
            "product review budget exhausted or corrupt".into()
        )),
        "charge_budget returns the exact exhausted-budget error"
    );
    assert_eq!(charge_durable_state(&fixture, &run), before_charge);
    drop(charge_store);
    pause.release();
    let (host, outcome) = worker.join().unwrap();
    assert!(
        outcome.is_err(),
        "an exhausted budget blocks the next stage"
    );
    let state: (i64, i64, i64) = db
        .query_row(
            "SELECT used,
                    (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND run_id=?2 AND status='complete'),
                    (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND run_id=?2 AND status='accepted')
             FROM product_review_budget_v1 WHERE order_id=?1",
            rusqlite::params![fixture.identity.order_id, run],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(state, (10000, 1, 0));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    drop(db);
    drop(host);
    drop(fixture.result_store.take());
    std::fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn charge_budget_sql_abort_rolls_back_the_budget_write() {
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: "mid-charge-failure-install".into(),
            database_id: "mid-charge-failure-db".into(),
        },
        "mid-charge-failure-source",
        "mid-charge-failure-order",
    );
    let mut fixture = genuine_host_inflight(identity);
    let worker = spawn_initial_dispatch(&mut fixture);
    let pause = fixture.stage_pause.clone();
    pause.wait_until_reached();
    let run = format!("{}:1:7:3", fixture.identity.order_id);
    let before = charge_durable_state(&fixture, &run);
    assert_eq!(
        before.0, "running",
        "charge probe holds the current active run"
    );
    assert_eq!(before.1, run);
    let conn = Connection::open(&fixture.db_path).unwrap();
    conn.execute_batch(&format!(
        "CREATE TRIGGER fail_charge_budget_update BEFORE UPDATE ON product_review_budget_v1
         WHEN OLD.order_id='{}' AND NEW.used=OLD.used+1
         BEGIN SELECT RAISE(ABORT,'injected charge_budget SQL failure'); END;",
        fixture.identity.order_id.replace('\'', "''")
    ))
    .unwrap();
    let mut charge_store =
        configured_charge_store(&fixture, std::sync::Arc::new(FixtureClock(fixture.now)));
    let charge = ReviewHostStore::charge_budget(
        &mut charge_store,
        &fixture.identity.order_id,
        &fixture.intent,
    );
    let injected = charge.expect_err("the BEFORE UPDATE trigger must abort charge_budget");
    assert!(
        matches!(&injected, foundry_exec::HostError::Persistence(message) if message.contains("injected charge_budget SQL failure")),
        "the observed persistence error is the injected BEFORE UPDATE failure: {injected:?}"
    );
    assert_eq!(charge_durable_state(&fixture, &run), before);
    conn.execute_batch("DROP TRIGGER fail_charge_budget_update;")
        .unwrap();
    conn.execute_batch(&format!(
        "CREATE TRIGGER fail_charge_budget_after_write AFTER UPDATE ON product_review_budget_v1
         WHEN OLD.order_id='{}' AND NEW.used=OLD.used+1
         BEGIN SELECT RAISE(ABORT,'injected post-write charge_budget SQL failure'); END;",
        fixture.identity.order_id.replace('\'', "''")
    ))
    .unwrap();
    let post_write_charge = ReviewHostStore::charge_budget(
        &mut charge_store,
        &fixture.identity.order_id,
        &fixture.intent,
    );
    let post_write_error = post_write_charge
        .expect_err("the AFTER UPDATE trigger must abort after the budget row was written");
    assert!(
        matches!(&post_write_error, foundry_exec::HostError::Persistence(message) if message.contains("injected post-write charge_budget SQL failure")),
        "the observed error is raised only after the budget write: {post_write_error:?}"
    );
    assert_eq!(
        charge_durable_state(&fixture, &run),
        before,
        "the transaction rolls back the budget row even when SQL fails after UPDATE"
    );
    conn.execute_batch("DROP TRIGGER fail_charge_budget_after_write;")
        .unwrap();
    drop(charge_store);
    pause.release();
    let (host, outcome) = worker.join().unwrap();
    assert!(
        outcome.is_ok(),
        "the genuine run completes after the scoped trigger is removed"
    );
    drop(host);
    drop(fixture.result_store.take());
    std::fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn charge_budget_samples_clock_after_waiting_for_begin_immediate() {
    fn probe(order: &str, advance_clock: bool) {
        let identity = FixtureIdentity::new(
            ProductConfig {
                installation_id: format!("{order}-install"),
                database_id: format!("{order}-db"),
            },
            format!("{order}-source"),
            order,
        )
        .with_service_credential_minutes(2);
        let mut fixture = genuine_host_inflight(identity);
        let worker = spawn_initial_dispatch(&mut fixture);
        let pause = fixture.stage_pause.clone();
        pause.wait_until_reached();
        let run = format!("{}:1:7:3", fixture.identity.order_id);
        let before = charge_durable_state(&fixture, &run);
        assert_eq!(
            before.0, "running",
            "the probe reaches an active running run"
        );
        assert_eq!(
            before.1, run,
            "the active binding matches the passed intent"
        );
        let credential_expiry: String = Connection::open(&fixture.db_path)
            .unwrap()
            .query_row(
                "SELECT expires_at FROM product_service_credentials_v1 WHERE service_id=?1",
                [&fixture.identity.service_id],
                |row| row.get(0),
            )
            .unwrap();
        let credential_expiry = chrono::DateTime::parse_from_rfc3339(&credential_expiry)
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert!(fixture.now < credential_expiry);
        assert!(fixture.now + Duration::minutes(3) >= credential_expiry);
        assert!(
            fixture.now + Duration::minutes(3)
                < fixture.claim.lease_until(),
            "clock advance expires the service credential while the worker claim remains live"
        );
        let clock = AdjustableFixtureClock::new(fixture.now);
        let mut charge_store =
            configured_charge_store(&fixture, std::sync::Arc::new(clock.clone()));
        let (observed_tx, observed_rx) = std::sync::mpsc::channel();
        charge_store.set_charge_budget_observer(Some(std::sync::Arc::new(move |event| {
            observed_tx.send(event).unwrap();
        })));
        let lock = Connection::open(&fixture.db_path).unwrap();
        lock.execute_batch("BEGIN IMMEDIATE;").unwrap();
        let order = fixture.identity.order_id.clone();
        let intent = fixture.intent.clone();
        let charge_thread = std::thread::spawn(move || {
            ReviewHostStore::charge_budget(&mut charge_store, &order, &intent)
        });
        assert_eq!(
            observed_rx.recv().unwrap(),
            foundry_ledger::ChargeBudgetObservation::BeginAttempt,
            "the observer fires immediately before charge_budget attempts BEGIN IMMEDIATE"
        );
        let busy = observed_rx.recv().unwrap();
        assert!(
            matches!(busy, foundry_ledger::ChargeBudgetObservation::Busy { attempts } if attempts >= 0),
            "the busy handler proves the BEGIN attempt reached SQLite contention: {busy:?}"
        );
        if advance_clock {
            clock.set(fixture.now + Duration::minutes(3));
        }
        lock.execute_batch("COMMIT;").unwrap();
        let charge = charge_thread.join().unwrap();
        let after = charge_durable_state(&fixture, &run);
        if advance_clock {
            assert!(
                charge.is_err(),
                "credentials expired while BEGIN IMMEDIATE waited"
            );
            assert_eq!(
                after, before,
                "expiry rejection leaves the budget and run unchanged"
            );
        } else {
            charge.expect("the no-clock-advance control proves identity and authority checks pass");
            assert_eq!(
                after.2,
                before.2 + 1,
                "the valid control charges exactly once"
            );
            assert_eq!(
                (after.0, after.1, after.3, after.4, after.5, after.6),
                (before.0, before.1, before.3, before.4, before.5, before.6)
            );
        }
        drop(observed_rx);
        pause.release();
        let (host, _continuation) = worker.join().unwrap();
        drop(host);
        drop(fixture.result_store.take());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    probe("charge-expiry-lock-wait-control", false);
    probe("charge-expiry-lock-wait-expired", true);
}

#[test]
fn continuation_run8_resuming_charge_rejects_expired_service_credential() {
    let order = "continuation-expired-run8";
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: format!("{order}-install"),
            database_id: format!("{order}-db"),
        },
        format!("{order}-source"),
        order,
    )
    .with_script(ReviewScript::ResumePass)
    .with_service_credential_minutes(2)
    .with_adjustable_host_clock();
    let (mut fixture, accepted) = accepted_hold_with_identity(
        identity,
        order,
        ProductHoldDecision::Resume("replan_full".into()),
    );
    let clock = fixture
        .host_clock
        .as_ref()
        .expect("this fixture supplies an adjustable host transaction clock")
        .clone();
    assert!(fixture.now < fixture.claim.lease_until());
    let service_expiry: String = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT expires_at FROM product_service_credentials_v1 WHERE service_id=?1",
            [&fixture.identity.service_id],
            |row| row.get(0),
        )
        .unwrap();
    let service_expiry = chrono::DateTime::parse_from_rfc3339(&service_expiry)
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert!(fixture.now < service_expiry);
    assert!(fixture.now + Duration::minutes(3) < fixture.claim.lease_until());
    let pause = fixture.stage_pause.clone();
    pause.arm("replan_full-g1");
    let mut host = fixture.host.take().unwrap();
    let session = fixture.human_session_digest.clone();
    let operation = accepted.operation_id.clone();
    let now = fixture.now;
    let worker = std::thread::spawn(move || {
        let result = host.continue_accepted_product_hold(&session, &operation, now);
        (host, result)
    });
    pause.wait_until_reached();
    let run = format!("{order}:1:8:3");
    let before_expiry: (String, i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT status FROM product_review_runs_v1 WHERE order_id=?1 AND run_id=?2),
                    (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND run_id=?2 AND stage_id='replan_full-g1' AND status='accepted'),
                    (SELECT used FROM product_review_budget_v1 WHERE order_id=?1)",
            rusqlite::params![order, run],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(before_expiry, ("resuming".into(), 1, 5));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 5);
    clock.set(fixture.now + Duration::minutes(3));
    pause.release();
    let (host, outcome) = worker.join().unwrap();
    let error = outcome.expect_err("the next run 8 stage charge sees expired service authority");
    assert!(
        format!("{error:?}").contains("review service authority is absent or expired"),
        "the exact continuation charge failure is credential expiry: {error:?}"
    );
    let state: (i64, String, i64, i64, i64, i64, String) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM product_review_runs_v1 WHERE order_id=?1),
                    (SELECT status FROM product_review_runs_v1 WHERE order_id=?1 AND run_id=?2),
                    (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND run_id=?2 AND status='complete'),
                    (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND run_id=?2 AND status='accepted'),
                    (SELECT used FROM product_review_budget_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_artifacts_v1 WHERE order_id=?1),
                    (SELECT phase FROM product_review_hold_progress_v1 WHERE operation_id=?3)",
            rusqlite::params![order, format!("{order}:1:8:3"), accepted.operation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
        )
        .unwrap();
    assert_eq!(state, (2, "resuming".into(), 1, 0, 5, 5, "recovery_required".into()));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 5);
    assert_eq!(fixture.restores.load(Ordering::SeqCst), 1);
    assert_eq!(host.dispatch_count(), 1);
    drop(host);
    drop(fixture.result_store.take());
    std::fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn revocation_between_hold_acceptance_and_continuation_blocks_pre_stage_work() {
    let armed = Arc::new(AtomicBool::new(false));
    let (attempt_tx, attempt_rx) = mpsc::channel();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let observed_armed = armed.clone();
    let observed_gate = gate.clone();
    let observer: foundry_ledger::ChargeBudgetObserver = Arc::new(move |event| {
        if event == foundry_ledger::ChargeBudgetObservation::StageStartBeginAttempt
            && observed_armed.swap(false, Ordering::SeqCst)
        {
            attempt_tx
                .send(())
                .expect("G21 stage-start attempt receiver remains live");
            let (lock, changed) = &*observed_gate;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = changed.wait(released).unwrap();
            }
        }
    });
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: "continuation-pre-stage-revoke-install".into(),
            database_id: "continuation-pre-stage-revoke-db".into(),
        },
        "continuation-pre-stage-revoke-source",
        "continuation-pre-stage-revoke",
    )
    .with_script(ReviewScript::ResumePass);
    let mut fixture = genuine_host_done_with_stage_start_observer(identity, observer);
    grant_review_hold_scope(&fixture);
    let mut acceptance_store = ProductStore::open(&fixture.db_path, fixture.config.clone()).unwrap();
    let hold = acceptance_store
        .list_product_review_holds(&fixture.human_session_digest, fixture.now)
        .unwrap()
        .into_iter()
        .find(|hold| hold.order_id == fixture.identity.order_id)
        .expect("the genuine initial run created a listed hold");
    let accepted = acceptance_store
        .accept_product_review_hold(
            &fixture.human_session_digest,
            &fixture.identity.order_id,
            &hold.hold_instance_id,
            &hold.expectation,
            "continuation-pre-stage-revoke-accept",
            ProductHoldDecision::Resume("replan_full".into()),
            fixture.now,
        )
        .unwrap();
    drop(acceptance_store);
    let order = fixture.identity.order_id.clone();
    let calls_before = fixture.calls.load(Ordering::SeqCst);
    let restores_before = fixture.restores.load(Ordering::SeqCst);
    let dispatch_before = fixture.host.as_ref().unwrap().dispatch_count();
    let original: (i64, i64, i64, i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_artifacts_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                    (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1)",
            [&order],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .unwrap();
    assert_eq!(original, (4, 3, 4, 0, 0));
    let mut host = fixture.host.take().unwrap();
    let session = fixture.human_session_digest.clone();
    let operation = accepted.operation_id.clone();
    let now = fixture.now;
    armed.store(true, Ordering::SeqCst);
    let worker = std::thread::spawn(move || {
        let outcome = host.continue_accepted_product_hold(&session, &operation, now);
        (host, outcome)
    });
    if let Err(timeout) = attempt_rx.recv_timeout(std::time::Duration::from_secs(30)) {
        let (_, outcome) = worker.join().expect("continuation worker did not panic");
        panic!(
            "real continuation did not reach the stage-start boundary ({timeout}); outcome={outcome:?}"
        );
    }
    disable_fixture_service(&fixture);
    {
        let (lock, changed) = &*gate;
        *lock.lock().unwrap() = true;
        changed.notify_all();
    }
    let (host, outcome) = worker.join().unwrap();
    let error = outcome.expect_err("committed revocation wins before stage-start transaction");
    assert!(
        format!("{error:?}").contains("review service authority is absent or expired"),
        "the stage-start transaction rejects on revoked service authority: {error:?}"
    );
    let state: (i64, i64, i64, String) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM product_review_runs_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND run_id=?2),
                    (SELECT count(*) FROM product_review_authority_v1 WHERE order_id=?1),
                    (SELECT phase FROM product_review_hold_progress_v1 WHERE operation_id=?3)",
            rusqlite::params![fixture.identity.order_id, format!("{}:1:8:3", fixture.identity.order_id), accepted.operation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(state, (2, 0, 2, "recovery_required".into()));
    let after: (i64, i64, i64, i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_artifacts_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                    (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1)",
            [&order],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .unwrap();
    assert_eq!(after, original);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), calls_before);
    assert_eq!(fixture.restores.load(Ordering::SeqCst), restores_before);
    assert_eq!(host.dispatch_count(), dispatch_before);
    let mut finalizer = fixture.result_store.take().unwrap();
    assert_eq!(
        finalizer.finalize_product_review_result(&fixture.service, &fixture.expected),
        Err(foundry_ledger::ProductResultError::Unauthorized),
        "finalization remains refused after the revoked stage-start attempt"
    );
    drop(host);
    drop(finalizer);
    std::fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn revocation_after_completed_continuation_stage_preserves_it_and_blocks_the_next() {
    let (mut fixture, accepted) = accepted_resume("continuation-post-stage-revoke");
    let pause = fixture.stage_pause.clone();
    pause.arm("replan_full-g1");
    let mut host = fixture.host.take().unwrap();
    let session = fixture.human_session_digest.clone();
    let operation = accepted.operation_id.clone();
    let now = fixture.now;
    let worker = std::thread::spawn(move || {
        let result = host.continue_accepted_product_hold(&session, &operation, now);
        (host, result)
    });
    pause.wait_until_reached();
    let before_revoke: (i64, i64, String) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND run_id=?2 AND stage_id='replan_full-g1' AND status='complete'),
                    (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND run_id=?2 AND stage_id='replan_full-g1' AND status='accepted'),
                    (SELECT status FROM product_review_runs_v1 WHERE order_id=?1 AND run_id=?2)",
            rusqlite::params![fixture.identity.order_id, format!("{}:1:8:3", fixture.identity.order_id)],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        before_revoke,
        (0, 1, "resuming".into()),
        "the real executor pause is reached after replan_full-g1 is reserved and before its result is committed"
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 5);
    assert_eq!(fixture.restores.load(Ordering::SeqCst), 0);
    let charged_before_revoke: i64 = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT used FROM product_review_budget_v1 WHERE order_id=?1",
            [&fixture.identity.order_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(charged_before_revoke, 5);
    disable_fixture_service(&fixture);
    pause.release();
    let (mut host, outcome) = worker.join().unwrap();
    assert!(outcome.is_err());
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 5);
    assert_eq!(fixture.restores.load(Ordering::SeqCst), 1);
    assert_eq!(host.dispatch_count(), 1);
    let after: (i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND run_id=?2 AND status='complete'),
                    (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND run_id=?2 AND status='accepted')",
            rusqlite::params![fixture.identity.order_id, format!("{}:1:8:3", fixture.identity.order_id)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(after, (before_revoke.0 + 1, 0));
    let continuation: (i64, i64, i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND status='complete'),
                    (SELECT count(*) FROM product_artifacts_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?1),
                    (SELECT used FROM product_review_budget_v1 WHERE order_id=?1)",
            [&fixture.identity.order_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(continuation, (5, 5, 4, 5));
    let mut finalizer = fixture.result_store.take().unwrap();
    let followup_expectation = ProductResultExpectation {
        host_key: foundry_exec::HostKey {
            run: u64::try_from(accepted.reserved_run_id).unwrap(),
            ..fixture.expected.host_key.clone()
        },
        host_result_digest: fixture.expected.host_result_digest.clone(),
    };
    assert_eq!(
        finalizer.finalize_product_review_result(&fixture.service, &followup_expectation),
        Err(foundry_ledger::ProductResultError::Unauthorized),
        "the revoked followup run cannot be finalized as a product result"
    );
    let run = format!(
        "{}:1:{}:3",
        fixture.identity.order_id, accepted.reserved_run_id
    );
    let recovery: (String, String) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT status FROM product_review_runs_v1 WHERE order_id=?1 AND run_id=?2),
                    (SELECT phase FROM product_review_hold_progress_v1 WHERE operation_id=?3)",
            rusqlite::params![fixture.identity.order_id, run, accepted.operation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(recovery, ("resuming".into(), "recovery_required".into()));
    let before_retry = (
        fixture.calls.load(Ordering::SeqCst),
        fixture.restores.load(Ordering::SeqCst),
        host.dispatch_count(),
    );
    assert_eq!(before_retry, (5, 1, 1));
    assert!(
        host.continue_accepted_product_hold(
            &fixture.human_session_digest,
            &accepted.operation_id,
            fixture.now,
        )
        .is_err(),
        "a recovery-required continuation cannot execute another edge"
    );
    assert_eq!(
        (
            fixture.calls.load(Ordering::SeqCst),
            fixture.restores.load(Ordering::SeqCst),
            host.dispatch_count(),
        ),
        before_retry,
        "the next edge observes the persisted trace without another restore or execution"
    );
    let result_rows: (i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                    (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1)",
            [&fixture.identity.order_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(result_rows, (0, 0));
    drop(host);
    std::fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn direct_commitpass_rejects_same_owner_epoch_fence_aba_before_materialization() {
    let (mut fixture, accepted) = accepted_hold(
        "continuation-same-owner-aba",
        ReviewScript::DirectCommitPass,
        ProductHoldDecision::CommitPass,
    );
    let conn = Connection::open(&fixture.db_path).unwrap();
    let original: (String, i64, i64, i64) = conn
        .query_row(
            "SELECT owner_id,epoch,fence,source_revision FROM product_work_claims_v1
             WHERE order_id=?1 AND lifecycle=1",
            [&fixture.identity.order_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        conn.execute(
            "UPDATE product_work_claims_v1 SET epoch=epoch+1,fence=fence+1
             WHERE order_id=?1 AND lifecycle=1 AND owner_id=?2",
            rusqlite::params![fixture.identity.order_id, original.0],
        )
        .unwrap(),
        1
    );
    let reassigned: (String, i64, i64, i64) = conn
        .query_row(
            "SELECT owner_id,epoch,fence,source_revision FROM product_work_claims_v1
             WHERE order_id=?1 AND lifecycle=1",
            [&fixture.identity.order_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(reassigned.0, original.0, "the worker owner is unchanged");
    assert_eq!(reassigned.1, original.1 + 1);
    assert_eq!(reassigned.2, original.2 + 1);
    assert_eq!(reassigned.3, original.3);
    let work_before_refusal = work_claim_backlog_state(&conn, &fixture.identity.order_id);

    let mut host = fixture.host.take().unwrap();
    assert!(
        host.continue_accepted_product_hold(
            &fixture.human_session_digest,
            &accepted.operation_id,
            fixture.now,
        )
        .is_err()
    );
    let state: (i64, i64, String) = conn
        .query_row(
            "SELECT (SELECT count(*) FROM product_review_runs_v1 WHERE order_id=?1 AND run_id=?2),
                    (SELECT count(*) FROM product_review_host_accept_v1 WHERE order_id=?1 AND run_id=8),
                    (SELECT phase FROM product_review_hold_progress_v1 WHERE operation_id=?3)",
            rusqlite::params![fixture.identity.order_id, format!("{}:1:8:3", fixture.identity.order_id), accepted.operation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(state, (0, 0, "accepted".into()));
    assert_eq!(
        work_claim_backlog_state(&conn, &fixture.identity.order_id),
        work_before_refusal,
        "CommitPass continuation refusal preserves the exact owner/ABA claim and backlog"
    );
    assert_eq!(
        fixture
            .result_store
            .as_mut()
            .unwrap()
            .finalize_product_review_result(&fixture.service, &fixture.expected),
        Err(foundry_ledger::ProductResultError::Ineligible),
        "the strict result finalizer rejects the same-owner epoch/fence ABA after continuation refusal"
    );
    assert_eq!(
        work_claim_backlog_state(&conn, &fixture.identity.order_id),
        work_before_refusal,
        "strict finalizer refusal preserves the same-owner replacement claim and backlog"
    );
    drop(conn);
    drop(host);
    drop(fixture.result_store.take());
    std::fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn accepted_resume_rejects_isolated_service_and_credential_rotations() {
    for rotate_credential in [false, true] {
        let label = if rotate_credential { "B" } else { "A" };
        let order = format!("continuation-{label}-only-rotation");
        let script = if rotate_credential {
            ReviewScript::ResumeCommitPass
        } else {
            ReviewScript::ResumePass
        };
        let (mut fixture, accepted) = accepted_hold(
            &order,
            script,
            ProductHoldDecision::Resume("replan_full".into()),
        );
        let before: (i64, i64, i64, i64) = Connection::open(&fixture.db_path)
            .unwrap()
            .query_row(
                "SELECT service_revision,credential_revision,drive_review_grant_revision,source_instance_revision
                 FROM product_review_authority_v1 WHERE order_id=?1 AND run_id=7",
                [&fixture.identity.order_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        let mut owner = ProductStore::open(&fixture.db_path, fixture.config.clone()).unwrap();
        if rotate_credential {
            let secret = owner
                .issue_product_service_credential(
                    &fixture.owner_session_digest,
                    &fixture.identity.service_id,
                    1,
                    fixture.now + Duration::hours(1),
                    "continuation-B-credential-rotation",
                    fixture.now,
                )
                .unwrap()
                .expose_once_to_file_writer()
                .to_owned();
            assert!(!secret.is_empty());
        } else {
            let grants = [
                foundry_ledger::ProductServiceAction::IntakeRead,
                foundry_ledger::ProductServiceAction::AdmitDelivery,
                foundry_ledger::ProductServiceAction::AckDelivery,
                foundry_ledger::ProductServiceAction::DriveReview,
            ]
            .into_iter()
            .map(|action| foundry_ledger::ProductServiceGrant {
                source_instance_id: fixture.identity.source_instance_id.clone(),
                action,
            })
            .collect::<Vec<_>>();
            owner
                .manage_product_service(
                    &fixture.owner_session_digest,
                    &fixture.identity.service_id,
                    &grants,
                    true,
                    fixture.now + Duration::hours(2),
                    before.0,
                    "continuation-A-service-rotation",
                    fixture.now,
                )
                .unwrap();
        }
        let mut host = fixture.host.take().unwrap();
        assert!(
            host.continue_accepted_product_hold(
                &fixture.human_session_digest,
                &accepted.operation_id,
                fixture.now,
            )
            .is_err()
        );
        let after_snapshot: (i64, i64, i64, i64) = Connection::open(&fixture.db_path)
            .unwrap()
            .query_row(
                "SELECT service_revision,credential_revision,drive_review_grant_revision,source_instance_revision
                 FROM product_review_authority_v1 WHERE order_id=?1 AND run_id=7",
                [&fixture.identity.order_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            after_snapshot, before,
            "the accepted authority snapshot is immutable"
        );
        let state: (i64, String) = Connection::open(&fixture.db_path)
            .unwrap()
            .query_row(
                "SELECT (SELECT count(*) FROM product_review_runs_v1 WHERE order_id=?1 AND run_id=?2),
                        (SELECT phase FROM product_review_hold_progress_v1 WHERE operation_id=?3)",
                rusqlite::params![fixture.identity.order_id, format!("{}:1:8:3", fixture.identity.order_id), accepted.operation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, (0, "accepted".into()));
        drop(owner);
        drop(host);
        drop(fixture.result_store.take());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }
}

#[test]
fn direct_commitpass_reassignment_is_rejected_before_continuation() {
    let (mut fixture, accepted) = accepted_hold(
        "direct-commitpass-reassignment",
        ReviewScript::DirectCommitPass,
        ProductHoldDecision::CommitPass,
    );
    let conn = Connection::open(&fixture.db_path).unwrap();
    let changed = conn
        .execute(
            "UPDATE product_work_claims_v1 SET owner_id='replacement-worker',epoch=epoch+1,fence=fence+1
             WHERE order_id=?1 AND lifecycle=1",
            [&fixture.identity.order_id],
        )
        .unwrap();
    assert_eq!(changed, 1, "the real completed claim was reassigned");
    let reassigned: (String, i64, i64) = conn
        .query_row(
            "SELECT owner_id,epoch,fence FROM product_work_claims_v1 WHERE order_id=?1 AND lifecycle=1",
            [&fixture.identity.order_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(reassigned.0, "replacement-worker");
    let replacement_before = work_claim_backlog_state(&conn, &fixture.identity.order_id);
    let mut host = fixture.host.take().unwrap();
    assert!(host
        .continue_accepted_product_hold(
            &fixture.human_session_digest,
            &accepted.operation_id,
            fixture.now,
        )
        .is_err());
    let state: (i64, String) = conn
        .query_row(
            "SELECT (SELECT count(*) FROM product_review_runs_v1 WHERE order_id=?1 AND run_id=?2),
                    (SELECT phase FROM product_review_hold_progress_v1 WHERE operation_id=?3)",
            rusqlite::params![fixture.identity.order_id, format!("{}:1:8:3", fixture.identity.order_id), accepted.operation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, (0, "accepted".into()));
    assert_eq!(
        fixture
            .result_store
            .as_mut()
            .unwrap()
            .finalize_product_review_result(&fixture.service, &fixture.expected),
        Err(foundry_ledger::ProductResultError::Ineligible),
        "the strict result finalizer rejects a reassigned owner after continuation refusal"
    );
    assert_eq!(
        work_claim_backlog_state(&conn, &fixture.identity.order_id),
        replacement_before,
        "strict finalizer refusal preserves the reassigned replacement claim and backlog"
    );
    drop(conn);
    drop(host);
    drop(fixture.result_store.take());
    std::fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn commitpass_resolves_origin_without_materializing_reserved_run() {
    assert_continuation_result(ReviewScript::ResumeCommitPass, "p2:B", false);
    assert_continuation_result(ReviewScript::DirectCommitPass, "p2:direct", false);
}

#[test]
fn commit_fail_and_abort_continuations_are_ineligible_finalizer_controls() {
    for (label, decision) in [
        ("commit-fail", ProductHoldDecision::CommitFail),
        ("abort", ProductHoldDecision::Abort),
    ] {
        let order = format!("direct-control-{label}");
        let mut fixture = genuine_host_done(
            FixtureIdentity::new(
                ProductConfig {
                    installation_id: format!("{order}-install"),
                    database_id: format!("{order}-db"),
                },
                format!("{order}-source"),
                &order,
            )
            .with_script(ReviewScript::ResumePass),
        );
        grant_review_hold_scope(&fixture);
        let before_s1: (i64, i64) = Connection::open(&fixture.db_path).unwrap().query_row(
            "SELECT (SELECT count(*) FROM ledger WHERE order_id=?1),
                    (SELECT count(*) FROM product_transition_receipts_v1 WHERE order_id=?1)",
            [&order], |r| Ok((r.get(0)?,r.get(1)?)),
        ).unwrap();
        let mut owner = ProductStore::open(&fixture.db_path, fixture.config.clone()).unwrap();
        let hold = owner.list_product_review_holds(&fixture.human_session_digest, fixture.now)
            .unwrap().into_iter().find(|hold| hold.order_id == order).unwrap();
        let accepted = owner.accept_product_review_hold(
            &fixture.human_session_digest, &order, &hold.hold_instance_id,
            &hold.expectation, &format!("{order}-decision"), decision, fixture.now,
        ).unwrap();
        let mut host = fixture.host.take().unwrap();
        host.continue_accepted_product_hold(
            &fixture.human_session_digest, &accepted.operation_id, fixture.now,
        ).unwrap();
        let terminal: foundry_exec::HostResult = Connection::open(&fixture.db_path).unwrap().query_row(
            "SELECT result_json FROM product_review_host_results_v1
             WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND activation=?4",
            rusqlite::params![order, fixture.expected.host_key.attempt as i64,
                7_i64,
                fixture.expected.host_key.activation as i64],
            |r| serde_json::from_str(&r.get::<_,String>(0)?).map_err(|e|
                rusqlite::Error::FromSqlConversionFailure(0,rusqlite::types::Type::Text,Box::new(e))),
        ).unwrap();
        let terminal_expectation = ProductResultExpectation {
            host_key: fixture.expected.host_key.clone(),
            host_result_digest: foundry_ledger::product_host_result_digest(&terminal).unwrap(),
        };
        let error = fixture.result_store.as_mut().unwrap()
            .finalize_product_review_result(&fixture.service, &terminal_expectation)
            .expect_err("CommitFail and Abort are not successful final results");
        assert_eq!(error, foundry_ledger::ProductResultError::Ineligible);
        let after: (i64, i64, i64, i64) = Connection::open(&fixture.db_path).unwrap().query_row(
            "SELECT (SELECT count(*) FROM ledger WHERE order_id=?1),
                    (SELECT count(*) FROM product_transition_receipts_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                    (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1)",
            [&order], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
        ).unwrap();
        assert_eq!(after.0, before_s1.0 + 1, "{label} appends exactly one existing failure outcome event");
        assert_eq!(after.1, before_s1.1 + 1, "{label} appends exactly one transition receipt");
        assert_eq!((after.2,after.3), (0,0), "{label} finalizer refusal writes no tuple/result");
        drop(owner);
        drop(host);
        drop(fixture.result_store.take());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }
}

#[test]
fn legacy_census_corrupt_reachable_acceptance_is_corrupt_evidence() {
    assert_continuation_result(ReviewScript::ResumePass, "p2:legacy-corrupt", true);
}

#[test]
fn completed_result_rejects_reclaimed_and_same_owner_aba() {
    assert_continuation_result_with_reclaim(ReviewScript::ResumePass, "p2:A", false, Some(false));
    assert_continuation_result_with_reclaim(ReviewScript::ResumePass, "p2:A", false, Some(true));
    assert_continuation_result_with_reclaim(ReviewScript::ResumeCommitPass, "p2:B", false, Some(false));
    assert_continuation_result_with_reclaim(ReviewScript::ResumeCommitPass, "p2:B", false, Some(true));
}

#[test]
fn completed_backlog_public_reclaim_requires_expired_lease() {
    let worker_id = "completed-backlog-replacement-worker";
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: format!("{worker_id}-install"),
            database_id: format!("{worker_id}-db"),
        },
        format!("{worker_id}-source"),
        "p2:A",
    );
    let mut fixture = genuine_host_done(identity);
    let root = fixture.root.clone();
    let db = fixture.db_path.clone();
    let order = fixture.identity.order_id.clone();
    let mut result_store = fixture.result_store.take().unwrap();
    let connection = Connection::open(&db).unwrap();
    let old_work: (String, i64, i64, String, String, i64) = connection
        .query_row(
            "SELECT c.owner_id,c.epoch,c.fence,c.state,c.lease_until,b.attempts
             FROM product_work_claims_v1 c JOIN product_work_backlog_v1 b USING(order_id,lifecycle)
             WHERE c.order_id=?1 AND c.lifecycle=1",
            [&order],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        )
        .unwrap();
    assert_eq!(old_work.3, "completed");
    assert!(
        chrono::DateTime::parse_from_rfc3339(&old_work.4)
            .unwrap()
            .with_timezone(&chrono::Utc)
            <= chrono::Utc::now(),
        "the completed claim lease is expired at the genuine terminal completion boundary"
    );
    let preserved_before_reclaim = result_evidence_rows(&connection, &order);
    let now = chrono::Utc::now();
    let mut replacement_store = ProductStore::open(&db, fixture.config.clone()).unwrap();
    let replacement = match replacement_store
        .claim_product_work_for_service(
            &fixture.service,
            worker_id,
            now + Duration::minutes(6),
            Duration::minutes(5),
        )
        .unwrap()
    {
        foundry_ledger::ProductWorkClaimResult::Claimed(claim) => claim,
        other => panic!("completed result lease should be publicly reclaimable: {other:?}"),
    };
    let replacement_binding = replacement.review_binding();
    assert_eq!(replacement_binding.epoch, 2);
    assert!(replacement_binding.fence > old_work.2);
    assert_eq!(replacement_binding.fence, 2);
    assert_eq!(replacement_binding.worker_id, worker_id);
    assert_ne!(old_work.0, worker_id);
    let attempts: i64 = connection
        .query_row(
            "SELECT attempts FROM product_work_backlog_v1 WHERE order_id=?1 AND lifecycle=1",
            [&order],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(attempts, old_work.5 + 1);
    assert_eq!(
        result_evidence_rows_without_work(&connection, &order),
        without_work_rows(&preserved_before_reclaim),
        "public reclaim changes only the claim/backlog pair before finalizer refusal"
    );
    let preserved_after_reclaim = result_evidence_rows(&connection, &order);
    assert_eq!(
        result_store.finalize_product_review_result(&fixture.service, &fixture.expected),
        Err(foundry_ledger::ProductResultError::StaleBinding)
    );
    let result_counts: (i64, i64) = connection
        .query_row(
            "SELECT (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                    (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1)",
            [&order],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(result_counts, (0, 0), "T/R remains +0/+0");
    assert_eq!(result_evidence_rows(&connection, &order), preserved_after_reclaim);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 4);
    assert_eq!(fixture.restores.load(Ordering::SeqCst), 0);
    drop(connection);
    drop(replacement_store);
    drop(result_store);
    drop(fixture.host.take());
    std::fs::remove_dir_all(root).unwrap();
}

fn assert_continuation_result(script: ReviewScript, order: &str, corrupt_legacy_edge: bool) {
    assert_continuation_result_with_reclaim(script, order, corrupt_legacy_edge, None);
}

fn assert_continuation_result_with_reclaim(
    script: ReviewScript,
    order: &str,
    corrupt_legacy_edge: bool,
    reclaim_same_owner: Option<bool>,
) {
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: format!("{order}-install"),
            database_id: format!("{order}-db"),
        },
        format!("{}-source", order.replace(':', "-")),
        order,
    )
    .with_script(script);
    let mut fixture = genuine_host_done(identity);
    grant_review_hold_scope(&fixture);
    assert!(fixture.terminal.as_ref().unwrap().held.is_some());
    let mut preaccept_store = fixture.result_store.take().unwrap();
    assert_eq!(
        preaccept_store.finalize_product_review_result(&fixture.service, &fixture.expected),
        Err(foundry_ledger::ProductResultError::Ineligible),
        "a genuine held terminal host result is not finalizable"
    );
    let preaccept_counts: (i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                    (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1)",
            [&fixture.identity.order_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(preaccept_counts, (0, 0));
    drop(preaccept_store);
    let db = fixture.db_path.clone();
    let root = fixture.root.clone();
    let artifact_root = fixture.artifact_root.clone();
    let now = fixture.now;
    let service = fixture.service.clone();
    let session = fixture.human_session_digest.clone();
    let calls = fixture.calls.clone();
    let restores = fixture.restores.clone();
    let mut host = fixture.host.take().unwrap();
    drop(fixture.result_store.take());
    let initial_authority = read_authority_rows(&db, order);
    assert_eq!(
        initial_authority.len(),
        1,
        "one initial activation snapshot"
    );
    let authority_guard = Connection::open(&db).unwrap();
    assert!(
        authority_guard
            .execute(
                "UPDATE product_review_authority_v1 SET credential_revision=credential_revision+1
             WHERE order_id=?1 AND run_id=7",
                [order],
            )
            .is_err()
    );
    assert!(
        authority_guard
            .execute(
                "DELETE FROM product_review_authority_v1 WHERE order_id=?1 AND run_id=7",
                [order],
            )
            .is_err()
    );
    assert!(
        authority_guard
            .execute(
                "INSERT OR REPLACE INTO product_review_authority_v1
             SELECT * FROM product_review_authority_v1 WHERE order_id=?1 AND run_id=7",
                [order],
            )
            .is_err()
    );
    drop(authority_guard);
    assert_eq!(read_authority_rows(&db, order), initial_authority);

    let mut store = ProductStore::open(&db, fixture.config.clone()).unwrap();
    let first = store
        .list_product_review_holds(&session, now)
        .unwrap()
        .into_iter()
        .find(|hold| hold.order_id == order)
        .expect("initial genuine host hold is listed from ProductStore");
    assert_eq!(first.expectation.originating_run, 7);
    let first_decision = if script == ReviewScript::DirectCommitPass {
        ProductHoldDecision::CommitPass
    } else {
        ProductHoldDecision::Resume("replan_full".into())
    };
    let first_receipt = store
        .accept_product_review_hold(
            &session,
            order,
            &first.hold_instance_id,
            &first.expectation,
            &format!("{order}-hold-1"),
            first_decision,
            now,
        )
        .unwrap();
    let s1_before_decision: (i64, i64) = Connection::open(&db).unwrap().query_row(
        "SELECT (SELECT count(*) FROM ledger WHERE order_id=?1),
                (SELECT count(*) FROM product_transition_receipts_v1 WHERE order_id=?1)",
        [order], |r| Ok((r.get(0)?,r.get(1)?)),
    ).unwrap();
    assert_eq!(first_receipt.reserved_run_id, 8);
    let reserved_only: (i64, i64) = Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM product_review_runs_v1 WHERE order_id=?1 AND run_id=?2),
                    (SELECT count(*) FROM product_review_host_accept_v1 WHERE order_id=?1 AND run_id=8)",
            rusqlite::params![order, format!("{order}:1:8:3")],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        reserved_only,
        (0, 0),
        "acceptance reserves but does not materialize Resume work"
    );
    let resume_copy_c0 = result_evidence_rows(&Connection::open(&db).unwrap(), order);
    let resume_copy_intent: String = Connection::open(&db).unwrap().query_row(
        "SELECT b.intent_json FROM product_review_bindings_v1 b
         JOIN product_review_active_v1 a ON a.key_json=b.key_json WHERE a.order_id=?1",
        [order], |r| r.get(0),
    ).unwrap();
    let resume_copy_active: (String, String, i64) = Connection::open(&db).unwrap().query_row(
        "SELECT key_json,journal_run,epoch FROM product_review_active_v1 WHERE order_id=?1",
        [order], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).unwrap();
    let resume_copy_progress: (String, i64, String, i64, i64, i64, Option<String>) =
        Connection::open(&db).unwrap().query_row(
            "SELECT phase,progress_revision,owner_id,epoch,fence,reserved_run_id,recovery_reason
             FROM product_review_hold_progress_v1 WHERE operation_id=?1",
            [&first_receipt.operation_id],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)),
        ).unwrap();
    if script != ReviewScript::DirectCommitPass {
        let copy_fault = Connection::open(&db).unwrap();
        copy_fault
            .execute_batch(
                "CREATE TRIGGER fail_resume_authority_copy BEFORE INSERT ON product_review_authority_v1
                 WHEN NEW.run_id!=7
                 BEGIN SELECT RAISE(ABORT,'injected Resume authority copy failure'); END;",
            )
            .unwrap();
        assert!(
            host.continue_accepted_product_hold(&session, &first_receipt.operation_id, now)
                .is_err()
        );
        let rolled_back: (i64, String, i64, i64) = copy_fault
            .query_row(
                "SELECT (SELECT count(*) FROM product_review_authority_v1 WHERE order_id=?1),
                        (SELECT phase FROM product_review_hold_progress_v1 WHERE operation_id=?2),
                        (SELECT count(*) FROM product_review_runs_v1 WHERE order_id=?1 AND run_id=?3),
                        (SELECT count(*) FROM product_review_host_accept_v1 WHERE order_id=?1 AND run_id=8)",
                rusqlite::params![order, first_receipt.operation_id, format!("{order}:1:8:3")],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            rolled_back,
            (1, "accepted".into(), 0, 0),
            "failed Resume copy must roll back materialization and leave the accepted reservation retryable"
        );
        assert_eq!(result_evidence_rows(&copy_fault, order), resume_copy_c0);
        let intent_after: String = copy_fault.query_row(
            "SELECT b.intent_json FROM product_review_bindings_v1 b
             JOIN product_review_active_v1 a ON a.key_json=b.key_json WHERE a.order_id=?1",
            [order], |r| r.get(0),
        ).unwrap();
        let active_after: (String, String, i64) = copy_fault.query_row(
            "SELECT key_json,journal_run,epoch FROM product_review_active_v1 WHERE order_id=?1",
            [order], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        ).unwrap();
        let progress_after: (String, i64, String, i64, i64, i64, Option<String>) = copy_fault.query_row(
            "SELECT phase,progress_revision,owner_id,epoch,fence,reserved_run_id,recovery_reason
             FROM product_review_hold_progress_v1 WHERE operation_id=?1",
            [&first_receipt.operation_id],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)),
        ).unwrap();
        assert_eq!(intent_after, resume_copy_intent);
        assert_eq!(active_after, resume_copy_active);
        assert_eq!(progress_after, resume_copy_progress);
        copy_fault
            .execute_batch("DROP TRIGGER fail_resume_authority_copy;")
            .unwrap();
        let zero_copy = Connection::open(&db).unwrap();
        zero_copy
            .execute_batch(
                "CREATE TRIGGER suppress_resume_authority_copy BEFORE INSERT ON product_review_authority_v1
                 WHEN NEW.run_id!=7 BEGIN SELECT RAISE(IGNORE); END;",
            )
            .unwrap();
        assert!(
            host.continue_accepted_product_hold(&session, &first_receipt.operation_id, now)
                .is_err()
        );
        let zero_copy_rollback: (i64, String, i64, i64) = zero_copy
            .query_row(
                "SELECT (SELECT count(*) FROM product_review_authority_v1 WHERE order_id=?1),
                        (SELECT phase FROM product_review_hold_progress_v1 WHERE operation_id=?2),
                        (SELECT count(*) FROM product_review_runs_v1 WHERE order_id=?1 AND run_id=?3),
                        (SELECT count(*) FROM product_review_host_accept_v1 WHERE order_id=?1 AND run_id=8)",
                rusqlite::params![order, first_receipt.operation_id, format!("{order}:1:8:3")],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(zero_copy_rollback, (1, "accepted".into(), 0, 0));
        assert_eq!(result_evidence_rows(&zero_copy, order), resume_copy_c0);
        let zero_intent: String = zero_copy.query_row(
            "SELECT b.intent_json FROM product_review_bindings_v1 b
             JOIN product_review_active_v1 a ON a.key_json=b.key_json WHERE a.order_id=?1",
            [order], |r| r.get(0),
        ).unwrap();
        let zero_active: (String, String, i64) = zero_copy.query_row(
            "SELECT key_json,journal_run,epoch FROM product_review_active_v1 WHERE order_id=?1",
            [order], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        ).unwrap();
        let zero_progress: (String, i64, String, i64, i64, i64, Option<String>) = zero_copy.query_row(
            "SELECT phase,progress_revision,owner_id,epoch,fence,reserved_run_id,recovery_reason
             FROM product_review_hold_progress_v1 WHERE operation_id=?1",
            [&first_receipt.operation_id],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)),
        ).unwrap();
        assert_eq!(zero_intent, resume_copy_intent);
        assert_eq!(zero_active, resume_copy_active);
        assert_eq!(zero_progress, resume_copy_progress);
        zero_copy
            .execute_batch("DROP TRIGGER suppress_resume_authority_copy;")
            .unwrap();
    }
    let direct_commitpass_before = if script == ReviewScript::DirectCommitPass {
        Some(result_runtime_counts(&Connection::open(&db).unwrap(), order))
    } else {
        None
    };
    host.continue_accepted_product_hold(&session, &first_receipt.operation_id, now)
        .unwrap();
    if let Some(before) = direct_commitpass_before {
        assert_eq!(
            result_runtime_counts(&Connection::open(&db).unwrap(), order),
            before,
            "direct CommitPass boundary leaves stage/session/artifact/run/reservation counts unchanged"
        );
    }
    let authority_after_first = read_authority_rows(&db, order);
    let expected_authority_rows = if script == ReviewScript::DirectCommitPass {
        1
    } else {
        2
    };
    assert_eq!(authority_after_first.len(), expected_authority_rows);
    if expected_authority_rows == 2 {
        assert_eq!(
            authority_after_first[0].1, authority_after_first[1].1,
            "Resume copies all eight original service/source authority members unchanged"
        );
    }

    let terminal_run = if script == ReviewScript::DirectCommitPass {
        7
    } else {
        8
    };
    let mut second_receipt = None;
    if script == ReviewScript::ResumeCommitPass {
        let second = store
            .list_product_review_holds(&session, now)
            .unwrap()
            .into_iter()
            .find(|hold| hold.order_id == order)
            .expect("scenario B creates its second genuine hold on run 8");
        assert_eq!(second.expectation.originating_run, 8);
        let receipt = store
            .accept_product_review_hold(
                &session,
                order,
                &second.hold_instance_id,
                &second.expectation,
                &format!("{order}-hold-2"),
                ProductHoldDecision::CommitPass,
                now,
            )
            .unwrap();
        assert_eq!(receipt.reserved_run_id, 9);
        let commitpass_runtime_before = result_runtime_counts(&Connection::open(&db).unwrap(), order);
        host.continue_accepted_product_hold(&session, &receipt.operation_id, now)
            .unwrap();
        assert_eq!(
            result_runtime_counts(&Connection::open(&db).unwrap(), order),
            commitpass_runtime_before,
            "B CommitPass boundary leaves stage/session/artifact/run/reservation counts unchanged"
        );
        assert_eq!(
            read_authority_rows(&db, order).len(),
            2,
            "CommitPass reserves a successor without copying or materializing successor authority"
        );
        second_receipt = Some(receipt);
    }
    let s1_after_decisions: (i64, i64) = Connection::open(&db).unwrap().query_row(
        "SELECT (SELECT count(*) FROM ledger WHERE order_id=?1),
                (SELECT count(*) FROM product_transition_receipts_v1 WHERE order_id=?1)",
        [order], |r| Ok((r.get(0)?,r.get(1)?)),
    ).unwrap();
    let terminal_s1_delta = if matches!(script, ReviewScript::DirectCommitPass | ReviewScript::ResumeCommitPass) { 1 } else { 0 };
    assert_eq!(
        s1_after_decisions,
        (s1_before_decision.0 + terminal_s1_delta, s1_before_decision.1 + terminal_s1_delta),
        "terminal CommitPass appends one S1 outcome/receipt; Resume alone appends neither"
    );

    let terminal: foundry_exec::HostResult = Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT result_json FROM product_review_host_results_v1
             WHERE order_id=?1 AND attempt=1 AND run_id=?2 AND activation=3",
            rusqlite::params![order, terminal_run],
            |row| {
                serde_json::from_str(&row.get::<_, String>(0)?).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })
            },
        )
        .unwrap();
    assert_eq!(terminal.exit_code, 0);
    assert!(terminal.held.is_none());
    if script == ReviewScript::ResumePass || script == ReviewScript::ResumeCommitPass {
        assert_eq!(
            terminal
                .generations
                .iter()
                .map(|g| g.commit.as_str())
                .collect::<Vec<_>>(),
            vec![
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "cccccccccccccccccccccccccccccccccccccccc",
                "dddddddddddddddddddddddddddddddddddddddd"
            ]
        );
    } else {
        assert_eq!(terminal.generations.len(), 1);
        assert_eq!(
            terminal.generations[0].commit,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
    }
    if script == ReviewScript::ResumePass {
        let observations = fixture.task_observations.lock().unwrap();
        let replan = observations
            .iter()
            .find(|task| task.stage_id == "replan_full-g1")
            .expect("the resumed plan generation executed through the shared fixture executor");
        let fix = observations
            .iter()
            .find(|task| task.stage_id == "fixdie-g2")
            .expect("the single-finding fix generation executed through the shared fixture executor");
        assert_eq!(
            replan.session_id,
            format!("{order}:replan_full:fresh:{order}:1:8:3:resume-1"),
            "generation 1 uses the frozen fresh replan session id"
        );
        assert_eq!(
            fix.session_id,
            format!("{order}:die:fresh:{order}:1:8:3:resume-1"),
            "generation 2 uses the frozen fresh fix session id"
        );
        let run8 = fixture.artifact_root.join(format!(
            "{:x}",
            sha2::Sha256::digest(format!("{order}:1:8:3").as_bytes())
        ));
        assert_eq!(
            std::path::Path::new(&terminal.generations[1].fix_log_path),
            run8.join("exec/resume-1/3-replan_full-g1/0/accepted-output"),
            "generation 1 path matches the frozen artifact-root contract"
        );
        assert_eq!(
            std::path::Path::new(&terminal.generations[2].fix_log_path),
            run8.join("exec/resume-1/3-fixdie-g2/0/accepted-output"),
            "generation 2 path matches the frozen artifact-root contract"
        );
        assert_eq!(replan.artifact_commit, "b".repeat(40));
        assert_eq!(fix.artifact_commit, "c".repeat(40));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(replan.inputs.get("plan_full").unwrap()).unwrap(),
            serde_json::json!([]),
            "the resumed generation records its exact plan_full script input"
        );
        let findings: serde_json::Value =
            serde_json::from_str(fix.inputs.get("findings").unwrap()).unwrap();
        assert_eq!(findings.as_array().map(Vec::len), Some(1));
    }
    let expected = ProductResultExpectation {
        host_key: foundry_exec::HostKey {
            order: order.into(),
            attempt: 1,
            run: terminal_run,
            activation: 3,
        },
        host_result_digest: foundry_ledger::product_host_result_digest(&terminal).unwrap(),
    };
    if order == "g19-a" || order == "g19-b" {
        assert_pending_result_authority_matrix(&fixture, &expected);
    }
    let mut result_store = ProductStore::open_with_clock(
        &db,
        fixture.config.clone(),
        std::sync::Arc::new(product_result_fixture::FixtureClock(
            now + Duration::minutes(6),
        )),
    )
    .unwrap();
    result_store
        .configure_product_artifact_root(&artifact_root)
        .unwrap();
    let ledger_before: i64 = Connection::open(&db)
        .unwrap()
        .query_row("SELECT count(*) FROM ledger", [], |row| row.get(0))
        .unwrap();
    if matches!(script, ReviewScript::ResumePass | ReviewScript::ResumeCommitPass) {
        // Corrupt the ancestor before first finalization so this exercises
        // the new-result ancestry validator, not stored-result replay.
        let mutation = Connection::open(&db).unwrap();
        mutation
            .execute_batch("DROP TRIGGER product_review_authority_no_update;")
            .unwrap();
        let original_revision: i64 = mutation
            .query_row(
                "SELECT drive_review_grant_revision FROM product_review_authority_v1
                 WHERE order_id=?1 AND run_id=7",
                [order],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            mutation
                .execute(
                    "UPDATE product_review_authority_v1
                     SET drive_review_grant_revision=drive_review_grant_revision+1
                     WHERE order_id=?1 AND run_id=7",
                    [order],
                )
                .unwrap(),
            1
        );
        let changed: i64 = mutation
            .query_row(
                "SELECT drive_review_grant_revision FROM product_review_authority_v1
                 WHERE order_id=?1 AND run_id=7",
                [order],
                |row| row.get(0),
            )
            .unwrap();
        assert_ne!(changed, original_revision);
        assert_eq!(
            result_store.finalize_product_review_result(&service, &expected),
            Err(foundry_ledger::ProductResultError::CorruptEvidence),
            "changed ancestor authority must not be hidden by the copied terminal snapshot"
        );
        let uncommitted: (i64, i64) = mutation
            .query_row(
                "SELECT (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                        (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1)",
                [order],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(uncommitted, (0, 0));
        assert_eq!(calls.load(Ordering::SeqCst), 14);
        assert_eq!(restores.load(Ordering::SeqCst), 3);
        mutation
            .execute(
                "UPDATE product_review_authority_v1 SET drive_review_grant_revision=?1
                 WHERE order_id=?2 AND run_id=7",
                rusqlite::params![original_revision, order],
            )
            .unwrap();
        mutation
            .execute_batch(
                "CREATE TRIGGER product_review_authority_no_update BEFORE UPDATE ON product_review_authority_v1 BEGIN SELECT RAISE(ABORT,'review authority is immutable'); END;",
            )
            .unwrap();

        let mut wrong_followup_log = terminal.clone();
        wrong_followup_log.generations[1].fix_log_path = "/outside/followup-fix.log".into();
        let wrong_followup = ProductResultExpectation {
            host_key: expected.host_key.clone(),
            host_result_digest: foundry_ledger::product_host_result_digest(&wrong_followup_log)
                .unwrap(),
        };
        replace_host_result_evidence(&db, &expected.host_key, &wrong_followup_log);
        assert_eq!(
            result_store.finalize_product_review_result(&service, &wrong_followup),
            Err(foundry_ledger::ProductResultError::InvalidEvidence),
            "each followup verification output must resolve to its registered fix log"
        );
        replace_host_result_evidence(&db, &expected.host_key, &terminal);

        let (fix_artifact, original_output): (String, String) = mutation
            .query_row(
                "SELECT a.artifact_id,s.output_json FROM product_artifacts_v1 a
                 JOIN product_review_stages_v1 s
                   ON s.order_id=a.order_id AND s.run_id=a.run_id
                      AND s.sequence=a.stage_sequence AND s.stage_id=a.stage_id
                 WHERE a.order_id=?1 AND a.run_id=?2 AND a.stage_id='replan_full-g1'",
                rusqlite::params![order, format!("{order}:1:8:3")],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let mut wrong_output: serde_json::Value = serde_json::from_str(&original_output).unwrap();
        wrong_output["produced_commit"] = serde_json::Value::String("e".repeat(40));
        mutation
            .execute(
                "UPDATE product_review_stages_v1 SET output_json=?1
                 WHERE order_id=?2 AND run_id=?3 AND stage_id='replan_full-g1'",
                rusqlite::params![wrong_output.to_string(), order, format!("{order}:1:8:3")],
            )
            .unwrap();
        assert_eq!(
            result_store.finalize_product_review_result(&service, &expected),
            Err(foundry_ledger::ProductResultError::InvalidEvidence),
            "verification stage output must produce the exact next generation commit"
        );
        mutation
            .execute(
                "UPDATE product_review_stages_v1 SET output_json=?1
                 WHERE order_id=?2 AND run_id=?3 AND stage_id='replan_full-g1'",
                rusqlite::params![original_output, order, format!("{order}:1:8:3")],
            )
            .unwrap();
        let clean_inspected_commit = result_evidence_rows(&mutation, order);
        let mut wrong_inspected: serde_json::Value = serde_json::from_str(&original_output).unwrap();
        let original_inspected = wrong_inspected["inspected_commit"].clone();
        wrong_inspected["inspected_commit"] = serde_json::Value::String("f".repeat(40));
        mutation
            .execute(
                "UPDATE product_review_stages_v1 SET output_json=?1
                 WHERE order_id=?2 AND run_id=?3 AND stage_id='replan_full-g1'",
                rusqlite::params![wrong_inspected.to_string(), order, format!("{order}:1:8:3")],
            )
            .unwrap();
        assert_eq!(
            result_store.finalize_product_review_result(&service, &expected),
            Err(foundry_ledger::ProductResultError::InvalidEvidence),
            "independently changed inspected_commit evidence is rejected on a clean A baseline"
        );
        assert_eq!(
            result_evidence_rows(&mutation, order)
                .into_iter()
                .filter(|row| row.starts_with("artifact_tuples:") || row.starts_with("product_result_bindings_v1:"))
                .count(),
            0,
            "the inspected-commit negative has no tuple/result side effects"
        );
        wrong_inspected["inspected_commit"] = original_inspected;
        mutation
            .execute(
                "UPDATE product_review_stages_v1 SET output_json=?1
                 WHERE order_id=?2 AND run_id=?3 AND stage_id='replan_full-g1'",
                rusqlite::params![wrong_inspected.to_string(), order, format!("{order}:1:8:3")],
            )
            .unwrap();
        assert_eq!(result_evidence_rows(&mutation, order), clean_inspected_commit);

        let foreign_artifact: (String, String, String) = mutation
            .query_row(
                "SELECT artifact_id,source_instance_id,repository_key FROM product_artifacts_v1
                 WHERE order_id=?1 AND run_id=?2 AND stage_id='replan_full-g1'",
                rusqlite::params![order, format!("{order}:1:8:3")],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let clean_foreign_artifact = result_evidence_rows(&mutation, order);
        mutation
            .execute_batch("DROP TRIGGER product_artifact_no_update;")
            .unwrap();
        assert_eq!(
            mutation
                .execute(
                    "UPDATE product_artifacts_v1 SET source_instance_id='foreign-source-origin'
                     WHERE artifact_id=?1",
                    [&foreign_artifact.0],
                )
                .unwrap(),
            1
        );
        assert_eq!(
            result_store.finalize_product_review_result(&service, &expected),
            Err(foundry_ledger::ProductResultError::InvalidEvidence),
            "a fix artifact registered under a foreign source origin is rejected independently"
        );
        assert_eq!(
            result_evidence_rows(&mutation, order)
                .into_iter()
                .filter(|row| row.starts_with("artifact_tuples:") || row.starts_with("product_result_bindings_v1:"))
                .count(),
            0,
            "the foreign-origin artifact negative has no tuple/result side effects"
        );
        mutation
            .execute(
                "UPDATE product_artifacts_v1 SET source_instance_id=?1 WHERE artifact_id=?2",
                rusqlite::params![foreign_artifact.1, foreign_artifact.0],
            )
            .unwrap();
        mutation
            .execute_batch(
                "CREATE TRIGGER product_artifact_no_update BEFORE UPDATE ON product_artifacts_v1
                   BEGIN SELECT RAISE(ABORT,'product artifact binding is immutable'); END;",
            )
            .unwrap();
        assert_eq!(result_evidence_rows(&mutation, order), clean_foreign_artifact);
        assert!(!foreign_artifact.2.is_empty(), "the original registered repository key is present");
        let restored_artifact_id: String = mutation
            .query_row(
                "SELECT artifact_id FROM product_artifacts_v1 WHERE artifact_id=?1",
                [&fix_artifact],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(restored_artifact_id, fix_artifact);

        // Each continuation receipt, frozen hold snapshot, and progress row
        // is independently required by the result ancestry. Corrupt them one
        // at a time on this genuine pre-finalization baseline, then restore
        // the exact bytes before the next mutation.
        let original_receipt: String = mutation
            .query_row(
                "SELECT receipt_json FROM product_review_hold_acceptances_v1 WHERE operation_id=?1",
                [&first_receipt.operation_id],
                |row| row.get(0),
            )
            .unwrap();
        let original_acceptance: foundry_ledger::ProductHoldAcceptance =
            serde_json::from_str(&original_receipt).unwrap();
        let mut changed_acceptances = Vec::new();
        macro_rules! changed_acceptance {
            ($field:ident, $value:expr) => {{
                let mut changed = original_acceptance.clone();
                changed.$field = $value;
                changed_acceptances.push((stringify!($field), changed));
            }};
        }
        changed_acceptance!(operation_id, format!("{}-changed", original_acceptance.operation_id));
        changed_acceptance!(actor_id, format!("{}-changed", original_acceptance.actor_id));
        changed_acceptance!(order_id, format!("{}-changed", original_acceptance.order_id));
        changed_acceptance!(lifecycle, original_acceptance.lifecycle + 1);
        changed_acceptance!(hold_instance_id, format!("{}-changed", original_acceptance.hold_instance_id));
        changed_acceptance!(hold_revision, original_acceptance.hold_revision + 1);
        changed_acceptance!(attempt, original_acceptance.attempt + 1);
        changed_acceptance!(originating_run, original_acceptance.originating_run + 1);
        changed_acceptance!(activation, original_acceptance.activation + 1);
        changed_acceptance!(host_epoch, original_acceptance.host_epoch + 1);
        changed_acceptance!(decision, foundry_ledger::ProductHoldDecision::CommitPass);
        changed_acceptance!(source_revision, original_acceptance.source_revision + 1);
        changed_acceptance!(authority_revision, original_acceptance.authority_revision + 1);
        changed_acceptance!(source_instance_id, format!("{}-changed", original_acceptance.source_instance_id));
        changed_acceptance!(scope_revision, original_acceptance.scope_revision + 1);
        changed_acceptance!(grant_revision, original_acceptance.grant_revision + 1);
        changed_acceptance!(predecessor_seq, original_acceptance.predecessor_seq + 1);
        changed_acceptance!(reserved_run_id, original_acceptance.reserved_run_id + 1);
        changed_acceptance!(accepted_at, format!("{}-changed", original_acceptance.accepted_at));
        mutation
            .execute_batch("DROP TRIGGER product_review_acceptance_no_update;")
            .unwrap();
        for (member, changed) in changed_acceptances {
            let changed_json = serde_json::to_string(&changed).unwrap();
            assert_ne!(changed_json, original_receipt, "G08 acceptance member changes: {member}");
            mutation
                .execute(
                    "UPDATE product_review_hold_acceptances_v1 SET receipt_json=?1 WHERE operation_id=?2",
                    rusqlite::params![changed_json, first_receipt.operation_id],
                )
                .unwrap();
            let c0_before = result_evidence_rows(&mutation, order);
            assert_eq!(
                result_store.finalize_product_review_result(&service, &expected),
                Err(foundry_ledger::ProductResultError::CorruptEvidence),
                "independent accepted-hold wire member corruption is rejected: {member}"
            );
            assert_eq!(
                result_evidence_rows(&mutation, order), c0_before,
                "each rejected accepted-hold member preserves the complete C0 durable state: {member}"
            );
            mutation
                .execute(
                    "UPDATE product_review_hold_acceptances_v1 SET receipt_json=?1 WHERE operation_id=?2",
                    rusqlite::params![original_receipt, first_receipt.operation_id],
                )
                .unwrap();
        }
        let mut foreign_receipt: foundry_ledger::ProductHoldAcceptance =
            serde_json::from_str(&original_receipt).unwrap();
        foreign_receipt.order_id = "foreign-order".into();
        mutation
            .execute(
                "UPDATE product_review_hold_acceptances_v1 SET receipt_json=?1 WHERE operation_id=?2",
                rusqlite::params![serde_json::to_string(&foreign_receipt).unwrap(), first_receipt.operation_id],
            )
            .unwrap();
        assert_eq!(
            result_store.finalize_product_review_result(&service, &expected),
            Err(foundry_ledger::ProductResultError::CorruptEvidence),
            "a foreign hold receipt must not satisfy continuation ancestry"
        );
        mutation
            .execute(
                "UPDATE product_review_hold_acceptances_v1 SET receipt_json=?1 WHERE operation_id=?2",
                rusqlite::params![original_receipt, first_receipt.operation_id],
            )
            .unwrap();
        mutation
            .execute_batch(
                "CREATE TRIGGER product_review_acceptance_no_update BEFORE UPDATE ON product_review_hold_acceptances_v1 BEGIN SELECT RAISE(ABORT,'product review acceptance is immutable'); END;",
            )
            .unwrap();

        let original_progress: (String, i64, String, i64, i64, i64, Option<String>) = mutation
            .query_row(
                "SELECT phase,progress_revision,owner_id,epoch,fence,reserved_run_id,recovery_reason
                 FROM product_review_hold_progress_v1 WHERE operation_id=?1",
                [&first_receipt.operation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
            )
            .unwrap();
        mutation
            .execute(
                "UPDATE product_review_hold_progress_v1 SET fence=fence+1 WHERE operation_id=?1",
                [&first_receipt.operation_id],
            )
            .unwrap();
        assert_eq!(
            result_store.finalize_product_review_result(&service, &expected),
            Err(foundry_ledger::ProductResultError::CorruptEvidence),
            "a changed progress fence must not satisfy continuation ancestry"
        );
        mutation
            .execute(
                "UPDATE product_review_hold_progress_v1
                 SET phase=?1,progress_revision=?2,owner_id=?3,epoch=?4,fence=?5,
                     reserved_run_id=?6,recovery_reason=?7 WHERE operation_id=?8",
                rusqlite::params![
                    original_progress.0,
                    original_progress.1,
                    original_progress.2,
                    original_progress.3,
                    original_progress.4,
                    original_progress.5,
                    original_progress.6,
                    first_receipt.operation_id
                ],
            )
            .unwrap();

        // Complete the independent clean-A progress matrix. Every mutation
        // starts from the same genuine Resume ancestry, is restored exactly,
        // and is followed by a finalization attempt before the next case.
        let c0_progress_matrix = result_evidence_rows(&mutation, order);
        for (column, sql_value) in [
            ("phase", "'accepted'") ,
            ("owner_id", "'foreign-progress-owner'"),
        ] {
            let update = format!(
                "UPDATE product_review_hold_progress_v1 SET {column}={sql_value} WHERE operation_id=?1"
            );
            assert_eq!(mutation.execute(&update, [&first_receipt.operation_id]).unwrap(), 1);
            assert_eq!(
                result_store.finalize_product_review_result(&service, &expected),
                Err(foundry_ledger::ProductResultError::CorruptEvidence),
                "independent progress {column} mutation is rejected on clean A"
            );
            mutation.execute(
                "UPDATE product_review_hold_progress_v1
                 SET phase=?1,owner_id=?2 WHERE operation_id=?3",
                rusqlite::params![original_progress.0, original_progress.2, first_receipt.operation_id],
            ).unwrap();
            assert_eq!(result_evidence_rows(&mutation, order), c0_progress_matrix);
        }
        // progress_revision first-finalization vector removed per parent adjudication 2026-10-21
        // (Discord 1562410025484296274, option A): rubric G10 enumerates progress phase/owner/fence
        // only, and the frozen v3 contract defines progress_revision solely as a recorded payload
        // field — no independent durable original exists at first finalization, so a rejection
        // assertion there has no contract basis. Replay-side progress_revision corruption remains
        // covered by validate_stored_hold_progress_in + the prior-payload comparison.
        for (column, value) in [
            ("epoch", original_progress.3 + 1),
            ("reserved_run_id", original_progress.5 + 1),
        ] {
            let update = format!(
                "UPDATE product_review_hold_progress_v1 SET {column}=?1 WHERE operation_id=?2"
            );
            assert_eq!(mutation.execute(&update, rusqlite::params![value, first_receipt.operation_id]).unwrap(), 1);
            assert_eq!(
                result_store.finalize_product_review_result(&service, &expected),
                Err(foundry_ledger::ProductResultError::CorruptEvidence),
                "independent progress {column} mutation is rejected on clean A"
            );
            mutation.execute(
                "UPDATE product_review_hold_progress_v1
                 SET progress_revision=?1,epoch=?2,reserved_run_id=?3 WHERE operation_id=?4",
                rusqlite::params![original_progress.1, original_progress.3, original_progress.5, first_receipt.operation_id],
            ).unwrap();
            assert_eq!(result_evidence_rows(&mutation, order), c0_progress_matrix);
        }

        let active_before: (String, String, i64) = mutation.query_row(
            "SELECT key_json,journal_run,epoch FROM product_review_active_v1 WHERE order_id=?1",
            [order], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        ).unwrap();
        for (column, value) in [
            ("key_json", "'{}'".to_owned()),
            ("journal_run", "'foreign-journal-run'".to_owned()),
        ] {
            let update = format!(
                "UPDATE product_review_active_v1 SET {column}={value} WHERE order_id=?1"
            );
            assert_eq!(mutation.execute(&update, [order]).unwrap(), 1);
            assert_eq!(
                result_store.finalize_product_review_result(&service, &expected),
                Err(foundry_ledger::ProductResultError::CorruptEvidence),
                "active key/journal mismatch is rejected on clean A: {column}"
            );
            mutation.execute(
                "UPDATE product_review_active_v1 SET key_json=?1,journal_run=?2,epoch=?3 WHERE order_id=?4",
                rusqlite::params![active_before.0, active_before.1, active_before.2, order],
            ).unwrap();
            assert_eq!(result_evidence_rows(&mutation, order), c0_progress_matrix);
        }
        assert_eq!(mutation.execute(
            "UPDATE product_review_active_v1 SET epoch=epoch+1 WHERE order_id=?1", [order],
        ).unwrap(), 1);
        assert_eq!(
            result_store.finalize_product_review_result(&service, &expected),
            Err(foundry_ledger::ProductResultError::CorruptEvidence),
            "active epoch mismatch is rejected on clean A"
        );
        mutation.execute(
            "UPDATE product_review_active_v1 SET key_json=?1,journal_run=?2,epoch=?3 WHERE order_id=?4",
            rusqlite::params![active_before.0, active_before.1, active_before.2, order],
        ).unwrap();
        assert_eq!(result_evidence_rows(&mutation, order), c0_progress_matrix);

        let (snapshot_json, snapshot_digest): (String, String) = mutation
            .query_row(
                "SELECT snapshot_json,snapshot_digest FROM product_review_holds_v1 WHERE hold_instance_id=?1",
                [&first_receipt.hold_instance_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        mutation
            .execute_batch("DROP TRIGGER product_review_hold_exact_cas;")
            .unwrap();
        mutation
            .execute(
                "UPDATE product_review_holds_v1 SET snapshot_json='{}' WHERE hold_instance_id=?1",
                [&first_receipt.hold_instance_id],
            )
            .unwrap();
        assert_eq!(
            result_store.finalize_product_review_result(&service, &expected),
            Err(foundry_ledger::ProductResultError::CorruptEvidence),
            "a changed frozen hold snapshot must not satisfy continuation ancestry"
        );
        mutation
            .execute(
                "UPDATE product_review_holds_v1 SET snapshot_json=?1,snapshot_digest=?2 WHERE hold_instance_id=?3",
                rusqlite::params![snapshot_json, snapshot_digest, first_receipt.hold_instance_id],
            )
            .unwrap();
        mutation
            .execute_batch(
                "CREATE TRIGGER product_review_hold_exact_cas BEFORE UPDATE ON product_review_holds_v1
                   WHEN OLD.state!='active' OR NEW.state!='accepted' OR NEW.order_id!=OLD.order_id OR NEW.lifecycle!=OLD.lifecycle OR NEW.attempt!=OLD.attempt OR NEW.originating_run!=OLD.originating_run OR NEW.activation!=OLD.activation OR NEW.host_epoch!=OLD.host_epoch OR NEW.hold_instance_id!=OLD.hold_instance_id OR NEW.hold_revision!=OLD.hold_revision OR NEW.expected_predecessor!=OLD.expected_predecessor OR NEW.snapshot_digest!=OLD.snapshot_digest OR NEW.snapshot_json!=OLD.snapshot_json OR NEW.allowed_targets_json!=OLD.allowed_targets_json OR NEW.deadline!=OLD.deadline OR NEW.source_revision!=OLD.source_revision OR NEW.admission_seq!=OLD.admission_seq OR NEW.admission_hash!=OLD.admission_hash
                   BEGIN SELECT RAISE(ABORT,'product review hold CAS binding changed'); END;",
            )
            .unwrap();

        // A real account authority ABA must not revive the human revision
        // captured in the immutable hold receipt.
        // Capture the account row first so the ABA state can be restored below;
        // the subsequent genuine finalization requires revision == receipt.authority_revision.
        let account_before: (String, i64, i64) = mutation
            .query_row(
                "SELECT role,revision,enabled FROM product_accounts_v1 WHERE actor_id=?1",
                [&first_receipt.actor_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let resolve_grant_before: (i64, i64) = mutation
            .query_row(
                "SELECT grant_revision,enabled FROM product_scope_grants_v1
                 WHERE subject_id=?1 AND source_instance_id=?2 AND capability='resolve_review_hold'",
                rusqlite::params![first_receipt.actor_id, first_receipt.source_instance_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        store.set_account_authority(
            &fixture.owner_session_digest, &first_receipt.actor_id, "operator", true, now,
        ).unwrap();
        store.set_account_authority(
            &fixture.owner_session_digest, &first_receipt.actor_id, "approver", true, now,
        ).unwrap();
        assert_eq!(
            result_store.finalize_product_review_result(&service, &expected),
            Err(foundry_ledger::ProductResultError::StaleBinding),
            "completed pending lineage rejects human account role ABA"
        );
        // Lookup must NOT reapply new-finalization authority revisions (frozen contract
        // line 1219); with a consistent snapshot and no stored result it returns proven
        // absence (contract line 332). The finalize-side ABA rejection above carries the
        // G19 obligation; this pins the contract-mandated lookup semantics.
        assert_eq!(
            result_store.lookup_product_review_result(&service, &expected),
            Ok(None),
            "lookup does not reapply finalization authority revisions and returns proven absence"
        );

        // Restore the account before probing scope so account currentness
        // cannot short-circuit either independent scope predicate.
        mutation
            .execute(
                "UPDATE product_accounts_v1 SET role=?1,revision=?2,enabled=?3 WHERE actor_id=?4",
                rusqlite::params![
                    account_before.0,
                    account_before.1,
                    account_before.2,
                    first_receipt.actor_id
                ],
            )
            .unwrap();
        mutation
            .execute(
                "UPDATE product_scope_grants_v1 SET grant_revision=?1,enabled=?2
                 WHERE subject_id=?3 AND source_instance_id=?4 AND capability='resolve_review_hold'",
                rusqlite::params![resolve_grant_before.0, resolve_grant_before.1, first_receipt.actor_id, first_receipt.source_instance_id],
            )
            .unwrap();

        let accepted: String = mutation
            .query_row(
                "SELECT receipt_json FROM product_review_hold_acceptances_v1 WHERE operation_id=?1",
                [&first_receipt.operation_id],
                |row| row.get(0),
            )
            .unwrap();
        let accepted: foundry_ledger::ProductHoldAcceptance =
            serde_json::from_str(&accepted).unwrap();
        let _human_scope_revision: i64 = mutation
            .query_row(
                "SELECT revision FROM product_scope_subject_revisions_v1
                 WHERE subject_id=?1 AND source_instance_id=?2",
                rusqlite::params![accepted.actor_id, accepted.source_instance_id],
                |row| row.get(0),
            )
            .unwrap();
        let _human_grant_revision: i64 = mutation
            .query_row(
                "SELECT grant_revision FROM product_scope_grants_v1
                 WHERE subject_id=?1 AND source_instance_id=?2 AND capability='resolve_review_hold'",
                rusqlite::params![accepted.actor_id, accepted.source_instance_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            mutation
                .execute(
                    "UPDATE product_scope_subject_revisions_v1 SET revision=?1
                     WHERE subject_id=?2 AND source_instance_id=?3",
                    rusqlite::params![accepted.scope_revision + 1, accepted.actor_id, accepted.source_instance_id],
                )
                .unwrap(),
            1
        );
        assert_eq!(
            result_store.finalize_product_review_result(&service, &expected),
            Err(foundry_ledger::ProductResultError::StaleBinding),
            "completed-pending lineage independently rejects a changed human scope revision"
        );
        assert_eq!(result_store.lookup_product_review_result(&service, &expected), Ok(None));
        assert_eq!(
            mutation
                .query_row::<i64, _, _>(
                    "SELECT count(*) FROM artifact_tuples WHERE order_id=?1",
                    [order],
                    |row| row.get(0),
                )
                .unwrap(),
            0
        );
        mutation
            .execute(
                "UPDATE product_scope_subject_revisions_v1 SET revision=?1
                 WHERE subject_id=?2 AND source_instance_id=?3",
                rusqlite::params![
                    accepted.scope_revision,
                    accepted.actor_id,
                    accepted.source_instance_id
                ],
            )
            .unwrap();
        assert_eq!(
            mutation
                .execute(
                    "UPDATE product_scope_grants_v1 SET grant_revision=?1
                     WHERE subject_id=?2 AND source_instance_id=?3 AND capability='resolve_review_hold'",
                    rusqlite::params![accepted.grant_revision + 1, accepted.actor_id, accepted.source_instance_id],
                )
                .unwrap(),
            1
        );
        assert_eq!(
            result_store.finalize_product_review_result(&service, &expected),
            Err(foundry_ledger::ProductResultError::StaleBinding),
            "completed-pending lineage independently rejects a changed ResolveReviewHold grant revision"
        );
        assert_eq!(result_store.lookup_product_review_result(&service, &expected), Ok(None));
        mutation
            .execute(
                "UPDATE product_scope_grants_v1 SET grant_revision=?1
                 WHERE subject_id=?2 AND source_instance_id=?3 AND capability='resolve_review_hold'",
                rusqlite::params![
                    accepted.grant_revision,
                    accepted.actor_id,
                    accepted.source_instance_id
                ],
            )
            .unwrap();
        if script == ReviewScript::ResumePass {
        let reservation_before: i64 = mutation
            .query_row(
                "SELECT followup_run_id FROM product_review_run_reservations_v1 WHERE operation_id=?1",
                [&first_receipt.operation_id],
                |row| row.get(0),
            )
            .unwrap();
        let clean_origin_reservation = result_evidence_rows(&mutation, order);
        assert_eq!(
            mutation
                .execute(
                    "UPDATE product_review_run_reservations_v1 SET followup_run_id=?1 WHERE operation_id=?2",
                    rusqlite::params![reservation_before + 99, first_receipt.operation_id],
                )
                .unwrap(),
            1
        );
        assert_eq!(
            result_store.finalize_product_review_result(&service, &expected),
            Err(foundry_ledger::ProductResultError::Ineligible),
            "a reservation whose followup origin disagrees with the accepted receipt is rejected as ineligible ancestry"
        );
        assert_eq!(
            result_evidence_rows(&mutation, order)
                .into_iter()
                .filter(|row| row.starts_with("artifact_tuples:") || row.starts_with("product_result_bindings_v1:"))
                .count(),
            0,
            "origin/reservation mismatch rejects before result materialization"
        );
        mutation
            .execute(
                "UPDATE product_review_run_reservations_v1 SET followup_run_id=?1 WHERE operation_id=?2",
                rusqlite::params![reservation_before, first_receipt.operation_id],
            )
            .unwrap();
        assert_eq!(result_evidence_rows(&mutation, order), clean_origin_reservation);
        }
        drop(mutation);
    }
    if corrupt_legacy_edge {
        // Start with a genuine Resume result chain. These deliberate SQL
        // mutations simulate legacy census membership and damage one edge
        // reachable from it; validation must inspect that edge before it
        // classifies missing authority as legacy-ineligible.
        let corrupt = Connection::open(&db).unwrap();
        corrupt
            .execute_batch(
                "DROP TRIGGER product_result_legacy_hosts_no_insert;
                 DROP TRIGGER product_review_authority_no_delete;
                 DROP TRIGGER product_review_acceptance_no_update;
                 INSERT INTO product_result_legacy_hosts_v1(order_id,attempt,run_id,activation)
                   VALUES('p2:legacy-corrupt',1,7,3);
                 DELETE FROM product_review_authority_v1 WHERE order_id='p2:legacy-corrupt';
                 UPDATE product_review_hold_acceptances_v1
                   SET receipt_json=receipt_json || ' '
                   WHERE operation_id=(SELECT operation_id FROM product_review_hold_acceptances_v1
                     WHERE order_id='p2:legacy-corrupt' ORDER BY rowid LIMIT 1);",
            )
            .unwrap();
        let corrupted: String = corrupt
            .query_row(
                "SELECT receipt_json FROM product_review_hold_acceptances_v1
                 WHERE order_id='p2:legacy-corrupt' ORDER BY rowid LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(corrupted.ends_with(' '), "corruption injection must take effect");
        drop(corrupt);
        assert_eq!(
            result_store.finalize_product_review_result(&service, &expected),
            Err(foundry_ledger::ProductResultError::CorruptEvidence),
            "a corrupt accepted edge reachable from a census root is corruption, not legacy ineligibility"
        );
        let absent: (i64, i64) = Connection::open(&db)
            .unwrap()
            .query_row(
                "SELECT (SELECT count(*) FROM artifact_tuples WHERE order_id='p2:legacy-corrupt'),
                        (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id='p2:legacy-corrupt')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(absent, (0, 0), "corrupt history must not be finalized");
        drop(result_store);
        drop(host);
        std::fs::remove_dir_all(root).unwrap();
        return;
    }
    if let Some(same_owner) = reclaim_same_owner {
        let worker_id = if same_owner {
            fixture.claim.review_binding().worker_id
        } else {
            format!("{order}-replacement-worker")
        };
        let connection = Connection::open(&db).unwrap();
        let old_work: (String, i64, i64, String, String, i64) = connection
            .query_row(
                "SELECT c.owner_id,c.epoch,c.fence,c.state,c.lease_until,b.attempts
                 FROM product_work_claims_v1 c JOIN product_work_backlog_v1 b USING(order_id,lifecycle)
                 WHERE c.order_id=?1 AND c.lifecycle=1",
                [order],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .unwrap();
        assert_eq!(
            old_work.3, "claimed",
            "frozen Resume and CommitPass results retain a claimed work backlog"
        );
        let result_counts: (i64, i64) = connection
            .query_row(
                "SELECT (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                        (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1)",
                [order],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(result_counts, (0, 0));
        let mut replacement_store = ProductStore::open(&db, fixture.config.clone()).unwrap();
        let replacement = match replacement_store
            .claim_product_work_for_service(
                &service,
                &worker_id,
                now + Duration::minutes(6),
                Duration::minutes(5),
            )
            .unwrap()
        {
            foundry_ledger::ProductWorkClaimResult::Claimed(claim) => claim,
            other => panic!("completed result lease should be publicly reclaimable: {other:?}"),
        };
        let replacement_binding = replacement.review_binding();
        assert_eq!(replacement_binding.epoch, 2, "reclaim advances the claim epoch");
        assert!(replacement_binding.fence > old_work.2, "reclaim advances the global fence");
        assert_eq!(
            replacement_binding.fence,
            if script == ReviewScript::ResumeCommitPass { 6 } else { 4 },
            "the A/B reclaim fence follows the frozen deterministic fixture coordinates"
        );
        assert_eq!(replacement_binding.worker_id, worker_id);
        assert_eq!(old_work.0 == worker_id, same_owner);
        let new_attempts: i64 = connection
            .query_row(
                "SELECT attempts FROM product_work_backlog_v1 WHERE order_id=?1 AND lifecycle=1",
                [order],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(new_attempts, old_work.5 + 1, "reclaim increments attempts once");
        // Snapshot taken AFTER the reclaim: the reclaim legitimately advances the
        // claim/backlog rows (asserted above), so the preservation baseline for the
        // refused finalization must reflect the post-reclaim durable state.
        let preserved = result_evidence_rows(&connection, order);
        let replacement_work_before_refusal = work_claim_backlog_state(&connection, order);
        let error = result_store
            .finalize_product_review_result(&service, &expected)
            .expect_err("the pre-reclaim result cannot finalize under a replacement claim");
        assert_eq!(error, foundry_ledger::ProductResultError::StaleBinding);
        assert_eq!(
            result_evidence_rows(&connection, order),
            preserved,
            "rejected finalization preserves genuine old stage and result evidence"
        );
        assert_eq!(
            work_claim_backlog_state(&connection, order),
            replacement_work_before_refusal,
            "refused finalization preserves the replacement claim and backlog"
        );
        let after_rejection: (i64, i64) = connection
            .query_row(
                "SELECT (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                        (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1)",
                [order],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(after_rejection, (0, 0), "stale finalization has T/R +0/+0");
        assert_eq!(fixture.calls.load(Ordering::SeqCst), if script == ReviewScript::Passing { 4 } else { 14 });
        assert_eq!(fixture.restores.load(Ordering::SeqCst), if script == ReviewScript::Passing { 0 } else { 3 });
        drop(connection);
        drop(replacement_store);
        drop(result_store);
        drop(host);
        std::fs::remove_dir_all(root).unwrap();
        return;
    }

    let result = result_store
        .finalize_product_review_result(&service, &expected)
        .unwrap();
    assert_eq!(
        Connection::open(&db)
            .unwrap()
            .query_row::<i64, _, _>("SELECT count(*) FROM ledger", [], |row| row.get(0))
            .unwrap(),
        ledger_before,
        "result finalization must not append S1"
    );
    if terminal_run == 8 {
        assert_eq!(
            (
                calls.load(Ordering::SeqCst),
                restores.load(Ordering::SeqCst)
            ),
            (14, 3)
        );
        assert_eq!(result.artifact_ids.len(), 14);
    } else {
        assert_eq!(
            (
                calls.load(Ordering::SeqCst),
                restores.load(Ordering::SeqCst)
            ),
            (4, 0)
        );
        assert_eq!(result.artifact_ids.len(), 4);
    }

    let unused_successor = if terminal_run == 7 { 8 } else { 9 };
    let conn = Connection::open(&db).unwrap();
    let counts: (i64, i64, i64, i64, i64, i64, i64) = conn
        .query_row(
            "SELECT
               (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND status='complete'),
               (SELECT count(*) FROM product_artifacts_v1 WHERE order_id=?1),
               (SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?1),
               (SELECT used FROM product_review_budget_v1 WHERE order_id=?1),
               (SELECT count(*) FROM product_review_run_reservations_v1 WHERE order_id=?1),
               (SELECT count(*) FROM product_review_hold_progress_v1 p JOIN product_review_hold_acceptances_v1 a USING(operation_id) WHERE a.order_id=?1 AND p.phase='completed'),
               (SELECT count(*) FROM product_review_runs_v1 WHERE order_id=?1 AND run_id=?2)",
            rusqlite::params![order, format!("{order}:1:{unused_successor}:3")],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?)),
        )
        .unwrap();
    let expected_count = if terminal_run == 8 { 14 } else { 4 };
    let expected_reservations = if second_receipt.is_some() { 2 } else { 1 };
    assert_eq!(
        counts,
        (
            expected_count,
            expected_count,
            if terminal_run == 8 { 8 } else { 3 },
            expected_count,
            expected_reservations,
            expected_reservations,
            0
        )
    );
    let successor_census: (i64,i64,i64,i64,i64,i64,i64,i64,i64) = conn.query_row(
        "SELECT (SELECT count(*) FROM product_review_host_accept_v1 WHERE order_id=?1 AND run_id=?2),
                (SELECT count(*) FROM product_review_host_results_v1 WHERE order_id=?1 AND run_id=?2),
                (SELECT count(*) FROM product_review_runs_v1 WHERE order_id=?1 AND run_id=?3),
                (SELECT count(*) FROM product_review_work_bindings_v1 WHERE order_id=?1 AND run_id=?3),
                (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND run_id=?3),
                (SELECT count(*) FROM product_artifacts_v1 WHERE order_id=?1 AND run_id=?3),
                (SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?1 AND run_id=?3),
                (SELECT count(*) FROM product_review_authority_v1 WHERE order_id=?1 AND run_id=?2),
                (SELECT count(*) FROM product_review_run_reservations_v1 WHERE order_id=?1 AND followup_run_id=?2)",
        rusqlite::params![order, unused_successor, format!("{order}:1:{unused_successor}:3")],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?)),
    ).unwrap();
    let reserved_unused_successor = if terminal_run == 7 || second_receipt.is_some() { 1 } else { 0 };
    assert_eq!(
        successor_census,
        (0,0,0,0,0,0,0,0,reserved_unused_successor),
        "the terminal CommitPass successor is reserved at most once and has no materialized host/runtime rows"
    );
    let reservation_allocator_before_replay: i64 = conn.query_row(
        "SELECT count(*) FROM product_review_run_reservations_v1 WHERE order_id=?1", [order], |r| r.get(0),
    ).unwrap();
    assert_eq!(host.dispatch_count(), 1);
    let payload_json: String = conn
        .query_row(
            "SELECT payload_json FROM product_result_bindings_v1 WHERE result_id=?1",
            [&result.result_id],
            |row| row.get(0),
        )
        .unwrap();
    let payload: serde_json::Value = serde_json::from_str(&payload_json).unwrap();
    let holds = payload["accepted_holds"].as_array().unwrap();
    assert_eq!(holds.len(), expected_reservations as usize);
    assert_eq!(
        payload["artifacts"].as_array().unwrap().len(),
        expected_count as usize
    );
    let artifact_order: Vec<(String, i64)> = payload["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|artifact| {
            (
                artifact["journal_run"].as_str().unwrap().to_owned(),
                artifact["stage_sequence"].as_i64().unwrap(),
            )
        })
        .collect();
    let expected_artifact_order: Vec<(String, i64)> = if terminal_run == 8 {
        (1..=4)
            .map(|n| (format!("{order}:1:7:3"), n))
            .chain((1..=10).map(|n| (format!("{order}:1:8:3"), n)))
            .collect()
    } else {
        (1..=4).map(|n| (format!("{order}:1:7:3"), n)).collect()
    };
    assert_eq!(artifact_order, expected_artifact_order);
    assert_eq!(payload["tuple"]["run"], terminal_run);
    assert_eq!(payload["tuple"]["generation"], 0);
    for (index, hold) in holds.iter().enumerate() {
        let operation_id = hold["operation_id"].as_str().unwrap();
        let acceptance: String = conn
            .query_row(
                "SELECT receipt_json FROM product_review_hold_acceptances_v1 WHERE operation_id=?1",
                [operation_id],
                |row| row.get(0),
            )
            .unwrap();
        let frozen_digest: String = conn
            .query_row(
                "SELECT snapshot_digest FROM product_review_holds_v1 WHERE hold_instance_id=(SELECT hold_instance_id FROM product_review_hold_acceptances_v1 WHERE operation_id=?1)",
                [operation_id],
                |row| row.get(0),
            )
            .unwrap();
        let progress: (String, i64, String, i64, i64, i64, Option<String>) = conn
            .query_row(
                "SELECT phase,progress_revision,owner_id,epoch,fence,reserved_run_id,recovery_reason FROM product_review_hold_progress_v1 WHERE operation_id=?1",
                [operation_id],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?)),
            )
            .unwrap();
        assert_eq!(hold["receipt_json"], acceptance);
        assert_eq!(hold["snapshot_digest"], frozen_digest);
        assert_eq!(progress.0, "completed");
        assert_eq!(progress.1, 3, "frozen progress revision is acceptance + claim + completion");
        assert_eq!(progress.3, 0, "frozen initial owner epoch");
        assert_eq!(progress.4, index as i64 * 2 + 3, "frozen initial owner fence");
        assert_eq!(progress.5, index as i64 + 8, "frozen reserved successor run");
        assert_eq!(hold["progress_revision"], progress.1);
        assert_eq!(hold["owner_id"], progress.2);
        assert_eq!(hold["epoch"], progress.3);
        assert_eq!(hold["fence"], progress.4);
        assert_eq!(hold["reserved_run_id"], progress.5);
        assert_eq!(
            progress.2,
            format!("{operation_id}:{}:{}", progress.3, progress.4)
        );
        assert!(progress.6.is_none());
        if index > 0 {
            let previous = holds[index - 1]["receipt_json"].as_str().unwrap();
            let previous: serde_json::Value = serde_json::from_str(previous).unwrap();
            let current: serde_json::Value = serde_json::from_str(&acceptance).unwrap();
            assert!(
                previous["originating_run"].as_i64().unwrap()
                    <= current["originating_run"].as_i64().unwrap()
            );
        }
    }
    let producer_census: Vec<(i64, i64, i64, i64)> = [7_i64, 8, 9]
        .into_iter()
        .map(|run| conn.query_row(
            "SELECT (SELECT count(*) FROM product_review_host_accept_v1 WHERE order_id=?1 AND run_id=?2),
                    (SELECT count(*) FROM product_review_host_results_v1 WHERE order_id=?1 AND run_id=?2),
                    (SELECT count(*) FROM product_review_runs_v1 WHERE order_id=?1 AND run_id=?3),
                    (SELECT count(*) FROM product_review_work_bindings_v1 WHERE order_id=?1 AND run_id=?3)",
            rusqlite::params![order, run, format!("{order}:1:{run}:3")],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
        ).unwrap())
        .collect();
    let continuation_present = if terminal_run == 8 { 1 } else { 0 };
    assert_eq!(
        producer_census,
        vec![(1,1,1,1),(continuation_present,continuation_present,continuation_present,continuation_present),(0,0,0,0)],
        "both genuine host producers and the unmaterialized successor are censused independently"
    );
    assert_eq!(
        result_store
            .finalize_product_review_result(&service, &expected)
            .unwrap(),
        result
    );
    assert_eq!(
        conn.query_row::<i64,_,_>(
            "SELECT count(*) FROM product_review_run_reservations_v1 WHERE order_id=?1", [order], |r| r.get(0),
        ).unwrap(),
        reservation_allocator_before_replay,
        "result replay does not advance the reserved-run allocator"
    );
    if let Some(second) = second_receipt {
        assert_eq!(second.originating_run, 8);
        assert_eq!(second.reserved_run_id, 9);
        assert_eq!(result.run_id, 8);
        assert_eq!(
            result_store
                .finalize_product_review_result(&service, &expected)
                .unwrap(),
            result
        );
    }
    if order == "p2:A" {
        let (result_id, original_payload, original_receipt): (String, String, String) = conn
            .query_row(
                "SELECT result_id,payload_json,receipt_json FROM product_result_bindings_v1
                 WHERE order_id=?1",
                [order],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        conn.execute_batch(
            "CREATE TEMP TABLE saved_followup_authority AS
               SELECT * FROM product_review_authority_v1 WHERE order_id='p2:A' AND run_id=8;
             DROP TRIGGER product_review_authority_no_delete;",
        )
        .unwrap();
        assert_eq!(
            conn.execute(
                "DELETE FROM product_review_authority_v1 WHERE order_id='p2:A' AND run_id=8",
                [],
            )
            .unwrap(),
            1,
            "modern Resume copy snapshot is actually absent for this negative vector"
        );
        assert_eq!(
            result_store.lookup_product_review_result(&service, &expected),
            Err(foundry_ledger::ProductResultError::CorruptEvidence),
            "lookup classifies a modern Resume run with missing copied authority as corrupt"
        );
        assert_eq!(
            result_store.finalize_product_review_result(&service, &expected),
            Err(foundry_ledger::ProductResultError::CorruptEvidence),
            "strict replay finalization classifies a modern missing copy as corrupt"
        );
        let no_copy_effects: (i64, i64, String, String) = conn
            .query_row(
                "SELECT (SELECT count(*) FROM artifact_tuples WHERE order_id='p2:A'),
                        (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id='p2:A'),
                        (SELECT payload_json FROM product_result_bindings_v1 WHERE result_id=?1),
                        (SELECT receipt_json FROM product_result_bindings_v1 WHERE result_id=?1)",
                [&result_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(no_copy_effects, (1, 1, original_payload.clone(), original_receipt.clone()));
        conn.execute(
            "INSERT INTO product_review_authority_v1 SELECT * FROM saved_followup_authority",
            [],
        )
        .unwrap();
        conn.execute_batch(
            "CREATE TRIGGER product_review_authority_no_delete BEFORE DELETE ON product_review_authority_v1 BEGIN SELECT RAISE(ABORT,'review authority is immutable'); END;",
        )
        .unwrap();
        assert_eq!(
            conn.query_row::<i64, _, _>(
                "SELECT count(*) FROM product_review_authority_v1 WHERE order_id='p2:A' AND run_id=8",
                [],
                |r| r.get(0),
            )
            .unwrap(),
            1,
            "restored copied snapshot is present before acceptance-wire checks"
        );
        let payload_value: serde_json::Value = serde_json::from_str(&original_payload).unwrap();
        let accepted_hold_receipt = payload_value["accepted_holds"][0]["receipt_json"]
            .as_str()
            .unwrap();
        let encoded_hold_receipt = serde_json::to_string(accepted_hold_receipt).unwrap();
        let changed_hold_receipt = serde_json::to_string(&format!("{accepted_hold_receipt} "))
            .unwrap();
        assert!(original_payload.contains(&encoded_hold_receipt));
        let changed_payload =
            original_payload.replacen(&encoded_hold_receipt, &changed_hold_receipt, 1);
        assert_ne!(changed_payload, original_payload);
        conn.execute_batch("DROP TRIGGER product_result_bindings_no_update;")
            .unwrap();
        assert_eq!(
            conn.execute(
                "UPDATE product_result_bindings_v1 SET payload_json=?1 WHERE result_id=?2",
                rusqlite::params![changed_payload, result_id],
            )
            .unwrap(),
            1
        );
        let mutated_payload: String = conn
            .query_row(
                "SELECT payload_json FROM product_result_bindings_v1 WHERE result_id=?1",
                [&result_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_ne!(mutated_payload, original_payload, "accepted-hold wire mutation is persisted");
        assert_eq!(
            result_store.lookup_product_review_result(&service, &expected),
            Err(foundry_ledger::ProductResultError::CorruptEvidence),
            "the result lookup verifies the exact accepted-hold receipt bytes"
        );
        assert_eq!(
            result_store.finalize_product_review_result(&service, &expected),
            Err(foundry_ledger::ProductResultError::CorruptEvidence),
            "the strict finalizer rejects an independently mutated accepted-hold receipt"
        );
        let after_hold_wire_rejection: (i64, i64, i64, i64, String, String) = conn
            .query_row(
                "SELECT (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                        (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_outcomes_v1),
                        (SELECT count(*) FROM product_token_consumptions_v1),
                        (SELECT payload_json FROM product_result_bindings_v1 WHERE result_id=?2),
                        (SELECT receipt_json FROM product_result_bindings_v1 WHERE result_id=?2)",
                rusqlite::params![order, result_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .unwrap();
        assert_eq!(
            after_hold_wire_rejection,
            (1, 1, 0, 0, mutated_payload, original_receipt),
            "corrupt hold-wire rejection leaves the committed result and approval participants unchanged"
        );
        conn.execute(
            "UPDATE product_result_bindings_v1 SET payload_json=?1 WHERE result_id=?2",
            rusqlite::params![original_payload, result_id],
        )
        .unwrap();
        conn.execute_batch(
            "CREATE TRIGGER product_result_bindings_no_update BEFORE UPDATE ON product_result_bindings_v1 BEGIN SELECT RAISE(ABORT,'product result is immutable'); END;",
        )
        .unwrap();
        assert_eq!(
            result_store
                .finalize_product_review_result(&service, &expected)
                .unwrap(),
            result,
            "restoring exact accepted-hold bytes restores strict replay"
        );
    }
    drop(conn);
    drop(host);
    drop(result_store);
    std::fs::remove_dir_all(root).unwrap();
}

fn assert_pending_result_authority_matrix(
    fixture: &product_result_fixture::GenuineResultFixture,
    expected: &ProductResultExpectation,
) {
    let grants = [
        foundry_ledger::ProductServiceAction::IntakeRead,
        foundry_ledger::ProductServiceAction::AdmitDelivery,
        foundry_ledger::ProductServiceAction::AckDelivery,
        foundry_ledger::ProductServiceAction::DriveReview,
    ]
    .into_iter()
    .map(|action| foundry_ledger::ProductServiceGrant {
        source_instance_id: fixture.identity.source_instance_id.clone(),
        action,
    })
    .collect::<Vec<_>>();

    let accepted: foundry_ledger::ProductHoldAcceptance = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT receipt_json FROM product_review_hold_acceptances_v1
             WHERE order_id=?1 ORDER BY operation_id LIMIT 1",
            [&fixture.identity.order_id],
            |row| {
                serde_json::from_str(&row.get::<_, String>(0)?).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })
            },
        )
        .unwrap();

    for change in [
        "service",
        "credential",
        "source",
        "revoke_regrant",
        "human_account",
        "human_scope",
        "resolve_grant",
    ] {
        let db = fixture.root.join(format!("g19-{change}.sqlite"));
        Connection::open(&fixture.db_path)
            .unwrap()
            .execute("VACUUM INTO ?1", [db.to_string_lossy().as_ref()])
            .unwrap();
        let mut owner = ProductStore::open(&db, fixture.config.clone()).unwrap();
        let context = if change == "human_account" {
            assert_eq!(
                Connection::open(&db)
                    .unwrap()
                    .execute(
                        "UPDATE product_accounts_v1 SET revision=revision+1 WHERE actor_id=?1",
                        [&accepted.actor_id],
                    )
                    .unwrap(),
                1
            );
            fixture.service.clone()
        } else if change == "human_scope" {
            assert_eq!(
                Connection::open(&db)
                    .unwrap()
                    .execute(
                        "UPDATE product_scope_subject_revisions_v1 SET revision=?1
                         WHERE subject_id=?2 AND source_instance_id=?3",
                        rusqlite::params![accepted.scope_revision + 1, accepted.actor_id, accepted.source_instance_id],
                    )
                    .unwrap(),
                1
            );
            fixture.service.clone()
        } else if change == "resolve_grant" {
            assert_eq!(
                Connection::open(&db)
                    .unwrap()
                    .execute(
                        "UPDATE product_scope_grants_v1 SET grant_revision=?1
                         WHERE subject_id=?2 AND source_instance_id=?3 AND capability='resolve_review_hold'",
                        rusqlite::params![accepted.grant_revision + 1, accepted.actor_id, accepted.source_instance_id],
                    )
                    .unwrap(),
                1
            );
            fixture.service.clone()
        } else if change == "source" {
            let changed = Connection::open(&db)
                .unwrap()
                .execute(
                    "UPDATE product_sources_v1 SET revision=revision+1 WHERE connector_id=?1",
                    [&fixture.identity.connector_id],
                )
                .unwrap();
            assert_eq!(changed, 1, "the connector source revision changes on an isolated baseline");
            owner
                .authenticate_product_service_for_action(
                    &fixture.service_token,
                    &fixture.identity.connector_id,
                    &fixture.identity.api_origin,
                    &fixture.identity.repository_id,
                    foundry_ledger::ProductServiceAction::DriveReview,
                    fixture.now,
                )
                .unwrap()
        } else if change == "credential" {
            let service_revision: i64 = Connection::open(&db)
                .unwrap()
                .query_row(
                    "SELECT revision FROM product_services_v1 WHERE service_id=?1",
                    [&fixture.identity.service_id],
                    |row| row.get(0),
                )
                .unwrap();
            let credential = owner
                .issue_product_service_credential(
                    &fixture.owner_session_digest,
                    &fixture.identity.service_id,
                    service_revision,
                    fixture.now + Duration::hours(1),
                    "g19-credential-rotation",
                    fixture.now,
                )
                .unwrap();
            let token = credential.expose_once_to_file_writer().to_owned();
            owner
                .authenticate_product_service_for_action(
                    &token,
                    &fixture.identity.connector_id,
                    &fixture.identity.api_origin,
                    &fixture.identity.repository_id,
                    foundry_ledger::ProductServiceAction::DriveReview,
                    fixture.now,
                )
                .unwrap()
        } else {
            let current_revision: i64 = Connection::open(&db)
                .unwrap()
                .query_row(
                    "SELECT revision FROM product_services_v1 WHERE service_id=?1",
                    [&fixture.identity.service_id],
                    |row| row.get(0),
                )
                .unwrap();
            let service_revision = if change == "service" {
                owner
                    .manage_product_service(
                        &fixture.owner_session_digest,
                        &fixture.identity.service_id,
                        &grants,
                        true,
                        fixture.now + Duration::hours(3),
                        current_revision,
                        "g19-service-rotation",
                        fixture.now,
                    )
                    .unwrap()
                    .revision
            } else {
                let revoked = owner
                    .manage_product_service(
                        &fixture.owner_session_digest,
                        &fixture.identity.service_id,
                        &[],
                        false,
                        fixture.now + Duration::hours(3),
                        current_revision,
                        "g19-service-revoke",
                        fixture.now,
                    )
                    .unwrap();
                owner
                    .manage_product_service(
                        &fixture.owner_session_digest,
                        &fixture.identity.service_id,
                        &grants,
                        true,
                        fixture.now + Duration::hours(3),
                        revoked.revision,
                        "g19-service-regrant",
                        fixture.now,
                    )
                    .unwrap()
                    .revision
            };
            let credential = owner
                .issue_product_service_credential(
                    &fixture.owner_session_digest,
                    &fixture.identity.service_id,
                    service_revision,
                    fixture.now + Duration::hours(1),
                    "g19-service-current-credential",
                    fixture.now,
                )
                .unwrap();
            let token = credential.expose_once_to_file_writer().to_owned();
            owner
                .authenticate_product_service_for_action(
                    &token,
                    &fixture.identity.connector_id,
                    &fixture.identity.api_origin,
                    &fixture.identity.repository_id,
                    foundry_ledger::ProductServiceAction::DriveReview,
                    fixture.now,
                )
                .unwrap()
        };
        let mut results = ProductStore::open(&db, fixture.config.clone()).unwrap();
        results
            .configure_product_artifact_root(&fixture.artifact_root)
            .unwrap();
        assert_eq!(
            results.finalize_product_review_result(&context, expected),
            Err(foundry_ledger::ProductResultError::StaleBinding),
            "completed-pending {} authority change is rejected by finalization",
            change
        );
        assert_eq!(
            results.lookup_product_review_result(&context, expected),
            Ok(None),
            "completed-pending {} authority change leaves lookup at proven absence",
            change
        );
        let no_result: (i64, i64) = Connection::open(&db)
            .unwrap()
            .query_row(
                "SELECT (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                        (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1)",
                [&fixture.identity.order_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(no_result, (0, 0), "{} keeps tuple/result at C0", change);
        drop(context);
        drop(results);
        drop(owner);
    }
}

fn result_evidence_rows(connection: &Connection, order: &str) -> Vec<String> {
    let mut snapshot = Vec::new();
    for table in [
        "product_review_stages_v1",
        "product_artifacts_v1",
        "product_review_sessions_v1",
        "product_review_host_results_v1",
        "product_review_host_accept_v1",
        "product_review_work_bindings_v1",
        "product_review_active_v1",
        "product_review_runs_v1",
        "product_review_authority_v1",
        "product_review_hold_acceptances_v1",
        "product_work_claims_v1",
        "product_work_backlog_v1",
        "product_result_bindings_v1",
        "artifact_tuples",
        "product_transition_receipts_v1",
        "ledger",
    ] {
        let mut statement = connection
            .prepare(&format!("SELECT * FROM {table} WHERE order_id=?1 ORDER BY rowid"))
            .unwrap();
        let columns = statement.column_count();
        let rows = statement
            .query_map([order], |row| {
                let mut values = Vec::with_capacity(columns);
                for column in 0..columns {
                    values.push(format!("{:?}", row.get::<_, rusqlite::types::Value>(column)?));
                }
                Ok(values.join("\u{1f}"))
            })
            .unwrap();
        for row in rows {
            snapshot.push(format!("{table}:{}", row.unwrap()));
        }
    }
    snapshot
}

fn result_runtime_counts(connection: &Connection, order: &str) -> (i64, i64, i64, i64, i64, i64) {
    connection
        .query_row(
            "SELECT (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_artifacts_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_runs_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_run_reservations_v1 WHERE order_id=?1),
                    COALESCE((SELECT seq FROM sqlite_sequence WHERE name='product_review_run_reservations_v1'),0)",
            [order],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        )
        .unwrap()
}

fn result_evidence_rows_without_work(connection: &Connection, order: &str) -> Vec<String> {
    result_evidence_rows(connection, order)
        .into_iter()
        .filter(|row| {
            !row.starts_with("product_work_claims_v1:")
                && !row.starts_with("product_work_backlog_v1:")
        })
        .collect()
}

fn without_work_rows(rows: &[String]) -> Vec<String> {
    rows.iter()
        .filter(|row| {
            !row.starts_with("product_work_claims_v1:")
                && !row.starts_with("product_work_backlog_v1:")
        })
        .cloned()
        .collect()
}

fn work_claim_backlog_state(connection: &Connection, order: &str) -> (String, i64, i64, String, String, i64, i64, String) {
    connection.query_row(
        "SELECT c.owner_id,c.epoch,c.fence,c.state,c.lease_until,b.attempts,c.source_revision,b.state
         FROM product_work_claims_v1 c JOIN product_work_backlog_v1 b USING(order_id,lifecycle)
         WHERE c.order_id=?1 AND c.lifecycle=1",
        [order],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?)),
    ).unwrap()
}

fn read_authority_rows(
    db: &std::path::Path,
    order: &str,
) -> Vec<(String, (String, i64, i64, i64, String, i64, i64, i64))> {
    let conn = Connection::open(db).unwrap();
    let mut statement = conn
        .prepare(
            "SELECT key_json,service_id,service_revision,credential_revision,
                    drive_review_grant_revision,source_instance_id,source_generation,
                    source_instance_revision,connector_revision
             FROM product_review_authority_v1 WHERE order_id=?1 ORDER BY run_id",
        )
        .unwrap();
    statement
        .query_map([order], |row| {
            Ok((
                row.get(0)?,
                (
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ),
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn replace_host_result_evidence(
    db: &std::path::Path,
    key: &foundry_exec::HostKey,
    result: &foundry_exec::HostResult,
) {
    let conn = Connection::open(db).unwrap();
    let result_json = serde_json::to_string(result).unwrap();
    let journal_run = format!(
        "{}:{}:{}:{}",
        key.order, key.attempt, key.run, key.activation
    );
    assert_eq!(
        conn.execute(
            "UPDATE product_review_host_results_v1 SET result_json=?1
             WHERE order_id=?2 AND attempt=?3 AND run_id=?4 AND activation=?5",
            rusqlite::params![
                result_json,
                key.order,
                key.attempt as i64,
                key.run as i64,
                key.activation as i64
            ],
        )
        .unwrap(),
        1
    );
    assert_eq!(
        conn.execute(
            "UPDATE product_review_runs_v1 SET result_json=?1
             WHERE order_id=?2 AND attempt=?3 AND run_id=?4",
            rusqlite::params![result_json, key.order, key.attempt as i64, journal_run],
        )
        .unwrap(),
        1
    );
}
