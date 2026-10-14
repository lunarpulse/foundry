use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::Utc;
use foundry_exec::{
    emit::{complete_after_w4, emit_once, EmitLog, EmitOutcome, EmitSink},
    graph::{GraphDriver, GraphStep},
    ports::{DispatchHost, IntentLog, OutcomeStore},
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

impl EmitLog for Log {
    fn current_epoch(&self) -> Epoch {
        self.epoch
    }
    fn promotion_of(&self, key: &ArtifactTupleKey) -> Option<PromotionReceipt> {
        self.promotions.get(key).cloned()
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
}
impl DispatchHost for Host {
    type Result = foundry_exec::DispatchResult;
    fn dispatch(&mut self, _: &ExecutionIntent) -> Self::Result {
        self.calls += 1;
        foundry_exec::DispatchResult {
            cursor: self.cursor.clone(),
            ..Default::default()
        }
    }
    fn dispatch_count(&self) -> u64 {
        self.calls
    }
    fn recover_result(&mut self, _: &ExecutionIntent) -> Option<Self::Result> {
        None
    }
}

#[derive(Default)]
struct Sink {
    publishes: u64,
    receipts: HashMap<ArtifactTupleKey, EmitReceipt>,
}
impl EmitSink for Sink {
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
    let mut emit_log = Log::default();
    let artifact = tuple();
    promote(&mut emit_log, &artifact);
    let mut sink = Sink::default();
    let mut host = Host {
        calls: 0,
        cursor: Some("done".into()),
    };
    let mut driver = GraphDriver::new(
        &pack,
        &mut intents,
        &mut host,
        &mut outcomes,
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
    let mut emit_log = Log::default();
    let artifact = tuple();
    promote(&mut emit_log, &artifact);
    let mut sink = Sink::default();
    let mut host = Host {
        calls: 0,
        cursor: Some("done".into()),
    };
    let mut driver = GraphDriver::new(
        &pack,
        &mut intents,
        &mut host,
        &mut outcomes,
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
    let mut emit_log = Log::default();
    let mut sink = Sink::default();
    let mut host = Host::default();
    let mut driver = GraphDriver::new(
        &pack,
        &mut intents,
        &mut host,
        &mut outcomes,
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
