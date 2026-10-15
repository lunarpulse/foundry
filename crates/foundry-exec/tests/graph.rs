// Drops of executor/host adapters below are intentional: they terminate &mut
// borrow chains so the underlying worker/ledger can be re-read. Types are
// non-Drop by design, hence the targeted allow.
#![allow(clippy::drop_non_drop)]

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::Utc;
use foundry_exec::{
    emit::{complete_after_w4, emit_once, EmitLog, EmitOutcome, EmitSink},
    graph::{GraphDriver, GraphStep},
    ports::{CounterLog, CounterRecord, DispatchHost, IntentLog, OutcomeStore},
    types::{
        ArtifactLocation, ArtifactTuple, ArtifactTupleKey, Attempt, EmitReceipt, Epoch,
        ExecutionIntent, ExecutionOutcome, Generation, ImmutableCommit, OrderId, PromotionReceipt,
        RunId,
    },
};
use foundry_pack::load_pack;

#[path = "support/tuple_store.rs"]
mod tuple_store;
use tuple_store::MemoryTuples;

type Key = (OrderId, Attempt, RunId);

struct Log {
    intents: Vec<ExecutionIntent>,
    started: HashSet<(Key, u64)>,
    outcomes: HashMap<(Key, u64), ExecutionOutcome>,
    promotions: HashMap<ArtifactTupleKey, PromotionReceipt>,
    emits: HashMap<ArtifactTupleKey, EmitReceipt>,
    completed: HashSet<ArtifactTupleKey>,
    epoch: Epoch,
}

impl Default for Log {
    fn default() -> Self {
        Self {
            intents: Vec::new(),
            started: HashSet::new(),
            outcomes: HashMap::new(),
            promotions: HashMap::new(),
            emits: HashMap::new(),
            completed: HashSet::new(),
            epoch: Epoch(0),
        }
    }
}

impl IntentLog for Log {
    fn append(&mut self, intent: &ExecutionIntent) {
        self.intents.push(intent.clone());
    }
    fn mark_started(&mut self, key: &Key, activation: u64) {
        self.started.insert((key.clone(), activation));
    }
    fn latest(&self, key: &Key) -> Option<ExecutionIntent> {
        self.intents
            .iter()
            .rev()
            .find(|i| (i.order.clone(), i.attempt, i.run) == *key)
            .cloned()
    }
    fn get(&self, key: &Key, activation: u64) -> Option<ExecutionIntent> {
        self.intents
            .iter()
            .find(|i| (i.order.clone(), i.attempt, i.run) == *key && i.activation == activation)
            .cloned()
    }
    fn is_started(&self, key: &Key, activation: u64) -> bool {
        self.started.contains(&(key.clone(), activation))
    }
}

impl OutcomeStore for Log {
    fn commit(&mut self, outcome: &ExecutionOutcome) {
        self.outcomes
            .entry((outcome.intent_key.clone(), outcome.activation))
            .or_insert_with(|| outcome.clone());
    }
    fn latest(&self, key: &Key) -> Option<ExecutionOutcome> {
        self.outcomes.values()
            .filter(|outcome| &outcome.intent_key == key)
            .max_by_key(|outcome| outcome.activation)
            .cloned()
    }
    fn get(&self, key: &Key, activation: u64) -> Option<ExecutionOutcome> {
        self.outcomes.get(&(key.clone(), activation)).cloned()
    }
}

#[derive(Default)]
struct MemoryCounterLog(Vec<(String, u64, CounterRecord)>);

impl CounterLog for MemoryCounterLog {
    fn append(&mut self, order: &str, run: u64, record: CounterRecord) -> Result<(), String> {
        self.0.push((order.into(), run, record));
        Ok(())
    }
    fn load(&self, order: &str, run: u64) -> Vec<CounterRecord> {
        self.0.iter().filter(|(o, r, _)| o == order && *r == run)
            .map(|(_, _, record)| record.clone()).collect()
    }
}

impl EmitLog for Log {
    fn current_epoch(&self) -> Epoch {
        self.epoch
    }
    fn promotion_of(&self, key: &ArtifactTupleKey) -> Option<PromotionReceipt> {
        self.promotions.get(key).cloned()
    }
    fn emit_of(&self, key: &ArtifactTupleKey) -> Option<EmitReceipt> {
        self.emits.get(key).cloned()
    }
    fn record_emit(&mut self, receipt: &EmitReceipt) {
        self.emits
            .entry(receipt.key.clone())
            .or_insert_with(|| receipt.clone());
    }
    fn mark_complete(&mut self, key: &ArtifactTupleKey) {
        self.completed.insert(key.clone());
    }
    fn is_complete(&self, key: &ArtifactTupleKey) -> bool {
        self.completed.contains(key)
    }
}

#[derive(Default)]
struct Host {
    calls: u64,
    cursor: Option<String>,
    recovered: Option<(Key, u64, foundry_exec::DispatchResult)>,
}
impl DispatchHost for Host {
    type Result = foundry_exec::DispatchResult;
    fn dispatch(&mut self, intent: &ExecutionIntent) -> Self::Result {
        self.calls += 1;
        let result = foundry_exec::DispatchResult {
            cursor: self.cursor.clone(),
            ..Default::default()
        };
        self.recovered = Some(((intent.order.clone(), intent.attempt, intent.run),
            intent.activation, result.clone()));
        result
    }
    fn dispatch_count(&self) -> u64 {
        self.calls
    }
    fn recover_result(&mut self, intent: &ExecutionIntent) -> Option<Self::Result> {
        self.recovered.as_ref().and_then(|(key, activation, result)| {
            (*key == (intent.order.clone(), intent.attempt, intent.run)
                && *activation == intent.activation).then(|| result.clone())
        })
    }
}

#[derive(Default)]
struct Sink {
    publishes: u64,
    receipts: HashMap<ArtifactTupleKey, EmitReceipt>,
    requests: Vec<(String, String)>,
}
impl EmitSink for Sink {
    fn publish_named(&mut self, tuple: &ArtifactTuple, event: &str, payload: &str) -> EmitReceipt {
        self.requests.push((event.into(), payload.into()));
        let mut receipt = self.publish(tuple);
        receipt.event = event.into();
        self.receipts.insert(receipt.key.clone(), receipt.clone());
        receipt
    }
    fn publish(&mut self, tuple: &ArtifactTuple) -> EmitReceipt {
        self.publishes += 1;
        let receipt = EmitReceipt {
            key: tuple.key(),
            event: "InspectionPassed".into(),
            at: Utc::now(),
        };
        self.receipts.insert(receipt.key.clone(), receipt.clone());
        receipt
    }
    fn receipt_of(&self, key: &ArtifactTupleKey) -> Option<EmitReceipt> {
        self.receipts.get(key).cloned()
    }
}

fn tuple() -> ArtifactTuple {
    ArtifactTuple {
        order: OrderId("order-1".into()),
        attempt: Attempt(1),
        run: RunId(2),
        generation: Generation(3),
        immutable_commit: ImmutableCommit("abc".into()),
        artifact_location: ArtifactLocation("artifact://one".into()), generations: Vec::new(),
    }
}
fn promote(log: &mut Log, tuple: &ArtifactTuple) {
    log.promotions.insert(
        tuple.key(),
        PromotionReceipt {
            tuple: tuple.key(),
            promoted_at: Utc::now(),
        },
    );
}

#[test]
fn duplicate_emit_returns_same_receipt_without_publishing_twice() {
    let tuple = tuple();
    let mut log = Log::default();
    promote(&mut log, &tuple);
    let mut sink = Sink::default();
    let first = emit_once(&mut sink, &mut log, &tuple, Epoch(0));
    let second = emit_once(&mut sink, &mut log, &tuple, Epoch(0));
    let EmitOutcome::Published(receipt) = first else {
        panic!("first emit should publish")
    };
    assert_eq!(second, EmitOutcome::Existing(receipt));
    assert_eq!(sink.publishes, 1);
}

#[test]
fn stale_first_emit_is_rejected() {
    let tuple = tuple();
    let mut log = Log {
        epoch: Epoch(2),
        ..Log::default()
    };
    promote(&mut log, &tuple);
    let mut sink = Sink::default();
    assert_eq!(
        emit_once(&mut sink, &mut log, &tuple, Epoch(1)),
        EmitOutcome::StaleRejected
    );
    assert_eq!(sink.publishes, 0);
}

#[test]
fn w3_recovery_emits_promoted_tuple_once() {
    let tuple = tuple();
    let mut log = Log::default();
    promote(&mut log, &tuple);
    let mut sink = Sink::default();
    assert!(matches!(
        emit_once(&mut sink, &mut log, &tuple, Epoch(0)),
        EmitOutcome::Published(_)
    ));
    assert_eq!(sink.publishes, 1);
}

#[test]
fn w4_recovery_only_marks_completion() {
    let tuple = tuple();
    let mut log = Log::default();
    promote(&mut log, &tuple);
    let mut sink = Sink::default();
    let receipt = match emit_once(&mut sink, &mut log, &tuple, Epoch(0)) {
        EmitOutcome::Published(r) => r,
        other => panic!("unexpected outcome {other:?}"),
    };
    // Recovery observes the committed receipt and completes without calling publish.
    assert_eq!(
        emit_once(&mut sink, &mut log, &tuple, Epoch(0)),
        EmitOutcome::Existing(receipt)
    );
    assert!(complete_after_w4(&mut log, &tuple.key()));
    assert!(!complete_after_w4(&mut log, &tuple.key()));
    assert_eq!(sink.publishes, 1);
}

#[test]
fn fixture_pack_dispatches_to_route_and_route_emits() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/fixture-min/v1");
    let pack = load_pack(root).expect("fixture-min loads");
    let mut intents = Log::default();
    let mut outcomes = Log::default();
    let mut counters = MemoryCounterLog::default();
    let mut emit_log = Log::default();
    let artifact = tuple();
    let tuples = MemoryTuples::default();
    promote(&mut emit_log, &artifact);
    let mut sink = Sink::default();
    let mut host = Host {
        calls: 0,
        cursor: Some("done".into()),
        ..Default::default()
    };
    let mut driver = GraphDriver::new(
        &pack,
        &mut intents,
        &mut host,
        &mut outcomes,
        &mut counters,
        &mut sink,
        &mut emit_log,
        OrderId("order-1".into()),
        Attempt(1),
        RunId(2),
        Generation(3),
        Epoch(0),
        "fixture",
        Some(artifact),
    ).with_tuple_store(&tuples);
    assert!(matches!(driver.start(), GraphStep::Advanced { ref next, .. } if next == "done"));
    assert!(matches!(driver.advance("done"), GraphStep::Emitted { .. }));
}

#[test]
fn graph_w3_and_w4_resume_without_republishing() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/fixture-min/v1");
    let pack = load_pack(root).expect("fixture-min loads");
    let mut intents = Log::default();
    let mut outcomes = Log::default();
    let mut counters = MemoryCounterLog::default();
    let mut emit_log = Log::default();
    let artifact = tuple();
    let tuples = MemoryTuples::default();
    promote(&mut emit_log, &artifact);
    let mut sink = Sink::default();
    let mut host = Host {
        calls: 0,
        cursor: Some("done".into()),
        ..Default::default()
    };
    let mut driver = GraphDriver::new(
        &pack,
        &mut intents,
        &mut host,
        &mut outcomes,
        &mut counters,
        &mut sink,
        &mut emit_log,
        OrderId("order-1".into()),
        Attempt(1),
        RunId(2),
        Generation(3),
        Epoch(0),
        "fixture",
        Some(artifact.clone()),
    ).with_tuple_store(&tuples);
    driver.arm_fault(foundry_exec::CrashPoint::W3);
    assert!(matches!(driver.start(), GraphStep::Advanced { .. }));
    assert!(matches!(driver.advance("done"), GraphStep::Fault(_)));
    driver.arm_fault(foundry_exec::CrashPoint::W4);
    assert!(matches!(driver.advance("done"), GraphStep::Fault(_)));
    assert!(matches!(driver.advance("done"), GraphStep::Emitted { .. }));
    assert_eq!(sink.publishes, 1);
}

#[test]
fn route_condition_uses_current_counter_values() {
    let below = foundry_pack::parse_expr("count_lt(loopbacks, 2)").unwrap();
    assert!(foundry_pack::eval_condition(
        &below,
        &BTreeMap::from([("loopbacks".into(), 1)])
    ));
    assert!(!foundry_pack::eval_condition(
        &below,
        &BTreeMap::from([("loopbacks".into(), 2)])
    ));
}

#[test]
fn redrive_dispatches_next_activation_with_payload_context() {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
    let pack = load_pack(root).expect("review pack loads");
    let redrive = pack
        .nodes()
        .iter()
        .find(|node| node.kind == "redrive")
        .unwrap();
    let mut intents = Log::default();
    let mut outcomes = Log::default();
    let mut counters = MemoryCounterLog::default();
    let mut emit_log = Log::default();
    let mut sink = Sink::default();
    let mut host = Host::default();
    let mut driver = GraphDriver::new(
        &pack,
        &mut intents,
        &mut host,
        &mut outcomes,
        &mut counters,
        &mut sink,
        &mut emit_log,
        OrderId("order-1".into()),
        Attempt(1),
        RunId(2),
        Generation(3),
        Epoch(0),
        "fixture",
        None,
    );
    assert!(matches!(
        driver.advance(&redrive.id),
        GraphStep::Advanced { .. }
    ));
    assert_eq!(driver.activation(), 2);
    assert_eq!(intents.intents[0].activation, 1);
    assert!(intents.intents[0].external_ref.starts_with("redrive:"));
}

// Review regressions keep the original eight tests and their assertions intact.
#[derive(Default)]
struct Rig {
    intents: Log,
    host: Host,
    outcomes: Log,
    counters: MemoryCounterLog,
    sink: Sink,
    emits: Log,
    tuples: MemoryTuples,
}

impl Rig {
    fn promoted() -> Self {
        let mut rig = Self::default();
        promote(&mut rig.emits, &tuple());
        rig
    }

    fn driver<'a>(&'a mut self, pack: &'a foundry_pack::PackSnapshot)
        -> GraphDriver<'a, Log, Host, Log, MemoryCounterLog, Sink, Log>
    {
        GraphDriver::new(pack, &mut self.intents, &mut self.host, &mut self.outcomes,
            &mut self.counters, &mut self.sink, &mut self.emits, OrderId("order-1".into()), Attempt(1),
            RunId(2), Generation(3), Epoch(0), "fixture", Some(tuple())).with_tuple_store(&self.tuples)
    }
}

fn review_pack() -> foundry_pack::PackSnapshot {
    load_pack(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../skills/bmad-review-full/v1")).unwrap()
}

#[test]
fn review_redrive_w1_w2_retries_keep_activation_and_bump_once() {
    let pack = review_pack();
    for point in [foundry_exec::CrashPoint::W1, foundry_exec::CrashPoint::W2] {
        let mut rig = Rig::default();
        {
            let mut driver = rig.driver(&pack);
            driver.arm_fault(point);
            assert!(matches!(driver.advance("fixdie"), GraphStep::Fault(_)));
            assert_eq!(driver.activation(), 0);
            assert_eq!(driver.counters()["fix_rounds"], 0);
            assert!(matches!(driver.advance("fixdie"), GraphStep::Advanced { ref next, .. } if next == "lenses"));
            assert_eq!(driver.activation(), 2);
            assert_eq!(driver.counters()["fix_rounds"], 1);
        }
        assert_eq!(rig.intents.intents.len(), 1);
        assert_eq!(rig.intents.intents[0].activation, 1);
        assert_eq!(rig.host.calls, 1);
        assert_eq!(rig.outcomes.outcomes.len(), 1);
    }
}

#[test]
fn review_redrive_pending_activation_survives_driver_recreation() {
    let pack = review_pack();
    for point in [foundry_exec::CrashPoint::W1, foundry_exec::CrashPoint::W2] {
        let mut rig = Rig::default();
        {
            let mut driver = rig.driver(&pack);
            driver.arm_fault(point);
            assert!(matches!(driver.advance("fixdie"), GraphStep::Fault(_)));
        }
        {
            let mut recovered = rig.driver(&pack);
            assert_eq!(recovered.activation(), 1);
            assert!(matches!(recovered.advance("fixdie"), GraphStep::Advanced { .. }));
            assert_eq!(recovered.counters()["fix_rounds"], 1);
        }
        assert_eq!(rig.intents.intents.len(), 1);
        assert_eq!(rig.host.calls, 1);
    }
}

#[test]
fn review_dispatch_then_redrive_uses_the_next_unused_activation() {
    let pack = review_pack();
    let mut rig = Rig::default();
    {
        let mut driver = rig.driver(&pack);
        assert!(matches!(driver.start(), GraphStep::Advanced { ref next, .. } if next == "route1"));
        assert_eq!(driver.activation(), 1);
        assert!(matches!(driver.advance("fixdie"), GraphStep::Advanced { .. }));
        assert_eq!(driver.activation(), 2);
    }
    assert_eq!(rig.intents.intents.iter().map(|intent| intent.activation).collect::<Vec<_>>(), [0, 1]);
}

#[test]
fn review_route_refuses_unknown_context_before_fallback_or_emit() {
    let pack = review_pack();
    let mut rig = Rig::promoted();
    assert!(matches!(rig.driver(&pack).advance("route1"), GraphStep::Fault(ref reason)
        if reason.contains("runtime context")));
    assert_eq!(rig.sink.publishes, 0);
    assert!(rig.emits.completed.is_empty());
}

#[test]
fn review_duplicate_receipts_precede_fencing_and_promotion_checks() {
    let artifact = tuple();
    let mut rig = Rig::promoted();
    let EmitOutcome::Published(receipt) = emit_once(&mut rig.sink, &mut rig.emits, &artifact, Epoch(0)) else {
        panic!("first publication required")
    };
    rig.emits.epoch = Epoch(10);
    rig.emits.promotions.clear();
    rig.sink.receipts.clear(); // Orchestration receipt alone must suffice at W4.
    for epoch in [Epoch(0), Epoch(10), Epoch(11)] {
        assert_eq!(emit_once(&mut rig.sink, &mut rig.emits, &artifact, epoch), EmitOutcome::Existing(receipt.clone()));
    }
    assert_eq!(rig.sink.publishes, 1);
    assert!(complete_after_w4(&mut rig.emits, &artifact.key()));
    assert!(!complete_after_w4(&mut rig.emits, &artifact.key()));
}

#[test]
fn review_sink_only_receipt_is_reconciled_before_completion() {
    let artifact = tuple();
    let mut rig = Rig::default();
    let receipt = rig.sink.publish(&artifact);
    rig.emits.epoch = Epoch(2);
    assert_eq!(emit_once(&mut rig.sink, &mut rig.emits, &artifact, Epoch(0)), EmitOutcome::Existing(receipt.clone()));
    assert_eq!(rig.emits.emit_of(&artifact.key()), Some(receipt));
    assert!(complete_after_w4(&mut rig.emits, &artifact.key()));
    assert_eq!(rig.sink.publishes, 1);
}

#[test]
fn review_completion_requires_emit_and_stale_rejection_precedes_missing_promotion() {
    let artifact = tuple();
    let mut rig = Rig::default();
    rig.emits.epoch = Epoch(2);
    assert!(!complete_after_w4(&mut rig.emits, &artifact.key()));
    assert!(rig.emits.completed.is_empty());
    assert_eq!(emit_once(&mut rig.sink, &mut rig.emits, &artifact, Epoch(1)), EmitOutcome::StaleRejected);
    assert_eq!(emit_once(&mut rig.sink, &mut rig.emits, &artifact, Epoch(2)), EmitOutcome::PromotionRequired);
    promote(&mut rig.emits, &artifact);
    // B5 accepts current and future epochs for first publication.
    assert!(matches!(emit_once(&mut rig.sink, &mut rig.emits, &artifact, Epoch(3)), EmitOutcome::Published(_)));
    assert_eq!(rig.sink.publishes, 1);
}

#[test]
fn review_activation_exhaustion_stops_before_dispatch() {
    let pack = review_pack();
    let mut rig = Rig::default();
    let key = (OrderId("order-1".into()), Attempt(1), RunId(2));
    rig.intents.append(&ExecutionIntent {
        order: key.0.clone(), attempt: key.1, run: key.2, activation: u64::MAX,
        generation: Generation(3), epoch: Epoch(0), external_ref: "work".into(), recorded_at: Utc::now(),
    });
    for cursor in ["lenses", "fixdie"] {
        assert!(matches!(rig.driver(&pack).advance(cursor), GraphStep::Fault(ref reason) if reason.contains("overflow")));
    }
    assert_eq!(rig.host.calls, 0);
    assert_eq!(rig.intents.intents.len(), 1);
}

struct ReviewDir(std::path::PathBuf);
impl ReviewDir {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join("test-data").join(format!(
            "graph-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for ReviewDir {
    fn drop(&mut self) {
        use std::io::Write;
        let mut inventory = std::fs::OpenOptions::new().create(true).append(true)
            .open(std::env::temp_dir().join("runtime-temporaries.txt")).unwrap();
        for entry in std::fs::read_dir(&self.0).unwrap() {
            inventory.write_all(format!("{} (deleted)\n", entry.unwrap().path().display()).as_bytes()).unwrap();
        }
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

// Validated packs, rather than bypassing PackSnapshot's private fields.
fn changed_fixture(change: impl FnOnce(String) -> String) -> (ReviewDir, foundry_pack::PackSnapshot) {
    changed_fixture_with_outputs(change, "")
}

fn changed_fixture_with_outputs(
    change: impl FnOnce(String) -> String,
    outputs: &str,
) -> (ReviewDir, foundry_pack::PackSnapshot) {
    use sha2::{Digest, Sha256};
    let dir = ReviewDir::new();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/fixture-min/v1");
    let manifest = std::fs::read_to_string(source.join("manifest.toml")).unwrap();
    let manifest = if outputs.is_empty() { manifest } else {
        manifest.replace("\"policy.toml\"]", "\"policy.toml\", \"outputs.toml\"]")
    };
    let clean = manifest.lines().filter(|line| !line.starts_with("digest ="))
        .collect::<Vec<_>>().join("\n") + "\n";
    let policy = change(std::fs::read_to_string(source.join("policy.toml")).unwrap());
    let lens = std::fs::read(source.join("lens-a.md")).unwrap();
    let mut files = BTreeMap::from([
        ("manifest.toml", clean.into_bytes()), ("policy.toml", policy.as_bytes().to_vec()),
        ("lens-a.md", lens),
    ]);
    if !outputs.is_empty() {
        files.insert("outputs.toml", outputs.as_bytes().to_vec());
    }
    let mut canonical = Vec::new();
    for (name, bytes) in &files {
        canonical.extend_from_slice(&(name.len() as u64).to_le_bytes());
        canonical.extend_from_slice(name.as_bytes());
        canonical.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        canonical.extend_from_slice(bytes);
    }
    let digest = format!("{:x}", Sha256::digest(canonical));
    for (name, bytes) in files {
        std::fs::write(dir.0.join(name), bytes).unwrap();
    }
    std::fs::write(dir.0.join("manifest.toml"), format!("digest = \"{digest}\"\n{}",
        std::fs::read_to_string(dir.0.join("manifest.toml")).unwrap())).unwrap();
    let pack = load_pack(&dir.0).unwrap();
    (dir, pack)
}

#[test]
fn review_route_first_match_spaced_actions_and_bump_state() {
    let (_dir, pack) = changed_fixture(|policy| policy.replace(
        "[[nodes.branches]]\nwhen = \"true\"\ndo = \"emit(InspectionPassed, summary)\"",
        "[[nodes.branches]]\nwhen = 'count_lt(turns, 1)'\ndo = 'bump (turns)'\ngoto = 'end_hold'\n\n[[nodes.branches]]\nwhen = 'true'\ndo = 'emit ( InspectionFailed , summary )'",
    ) + "\n[counters]\nturns = 0\n");
    let mut rig = Rig::promoted();
    {
        let mut driver = rig.driver(&pack);
        assert!(matches!(driver.advance("done"), GraphStep::Advanced { ref next, ref effects }
            if next == "end_hold" && effects == &["bump:turns=1"]));
        assert_eq!(driver.counters()["turns"], 1);
        assert!(matches!(driver.advance("done"), GraphStep::Emitted { ref receipt }
            if receipt.event == "InspectionFailed"));
    }
    assert_eq!(rig.sink.publishes, 1);
}

#[test]
fn review_route_bump_is_not_replayed_or_used_for_branch_selection_after_crash() {
    let (_dir, pack) = changed_fixture(|policy| policy.replace(
        "[[nodes.branches]]\nwhen = \"true\"\ndo = \"emit(InspectionPassed, summary)\"",
        "[[nodes.branches]]\nwhen = 'count_lt(turns, 1)'\ndo = 'bump (turns); emit (InspectionPassed, summary)'\n\n[[nodes.branches]]\nwhen = 'true'\ngoto = 'end_hold'",
    ) + "\n[counters]\nturns = 0\n");
    for point in [foundry_exec::CrashPoint::W3, foundry_exec::CrashPoint::W4] {
        let mut rig = Rig::promoted();
        {
            let mut driver = rig.driver(&pack);
            driver.arm_fault(point);
            assert!(matches!(driver.advance("done"), GraphStep::Fault(_)));
            assert_eq!(driver.counters()["turns"], 0);
            assert!(matches!(driver.advance("done"), GraphStep::Emitted { .. }));
            assert_eq!(driver.counters()["turns"], 1);
        }
        assert_eq!(rig.sink.publishes, 1);
        assert!(rig.emits.is_complete(&tuple().key()));
    }
}

#[test]
fn review_revert_stops_before_staged_bump_or_graph_advance() {
    let (_dir, pack) = changed_fixture(|policy| policy.replace(
        "do = \"emit(InspectionPassed, summary)\"",
        "do = 'bump(turns); revert (origin)'\ngoto = 'end_hold'",
    ) + "\n[counters]\nturns = 0\n");
    let mut rig = Rig::promoted();
    {
        let mut driver = rig.driver(&pack);
        assert!(matches!(driver.advance("done"), GraphStep::Fault(ref reason) if reason.contains("revert")));
        assert_eq!(driver.counters()["turns"], 0);
    }
    assert_eq!(rig.sink.publishes, 0);
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct EmitState {
    promotions: Vec<PromotionReceipt>,
    emits: Vec<EmitReceipt>,
    completed: Vec<ArtifactTupleKey>,
    publishes: u64,
    epoch: u64,
}

fn read_emit(path: &std::path::Path) -> EmitState {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => EmitState::default(),
        Err(error) => panic!("durable emit read failed: {error}"),
    }
}

fn write_emit(path: &std::path::Path, state: &EmitState) {
    let pending = path.with_extension("pending");
    {
        use std::io::Write;
        let mut inventory = std::fs::OpenOptions::new().create(true).append(true)
            .open(std::env::temp_dir().join("runtime-temporaries.txt")).unwrap();
        inventory.write_all(format!("{} (rename temporary)\n", pending.display()).as_bytes()).unwrap();
    }
    std::fs::write(&pending, serde_json::to_vec(state).unwrap()).unwrap();
    std::fs::File::open(&pending).unwrap().sync_all().unwrap();
    std::fs::rename(&pending, path).unwrap();
    std::fs::File::open(path.parent().unwrap()).unwrap().sync_all().unwrap();
}

struct DiskEmitLog(std::path::PathBuf);
impl EmitLog for DiskEmitLog {
    fn current_epoch(&self) -> Epoch { Epoch(read_emit(&self.0).epoch) }
    fn promotion_of(&self, key: &ArtifactTupleKey) -> Option<PromotionReceipt> {
        read_emit(&self.0).promotions.into_iter().find(|receipt| &receipt.tuple == key)
    }
    fn emit_of(&self, key: &ArtifactTupleKey) -> Option<EmitReceipt> {
        read_emit(&self.0).emits.into_iter().find(|receipt| &receipt.key == key)
    }
    fn record_emit(&mut self, receipt: &EmitReceipt) {
        let mut state = read_emit(&self.0);
        if !state.emits.iter().any(|existing| existing.key == receipt.key) {
            state.emits.push(receipt.clone());
            write_emit(&self.0, &state);
        }
    }
    fn mark_complete(&mut self, key: &ArtifactTupleKey) {
        let mut state = read_emit(&self.0);
        assert!(state.emits.iter().any(|receipt| &receipt.key == key));
        if !state.completed.contains(key) {
            state.completed.push(key.clone());
            write_emit(&self.0, &state);
        }
    }
    fn is_complete(&self, key: &ArtifactTupleKey) -> bool {
        read_emit(&self.0).completed.contains(key)
    }
}

struct DiskSink(std::path::PathBuf);
impl EmitSink for DiskSink {
    fn publish(&mut self, artifact: &ArtifactTuple) -> EmitReceipt {
        self.publish_named(artifact, "InspectionPassed", "summary")
    }
    fn publish_named(&mut self, artifact: &ArtifactTuple, event: &str, payload: &str) -> EmitReceipt {
        assert_eq!(payload, "summary");
        let mut state = read_emit(&self.0);
        assert!(!state.emits.iter().any(|receipt| receipt.key == artifact.key()));
        let receipt = foundry_exec::emit::new_receipt(artifact, event);
        state.publishes += 1;
        state.emits.push(receipt.clone());
        // The simulated external event and its receipt share one atomic file commit.
        write_emit(&self.0, &state);
        receipt
    }
    fn receipt_of(&self, key: &ArtifactTupleKey) -> Option<EmitReceipt> {
        read_emit(&self.0).emits.into_iter().find(|receipt| &receipt.key == key)
    }
}

struct NoSinkAccess;
impl EmitSink for NoSinkAccess {
    fn publish(&mut self, _: &ArtifactTuple) -> EmitReceipt { panic!("W4 may not publish") }
    fn receipt_of(&self, _: &ArtifactTupleKey) -> Option<EmitReceipt> { panic!("W4 needs only the orchestration receipt") }
}

#[test]
fn review_w3_w4_reopen_both_stores_and_recover_via_receipts() {
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/fixture-min/v1");
    let pack = load_pack(source).unwrap();
    for point in [foundry_exec::CrashPoint::W3, foundry_exec::CrashPoint::W4] {
        let dir = ReviewDir::new();
        let log_path = dir.0.join("promotion-emit.json");
        let sink_path = dir.0.join("external.json");
        let tuples = MemoryTuples::default();
        write_emit(&log_path, &EmitState {
            promotions: vec![PromotionReceipt { tuple: tuple().key(), promoted_at: Utc::now() }],
            ..Default::default()
        });
        {
            let mut log = DiskEmitLog(log_path.clone());
            let mut sink = DiskSink(sink_path.clone());
            let mut intents = Log::default();
            let mut outcomes = Log::default();
            let mut counters = MemoryCounterLog::default();
            let mut host = Host::default();
            let mut driver = GraphDriver::new(&pack, &mut intents, &mut host, &mut outcomes,
                &mut counters, &mut sink, &mut log, OrderId("order-1".into()), Attempt(1), RunId(2),
                Generation(3), Epoch(0), "fixture", Some(tuple())).with_tuple_store(&tuples);
            driver.arm_fault(point);
            assert!(matches!(driver.advance("done"), GraphStep::Fault(ref reason)
                if reason.contains(&format!("{point:?}"))));
        }
        assert_eq!(read_emit(&log_path).promotions.len(), 1);
        assert!(read_emit(&log_path).completed.is_empty());
        let before = read_emit(&sink_path);
        assert_eq!(before.publishes, u64::from(point == foundry_exec::CrashPoint::W4));
        assert_eq!(read_emit(&log_path).emits.len(), before.emits.len());
        let receipt = {
            // Every adapter and driver is new; only durable files survive.
            let mut log = DiskEmitLog(log_path.clone());
            let mut intents = Log::default();
            let mut outcomes = Log::default();
            let mut counters = MemoryCounterLog::default();
            let mut host = Host::default();
            if point == foundry_exec::CrashPoint::W4 {
                let mut sink = NoSinkAccess;
                let mut driver = GraphDriver::new(&pack, &mut intents, &mut host, &mut outcomes,
                    &mut counters, &mut sink, &mut log, OrderId("order-1".into()), Attempt(1), RunId(2),
                    Generation(3), Epoch(0), "fixture", Some(tuple())).with_tuple_store(&tuples);
                let GraphStep::Emitted { receipt } = driver.advance("done") else { panic!("W4 must only complete") };
                assert_eq!(receipt, before.emits[0]);
                receipt
            } else {
                let mut sink = DiskSink(sink_path.clone());
                let mut driver = GraphDriver::new(&pack, &mut intents, &mut host, &mut outcomes,
                    &mut counters, &mut sink, &mut log, OrderId("order-1".into()), Attempt(1), RunId(2),
                    Generation(3), Epoch(0), "fixture", Some(tuple())).with_tuple_store(&tuples);
                let GraphStep::Emitted { receipt } = driver.advance("done") else { panic!("W3 must emit promoted tuple") };
                receipt
            }
        };
        assert_eq!(read_emit(&sink_path).publishes, 1);
        assert_eq!(read_emit(&sink_path).emits, vec![receipt.clone()]);
        assert_eq!(read_emit(&log_path).emits, vec![receipt.clone()]);
        assert_eq!(read_emit(&log_path).completed, [tuple().key()]);
        let mut log = DiskEmitLog(log_path.clone());
        assert!(!complete_after_w4(&mut log, &tuple().key()));
        assert_eq!(emit_once(&mut NoSinkAccess, &mut log, &tuple(), Epoch(0)), EmitOutcome::Existing(receipt));
    }
}

#[test]
fn review_w3_requires_promotion_and_duplicates_skip_both_crash_windows() {
    let pack = review_pack();
    let mut rig = Rig::default();
    {
        let mut driver = rig.driver(&pack);
        driver.arm_fault(foundry_exec::CrashPoint::W3);
        assert!(matches!(driver.advance("pass"), GraphStep::Fault(ref reason) if reason.contains("promotion receipt")));
    }
    promote(&mut rig.emits, &tuple());
    let EmitOutcome::Published(receipt) = emit_once(&mut rig.sink, &mut rig.emits, &tuple(), Epoch(0)) else {
        panic!("first emit required")
    };
    rig.emits.promotions.clear();
    rig.emits.epoch = Epoch(10);
    for point in [foundry_exec::CrashPoint::W3, foundry_exec::CrashPoint::W4] {
        let mut driver = rig.driver(&pack);
        driver.arm_fault(point);
        assert_eq!(driver.advance("pass"), GraphStep::Emitted { receipt: receipt.clone() });
    }
    assert_eq!(rig.sink.publishes, 1);
}

#[test]
fn review_emit_node_forwards_its_event_and_payload_reference() {
    let (_dir, pack) = changed_fixture_with_outputs(|policy| policy.replace(
        "kind = \"route\"\n[[nodes.branches]]\nwhen = \"true\"\ndo = \"emit(InspectionPassed, summary)\"",
        "kind = \"emit\"\nevent = \"InspectionFailed\"\npayload_from = \"custom\"",
    ), "\n[[output]]\nname = 'custom'\nfrom = 'lens-a'\n");
    let mut rig = Rig::promoted();
    assert!(matches!(rig.driver(&pack).advance("done"), GraphStep::Emitted { ref receipt }
        if receipt.event == "InspectionFailed"));
    assert_eq!(rig.sink.requests, [("InspectionFailed".into(), "custom".into())]);
    assert_eq!(rig.sink.publishes, 1);
}

#[test]
fn review_graph_rejects_artifact_identity_from_another_run() {
    let pack = review_pack();
    for field in 0..4 {
        let mut artifact = tuple();
        match field {
            0 => artifact.order = OrderId("other".into()),
            1 => artifact.attempt = Attempt(9),
            2 => artifact.run = RunId(9),
            _ => artifact.generation = Generation(9),
        }
        let mut rig = Rig::default();
        promote(&mut rig.emits, &artifact);
        {
            let mut driver = GraphDriver::new(&pack, &mut rig.intents, &mut rig.host,
                &mut rig.outcomes, &mut rig.counters, &mut rig.sink, &mut rig.emits, OrderId("order-1".into()),
                Attempt(1), RunId(2), Generation(3), Epoch(0), "fixture", Some(artifact));
            assert!(matches!(driver.advance("pass"), GraphStep::Fault(ref reason) if reason.contains("does not belong")));
        }
        assert_eq!(rig.sink.publishes, 0);
        assert!(rig.emits.completed.is_empty());
    }
}

impl Rig {
    fn restored<'a>(&'a mut self, pack: &'a foundry_pack::PackSnapshot)
        -> GraphDriver<'a, Log, Host, Log, MemoryCounterLog, Sink, Log>
    {
        GraphDriver::restore(pack, &mut self.intents, &mut self.host, &mut self.outcomes,
            &mut self.counters, &mut self.sink, &mut self.emits, OrderId("order-1".into()),
            Attempt(1), RunId(2), Generation(3), Epoch(0), "fixture", Some(tuple())).with_tuple_store(&self.tuples)
    }
}

fn b2c_route(actions: &str, condition: &str) -> (ReviewDir, foundry_pack::PackSnapshot) {
    changed_fixture(|policy| policy.replace(
        "when = \"true\"\ndo = \"emit(InspectionPassed, summary)\"",
        &format!("when = '{condition}'\ndo = '{actions}'{}\n\n[[nodes.branches]]\nwhen = 'true'",
            if actions.contains("emit") { "" } else { "\ngoto = 'end_hold'" }),
    ) + "\n[counters]\nturns = 0\nother = 0\n")
}

fn b2c_bump(rig: &mut Rig, value: u64) {
    rig.counters.append("order-1", 2, CounterRecord::Bump {
        node_id: "done".into(), counter: "turns".into(), value,
    }).unwrap();
}

#[test]
fn b2c_each_bump_action_is_recorded_and_replayed_in_order() {
    let (_dir, pack) = b2c_route("bump(turns); bump(other); bump(turns)", "true");
    for pending in 0..=3 {
        let mut rig = Rig::default();
        for (counter, value) in [("turns", 1), ("other", 1), ("turns", 2)].into_iter().take(pending) {
            rig.counters.append("order-1", 2, CounterRecord::Bump {
                node_id: "done".into(), counter: counter.into(), value,
            }).unwrap();
        }
        {
            let mut driver = rig.restored(&pack);
            assert!(matches!(driver.advance("done"), GraphStep::Advanced { .. }));
            assert_eq!(driver.counters()["turns"], 2, "pending={pending}");
            assert_eq!(driver.counters()["other"], 1);
        }
        let bumps: Vec<_> = rig.counters.load("order-1", 2).into_iter().filter_map(|r| match r {
            CounterRecord::Bump { counter, value, .. } => Some((counter, value)), _ => None,
        }).collect();
        assert_eq!(bumps, [("turns".into(), 1), ("other".into(), 1), ("turns".into(), 2)]);
    }
}

#[test]
fn b2c_pending_restore_selects_the_original_route_branch() {
    let (_dir, pack) = b2c_route("bump(turns)", "count_lt(turns, 1)");
    let mut rig = Rig::default();
    b2c_bump(&mut rig, 1);
    let mut driver = rig.restored(&pack);
    assert_eq!(driver.counters()["turns"], 1);
    assert!(matches!(driver.start(), GraphStep::Advanced { ref next, ref effects }
        if next == "end_hold" && effects == &["bump:turns=1"]));
}

#[test]
fn b2c_emit_recovery_preserves_original_branch_and_completion_order() {
    let (_dir, pack) = b2c_route("bump(turns); emit(InspectionPassed, summary)", "count_lt(turns, 1)");
    for point in [None, Some(foundry_exec::CrashPoint::W3), Some(foundry_exec::CrashPoint::W4)] {
        let mut rig = Rig::promoted();
        {
            let mut driver = rig.driver(&pack);
            if let Some(point) = point {
                driver.arm_fault(point);
                assert!(matches!(driver.advance("done"), GraphStep::Fault(_)));
            } else {
                assert!(matches!(driver.advance("done"), GraphStep::Emitted { .. }));
            }
        }
        if point.is_some() {
            assert!(matches!(rig.restored(&pack).start(), GraphStep::Emitted { .. }));
        }
        let records = rig.counters.load("order-1", 2);
        let bumps: Vec<_> = records.iter().filter_map(|r| match r {
            CounterRecord::Bump { counter, value, .. } => Some((counter.as_str(), *value)), _ => None,
        }).collect();
        assert_eq!(bumps, [("turns", 1)]);
        assert!(matches!(records.last(), Some(CounterRecord::Completed { .. })));
        assert_eq!(rig.sink.publishes, 1);
    }
}

#[test]
fn b2c_rejects_under_over_and_duplicate_pending_bumps_before_effects() {
    let (_dir, pack) = b2c_route("bump(turns); emit(InspectionPassed, summary)", "true");
    for values in [vec![0], vec![2], vec![1, 2]] {
        let mut rig = Rig::promoted();
        for value in &values { b2c_bump(&mut rig, *value); }
        let before = rig.counters.load("order-1", 2);
        assert!(matches!(rig.restored(&pack).start(), GraphStep::Fault(_)), "values={values:?}");
        assert_eq!(rig.sink.publishes, 0);
        assert_eq!(rig.counters.load("order-1", 2), before);
    }
}

#[test]
fn b2c_restore_never_reuses_a_completion_from_an_earlier_visit() {
    let (_dir, pack) = b2c_route("bump(turns)", "true");
    let mut rig = Rig::default();
    // An older terminal visit followed by a newer visit returning to done.
    rig.counters.append("order-1", 2, CounterRecord::Completed { node_id: "done".into(), goto: None }).unwrap();
    rig.counters.append("order-1", 2, CounterRecord::Completed { node_id: "other".into(), goto: Some("done".into()) }).unwrap();
    let key = (OrderId("order-1".into()), Attempt(1), RunId(2));
    let intent = ExecutionIntent { order: key.0.clone(), attempt: key.1, run: key.2,
        activation: 0, generation: Generation(3), epoch: Epoch(0), external_ref: "fixture".into(), recorded_at: Utc::now() };
    rig.outcomes.intents.push(intent);
    rig.outcomes.commit(&ExecutionOutcome { intent_key: key, activation: 0,
        cursor: Some("other".into()), next_activation: None, receipts: vec![] });
    assert!(matches!(rig.restored(&pack).start(), GraphStep::Advanced { ref next, .. } if next == "end_hold"));
}

struct B2cFailCompletion<'a> { log: &'a mut MemoryCounterLog, fail: bool }
impl CounterLog for B2cFailCompletion<'_> {
    fn append(&mut self, order: &str, run: u64, record: CounterRecord) -> Result<(), String> {
        if self.fail && matches!(record, CounterRecord::Completed { .. }) {
            self.fail = false;
            return Err("crash before completion append".into());
        }
        self.log.append(order, run, record)
    }
    fn load(&self, order: &str, run: u64) -> Vec<CounterRecord> { self.log.load(order, run) }
}

#[test]
fn b2c_bound_redrive_recovery_does_not_dispatch_another_activation() {
    let pack = review_pack();
    for restart in [false, true] {
        let mut rig = Rig::default();
        {
            let mut counter = B2cFailCompletion { log: &mut rig.counters, fail: true };
            let mut driver = GraphDriver::new(&pack, &mut rig.intents, &mut rig.host,
                &mut rig.outcomes, &mut counter, &mut rig.sink, &mut rig.emits,
                OrderId("order-1".into()), Attempt(1), RunId(2), Generation(3), Epoch(0), "fixture", Some(tuple()));
            assert!(matches!(driver.advance("fixdie"), GraphStep::Fault(_)));
            if !restart {
                assert!(matches!(driver.advance("fixdie"), GraphStep::Advanced { .. }));
            }
        }
        if restart { assert!(matches!(rig.restored(&pack).start(), GraphStep::Advanced { .. })); }
        assert_eq!(rig.host.calls, 1, "restart={restart}");
        assert_eq!(rig.intents.intents.len(), 1);
    }
}

#[test]
fn b2c_legacy_host_history_fails_closed_when_activation_is_ambiguous() {
    let pack = review_pack();
    for history in 0..3 {
        let mut rig = Rig::default();
        {
            let mut driver = rig.driver(&pack);
            assert!(matches!(driver.advance("fixdie"), GraphStep::Advanced { .. }));
        }
        // Strip visit markers to reproduce the original Bump/Completed format.
        rig.counters.0.retain(|(_, _, record)| matches!(record, CounterRecord::Bump { .. }));
        if history != 2 {
            rig.counters.0.clear();
            if history == 1 {
                rig.counters.append("order-1", 2, CounterRecord::Completed {
                    node_id: "route1".into(), goto: Some("lenses".into()),
                }).unwrap();
            }
        }
        let before = rig.counters.load("order-1", 2);
        assert!(matches!(rig.restored(&pack).start(), GraphStep::Fault(ref error) if error.contains("ambiguous")));
        assert_eq!(rig.host.calls, 1);
        assert_eq!(rig.counters.load("order-1", 2), before);
    }
}

#[test]
fn b2c_outcome_fixture_latest_observes_committed_bindings() {
    let mut log = Log::default();
    let key = (OrderId("order-1".into()), Attempt(1), RunId(2));
    let outcome = ExecutionOutcome { intent_key: key.clone(), activation: 7,
        cursor: Some("done".into()), next_activation: None, receipts: vec![] };
    log.commit(&outcome);
    assert_eq!(OutcomeStore::latest(&log, &key), Some(outcome));
    assert_eq!(OutcomeStore::latest(&log, &(OrderId("other".into()), Attempt(1), RunId(2))), None);
}

#[test]
fn b3a_fixture_uses_configured_temp_directory() {
    let dir = ReviewDir::new();
    assert!(dir.0.starts_with(std::env::temp_dir()), "fixture escaped TMPDIR: {}", dir.0.display());
}

#[derive(Default)]
struct DieLedger {
    accepted: HashSet<foundry_exec::HostKey>,
    results: HashMap<foundry_exec::HostKey, foundry_exec::HostResult>,
}
impl foundry_exec::HostLedger for DieLedger {
    fn accept(&mut self, key: &foundry_exec::HostKey) -> Result<(), String> {
        self.accepted.insert(key.clone());
        Ok(())
    }
    fn is_accepted(&self, key: &foundry_exec::HostKey) -> bool { self.accepted.contains(key) }
    fn save(&mut self, key: &foundry_exec::HostKey, result: &foundry_exec::HostResult) -> Result<(), String> {
        assert!(self.accepted.contains(key));
        self.results.entry(key.clone()).or_insert_with(|| result.clone());
        Ok(())
    }
    fn load(&self, key: &foundry_exec::HostKey) -> Option<foundry_exec::HostResult> { self.results.get(key).cloned() }
}
struct GraphDieWorker { calls: u64, fail: bool }
impl foundry_exec::DieWorker for GraphDieWorker {
    fn run(&mut self, _: &foundry_exec::HostKey, _: &str) -> Result<foundry_exec::HostResult, String> {
        self.calls += 1;
        if self.fail { return Err("graph worker fault".into()); }
        Ok(foundry_exec::HostResult {
            base_commit: "base".into(), result_commit: "commit-is-not-a-graph-node".into(),
            log_path: "/work/worker.log".into(), exit_code: 0,
            generations: Vec::new(), held: None,
        })
    }
}

#[test]
fn b3a_die_worker_errors_reach_graph_fault_and_retry_never_repeats_work() {
    let pack = review_pack();
    for cursor in ["lenses", "fixdie"] {
        let mut rig = Rig::default();
        let mut worker = GraphDieWorker { calls: 0, fail: true };
        let mut ledger = DieLedger::default();
        let mut host = foundry_exec::DieHostAdapter::new(&mut worker, &mut ledger);
        let mut driver = GraphDriver::new(&pack, &mut rig.intents, &mut host, &mut rig.outcomes,
            &mut rig.counters, &mut rig.sink, &mut rig.emits, OrderId("order-1".into()), Attempt(1),
            RunId(2), Generation(3), Epoch(0), "fixture", Some(tuple()));
        assert!(matches!(driver.advance(cursor), GraphStep::Fault(reason)
            if reason.contains("Host(Worker") && reason.contains("graph worker fault")));
        assert!(matches!(driver.advance(cursor), GraphStep::Fault(reason) if reason.contains("AwaitResult")));
        drop(driver);
        drop(host);
        assert_eq!(worker.calls, 1);
        assert!(rig.outcomes.outcomes.is_empty());
        assert!(!rig.counters.0.iter().any(|(_, _, row)| matches!(row, CounterRecord::Completed { .. })));
    }
}

#[test]
fn b3a_die_graph_w2_restart_uses_pack_continuation_and_complete_host_receipt() {
    let pack = review_pack();
    for cursor in ["lenses", "fixdie"] {
        let mut rig = Rig::default();
        let mut worker = GraphDieWorker { calls: 0, fail: false };
        let mut ledger = DieLedger::default();
        {
            let mut host = foundry_exec::DieHostAdapter::new(&mut worker, &mut ledger);
            let mut driver = GraphDriver::new(&pack, &mut rig.intents, &mut host, &mut rig.outcomes,
                &mut rig.counters, &mut rig.sink, &mut rig.emits, OrderId("order-1".into()), Attempt(1),
                RunId(2), Generation(3), Epoch(0), "fixture", Some(tuple()));
            driver.arm_fault(foundry_exec::CrashPoint::W2);
            assert!(matches!(driver.advance(cursor), GraphStep::Fault(reason) if reason.contains("W2")));
        }
        assert_eq!(worker.calls, 1);
        let expected_next = pack.nodes().iter().find(|node| node.id == cursor).unwrap()
            .value["on_done"].as_str().unwrap();
        let mut host = foundry_exec::DieHostAdapter::new(&mut worker, &mut ledger);
        let mut driver = GraphDriver::restore(&pack, &mut rig.intents, &mut host, &mut rig.outcomes,
            &mut rig.counters, &mut rig.sink, &mut rig.emits, OrderId("order-1".into()), Attempt(1),
            RunId(2), Generation(3), Epoch(0), "fixture", Some(tuple()));
        assert!(matches!(driver.start(), GraphStep::Advanced { next, .. } if next == expected_next));
        drop(driver);
        drop(host);
        assert_eq!(worker.calls, 1);
        let bound = OutcomeStore::latest(&rig.outcomes, &(OrderId("order-1".into()), Attempt(1), RunId(2))).unwrap();
        assert!(matches!(bound.receipts.as_slice(), [foundry_exec::Receipt::Host(result)]
            if result.result_commit == "commit-is-not-a-graph-node" && result.exit_code == 0));
        assert!(bound.cursor.is_none());
    }
}
