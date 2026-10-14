use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use foundry_exec::{
    graph::{GraphDriver, GraphStep},
    ports::{CounterLog, CounterRecord, DispatchHost, IntentLog, OutcomeStore},
    types::{Attempt, ExecutionIntent, ExecutionOutcome, Generation, OrderId, RunId},
    DispatchResult, Epoch,
};
use foundry_pack::{load_pack, PackSnapshot};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

type Key = (OrderId, Attempt, RunId);

struct TestDir(PathBuf);
impl TestDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = PathBuf::from("/tmp/b2c_review/test-data").join(format!("graph-durability-{}-{}",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        use std::io::Write;
        let mut inventory = fs::OpenOptions::new().create(true).append(true)
            .open("/tmp/b2c_review/runtime-temporaries.txt").unwrap();
        for entry in fs::read_dir(&self.0).unwrap() {
            inventory.write_all(format!("{} (deleted)\n", entry.unwrap().path().display()).as_bytes()).unwrap();
        }
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn read<T: DeserializeOwned + Default>(path: &Path) -> T {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => T::default(),
        Err(e) => panic!("counter log read failed: {e}"),
    }
}
fn write<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let temporary = path.with_extension("pending");
    {
        use std::io::Write;
        let mut inventory = std::fs::OpenOptions::new().create(true).append(true)
            .open("/tmp/b2c_review/runtime-temporaries.txt").unwrap();
        inventory.write_all(format!("{} (rename temporary)\n", temporary.display()).as_bytes()).unwrap();
    }
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    fs::write(&temporary, bytes).map_err(|e| e.to_string())?;
    File::open(&temporary).and_then(|f| f.sync_all()).map_err(|e| e.to_string())?;
    fs::rename(&temporary, path).map_err(|e| e.to_string())?;
    File::open(path.parent().unwrap()).and_then(|f| f.sync_all()).map_err(|e| e.to_string())
}

#[derive(Serialize, Deserialize)]
struct Row { order: String, run: u64, record: CounterRecord }
struct DiskCounterLog(PathBuf);
impl CounterLog for DiskCounterLog {
    fn append(&mut self, order: &str, run: u64, record: CounterRecord) -> Result<(), String> {
        let mut rows: Vec<Row> = read(&self.0);
        rows.push(Row { order: order.into(), run, record });
        write(&self.0, &rows)
    }
    fn load(&self, order: &str, run: u64) -> Vec<CounterRecord> {
        read::<Vec<Row>>(&self.0).into_iter()
            .filter(|row| row.order == order && row.run == run)
            .map(|row| row.record).collect()
    }
}

struct CrashAfterAppend(DiskCounterLog);
impl CounterLog for CrashAfterAppend {
    fn append(&mut self, order: &str, run: u64, record: CounterRecord) -> Result<(), String> {
        let bumped = matches!(record, CounterRecord::Bump { .. });
        self.0.append(order, run, record)?;
        if bumped { panic!("simulated process crash after durable counter append"); }
        Ok(())
    }
    fn load(&self, order: &str, run: u64) -> Vec<CounterRecord> {
        self.0.load(order, run)
    }
}

#[derive(Default)]
struct Intents(Vec<ExecutionIntent>);
impl IntentLog for Intents {
    fn append(&mut self, intent: &ExecutionIntent) { self.0.push(intent.clone()); }
    fn mark_started(&mut self, _: &Key, _: u64) {}
    fn latest(&self, key: &Key) -> Option<ExecutionIntent> {
        self.0.iter().rev().find(|i| (i.order.clone(), i.attempt, i.run) == *key).cloned()
    }
    fn get(&self, key: &Key, activation: u64) -> Option<ExecutionIntent> {
        self.0.iter().find(|i| (i.order.clone(), i.attempt, i.run) == *key && i.activation == activation).cloned()
    }
    fn is_started(&self, _: &Key, _: u64) -> bool { false }
}
#[derive(Default)]
struct Outcomes(Vec<ExecutionOutcome>);
impl OutcomeStore for Outcomes {
    fn commit(&mut self, outcome: &ExecutionOutcome) { self.0.push(outcome.clone()); }
    fn latest(&self, key: &Key) -> Option<ExecutionOutcome> {
        self.0.iter().rev().find(|o| o.intent_key == *key).cloned()
    }
    fn get(&self, key: &Key, activation: u64) -> Option<ExecutionOutcome> {
        self.0.iter().find(|o| o.intent_key == *key && o.activation == activation).cloned()
    }
}
#[derive(Default)]
struct Host;
impl DispatchHost for Host {
    type Result = DispatchResult;
    fn dispatch(&mut self, _: &ExecutionIntent) -> DispatchResult { DispatchResult::default() }
    fn dispatch_count(&self) -> u64 { 0 }
    fn recover_result(&mut self, _: &ExecutionIntent) -> Option<DispatchResult> { None }
}
#[derive(Default)]
struct Sink;
impl foundry_exec::emit::EmitSink for Sink {
    fn publish(&mut self, _: &foundry_exec::ArtifactTuple) -> foundry_exec::EmitReceipt { panic!("unused") }
    fn receipt_of(&self, _: &foundry_exec::ArtifactTupleKey) -> Option<foundry_exec::EmitReceipt> { None }
}
#[derive(Default)]
struct Emits;
impl foundry_exec::emit::EmitLog for Emits {
    fn current_epoch(&self) -> Epoch { Epoch(0) }
    fn promotion_of(&self, _: &foundry_exec::ArtifactTupleKey) -> Option<foundry_exec::PromotionReceipt> { None }
    fn emit_of(&self, _: &foundry_exec::ArtifactTupleKey) -> Option<foundry_exec::EmitReceipt> { None }
    fn record_emit(&mut self, _: &foundry_exec::EmitReceipt) {}
    fn mark_complete(&mut self, _: &foundry_exec::ArtifactTupleKey) {}
    fn is_complete(&self, _: &foundry_exec::ArtifactTupleKey) -> bool { false }
}

fn route_pack(dir: &TestDir) -> PackSnapshot {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/fixture-min/v1");
    let policy = fs::read_to_string(source.join("policy.toml")).unwrap()
        .replace("when = \"true\"\ndo = \"emit(InspectionPassed, summary)\"",
            "when = \"true\"\ndo = \"bump(turns)\"\ngoto = \"end_hold\"")
        + "\n[counters]\nturns = 0\n";
    pack_with_policy(dir, policy)
}

fn pack_with_policy(dir: &TestDir, policy: String) -> PackSnapshot {
    use sha2::{Digest, Sha256};
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/fixture-min/v1");
    let manifest = fs::read(source.join("manifest.toml")).unwrap();
    let manifest = String::from_utf8(manifest).unwrap();
    let manifest = manifest.lines().filter(|line| !line.starts_with("digest ="))
        .collect::<Vec<_>>().join("\n") + "\n";
    let lens = fs::read(source.join("lens-a.md")).unwrap();
    let mut files = std::collections::BTreeMap::from([
        ("manifest.toml", manifest.into_bytes()),
        ("policy.toml", policy.into_bytes()),
        ("lens-a.md", lens),
    ]);
    let mut canonical = Vec::new();
    for (name, bytes) in &files {
        canonical.extend_from_slice(&(name.len() as u64).to_le_bytes());
        canonical.extend_from_slice(name.as_bytes());
        canonical.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        canonical.extend_from_slice(bytes);
    }
    let digest = format!("{:x}", Sha256::digest(canonical));
    for (name, bytes) in files.iter_mut() { fs::write(dir.0.join(name), bytes).unwrap(); }
    fs::write(dir.0.join("manifest.toml"), format!("digest = \"{digest}\"\n{}",
        fs::read_to_string(dir.0.join("manifest.toml")).unwrap())).unwrap();
    load_pack(&dir.0).unwrap()
}

fn graph<'a>(
    pack: &'a PackSnapshot, intents: &'a mut Intents, host: &'a mut Host,
    outcomes: &'a mut Outcomes, counters: &'a mut DiskCounterLog, sink: &'a mut Sink,
    emits: &'a mut Emits, run: u64,
) -> GraphDriver<'a, Intents, Host, Outcomes, DiskCounterLog, Sink, Emits> {
    GraphDriver::new(pack, intents, host, outcomes, counters, sink, emits,
        OrderId("durable-order".into()), Attempt(1), RunId(run), Generation(1),
        Epoch(0), "test", None)
}

#[test]
fn route_counter_restores_after_driver_drop_and_matches_uninterrupted_run() {
    let dir = TestDir::new();
    let pack = route_pack(&dir);
    let log_path = dir.0.join("counters.json");
    let mut intents = Intents::default(); let mut host = Host; let mut outcomes = Outcomes::default();
    let mut sink = Sink; let mut emits = Emits;
    {
        let mut counter = DiskCounterLog(log_path.clone());
        let mut driver = graph(&pack, &mut intents, &mut host, &mut outcomes, &mut counter, &mut sink, &mut emits, 1);
        assert!(matches!(driver.advance("done"), GraphStep::Advanced { .. }));
        assert_eq!(driver.counters()["turns"], 1);
    }
    let mut counter = DiskCounterLog(log_path.clone());
    let mut intents2 = Intents::default(); let mut host2 = Host; let mut outcomes2 = Outcomes::default();
    let mut sink2 = Sink; let mut emits2 = Emits;
    let restored = GraphDriver::restore(&pack, &mut intents2, &mut host2, &mut outcomes2,
        &mut counter, &mut sink2, &mut emits2, OrderId("durable-order".into()), Attempt(1),
        RunId(1), Generation(1), Epoch(0), "test", None);
    assert_eq!(restored.counters()["turns"], 1);
    let mut baseline_counter = DiskCounterLog(dir.0.join("baseline.json"));
    let mut baseline_intents = Intents::default(); let mut baseline_host = Host;
    let mut baseline_outcomes = Outcomes::default(); let mut baseline_sink = Sink; let mut baseline_emits = Emits;
    let mut baseline = graph(&pack, &mut baseline_intents, &mut baseline_host, &mut baseline_outcomes,
        &mut baseline_counter, &mut baseline_sink, &mut baseline_emits, 1);
    assert!(matches!(baseline.advance("done"), GraphStep::Advanced { .. }));
    assert_eq!(restored.counters()["turns"], baseline.counters()["turns"]);
}

#[test]
fn route_append_then_reentry_reuses_value_without_double_bump() {
    let dir = TestDir::new(); let pack = route_pack(&dir); let log_path = dir.0.join("counters.json");
    let mut intents = Intents::default(); let mut host = Host; let mut outcomes = Outcomes::default();
    let mut sink = Sink; let mut emits = Emits;
    let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut counter = CrashAfterAppend(DiskCounterLog(log_path.clone()));
        let mut driver = GraphDriver::new(&pack, &mut intents, &mut host, &mut outcomes,
            &mut counter, &mut sink, &mut emits, OrderId("durable-order".into()), Attempt(1),
            RunId(1), Generation(1), Epoch(0), "test", None);
        let _ = driver.advance("done");
    }));
    assert!(crash.is_err());
    let mut counter = DiskCounterLog(log_path.clone());
    let mut intents2 = Intents::default(); let mut host2 = Host; let mut outcomes2 = Outcomes::default();
    let mut sink2 = Sink; let mut emits2 = Emits;
    let mut replay = graph(&pack, &mut intents2, &mut host2, &mut outcomes2, &mut counter, &mut sink2, &mut emits2, 1);
    assert!(matches!(replay.advance("done"), GraphStep::Advanced { .. }));
    assert_eq!(replay.counters()["turns"], 1);
    assert_eq!(counter.load("durable-order", 1).iter().filter(|record|
        matches!(record, CounterRecord::Bump { .. })).count(), 1);
}

#[test]
fn redrive_counter_survives_crash_before_dispatch_commit_and_reuses_on_retry() {
    let dir = TestDir::new();
    let pack = load_pack(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1")).unwrap();
    let path = dir.0.join("counters.json");
    let mut intents = Intents::default(); let mut host = Host; let mut outcomes = Outcomes::default();
    let mut sink = Sink; let mut emits = Emits;
    {
        let mut counter = DiskCounterLog(path.clone());
        let mut driver = GraphDriver::new(&pack, &mut intents, &mut host, &mut outcomes,
            &mut counter, &mut sink, &mut emits, OrderId("durable-order".into()), Attempt(1),
            RunId(3), Generation(1), Epoch(0), "test", None);
        driver.arm_fault(foundry_exec::CrashPoint::W1);
        assert!(matches!(driver.advance("fixdie"), GraphStep::Fault(_)));
        assert_eq!(driver.counters()["fix_rounds"], 0);
    }
    let mut counter = DiskCounterLog(path);
    let mut intents2 = Intents::default(); let mut host2 = Host; let mut outcomes2 = Outcomes::default();
    let mut sink2 = Sink; let mut emits2 = Emits;
    let mut restored = GraphDriver::restore(&pack, &mut intents2, &mut host2, &mut outcomes2,
        &mut counter, &mut sink2, &mut emits2, OrderId("durable-order".into()), Attempt(1),
        RunId(3), Generation(1), Epoch(0), "test", None);
    assert_eq!(restored.counters()["fix_rounds"], 1);
    assert!(matches!(restored.advance("fixdie"), GraphStep::Advanced { .. }));
    assert_eq!(restored.counters()["fix_rounds"], 1);
}

#[test]
fn counter_log_keeps_runs_isolated_across_driver_restarts() {
    let dir = TestDir::new(); let pack = route_pack(&dir); let path = dir.0.join("counters.json");
    for run in [1, 2] {
        let mut counter = DiskCounterLog(path.clone());
        let mut intents = Intents::default(); let mut host = Host; let mut outcomes = Outcomes::default();
        let mut sink = Sink; let mut emits = Emits;
        let mut driver = graph(&pack, &mut intents, &mut host, &mut outcomes, &mut counter, &mut sink, &mut emits, run);
        assert!(matches!(driver.advance("done"), GraphStep::Advanced { .. }));
        assert_eq!(driver.counters()["turns"], 1);
    }
    let counter = DiskCounterLog(path);
    assert_eq!(counter.load("durable-order", 1).into_iter().find(|r| matches!(r, CounterRecord::Bump { .. })).unwrap(), CounterRecord::Bump { node_id: "done".into(), counter: "turns".into(), value: 1 });
    assert_eq!(counter.load("durable-order", 2).into_iter().find(|r| matches!(r, CounterRecord::Bump { .. })).unwrap(), CounterRecord::Bump { node_id: "done".into(), counter: "turns".into(), value: 1 });
    assert!(counter.load("durable-order", 3).is_empty());
}

#[test]
fn route_self_loop_records_each_visit() {
    let dir = TestDir::new(); let pack = route_pack(&dir); let path = dir.0.join("counters.json");
    let mut intents = Intents::default(); let mut host = Host; let mut outcomes = Outcomes::default();
    let mut sink = Sink; let mut emits = Emits; let mut counter = DiskCounterLog(path);
    let mut driver = graph(&pack, &mut intents, &mut host, &mut outcomes, &mut counter, &mut sink, &mut emits, 21);
    for _ in 0..3 {
        assert!(matches!(driver.advance("done"), GraphStep::Advanced { next, .. } if next == "end_hold"));
    }
    assert_eq!(driver.counters()["turns"], 3);
    let records = counter.load("durable-order", 21);
    assert_eq!(records.iter().filter(|r| matches!(r, CounterRecord::Bump { node_id, .. } if node_id == "done")).count(), 3);
    assert_eq!(records.iter().filter(|r| matches!(r, CounterRecord::Completed { node_id, goto: Some(goto) } if node_id == "done" && goto == "end_hold")).count(), 3);
}

#[test]
fn pending_bump_is_adopted_once_after_crash() {
    let dir = TestDir::new(); let pack = route_pack(&dir); let path = dir.0.join("counters.json");
    let mut intents = Intents::default(); let mut host = Host; let mut outcomes = Outcomes::default();
    let mut sink = Sink; let mut emits = Emits;
    let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut counter = CrashAfterAppend(DiskCounterLog(path.clone()));
        let mut driver = GraphDriver::new(&pack, &mut intents, &mut host, &mut outcomes, &mut counter,
            &mut sink, &mut emits, OrderId("durable-order".into()), Attempt(1), RunId(22), Generation(1), Epoch(0), "test", None);
        let _ = driver.advance("done");
    }));
    assert!(crash.is_err());
    let mut counter = DiskCounterLog(path); let mut intents = Intents::default();
    let mut host = Host; let mut outcomes = Outcomes::default(); let mut sink = Sink; let mut emits = Emits;
    let mut restored = GraphDriver::restore(&pack, &mut intents, &mut host, &mut outcomes, &mut counter,
        &mut sink, &mut emits, OrderId("durable-order".into()), Attempt(1), RunId(22), Generation(1), Epoch(0), "test", None);
    assert_eq!(restored.counters()["turns"], 1);
    assert!(matches!(restored.start(), GraphStep::Advanced { .. }));
    assert_eq!(restored.counters()["turns"], 1);
    assert_eq!(counter.load("durable-order", 22).iter().filter(|r| matches!(r, CounterRecord::Bump { .. })).count(), 1);
}

#[test]
fn completed_route_restores_to_next_node_without_rebumping() {
    let dir = TestDir::new(); let pack = route_pack(&dir); let path = dir.0.join("counters.json");
    let mut intents = Intents::default(); let mut host = Host; let mut outcomes = Outcomes::default();
    let mut sink = Sink; let mut emits = Emits;
    {
        let mut counter = DiskCounterLog(path.clone());
        let mut driver = graph(&pack, &mut intents, &mut host, &mut outcomes, &mut counter, &mut sink, &mut emits, 23);
        assert!(matches!(driver.advance("done"), GraphStep::Advanced { next, .. } if next == "end_hold"));
    }
    let mut counter = DiskCounterLog(path); let mut intents = Intents::default();
    let mut host = Host; let mut outcomes = Outcomes::default(); let mut sink = Sink; let mut emits = Emits;
    let mut restored = GraphDriver::restore(&pack, &mut intents, &mut host, &mut outcomes, &mut counter,
        &mut sink, &mut emits, OrderId("durable-order".into()), Attempt(1), RunId(23), Generation(1), Epoch(0), "test", None);
    assert!(matches!(restored.start(), GraphStep::HoldRequired(_)));
    assert_eq!(restored.counters()["turns"], 1);
    assert_eq!(counter.load("durable-order", 23).iter().filter(|r| matches!(r, CounterRecord::Bump { .. })).count(), 1);
}

#[test]
fn n_x_n_cycle_bumps_on_every_n_visit() {
    let dir = TestDir::new(); let pack = route_pack(&dir); let path = dir.0.join("counters.json");
    let mut intents = Intents::default(); let mut host = Host; let mut outcomes = Outcomes::default();
    let mut sink = Sink; let mut emits = Emits; let mut counter = DiskCounterLog(path);
    let mut driver = graph(&pack, &mut intents, &mut host, &mut outcomes, &mut counter, &mut sink, &mut emits, 24);
    for _ in 0..4 {
        assert!(matches!(driver.advance("done"), GraphStep::Advanced { next, .. } if next == "end_hold"));
        assert!(matches!(driver.advance("end_hold"), GraphStep::HoldRequired(_)));
    }
    assert_eq!(driver.counters()["turns"], 4);
    assert_eq!(counter.load("durable-order", 24).iter().filter(|r| matches!(r, CounterRecord::Bump { node_id, .. } if node_id == "done")).count(), 4);
}

#[test]
fn b2c_disk_log_isolates_orders_with_the_same_run() {
    let dir = TestDir::new(); let pack = route_pack(&dir);
    let mut counter = DiskCounterLog(dir.0.join("orders.json"));
    counter.append("other-order", 1, CounterRecord::Bump {
        node_id: "done".into(), counter: "turns".into(), value: u64::MAX,
    }).unwrap();
    let mut intents = Intents::default(); let mut host = Host; let mut outcomes = Outcomes::default();
    let mut sink = Sink; let mut emits = Emits;
    {
        let mut driver = GraphDriver::restore(&pack, &mut intents, &mut host, &mut outcomes,
            &mut counter, &mut sink, &mut emits, OrderId("durable-order".into()), Attempt(1),
            RunId(1), Generation(1), Epoch(0), "test", None);
        assert_eq!(driver.counters()["turns"], 0);
        assert!(matches!(driver.advance("done"), GraphStep::Advanced { .. }));
        assert_eq!(driver.counters()["turns"], 1);
    }
    assert_eq!(counter.load("other-order", 1), vec![CounterRecord::Bump {
        node_id: "done".into(), counter: "turns".into(), value: u64::MAX,
    }]);
    assert!(counter.load("durable-order", 2).is_empty());
}

