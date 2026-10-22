use chrono::Utc;
use foundry_exec::{
    ArtifactLocation, ArtifactTuple, ArtifactTupleKey, Attempt, CounterLog, CounterRecord,
    CrashPoint, DispatchHost, DispatchResult, EmitLog, EmitReceipt, EmitSink, Epoch,
    ExecutionIntent, ExecutionOutcome, Generation, GraphDriver, GraphStep, ImmutableCommit,
    IntentLog, OrderId, OutcomeStore, PortError, PromotionReceipt, Recorded, RunId, TupleStore,
};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[path = "support/tuple_store.rs"]
mod tuple_store;
use tuple_store::{DiskTuples, TestDir, WriteStop, read, write};

fn tuple(run: u64) -> ArtifactTuple {
    ArtifactTuple {
        order: OrderId("order".into()),
        attempt: Attempt(1),
        run: RunId(run),
        generation: Generation(2),
        immutable_commit: ImmutableCommit("commit".into()),
        artifact_location: ArtifactLocation("artifact".into()),
        generations: Vec::new(),
    }
}

// This is the entire downstream surface: no Path, directory or scanner argument.
fn consume(store: &dyn TupleStore) -> Result<Option<ArtifactTuple>, PortError> {
    store.latest(&OrderId("order".into()), Attempt(1))
}

#[test]
fn tuple_record_and_promotion_contract_survives_reopen() {
    let dir = TestDir::new();
    let path = dir.0.join("tuples.json");
    let artifact = tuple(2);
    let receipt = foundry_exec::emit::new_receipt(&artifact, "published");
    {
        let store = DiskTuples::new(path.clone());
        assert_eq!(consume(&store).unwrap(), None);
        assert_eq!(
            store.promote(&artifact.key(), &receipt),
            Err(PortError::MissingEmitReceipt)
        );
        assert!(!path.exists());
        assert_eq!(store.record(&artifact), Ok(Recorded));
        let before = fs::read(&path).unwrap();
        assert_eq!(store.record(&artifact), Ok(Recorded));
        for changed in [
            ArtifactTuple {
                artifact_location: ArtifactLocation("other".into()),
                ..artifact.clone()
            },
            ArtifactTuple {
                immutable_commit: ImmutableCommit("other".into()),
                ..artifact.clone()
            },
        ] {
            assert!(matches!(
                store.record(&changed),
                Err(PortError::Permanent(_))
            ));
            assert_eq!(fs::read(&path).unwrap(), before);
        }
        let bad = EmitReceipt {
            key: tuple(3).key(),
            ..receipt.clone()
        };
        assert_eq!(
            store.promote(&artifact.key(), &bad),
            Err(PortError::MissingEmitReceipt)
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        store.promote(&artifact.key(), &receipt).unwrap();
    }
    let store = DiskTuples::new(path.clone());
    assert_eq!(consume(&store).unwrap(), Some(artifact.clone()));
    let before = fs::read(&path).unwrap();
    store.promote(&artifact.key(), &receipt).unwrap();
    let conflicting = EmitReceipt {
        event: "other".into(),
        ..receipt
    };
    assert!(matches!(
        store.promote(&artifact.key(), &conflicting),
        Err(PortError::Permanent(_))
    ));
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn tuple_atomic_write_windows_never_expose_a_partial_record_or_promotion() {
    for promoting in [false, true] {
        for existing in [false, true] {
            if promoting && !existing {
                continue;
            } // promotion requires a recorded tuple.
            for stop in [
                WriteStop::PartialTemporary,
                WriteStop::BeforeRename,
                WriteStop::AfterRename,
            ] {
                let dir = TestDir::new();
                let path = dir.0.join("tuples.json");
                let old = tuple(1);
                let artifact = if promoting { old.clone() } else { tuple(2) };
                {
                    let mut store = DiskTuples::new(path.clone());
                    if existing {
                        store.record(&old).unwrap();
                    }
                    store.stop = Some(stop);
                    let result = if promoting {
                        store.promote(
                            &artifact.key(),
                            &foundry_exec::emit::new_receipt(&artifact, "published"),
                        )
                    } else {
                        store.record(&artifact).map(|_| ())
                    };
                    assert!(matches!(result, Err(PortError::Store(_))));
                }
                let store = DiskTuples::new(path.clone());
                let published = stop == WriteStop::AfterRename;
                let expected = if published || promoting {
                    Some(artifact.clone())
                } else if existing {
                    Some(old)
                } else {
                    None
                };
                assert_eq!(
                    consume(&store).unwrap(),
                    expected,
                    "{stop:?}/{promoting}/{existing}"
                );
                assert_eq!(store.promoted(&artifact.key()), promoting && published);
                // Recovery ignores orphan temporary bytes, then safely retries.
                store.record(&artifact).unwrap();
                let receipt = foundry_exec::emit::new_receipt(&artifact, "published");
                if !promoting || !published {
                    store.promote(&artifact.key(), &receipt).unwrap();
                }
                assert_eq!(consume(&store).unwrap(), Some(artifact));
            }
        }
    }
}

#[test]
fn tuple_corruption_and_io_errors_are_faults_instead_of_absence() {
    let dir = TestDir::new();
    let path = dir.0.join("tuples.json");
    fs::write(&path, b"{partial").unwrap();
    let store = DiskTuples::new(path.clone());
    assert!(matches!(consume(&store), Err(PortError::Store(_))));
    assert!(matches!(store.record(&tuple(2)), Err(PortError::Store(_))));
    assert_eq!(fs::read(&path).unwrap(), b"{partial");
    let store = DiskTuples::new(dir.0.clone()); // reading a directory must not mean NotFound.
    assert!(matches!(consume(&store), Err(PortError::Store(_))));
}

#[test]
fn tuple_latest_orders_runs_and_generations_within_the_requested_pair() {
    let dir = TestDir::new();
    let store = DiskTuples::new(dir.0.join("tuples.json"));
    let latest = ArtifactTuple {
        generation: Generation(10),
        ..tuple(2)
    };
    for artifact in [
        latest.clone(),
        tuple(1),
        tuple(2),
        ArtifactTuple {
            attempt: Attempt(2),
            ..tuple(99)
        },
        ArtifactTuple {
            order: OrderId("other".into()),
            ..tuple(99)
        },
    ] {
        store.record(&artifact).unwrap();
    }
    assert_eq!(consume(&store).unwrap(), Some(latest));
}

#[derive(Default, Serialize, Deserialize)]
struct GraphDb {
    records: Vec<CounterRecord>,
    emit: Option<EmitReceipt>,
    complete: bool,
}
struct Journal(PathBuf);
impl CounterLog for Journal {
    fn append(&mut self, _: &str, _: u64, record: CounterRecord) -> Result<(), String> {
        let mut db: GraphDb = read(&self.0).unwrap();
        db.records.push(record);
        write(&self.0, &db, None).map_err(|e| format!("{e:?}"))
    }
    fn load(&self, _: &str, _: u64) -> Vec<CounterRecord> {
        read::<GraphDb>(&self.0).unwrap().records
    }
}
impl EmitLog for Journal {
    fn current_epoch(&self) -> Epoch {
        Epoch(4)
    }
    fn promotion_of(&self, key: &ArtifactTupleKey) -> Option<PromotionReceipt> {
        Some(PromotionReceipt {
            tuple: key.clone(),
            promoted_at: Utc::now(),
        })
    }
    fn emit_of(&self, _: &ArtifactTupleKey) -> Option<EmitReceipt> {
        read::<GraphDb>(&self.0).unwrap().emit
    }
    fn record_emit(&mut self, receipt: &EmitReceipt) {
        let mut db: GraphDb = read(&self.0).unwrap();
        if db.emit.is_none() {
            db.emit = Some(receipt.clone());
            write(&self.0, &db, None).unwrap();
        }
    }
    fn mark_complete(&mut self, _: &ArtifactTupleKey) {
        let mut db: GraphDb = read(&self.0).unwrap();
        db.complete = true;
        write(&self.0, &db, None).unwrap();
    }
    fn is_complete(&self, _: &ArtifactTupleKey) -> bool {
        read::<GraphDb>(&self.0).unwrap().complete
    }
}
#[derive(Default, Serialize, Deserialize)]
struct SinkDb {
    publishes: u64,
    receipt: Option<EmitReceipt>,
}
struct Sink {
    path: PathBuf,
    forbid: bool,
}
impl EmitSink for Sink {
    fn publish(&mut self, tuple: &ArtifactTuple) -> EmitReceipt {
        self.publish_named(tuple, "InspectionPassed", "summary")
    }
    fn publish_named(&mut self, tuple: &ArtifactTuple, event: &str, _: &str) -> EmitReceipt {
        assert!(!self.forbid, "recovery touched sink");
        let mut db: SinkDb = read(&self.path).unwrap();
        assert!(db.receipt.is_none());
        let receipt = foundry_exec::emit::new_receipt(tuple, event);
        db.publishes += 1;
        db.receipt = Some(receipt.clone());
        write(&self.path, &db, None).unwrap();
        receipt
    }
    fn receipt_of(&self, _: &ArtifactTupleKey) -> Option<EmitReceipt> {
        assert!(!self.forbid, "recovery queried sink");
        read::<SinkDb>(&self.path).unwrap().receipt
    }
}
struct Unused;
type Key = (OrderId, Attempt, RunId);
impl IntentLog for Unused {
    fn append(&mut self, _: &ExecutionIntent) {
        panic!("unexpected dispatch")
    }
    fn mark_started(&mut self, _: &Key, _: u64) {
        panic!("unexpected dispatch")
    }
    fn latest(&self, _: &Key) -> Option<ExecutionIntent> {
        None
    }
    fn get(&self, _: &Key, _: u64) -> Option<ExecutionIntent> {
        None
    }
    fn is_started(&self, _: &Key, _: u64) -> bool {
        false
    }
}
impl OutcomeStore for Unused {
    fn commit(&mut self, _: &ExecutionOutcome) {
        panic!("unexpected outcome")
    }
    fn latest(&self, _: &Key) -> Option<ExecutionOutcome> {
        None
    }
    fn get(&self, _: &Key, _: u64) -> Option<ExecutionOutcome> {
        None
    }
}
impl DispatchHost for Unused {
    type Result = DispatchResult;
    fn dispatch(&mut self, _: &ExecutionIntent) -> DispatchResult {
        panic!("unexpected host")
    }
    fn dispatch_count(&self) -> u64 {
        0
    }
    fn recover_result(&mut self, _: &ExecutionIntent) -> Option<DispatchResult> {
        None
    }
}

fn drive(
    dir: &TestDir,
    store: Option<&dyn TupleStore>,
    point: Option<CrashPoint>,
    epoch: Epoch,
    forbid_sink: bool,
    restore: bool,
) -> GraphStep {
    let pack = foundry_pack::load_pack(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/fixture-min/v1"),
    )
    .unwrap();
    let (mut intents, mut host, mut outcomes) = (Unused, Unused, Unused);
    let mut counters = Journal(dir.0.join("counters.json"));
    let mut emits = Journal(dir.0.join("emit.json"));
    let mut sink = Sink {
        path: dir.0.join("sink.json"),
        forbid: forbid_sink,
    };
    let constructor = if restore {
        GraphDriver::restore
    } else {
        GraphDriver::new
    };
    let mut driver = constructor(
        &pack,
        &mut intents,
        &mut host,
        &mut outcomes,
        &mut counters,
        &mut sink,
        &mut emits,
        OrderId("order".into()),
        Attempt(1),
        RunId(2),
        Generation(2),
        epoch,
        "test",
        Some(tuple(2)),
    );
    if let Some(store) = store {
        driver = driver.with_tuple_store(store);
    }
    if let Some(point) = point {
        driver.arm_fault(point);
    }
    if restore {
        driver.start()
    } else {
        driver.advance("done")
    }
}

#[test]
fn graph_emit_requires_tuple_storage_before_any_publication() {
    let dir = TestDir::new();
    assert!(
        matches!(drive(&dir, None, None, Epoch(4), false, false), GraphStep::Fault(reason)
        if reason.contains("durable tuple store"))
    );
    assert_eq!(
        read::<SinkDb>(&dir.0.join("sink.json")).unwrap().publishes,
        0
    );
    assert!(
        read::<GraphDb>(&dir.0.join("emit.json"))
            .unwrap()
            .emit
            .is_none()
    );
}

#[test]
fn t30_w5_real_graph_emit_reopens_all_ports_and_downstream_consumes_only_the_tuple() {
    let dir = TestDir::new();
    let path = dir.0.join("tuples.json");
    assert_eq!(consume(&DiskTuples::new(path.clone())).unwrap(), None); // wait before record
    assert!(
        matches!(drive(&dir, Some(&DiskTuples::new(path.clone())), Some(CrashPoint::W5), Epoch(4), false, false),
        GraphStep::Fault(reason) if reason.contains("W5"))
    );
    assert_eq!(
        consume(&DiskTuples::new(path.clone())).unwrap(),
        Some(tuple(2))
    );
    assert!(DiskTuples::new(path.clone()).promoted(&tuple(2).key()));
    assert!(!read::<GraphDb>(&dir.0.join("emit.json")).unwrap().complete);
    assert!(
        !read::<GraphDb>(&dir.0.join("counters.json"))
            .unwrap()
            .records
            .iter()
            .any(|r| matches!(r, CounterRecord::Completed { .. }))
    );
    // Every adapter and driver is recreated. The old epoch may only reuse the receipt.
    assert!(matches!(
        drive(
            &dir,
            Some(&DiskTuples::new(path)),
            None,
            Epoch(1),
            true,
            true
        ),
        GraphStep::Emitted { .. }
    ));
    assert_eq!(
        read::<SinkDb>(&dir.0.join("sink.json")).unwrap().publishes,
        1
    );
    assert!(read::<GraphDb>(&dir.0.join("emit.json")).unwrap().complete);
}

#[test]
fn graph_recovery_repairs_failed_record_and_promote_without_republishing() {
    for promoting in [false, true] {
        for stop in [
            WriteStop::PartialTemporary,
            WriteStop::BeforeRename,
            WriteStop::AfterRename,
        ] {
            let dir = TestDir::new();
            let path = dir.0.join("tuples.json");
            let mut store = DiskTuples::new(path.clone());
            if promoting {
                store.record(&tuple(2)).unwrap();
            }
            store.stop = Some(stop);
            assert!(
                matches!(drive(&dir, Some(&store), None, Epoch(4), false, false), GraphStep::Fault(reason)
                if reason.contains(if promoting { "tuple promotion failed" } else { "tuple record failed" }))
            );
            assert!(!read::<GraphDb>(&dir.0.join("emit.json")).unwrap().complete);
            drop(store);
            let store = DiskTuples::new(path);
            let GraphStep::Emitted { receipt } =
                drive(&dir, Some(&store), None, Epoch(1), true, true)
            else {
                panic!("receipt recovery must finish tuple persistence")
            };
            assert_eq!(consume(&store).unwrap(), Some(tuple(2)));
            assert!(store.promoted(&tuple(2).key()));
            let sink: SinkDb = read(&dir.0.join("sink.json")).unwrap();
            assert_eq!(sink.publishes, 1);
            assert_eq!(sink.receipt, Some(receipt));
            assert!(read::<GraphDb>(&dir.0.join("emit.json")).unwrap().complete);
        }
    }
}

#[test]
fn graph_emit_epoch_policy_rejects_stale_first_emit_and_accepts_current_or_future() {
    for epoch in [Epoch(3), Epoch(4), Epoch(5)] {
        let dir = TestDir::new();
        let store = DiskTuples::new(dir.0.join("tuples.json"));
        let step = drive(&dir, Some(&store), None, epoch, false, false);
        if epoch < Epoch(4) {
            assert!(matches!(step, GraphStep::Fault(reason) if reason.contains("stale emit")));
            assert_eq!(consume(&store).unwrap(), None);
            assert_eq!(
                read::<SinkDb>(&dir.0.join("sink.json")).unwrap().publishes,
                0
            );
        } else {
            assert!(matches!(step, GraphStep::Emitted { .. }));
            assert_eq!(consume(&store).unwrap(), Some(tuple(2)));
        }
    }
}

// Reference adapter for the epoch gate contract. It protects real test-file
// writes, not production git/session adapters; their integration remains open.
struct DiskEpochGate {
    path: PathBuf,
    owner: std::sync::Arc<std::sync::Mutex<()>>,
}
impl foundry_exec::EpochGate for DiskEpochGate {
    fn with_epoch(
        &self,
        order: &OrderId,
        presented: Epoch,
        effect: &mut dyn FnMut() -> Result<(), PortError>,
    ) -> Result<(), PortError> {
        let _owner = self.owner.lock().unwrap();
        let mut epochs: std::collections::BTreeMap<String, u64> = read(&self.path)?;
        let active = Epoch(*epochs.get(&order.0).unwrap_or(&0));
        if presented < active {
            return Err(PortError::StaleEpoch { active, presented });
        }
        epochs.insert(order.0.clone(), presented.0);
        write(&self.path, &epochs, None)?; // epoch is durable before any protected write
        effect()
    }
}
fn assert_stale_write_isolated(target: &str) {
    use foundry_exec::EpochGate;
    let dir = TestDir::new();
    let path = dir.0.join("epochs.json");
    let destination = dir.0.join(target);
    let order = OrderId("order".into());
    {
        let gate = DiskEpochGate {
            path: path.clone(),
            owner: tuple_store::owner(&path),
        };
        gate.with_epoch(&order, Epoch(5), &mut || {
            assert!(
                gate.owner.try_lock().is_err(),
                "ownership must cover the effect"
            );
            fs::write(&destination, b"current").unwrap();
            Ok(())
        })
        .unwrap();
    }
    let gate = DiskEpochGate {
        owner: tuple_store::owner(&path),
        path,
    };
    let mut invoked = false;
    assert_eq!(
        gate.with_epoch(&order, Epoch(4), &mut || {
            invoked = true;
            fs::write(&destination, b"stale").unwrap();
            Ok(())
        }),
        Err(PortError::StaleEpoch {
            active: Epoch(5),
            presented: Epoch(4)
        })
    );
    assert!(!invoked);
    assert_eq!(fs::read(&destination).unwrap(), b"current");
}
#[test]
fn epoch_gate_stale_sibling_write_is_isolated_independently_of_response_rejection() {
    assert_stale_write_isolated("sibling-artifact");
}
#[test]
fn epoch_gate_stale_ref_write_is_isolated_independently_of_response_rejection() {
    assert_stale_write_isolated("git-ref-model");
}
#[test]
fn epoch_gate_stale_session_write_is_isolated_independently_of_response_rejection() {
    assert_stale_write_isolated("session");
}
#[test]
fn epoch_gate_active_epoch_is_monotonic_across_restart_and_failed_writes() {
    use foundry_exec::EpochGate;
    let dir = TestDir::new();
    let path = dir.0.join("epochs.json");
    let order = OrderId("order".into());
    {
        let gate = DiskEpochGate {
            path: path.clone(),
            owner: tuple_store::owner(&path),
        };
        for epoch in [Epoch(5), Epoch(5), Epoch(7)] {
            gate.with_epoch(&order, epoch, &mut || {
                let epochs: std::collections::BTreeMap<String, u64> = read(&path)?;
                assert_eq!(epochs[&order.0], epoch.0);
                Ok(())
            })
            .unwrap();
        }
        assert_eq!(
            gate.with_epoch(&order, Epoch(9), &mut || Err(PortError::Store(
                "write failed".into()
            ))),
            Err(PortError::Store("write failed".into()))
        );
    }
    let gate = DiskEpochGate {
        owner: tuple_store::owner(&path),
        path,
    };
    for epoch in [Epoch(5), Epoch(7), Epoch(8)] {
        assert_eq!(
            gate.with_epoch(&order, epoch, &mut || panic!("stale callback ran")),
            Err(PortError::StaleEpoch {
                active: Epoch(9),
                presented: epoch
            })
        );
    }
    gate.with_epoch(&order, Epoch(9), &mut || Ok(())).unwrap();
    gate.with_epoch(&OrderId("other".into()), Epoch(1), &mut || Ok(()))
        .unwrap();
}
