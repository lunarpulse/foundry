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
        self.intents
            .iter()
            .rev()
            .find(|i| (i.order.clone(), i.attempt, i.run) == *key)
            .and_then(|i| self.outcomes.get(&(key.clone(), i.activation)))
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
        artifact_location: ArtifactLocation("artifact://one".into()),
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
    );
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
    );
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
            RunId(2), Generation(3), Epoch(0), "fixture", Some(tuple()))
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
    // Preserve b1's explicitly tested future-epoch fence until b5 resolves it.
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
        let path = std::path::Path::new("/tmp/b2b_review/test-data").join(format!(
            "graph-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for ReviewDir {
    fn drop(&mut self) {
        use std::io::Write;
        let mut inventory = std::fs::OpenOptions::new().create(true).append(true)
            .open("/tmp/b2b_review/runtime-temporaries.txt").unwrap();
        for entry in std::fs::read_dir(&self.0).unwrap() {
            writeln!(inventory, "{} (deleted)", entry.unwrap().path().display()).unwrap();
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
    std::fs::write(dir.0.join("manifest.toml"), format!("{digest_line}\n{}",
        std::fs::read_to_string(dir.0.join("manifest.toml")).unwrap(),
        digest_line = format!("digest = \"{digest}\""))).unwrap();
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
                Generation(3), Epoch(0), "fixture", Some(tuple()));
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
                    Generation(3), Epoch(0), "fixture", Some(tuple()));
                let GraphStep::Emitted { receipt } = driver.advance("done") else { panic!("W4 must only complete") };
                assert_eq!(receipt, before.emits[0]);
                receipt
            } else {
                let mut sink = DiskSink(sink_path.clone());
                let mut driver = GraphDriver::new(&pack, &mut intents, &mut host, &mut outcomes,
                    &mut counters, &mut sink, &mut log, OrderId("order-1".into()), Attempt(1), RunId(2),
                    Generation(3), Epoch(0), "fixture", Some(tuple()));
                let GraphStep::Emitted { receipt } = driver.advance("done") else { panic!("W3 must emit promoted tuple") };
                receipt
            }
        };
        assert_eq!(read_emit(&sink_path).publishes, 1);
        assert_eq!(read_emit(&sink_path).emits, [receipt.clone()]);
        assert_eq!(read_emit(&log_path).emits, [receipt.clone()]);
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
