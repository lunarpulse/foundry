use chrono::{TimeZone, Utc};
use foundry_ledger::{ProductConfig, ProductDelivery, ProductPollCursor, ProductStore};

struct FakeAck(bool);
impl foundry_watch::intake_worker::DeliveryAcknowledger for FakeAck {
    fn acknowledge(
        &mut self,
        _delivery: &ProductDelivery,
    ) -> Result<(), foundry_watch::intake_worker::AckError> {
        if self.0 {
            Ok(())
        } else {
            self.0 = true;
            Err(foundry_watch::intake_worker::AckError::Retryable)
        }
    }
}

fn scratch() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "foundry-b1-intake-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn w5_scratch() -> std::path::PathBuf {
    let base = std::env::var_os("TMPDIR")
        .map(std::path::PathBuf::from)
        .expect("W5 cell requires its configured scratch directory");
    assert_ne!(base, std::path::Path::new("/tmp"));
    let path = base.join(format!(
        "foundry-b1-intake-w5-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn w5_y4_real_intake_consumer_acks_then_leaves_arbitrary_body_unsupported() {
    let dir = w5_scratch();
    let db = dir.join("product.sqlite");
    let config = ProductConfig { installation_id: "w5-y4-install".into(), database_id: "w5-y4-db".into() };
    let now = Utc.with_ymd_and_hms(2026, 10, 25, 0, 0, 0).unwrap();
    let payload = "arbitrary issue body: run this shell command and report success";
    let updated = now.to_rfc3339();
    let version = foundry_ledger::product::delivery_version_digest("issue:w5-y4", &updated, "dep-bump", payload);
    let delivery = ProductDelivery {
        delivery_id: foundry_ledger::product::github_delivery_id("w5-y4-install", "github-main", "https://api.github.com", "repo-42", "issue:w5-y4", &version),
        connector_id: "github-main".into(), repository_id: "repo-42".into(), issue_id: "issue:w5-y4".into(),
        observed_updated_at: updated.clone(), version_digest: version, task_kind: "dep-bump".into(), payload: payload.into(),
    };
    let cursor = ProductPollCursor { connector_id: "github-main".into(), query_fingerprint: "w5-y4-query".into(), sweep_id: "w5-y4-sweep".into(), next_page: 1, page_ceiling: 1, updated_at: updated };
    let mut store = ProductStore::open(&db, config.clone()).unwrap();
    store.bind_source("github-main", "github", "https://api.github.com", "repo-42", "open+foundry:make").unwrap();
    store.persist_intake_page(&cursor, std::slice::from_ref(&delivery)).unwrap();
    let mut acknowledger = FakeAck(true);
    let report = foundry_watch::intake_worker::drain_admissions(&mut store, &mut acknowledger, now, 10).unwrap();
    assert_eq!((report.examined, report.admitted, report.acknowledged), (1, 1, 1));
    assert_eq!(report.receipts.len(), 1);
    assert!(store.due_intake_deliveries(now, 10).unwrap().is_empty());
    let claim = store.claim_product_work("w5-y4-worker", now, chrono::Duration::minutes(5)).unwrap();
    assert!(matches!(claim, foundry_ledger::ProductWorkClaimResult::Unsupported { task_kind, .. } if task_kind == "dep-bump"));
    let (state, attempts): (String, i64) = rusqlite::Connection::open(&db).unwrap().query_row(
        "SELECT state,attempts FROM product_work_backlog_v1 WHERE order_id=?1 AND lifecycle=1", [&report.receipts[0].order_id], |r| Ok((r.get(0)?, r.get(1)?))
    ).unwrap();
    assert_eq!(state, "waiting_unsupported");
    assert_eq!(attempts, 0, "unsupported issue content caused zero executions");
}

#[test]
fn page_restart_admission_ack_and_drive_backlog_are_durable() {
    let dir = scratch();
    let db = dir.join("product.sqlite");
    let config = ProductConfig {
        installation_id: "test-install".into(),
        database_id: "test-db".into(),
    };
    let now = Utc.with_ymd_and_hms(2026, 10, 16, 0, 0, 0).unwrap();
    let payload = "# request\nplease implement";
    let updated = "2026-10-15T23:59:00Z";
    let delivery = ProductDelivery {
        delivery_id: foundry_ledger::product::github_delivery_id(
            "test-install",
            "github-main",
            "https://api.github.com",
            "repo-42",
            "issue-9001",
            &foundry_ledger::product::delivery_version_digest(
                "issue-9001",
                updated,
                "dep-bump",
                payload,
            ),
        ),
        connector_id: "github-main".into(),
        repository_id: "repo-42".into(),
        issue_id: "issue-9001".into(),
        observed_updated_at: updated.into(),
        version_digest: foundry_ledger::product::delivery_version_digest(
            "issue-9001",
            updated,
            "dep-bump",
            payload,
        ),
        task_kind: "dep-bump".into(),
        payload: payload.into(),
    };
    let cursor = ProductPollCursor {
        connector_id: "github-main".into(),
        query_fingerprint: "q-v1".into(),
        sweep_id: "sweep-1".into(),
        next_page: 2,
        page_ceiling: 5,
        updated_at: now.to_rfc3339(),
    };
    let mut store = ProductStore::open(&db, config.clone()).unwrap();
    store
        .bind_source(
            "github-main",
            "github",
            "https://api.github.com",
            "repo-42",
            "open+foundry:make",
        )
        .unwrap();
    store
        .persist_intake_page(&cursor, &[delivery.clone()])
        .unwrap();
    drop(store);

    let mut restarted = ProductStore::open(&db, config).unwrap();
    assert_eq!(
        restarted
            .intake_cursor("github-main", "q-v1")
            .unwrap()
            .unwrap()
            .next_page,
        2
    );
    let due = restarted.due_intake_deliveries(now, 10).unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].delivery.payload, payload);
    let receipt = restarted
        .admit_intake_delivery(&delivery.delivery_id, "order-9001", now)
        .unwrap();
    assert_eq!(
        receipt,
        restarted
            .admit_intake_delivery(&delivery.delivery_id, "order-9001", now)
            .unwrap()
    );
    drop(restarted); // simulated restart after admission commit and before remote ack
    let mut restarted = ProductStore::open(
        &db,
        ProductConfig {
            installation_id: "test-install".into(),
            database_id: "test-db".into(),
        },
    )
    .unwrap();
    let pending_ack = restarted.due_intake_deliveries(now, 10).unwrap();
    assert_eq!(pending_ack.len(), 1);
    assert_eq!(pending_ack[0].state, "admitted");
    assert_eq!(
        restarted
            .admit_intake_delivery(&delivery.delivery_id, "order-9001", now)
            .unwrap(),
        receipt
    );
    restarted
        .mark_intake_acked(&delivery.delivery_id, &receipt)
        .unwrap();
    assert!(restarted.due_intake_deliveries(now, 10).unwrap().is_empty());
    assert!(matches!(
        restarted.claim_product_work("worker-a", now, chrono::Duration::minutes(1)).unwrap(),
        foundry_ledger::ProductWorkClaimResult::Unsupported { task_kind, .. } if task_kind == "dep-bump"
    ));
    let snapshot = restarted.projection_snapshot().unwrap();
    assert_eq!(snapshot.lifecycles.len(), 1);
    assert_eq!(snapshot.lifecycles[0].order_id, "order-9001");

    drop(restarted);
    let _ = std::fs::remove_dir_all(dir);
}

fn prepared_claim_fixture() -> (
    std::path::PathBuf,
    std::path::PathBuf,
    chrono::DateTime<Utc>,
) {
    let dir = scratch();
    let db = dir.join("product.sqlite");
    let now = Utc.with_ymd_and_hms(2026, 10, 16, 0, 0, 0).unwrap();
    let config = ProductConfig {
        installation_id: "test-install".into(),
        database_id: "test-db".into(),
    };
    let mut store = ProductStore::open(&db, config).unwrap();
    store
        .bind_source(
            "github-main",
            "github",
            "https://api.github.com",
            "repo-42",
            "open+foundry:make",
        )
        .unwrap();
    let updated = "2026-10-15T23:59:00Z";
    let payload = "adapter-fixture-payload";
    let version = foundry_ledger::product::delivery_version_digest(
        "claim-issue",
        updated,
        "dep-bump",
        payload,
    );
    let delivery = ProductDelivery {
        delivery_id: foundry_ledger::product::github_delivery_id(
            "test-install",
            "github-main",
            "https://api.github.com",
            "repo-42",
            "claim-issue",
            &version,
        ),
        connector_id: "github-main".into(),
        repository_id: "repo-42".into(),
        issue_id: "claim-issue".into(),
        observed_updated_at: updated.into(),
        version_digest: version,
        task_kind: "dep-bump".into(),
        payload: payload.into(),
    };
    let cursor = ProductPollCursor {
        connector_id: "github-main".into(),
        query_fingerprint: "q-claim".into(),
        sweep_id: "claim-sweep".into(),
        next_page: 1,
        page_ceiling: 1,
        updated_at: now.to_rfc3339(),
    };
    store
        .persist_intake_page(&cursor, &[delivery.clone()])
        .unwrap();
    store
        .admit_intake_delivery(&delivery.delivery_id, "claim-order", now)
        .unwrap();
    drop(store);
    // Test-only adapter fixture: production intake never promotes arbitrary
    // GitHub payloads to ready; it leaves them waiting_unsupported.
    let fixture = rusqlite::Connection::open(&db).unwrap();
    fixture.execute("UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id='claim-order' AND lifecycle=1", []).unwrap();
    drop(fixture);
    (dir, db, now)
}

#[test]
fn durable_claim_has_single_connection_winner_and_reopen_reclaims_stale_proof() {
    let (dir, db, now) = prepared_claim_fixture();
    let config = ProductConfig {
        installation_id: "test-install".into(),
        database_id: "test-db".into(),
    };
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut workers = Vec::new();
    for worker in ["worker-a", "worker-b"] {
        let path = db.clone();
        let config = config.clone();
        let barrier = barrier.clone();
        let worker = worker.to_owned();
        workers.push(std::thread::spawn(move || {
            let mut store = ProductStore::open(&path, config).unwrap();
            barrier.wait();
            store
                .claim_product_work(&worker, now, chrono::Duration::seconds(10))
                .unwrap()
        }));
    }
    let outcomes = workers
        .into_iter()
        .map(|w| w.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| matches!(o, foundry_ledger::ProductWorkClaimResult::Claimed(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| matches!(o, foundry_ledger::ProductWorkClaimResult::Empty))
            .count(),
        1
    );
    let old = outcomes
        .into_iter()
        .find_map(|o| match o {
            foundry_ledger::ProductWorkClaimResult::Claimed(c) => Some(c),
            _ => None,
        })
        .unwrap();
    assert!(
        !format!("{old:?}").contains("adapter-fixture-payload"),
        "claim debug output redacts the immutable task body"
    );
    let mut reopened = ProductStore::open(&db, config).unwrap();
    reopened
        .validate_product_work_claim(&old, now + chrono::Duration::seconds(1))
        .unwrap();
    let replacement = match reopened
        .claim_product_work(
            "worker-recovery",
            now + chrono::Duration::seconds(10),
            chrono::Duration::seconds(10),
        )
        .unwrap()
    {
        foundry_ledger::ProductWorkClaimResult::Claimed(c) => c,
        other => panic!("expired claim was not reclaimed: {other:?}"),
    };
    assert!(replacement.lease_until() > old.lease_until());
    assert!(
        reopened
            .validate_product_work_claim(&old, now + chrono::Duration::seconds(10))
            .is_err()
    );
    reopened
        .validate_product_work_claim(&replacement, now + chrono::Duration::seconds(11))
        .unwrap();
    drop(reopened);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn product_claim_child_process() {
    let Ok(db) = std::env::var("FOUNDRY_B1_CLAIM_CHILD_DB") else {
        return;
    };
    use std::io::Read;
    let mut start = [0u8; 1];
    std::io::stdin().read_exact(&mut start).unwrap();
    let mut store = ProductStore::open(
        std::path::Path::new(&db),
        ProductConfig {
            installation_id: "test-install".into(),
            database_id: "test-db".into(),
        },
    )
    .unwrap();
    let now = Utc.with_ymd_and_hms(2026, 10, 16, 0, 0, 0).unwrap();
    let marker = std::env::var("FOUNDRY_B1_CLAIM_CHILD_RESULT").unwrap();
    let result = match store
        .claim_product_work(
            &format!("child-{}", std::process::id()),
            now,
            chrono::Duration::minutes(1),
        )
        .unwrap()
    {
        foundry_ledger::ProductWorkClaimResult::Claimed(_) => "CLAIMED",
        foundry_ledger::ProductWorkClaimResult::Empty => "EMPTY",
        other => panic!("unexpected child claim result: {other:?}"),
    };
    std::fs::write(marker, result).unwrap();
}

#[test]
fn separate_processes_serialize_claim_fence_and_single_winner() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let (dir, db, _) = prepared_claim_fixture();
    let exe = std::env::current_exe().unwrap();
    let mut children = Vec::new();
    let mut markers = Vec::new();
    for index in 0..2 {
        let marker = dir.join(format!("claim-result-{index}"));
        children.push(
            Command::new(&exe)
                .args(["product_claim_child_process", "--nocapture"])
                .env("FOUNDRY_B1_CLAIM_CHILD_DB", &db)
                .env("FOUNDRY_B1_CLAIM_CHILD_RESULT", &marker)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        markers.push(marker);
    }
    for child in &mut children {
        child.stdin.as_mut().unwrap().write_all(b"x").unwrap();
    }
    for child in children.into_iter() {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let outputs = markers
        .iter()
        .map(std::fs::read_to_string)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        outputs.iter().filter(|out| out.contains("CLAIMED")).count(),
        1,
        "child outputs: {outputs:?}"
    );
    assert_eq!(
        outputs.iter().filter(|out| out.contains("EMPTY")).count(),
        1,
        "child outputs: {outputs:?}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn durable_claim_fails_closed_after_source_rebind_and_lease_expiry() {
    let (dir, db, now) = prepared_claim_fixture();
    let config = ProductConfig {
        installation_id: "test-install".into(),
        database_id: "test-db".into(),
    };
    let mut store = ProductStore::open(&db, config).unwrap();
    let claim = match store
        .claim_product_work("worker-a", now, chrono::Duration::seconds(2))
        .unwrap()
    {
        foundry_ledger::ProductWorkClaimResult::Claimed(c) => c,
        other => panic!("ready work not claimed: {other:?}"),
    };
    store
        .validate_product_work_claim(&claim, now + chrono::Duration::seconds(1))
        .unwrap();
    assert!(
        store
            .validate_product_work_claim(&claim, now + chrono::Duration::seconds(2))
            .is_err()
    );
    store
        .bind_source(
            "github-main",
            "github",
            "https://api.github.com",
            "repo-42",
            "changed-scope",
        )
        .unwrap();
    assert!(
        store
            .validate_product_work_claim(&claim, now + chrono::Duration::seconds(1))
            .is_err()
    );
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn page_conflict_does_not_advance_cursor_or_overwrite_snapshot() {
    let dir = scratch();
    let db = dir.join("product.sqlite");
    let config = ProductConfig {
        installation_id: "test-install".into(),
        database_id: "test-db".into(),
    };
    let now = Utc.with_ymd_and_hms(2026, 10, 16, 0, 0, 0).unwrap();
    let cursor = ProductPollCursor {
        connector_id: "github-main".into(),
        query_fingerprint: "q-v1".into(),
        sweep_id: "sweep-1".into(),
        next_page: 1,
        page_ceiling: 5,
        updated_at: now.to_rfc3339(),
    };
    let mut store = ProductStore::open(&db, config).unwrap();
    store
        .bind_source(
            "github-main",
            "github",
            "https://api.github.com",
            "repo-42",
            "open+foundry:make",
        )
        .unwrap();
    let observed = "2026-10-15T23:59:00Z";
    let original_digest = foundry_ledger::product::delivery_version_digest(
        "issue-1", observed, "dep-bump", "original",
    );
    let item = ProductDelivery {
        delivery_id: foundry_ledger::product::github_delivery_id(
            "test-install",
            "github-main",
            "https://api.github.com",
            "repo-42",
            "issue-1",
            &original_digest,
        ),
        connector_id: "github-main".into(),
        repository_id: "repo-42".into(),
        issue_id: "issue-1".into(),
        observed_updated_at: observed.into(),
        version_digest: original_digest,
        task_kind: "dep-bump".into(),
        payload: "original".into(),
    };
    store.persist_intake_page(&cursor, &[item.clone()]).unwrap();
    let mut conflict = item;
    conflict.payload = "changed".into();
    let mut later = cursor;
    later.next_page = 3;
    assert!(store.persist_intake_page(&later, &[conflict]).is_err());
    assert_eq!(
        store
            .intake_cursor("github-main", "q-v1")
            .unwrap()
            .unwrap()
            .next_page,
        1
    );
    assert_eq!(
        store.due_intake_deliveries(now, 10).unwrap()[0]
            .delivery
            .payload,
        "original"
    );
    drop(store);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn consumer_retries_ack_after_durable_admission_without_duplicate_order() {
    let dir = scratch();
    let db = dir.join("product.sqlite");
    let config = ProductConfig {
        installation_id: "test-install".into(),
        database_id: "test-db".into(),
    };
    let now = Utc.with_ymd_and_hms(2026, 10, 16, 0, 0, 0).unwrap();
    let payload = "snapshot";
    let updated = "2026-10-15T23:59:00Z";
    let version = foundry_ledger::product::delivery_version_digest(
        "issue-consumer",
        updated,
        "dep-bump",
        payload,
    );
    let item = ProductDelivery {
        delivery_id: foundry_ledger::product::github_delivery_id(
            "test-install",
            "github-main",
            "https://api.github.com",
            "repo-42",
            "issue-consumer",
            &version,
        ),
        connector_id: "github-main".into(),
        repository_id: "repo-42".into(),
        issue_id: "issue-consumer".into(),
        observed_updated_at: updated.into(),
        version_digest: version,
        task_kind: "dep-bump".into(),
        payload: payload.into(),
    };
    let cursor = ProductPollCursor {
        connector_id: "github-main".into(),
        query_fingerprint: "q-v1".into(),
        sweep_id: "sweep-1".into(),
        next_page: 2,
        page_ceiling: 5,
        updated_at: now.to_rfc3339(),
    };
    let mut store = ProductStore::open(&db, config).unwrap();
    store
        .bind_source(
            "github-main",
            "github",
            "https://api.github.com",
            "repo-42",
            "open+foundry:make",
        )
        .unwrap();
    store.persist_intake_page(&cursor, &[item]).unwrap();
    let mut ack = FakeAck(false);
    let first =
        foundry_watch::intake_worker::drain_admissions(&mut store, &mut ack, now, 10).unwrap();
    assert_eq!(
        (first.admitted, first.acknowledged, first.retryable_failures),
        (1, 0, 1)
    );
    assert!(store.due_intake_deliveries(now, 10).unwrap().is_empty());
    let second = foundry_watch::intake_worker::drain_admissions(
        &mut store,
        &mut ack,
        now + chrono::Duration::seconds(30),
        10,
    )
    .unwrap();
    assert_eq!((second.admitted, second.acknowledged), (1, 1));
    assert_eq!(first.receipts[0], second.receipts[0]);
    assert_eq!(store.projection_snapshot().unwrap().lifecycles.len(), 1);
    drop(store);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn edited_issue_version_reuses_original_order_and_task_snapshot() {
    let dir = scratch();
    let db = dir.join("product.sqlite");
    let config = ProductConfig {
        installation_id: "test-install".into(),
        database_id: "test-db".into(),
    };
    let now = Utc.with_ymd_and_hms(2026, 10, 16, 0, 0, 0).unwrap();
    let mut store = ProductStore::open(&db, config).unwrap();
    store
        .bind_source(
            "github-main",
            "github",
            "https://api.github.com",
            "repo-42",
            "open+foundry:make",
        )
        .unwrap();
    let make_delivery = |updated: &str, payload: &str| {
        let issue = "stable-issue-7";
        let version =
            foundry_ledger::product::delivery_version_digest(issue, updated, "dep-bump", payload);
        ProductDelivery {
            delivery_id: foundry_ledger::product::github_delivery_id(
                "test-install",
                "github-main",
                "https://api.github.com",
                "repo-42",
                issue,
                &version,
            ),
            connector_id: "github-main".into(),
            repository_id: "repo-42".into(),
            issue_id: issue.into(),
            observed_updated_at: updated.into(),
            version_digest: version,
            task_kind: "dep-bump".into(),
            payload: payload.into(),
        }
    };
    let first = make_delivery("2026-10-15T20:00:00Z", "original snapshot");
    let cursor = ProductPollCursor {
        connector_id: "github-main".into(),
        query_fingerprint: "q".into(),
        sweep_id: "s".into(),
        next_page: 1,
        page_ceiling: 5,
        updated_at: now.to_rfc3339(),
    };
    store
        .persist_intake_page(&cursor, &[first.clone()])
        .unwrap();
    let receipt = store
        .admit_intake_delivery(&first.delivery_id, "order-7", now)
        .unwrap();
    let edited = make_delivery("2026-10-15T21:00:00Z", "edited snapshot");
    store
        .persist_intake_page(&cursor, &[edited.clone()])
        .unwrap();
    assert_eq!(
        store
            .admit_intake_delivery(&edited.delivery_id, "ignored-new-order", now)
            .unwrap(),
        receipt
    );
    assert_eq!(store.projection_snapshot().unwrap().lifecycles.len(), 1);
    assert_eq!(
        store
            .due_intake_deliveries(now, 10)
            .unwrap()
            .iter()
            .find(|d| d.delivery.delivery_id == edited.delivery_id)
            .unwrap()
            .delivery
            .payload,
        "edited snapshot"
    );
    // The stored order task remains the original snapshot; intake versions are observational only.
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn github_retry_backoff_is_durable_bounded_and_cleared_by_committed_page() {
    let dir = scratch();
    let db = dir.join("product.sqlite");
    let config = ProductConfig {
        installation_id: "test-install".into(),
        database_id: "test-db".into(),
    };
    let now = Utc.with_ymd_and_hms(2026, 10, 16, 0, 0, 0).unwrap();
    let mut store = ProductStore::open(&db, config.clone()).unwrap();
    store
        .bind_source(
            "github-main",
            "github",
            "https://api.github.com",
            "repo-42",
            "open+foundry:make",
        )
        .unwrap();
    let first = store
        .record_intake_poll_failure("github-main", "retry-q", now, None)
        .unwrap();
    assert_eq!(first, now + chrono::Duration::seconds(30));
    let second = store
        .record_intake_poll_failure("github-main", "retry-q", first, None)
        .unwrap();
    assert_eq!(second, first + chrono::Duration::seconds(60));
    let server_retry = second + chrono::Duration::hours(2);
    assert_eq!(
        store
            .record_intake_poll_failure("github-main", "retry-q", second, Some(server_retry))
            .unwrap(),
        server_retry
    );
    drop(store);

    let mut restarted = ProductStore::open(&db, config).unwrap();
    assert_eq!(
        restarted
            .intake_poll_retry_at("github-main", "retry-q")
            .unwrap(),
        Some(server_retry)
    );
    let cursor = ProductPollCursor {
        connector_id: "github-main".into(),
        query_fingerprint: "retry-q".into(),
        sweep_id: "sweep".into(),
        next_page: 1,
        page_ceiling: 5,
        updated_at: now.to_rfc3339(),
    };
    restarted.persist_intake_page(&cursor, &[]).unwrap();
    assert_eq!(
        restarted
            .intake_poll_retry_at("github-main", "retry-q")
            .unwrap(),
        None
    );
    drop(restarted);
    std::fs::remove_dir_all(dir).unwrap();
}
