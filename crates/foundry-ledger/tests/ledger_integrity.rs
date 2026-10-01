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
    let mut p = std::env::temp_dir();
    p.push(format!("foundry-test-{name}-{}.db", std::process::id()));
    p
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
    assert!(l.verify_chain().unwrap(), "chain intact after blocked update");
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
