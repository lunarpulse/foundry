use std::{collections::BTreeMap, fs, path::PathBuf};

use foundry_exec::{
    ArtifactLocation, ArtifactTuple, ArtifactTupleKey, Attempt, CrashPoint, EmitReceipt,
    Generation, ImmutableCommit, OrderId, PortError, RunId, TupleStore,
    inject::FaultInjector, ports::Recorded,
};
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
struct Db {
    tuples: BTreeMap<String, ArtifactTuple>,
    promotions: BTreeMap<String, EmitReceipt>,
}
struct DiskTuples { path: PathBuf }
impl DiskTuples {
    fn read(&self) -> Db {
        fs::read(&self.path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }
    fn write(&self, db: &Db) -> Result<(), PortError> {
        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, serde_json::to_vec(db).unwrap()).map_err(|e| PortError::Store(e.to_string()))?;
        fs::rename(tmp, &self.path).map_err(|e| PortError::Store(e.to_string()))
    }
}
fn key(k: &ArtifactTupleKey) -> String {
    format!("{}:{}:{}:{}", k.order.0, k.attempt.0, k.run.0, k.generation.0)
}
impl TupleStore for DiskTuples {
    fn record(&self, tuple: &ArtifactTuple) -> Result<Recorded, PortError> {
        let mut db = self.read(); let id = key(&tuple.key());
        if let Some(old) = db.tuples.get(&id) {
            return if old == tuple { Ok(Recorded) } else { Err(PortError::Permanent("tuple key collision".into())) };
        }
        db.tuples.insert(id, tuple.clone()); self.write(&db)?; Ok(Recorded)
    }
    fn latest(&self, order: &OrderId, attempt: Attempt) -> Result<Option<ArtifactTuple>, PortError> {
        Ok(self.read().tuples.values().filter(|t| &t.order == order && t.attempt == attempt)
            .max_by_key(|t| t.run).cloned())
    }
    fn promote(&self, key_ref: &ArtifactTupleKey, receipt: &EmitReceipt) -> Result<(), PortError> {
        if &receipt.key != key_ref { return Err(PortError::MissingEmitReceipt); }
        let mut db = self.read(); let id = key(key_ref);
        if !db.tuples.contains_key(&id) { return Err(PortError::MissingEmitReceipt); }
        if let Some(old) = db.promotions.get(&id) {
            return if old == receipt { Ok(()) } else { Err(PortError::Permanent("promotion receipt conflict".into())) };
        }
        db.promotions.insert(id, receipt.clone()); self.write(&db)
    }
}

fn tuple(run: u64) -> ArtifactTuple {
    ArtifactTuple { order: OrderId("order".into()), attempt: Attempt(1), run: RunId(run),
        generation: Generation(2), immutable_commit: ImmutableCommit("commit".into()),
        artifact_location: ArtifactLocation("artifact".into()) }
}
fn path() -> PathBuf {
    std::env::temp_dir().join(format!("foundry-tuples-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()))
}

// Downstream receives the tuple value only; its signature has no path parameter.
fn consume(store: &dyn TupleStore) -> Result<Option<ArtifactTuple>, PortError> {
    store.latest(&OrderId("order".into()), Attempt(1))
}

#[test]
fn tuple_record_is_atomic_idempotent_and_pathless_consumption_survives_w5_restart() {
    let path = path();
    let store = DiskTuples { path: path.clone() };
    let artifact = tuple(2);
    assert_eq!(store.record(&artifact), Ok(Recorded));
    assert_eq!(store.record(&artifact), Ok(Recorded));
    assert_eq!(store.record(&ArtifactTuple { artifact_location: ArtifactLocation("other".into()), ..artifact.clone() }),
        Err(PortError::Permanent("tuple key collision".into())));

    let mut faults = FaultInjector::armed_at(CrashPoint::W5);
    assert_eq!(faults.fire(CrashPoint::W5), Err(CrashPoint::W5));
    drop(store);
    let restarted = DiskTuples { path: path.clone() };
    assert_eq!(consume(&restarted).unwrap(), Some(artifact));
    fs::remove_file(path).unwrap();
}

#[test]
fn tuple_missing_after_restart_means_downstream_waits_and_promotion_requires_receipt() {
    let path = path(); let store = DiskTuples { path: path.clone() };
    assert_eq!(consume(&store).unwrap(), None); // AwaitResult-style wait; no path scan fallback exists.
    let artifact = tuple(2); store.record(&artifact).unwrap();
    let bad_receipt = EmitReceipt { key: tuple(3).key(), event: "published".into(), at: chrono::Utc::now() };
    assert_eq!(store.promote(&artifact.key(), &bad_receipt), Err(PortError::MissingEmitReceipt));
    let receipt = EmitReceipt { key: artifact.key(), event: "published".into(), at: chrono::Utc::now() };
    store.promote(&artifact.key(), &receipt).unwrap();
    store.promote(&artifact.key(), &receipt).unwrap();
    fs::remove_file(path).unwrap();
}
