use foundry_exec::{
    ArtifactLocation, ArtifactTuple, ArtifactTupleKey, Attempt, EmitReceipt, Generation,
    ImmutableCommit, OrderId, PortError, SqliteTupleStore, TupleStore,
};

struct Db(std::path::PathBuf);
impl Db {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(std::env::temp_dir().join(format!(
            "review-tuples-{}-{}.db",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }
}
impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(self.0.with_extension("db-wal"));
        let _ = std::fs::remove_file(self.0.with_extension("db-shm"));
    }
}

fn tuple(run: u64, generation: u64, digest: &str) -> ArtifactTuple {
    ArtifactTuple {
        order: OrderId("ord-1".into()),
        attempt: Attempt(2),
        run: foundry_exec::RunId(run),
        generation: Generation(generation),
        immutable_commit: ImmutableCommit(digest.into()),
        artifact_location: ArtifactLocation(format!("review-{run}.log")),
        generations: Vec::new(),
    }
}

#[test]
fn sqlite_tuple_store_is_idempotent_durable_and_orders_latest_by_run_then_generation() {
    let db = Db::new();
    let store = SqliteTupleStore::open(&db.0).unwrap();
    let a = tuple(7, 9, "digest-a");
    let b = tuple(8, 0, "digest-b");
    let c = tuple(8, 1, "digest-c");
    store.record(&a).unwrap();
    store.record(&a).unwrap();
    store.record(&b).unwrap();
    store.record(&c).unwrap();
    assert_eq!(
        store
            .latest(&OrderId("ord-1".into()), Attempt(2))
            .unwrap()
            .unwrap(),
        c
    );
    assert_eq!(
        store
            .latest_promoted(&OrderId("ord-1".into()), Attempt(2))
            .unwrap(),
        None
    );
    assert!(matches!(
        store.record(&tuple(7, 9, "different")),
        Err(PortError::Permanent(_))
    ));
    drop(store);
    let reopened = SqliteTupleStore::open(&db.0).unwrap();
    assert_eq!(
        reopened
            .latest(&OrderId("ord-1".into()), Attempt(2))
            .unwrap()
            .unwrap()
            .immutable_commit
            .0,
        "digest-c"
    );
}

#[test]
fn promotion_requires_matching_record_and_survives_reopen() {
    let db = Db::new();
    let store = SqliteTupleStore::open(&db.0).unwrap();
    let artifact = tuple(1, 0, "sha");
    let key: ArtifactTupleKey = artifact.key();
    let receipt = EmitReceipt {
        key: key.clone(),
        event: "publish".into(),
        at: chrono::Utc::now(),
    };
    assert_eq!(
        store.promote(&key, &receipt),
        Err(PortError::MissingEmitReceipt)
    );
    store.record(&artifact).unwrap();
    store.promote(&key, &receipt).unwrap();
    store.promote(&key, &receipt).unwrap();
    assert_eq!(
        store
            .latest_promoted(&artifact.order, artifact.attempt)
            .unwrap(),
        Some(artifact.clone())
    );
    drop(store);
    let reopened = SqliteTupleStore::open(&db.0).unwrap();
    reopened.promote(&key, &receipt).unwrap();
    let wrong = EmitReceipt {
        key: tuple(2, 0, "other").key(),
        event: "publish".into(),
        at: chrono::Utc::now(),
    };
    assert_eq!(
        reopened.promote(&key, &wrong),
        Err(PortError::MissingEmitReceipt)
    );
}

#[test]
fn latest_promoted_skips_record_only_newer_tuple() {
    let db = Db::new();
    let store = SqliteTupleStore::open(&db.0).unwrap();
    let prior = tuple(1, 0, "approved");
    let pending = tuple(2, 0, "pending");
    store.record(&prior).unwrap();
    store
        .promote(
            &prior.key(),
            &EmitReceipt {
                key: prior.key(),
                event: "publish".into(),
                at: chrono::Utc::now(),
            },
        )
        .unwrap();
    store.record(&pending).unwrap();
    assert_eq!(
        store.latest(&prior.order, prior.attempt).unwrap(),
        Some(pending)
    );
    assert_eq!(
        store.latest_promoted(&prior.order, prior.attempt).unwrap(),
        Some(prior)
    );
}

#[test]
fn standalone_tuple_opener_refuses_product_contract_before_schema_changes() {
    let db = Db::new();
    let conn = rusqlite::Connection::open(&db.0).unwrap();
    conn.execute_batch(
        "CREATE TABLE foundry_contract(singleton INTEGER PRIMARY KEY, contract TEXT NOT NULL);
         INSERT INTO foundry_contract(singleton,contract) VALUES(1,'ProductAtomicV1');",
    )
    .unwrap();
    drop(conn);
    assert!(SqliteTupleStore::open(&db.0).is_err());
    let conn = rusqlite::Connection::open(&db.0).unwrap();
    let tuple_schema: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='artifact_tuples')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!tuple_schema);
}

#[test]
fn independent_processes_serialize_colliding_records() {
    const CHILD_DB: &str = "FOUNDRY_T31_SQLITE_CHILD_DB";
    const CHILD_VALUE: &str = "FOUNDRY_T31_SQLITE_CHILD_VALUE";
    if let Some(path) = std::env::var_os(CHILD_DB) {
        let digest = std::env::var(CHILD_VALUE).unwrap();
        let store = SqliteTupleStore::open(path).unwrap();
        let result = store.record(&tuple(1, 0, &digest));
        assert!(result.is_ok() || matches!(result, Err(PortError::Permanent(_))));
        let offset = if digest == "a" { 10 } else { 20 };
        for run in offset..offset + 10 {
            store.record(&tuple(run, 0, &digest)).unwrap();
        }
        return;
    }
    let db = Db::new();
    drop(SqliteTupleStore::open(&db.0).unwrap());
    let spawn = |value| {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "independent_processes_serialize_colliding_records",
            ])
            .env(CHILD_DB, &db.0)
            .env(CHILD_VALUE, value)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let a = spawn("a");
    let b = spawn("b");
    for child in [a, b] {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let store = SqliteTupleStore::open(&db.0).unwrap();
    assert_eq!(
        store.latest(&OrderId("ord-1".into()), Attempt(2)).unwrap(),
        Some(tuple(29, 0, "b"))
    );
    let rows: u64 = rusqlite::Connection::open(&db.0)
        .unwrap()
        .query_row("SELECT count(*) FROM artifact_tuples", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 21);
    // One value won the common key; the other can never replace it.
    let a = store.record(&tuple(1, 0, "a"));
    let b = store.record(&tuple(1, 0, "b"));
    assert!(matches!(
        (a, b),
        (Ok(_), Err(PortError::Permanent(_))) | (Err(PortError::Permanent(_)), Ok(_))
    ));
}

#[test]
fn corrupt_and_truncated_databases_are_errors_not_empty_stores() {
    for truncated in [false, true] {
        let db = Db::new();
        if truncated {
            let store = SqliteTupleStore::open(&db.0).unwrap();
            store.record(&tuple(1, 0, "digest")).unwrap();
            drop(store);
            let bytes = std::fs::read(&db.0).unwrap();
            std::fs::write(&db.0, &bytes[..100]).unwrap();
        } else {
            std::fs::write(&db.0, b"not a sqlite database").unwrap();
        }
        assert!(matches!(
            SqliteTupleStore::open(&db.0),
            Err(PortError::Store(_))
        ));
    }
}

#[test]
fn changed_promotion_receipt_is_permanently_refused() {
    let db = Db::new();
    let store = SqliteTupleStore::open(&db.0).unwrap();
    let artifact = tuple(1, 0, "sha");
    let key = artifact.key();
    let receipt = EmitReceipt {
        key: key.clone(),
        event: "publish".into(),
        at: chrono::Utc::now(),
    };
    store.record(&artifact).unwrap();
    store.promote(&key, &receipt).unwrap();
    for changed in [
        EmitReceipt {
            event: "other".into(),
            ..receipt.clone()
        },
        EmitReceipt {
            at: receipt.at + chrono::Duration::seconds(1),
            ..receipt.clone()
        },
    ] {
        assert!(matches!(
            store.promote(&key, &changed),
            Err(PortError::Permanent(_))
        ));
    }
    drop(store);
    SqliteTupleStore::open(&db.0)
        .unwrap()
        .promote(&key, &receipt)
        .unwrap();
}
