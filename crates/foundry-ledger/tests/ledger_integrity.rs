//! T5 — ledger tests: append-only immutability + chain-hash verification.
use foundry_domain::ports::Ledger;
use foundry_domain::{ActorId, ActorKind, OrderEvent, OrderState};
use foundry_ledger::SqliteLedger;

fn actor() -> ActorId {
    ActorId {
        kind: ActorKind::Human,
        source: "local".into(),
        id: "luna".into(),
    }
}

fn temp_path(name: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "foundry-test-{name}-{}-{nonce}.db",
        std::process::id()
    ))
}

fn unique_temp_path(name: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "foundry-test-{name}-{}-{nonce}.db",
        std::process::id()
    ))
}

#[test]
fn appends_and_verifies_chain() {
    let path = temp_path("chain");
    let mut l = SqliteLedger::open(&path).unwrap();
    l.append(
        "o-1",
        &OrderEvent::Queued,
        OrderState::Received,
        OrderState::Queued,
        &actor(),
        chrono::Utc::now(),
    )
    .unwrap();
    l.append(
        "o-1",
        &OrderEvent::DieStarted,
        OrderState::Queued,
        OrderState::Working,
        &actor(),
        chrono::Utc::now(),
    )
    .unwrap();
    l.append(
        "o-1",
        &OrderEvent::DieSucceeded { attempt: 1 },
        OrderState::Working,
        OrderState::Inspecting,
        &actor(),
        chrono::Utc::now(),
    )
    .unwrap();
    assert!(l.verify_chain().unwrap());
    let _ = std::fs::remove_file(&path);
}

// The append-only SQLite trigger must block UPDATE — even for a caller with
// direct DB access. This is the "no invisible edits" guarantee (plan §11).
#[test]
fn update_is_blocked_by_trigger() {
    let path = temp_path("update");
    let mut l = SqliteLedger::open(&path).unwrap();
    l.append(
        "o-1",
        &OrderEvent::Queued,
        OrderState::Received,
        OrderState::Queued,
        &actor(),
        chrono::Utc::now(),
    )
    .unwrap();

    let conn = rusqlite::Connection::open(&path).unwrap();
    let blocked = conn
        .execute("UPDATE ledger SET intent='forged' WHERE seq=1", [])
        .is_err();
    assert!(blocked, "append-only trigger must block UPDATE");
    assert!(
        l.verify_chain().unwrap(),
        "chain intact after blocked update"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn delete_is_blocked() {
    let path = temp_path("delete");
    let mut l = SqliteLedger::open(&path).unwrap();
    l.append(
        "o-1",
        &OrderEvent::Queued,
        OrderState::Received,
        OrderState::Queued,
        &actor(),
        chrono::Utc::now(),
    )
    .unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    assert!(
        conn.execute("DELETE FROM ledger", []).is_err(),
        "append-only trigger must block DELETE"
    );
    let _ = std::fs::remove_file(&path);
}

// ── (M7 G.B4) 트랜잭션화 append 회귀 ───────────────────────────────

/// B4: 연속 append 가 트랜잭션 가드로 오염되지 않고 체인이 유지된다.
#[test]
fn b4_sequential_appends_with_tx_guard_keep_chain() {
    let path = temp_path("b4a");
    let mut l = SqliteLedger::open(&path).unwrap();
    for (ev, from, to) in [
        (OrderEvent::Queued, OrderState::Received, OrderState::Queued),
        (
            OrderEvent::DieStarted,
            OrderState::Queued,
            OrderState::Working,
        ),
        (
            OrderEvent::DieSucceeded { attempt: 1 },
            OrderState::Working,
            OrderState::Inspecting,
        ),
    ] {
        l.append("o-b4", &ev, from, to, &actor(), chrono::Utc::now())
            .unwrap();
    }
    let rows = l.read_all().unwrap();
    assert_eq!(rows.len(), 3);
    assert!(
        SqliteLedger::verify_rows(&rows),
        "chain verifies after sequential tx appends"
    );
    let seqs: Vec<i64> = rows.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3]);
}

/// B4: 재오픈한 두 번째 핸들이 이어 append 해도 체인 정합 (커밋 확정 단정).
#[test]
fn b4_reopened_handle_extends_committed_chain() {
    let path = temp_path("b4b");
    {
        let mut l1 = SqliteLedger::open(&path).unwrap();
        l1.append(
            "o-x",
            &OrderEvent::Queued,
            OrderState::Received,
            OrderState::Queued,
            &actor(),
            chrono::Utc::now(),
        )
        .unwrap();
    }
    {
        let mut l2 = SqliteLedger::open(&path).unwrap();
        l2.append(
            "o-x",
            &OrderEvent::DieStarted,
            OrderState::Queued,
            OrderState::Working,
            &actor(),
            chrono::Utc::now(),
        )
        .unwrap();
    }
    let l3 = SqliteLedger::open(&path).unwrap();
    let rows = l3.read_all().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(
        SqliteLedger::verify_rows(&rows),
        "prev_hash linkage across handles — COMMIT actually persisted"
    );
}

#[test]
fn append_error_rolls_back_and_next_append_can_write() {
    let path = unique_temp_path("append-rollback");
    let mut ledger = SqliteLedger::open(&path).unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER reject_ledger_insert BEFORE INSERT ON ledger
         BEGIN SELECT RAISE(ABORT, 'injected append error'); END;",
    )
    .unwrap();

    assert!(
        ledger
            .append(
                "o-rollback",
                &OrderEvent::Queued,
                OrderState::Received,
                OrderState::Queued,
                &actor(),
                chrono::Utc::now(),
            )
            .is_err()
    );
    assert!(
        ledger.read_all().unwrap().is_empty(),
        "failed append must roll back"
    );

    conn.execute_batch("DROP TRIGGER reject_ledger_insert;")
        .unwrap();
    ledger
        .append(
            "o-rollback",
            &OrderEvent::Queued,
            OrderState::Received,
            OrderState::Queued,
            &actor(),
            chrono::Utc::now(),
        )
        .unwrap();
    assert_eq!(ledger.read_all().unwrap().len(), 1);
    assert!(ledger.verify_chain().unwrap());

    drop(conn);
    drop(ledger);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
}
