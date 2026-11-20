#![cfg(feature = "product-pack-pins")]

//! These cases exercise the SQLite writer-lock ordering used by the product pin
//! transaction. Observation layer: actual SQLite connections and constraints;
//! they do not execute ProductStore's activation path or a worker executor.

use rusqlite::{Connection, Error, TransactionBehavior};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Db(PathBuf);
impl Db {
    fn new() -> Self {
        let root = PathBuf::from(std::env::var_os("TMPDIR").expect("configured scratch"));
        assert_ne!(root, PathBuf::from("/tmp"));
        let path = root.join(format!(
            "b2-w1-race-{}-{}-{}.sqlite",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let c = Connection::open(&path).unwrap();
        c.execute_batch("CREATE TABLE state(singleton INTEGER PRIMARY KEY CHECK(singleton=1),selection_revision INTEGER NOT NULL,admitted_revision INTEGER); INSERT INTO state VALUES(1,0,NULL); CREATE TABLE keys(actor TEXT NOT NULL,request_key TEXT NOT NULL,bytes BLOB NOT NULL,PRIMARY KEY(actor,request_key)); CREATE TABLE activations(order_id TEXT NOT NULL,attempt INTEGER NOT NULL,run_id INTEGER NOT NULL,activation INTEGER NOT NULL,PRIMARY KEY(order_id,attempt,run_id,activation),UNIQUE(order_id,attempt,run_id));").unwrap();
        Self(path)
    }
    fn open(&self) -> Connection {
        Connection::open(&self.0).unwrap()
    }
}
impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn p03_race_admission_first_wins_over_selection_change() {
    let db = Db::new();
    let mut winner = db.open();
    let loser = db.open();
    loser.busy_timeout(Duration::ZERO).unwrap();
    let tx = winner
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    tx.execute(
        "UPDATE state SET admitted_revision=selection_revision WHERE singleton=1",
        [],
    )
    .unwrap();
    assert!(
        matches!(loser.execute_batch("BEGIN IMMEDIATE"),Err(Error::SqliteFailure(e,_) ) if e.code==rusqlite::ErrorCode::DatabaseBusy)
    );
    tx.commit().unwrap();
    let changed=loser.execute("UPDATE state SET selection_revision=selection_revision+1 WHERE singleton=1 AND selection_revision=0",[]).unwrap();
    assert_eq!(changed, 1);
    let row: (i64, Option<i64>) = loser
        .query_row(
            "SELECT selection_revision,admitted_revision FROM state",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(row, (1, Some(0)));
}

#[test]
fn p03_race_selection_change_first_admission_stale() {
    let db = Db::new();
    let mut winner = db.open();
    let loser = db.open();
    loser.busy_timeout(Duration::ZERO).unwrap();
    let tx = winner
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    tx.execute(
        "UPDATE state SET selection_revision=1 WHERE singleton=1",
        [],
    )
    .unwrap();
    assert!(
        matches!(loser.execute_batch("BEGIN IMMEDIATE"),Err(Error::SqliteFailure(e,_) ) if e.code==rusqlite::ErrorCode::DatabaseBusy)
    );
    tx.commit().unwrap();
    let current: i64 = loser
        .query_row("SELECT selection_revision FROM state", [], |r| r.get(0))
        .unwrap();
    assert_ne!(current, 0, "admission CAS is stale after selection commits");
}

#[test]
fn p03_busy_returns_preserved_cause_and_single_retry_succeeds() {
    let db = Db::new();
    let mut winner = db.open();
    let loser = db.open();
    loser.busy_timeout(Duration::ZERO).unwrap();
    let tx = winner
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let first = loser.execute_batch("BEGIN IMMEDIATE").unwrap_err();
    assert!(
        matches!(first,Error::SqliteFailure(ref e,_) if e.code==rusqlite::ErrorCode::DatabaseBusy)
    );
    tx.commit().unwrap();
    loser.execute_batch("BEGIN IMMEDIATE").unwrap();
    loser.execute_batch("COMMIT").unwrap();
}

#[test]
fn p03_alias_and_same_numeric_run_rejected() {
    let db = Db::new();
    let c = db.open();
    c.execute("INSERT INTO activations VALUES('o',1,7,1)", [])
        .unwrap();
    assert!(
        c.execute("INSERT INTO activations VALUES('o',1,7,2)", [])
            .is_err()
    );
    assert!(
        c.execute("INSERT INTO activations VALUES('o',1,8,1)", [])
            .is_ok()
    );
}

#[test]
fn p03_interrupt_before_commit_reopens_uncertain() {
    let db = Db::new();
    {
        let mut c = db.open();
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        tx.execute(
            "UPDATE state SET admitted_revision=selection_revision WHERE singleton=1",
            [],
        )
        .unwrap();
        drop(tx);
    }
    let c = db.open();
    let revision: Option<i64> = c
        .query_row("SELECT admitted_revision FROM state", [], |r| r.get(0))
        .unwrap();
    assert_eq!(revision, None);
}

#[test]
fn p03_interrupt_after_commit_reopens_resolved() {
    let db = Db::new();
    {
        let mut c = db.open();
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        tx.execute(
            "UPDATE state SET admitted_revision=selection_revision WHERE singleton=1",
            [],
        )
        .unwrap();
        tx.commit().unwrap();
    }
    let c = db.open();
    let revision: Option<i64> = c
        .query_row("SELECT admitted_revision FROM state", [], |r| r.get(0))
        .unwrap();
    assert_eq!(revision, Some(0));
}

#[test]
fn p03b_exact_retry_idempotent_replay_rejected() {
    let db = Db::new();
    let c = db.open();
    c.execute("INSERT INTO keys VALUES('actor','key',x'0102')", [])
        .unwrap();
    let replay: Option<Vec<u8>> = c
        .query_row(
            "SELECT bytes FROM keys WHERE actor='actor' AND request_key='key'",
            [],
            |r| r.get(0),
        )
        .ok();
    assert_eq!(replay, Some(vec![1, 2]));
    assert!(
        c.execute("INSERT INTO keys VALUES('actor','key',x'0304')", [])
            .is_err()
    );
}
