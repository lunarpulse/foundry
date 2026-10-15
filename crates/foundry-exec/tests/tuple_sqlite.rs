use foundry_exec::{
    ArtifactLocation, ArtifactTuple, ArtifactTupleKey, Attempt, EmitReceipt, Generation,
    ImmutableCommit, OrderId, PortError, SqliteTupleStore, TupleStore,
};

struct Db(std::path::PathBuf);
impl Db {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(std::env::temp_dir().join(format!("review-tuples-{}-{}.db", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))))
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
        order: OrderId("ord-1".into()), attempt: Attempt(2),
        run: foundry_exec::RunId(run), generation: Generation(generation),
        immutable_commit: ImmutableCommit(digest.into()),
        artifact_location: ArtifactLocation(format!("review-{run}.log")),
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
    assert_eq!(store.latest(&OrderId("ord-1".into()), Attempt(2)).unwrap().unwrap(), c);
    assert!(matches!(store.record(&tuple(7, 9, "different")), Err(PortError::Permanent(_))));
    drop(store);
    let reopened = SqliteTupleStore::open(&db.0).unwrap();
    assert_eq!(reopened.latest(&OrderId("ord-1".into()), Attempt(2)).unwrap().unwrap().immutable_commit.0, "digest-c");
}

#[test]
fn promotion_requires_matching_record_and_survives_reopen() {
    let db = Db::new();
    let store = SqliteTupleStore::open(&db.0).unwrap();
    let artifact = tuple(1, 0, "sha");
    let key: ArtifactTupleKey = artifact.key();
    let receipt = EmitReceipt { key: key.clone(), event: "publish".into(), at: chrono::Utc::now() };
    assert_eq!(store.promote(&key, &receipt), Err(PortError::MissingEmitReceipt));
    store.record(&artifact).unwrap();
    store.promote(&key, &receipt).unwrap();
    store.promote(&key, &receipt).unwrap();
    drop(store);
    let reopened = SqliteTupleStore::open(&db.0).unwrap();
    reopened.promote(&key, &receipt).unwrap();
    let wrong = EmitReceipt { key: tuple(2, 0, "other").key(), event: "publish".into(), at: chrono::Utc::now() };
    assert_eq!(reopened.promote(&key, &wrong), Err(PortError::MissingEmitReceipt));
}
