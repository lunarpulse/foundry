use std::collections::{HashMap, HashSet};

use chrono::{TimeZone, Utc};
use foundry_exec::inject::{CrashPoint, FaultInjector};
use foundry_exec::lifecycle::{DispatchResult, Executor, ResumeAction, RunStep};
use foundry_exec::ports::{DispatchHost, IntentLog, OutcomeStore};
use foundry_exec::types::{
    Attempt, Epoch, ExecutionIntent, ExecutionOutcome, Generation, OrderId, RunId,
};

type Key = (OrderId, Attempt, RunId);

#[derive(Default)]
struct MemoryIntentLog {
    rows: Vec<ExecutionIntent>,
    started: HashSet<(Key, u64)>,
}

impl IntentLog for MemoryIntentLog {
    fn append(&mut self, intent: &ExecutionIntent) {
        self.rows.push(intent.clone());
    }

    fn mark_started(&mut self, key: &Key, activation: u64) {
        self.started.insert((key.clone(), activation));
    }

    fn latest(&self, key: &Key) -> Option<ExecutionIntent> {
        self.rows
            .iter()
            .rev()
            .find(|intent| (intent.order.clone(), intent.attempt, intent.run) == *key)
            .cloned()
    }

    fn is_started(&self, key: &Key, activation: u64) -> bool {
        self.started.contains(&(key.clone(), activation))
    }
}

#[derive(Default)]
struct FakeHost {
    calls: u64,
    recovered: Option<(Key, u64, DispatchResult)>,
}

impl DispatchHost for FakeHost {
    type Result = DispatchResult;

    fn dispatch(&mut self, intent: &ExecutionIntent) -> Self::Result {
        self.calls += 1;
        let result = DispatchResult {
            cursor: Some(format!("cursor-{}", self.calls)),
            ..DispatchResult::default()
        };
        self.recovered = Some((
            (intent.order.clone(), intent.attempt, intent.run),
            intent.activation,
            result.clone(),
        ));
        result
    }

    fn dispatch_count(&self) -> u64 {
        self.calls
    }

    fn recover_result(&mut self, intent: &ExecutionIntent) -> Option<Self::Result> {
        self.recovered
            .as_ref()
            .and_then(|(key, activation, result)| {
                (key == &(intent.order.clone(), intent.attempt, intent.run)
                    && *activation == intent.activation)
                    .then(|| result.clone())
            })
    }
}

#[derive(Default)]
struct MemoryOutcomeStore {
    rows: HashMap<Key, ExecutionOutcome>,
}

impl OutcomeStore for MemoryOutcomeStore {
    fn commit(&mut self, outcome: &ExecutionOutcome) {
        self.rows
            .insert(outcome.intent_key.clone(), outcome.clone());
    }

    fn latest(&self, key: &Key) -> Option<ExecutionOutcome> {
        self.rows.get(key).cloned()
    }
}

fn intent(activation: u64) -> ExecutionIntent {
    ExecutionIntent {
        order: OrderId("order-1".into()),
        attempt: Attempt(1),
        run: RunId(2),
        activation,
        generation: Generation(3),
        epoch: Epoch(4),
        external_ref: "external-1".into(),
        recorded_at: Utc.with_ymd_and_hms(2026, 10, 15, 0, 0, 0).unwrap(),
    }
}

fn key() -> Key {
    (OrderId("order-1".into()), Attempt(1), RunId(2))
}

#[test]
fn happy_path_appends_starts_dispatches_and_binds_once() {
    let mut intents = MemoryIntentLog::default();
    let mut host = FakeHost::default();
    let mut outcomes = MemoryOutcomeStore::default();
    let mut executor = Executor::new(&mut intents, &mut host, &mut outcomes);

    let RunStep::Bound(result) = executor.run(intent(8)).unwrap();
    assert!(executor.latest_intent(&key()).is_some());
    assert!(executor.is_started(&key(), 8));
    assert_eq!(executor.dispatch_count(), 1);
    assert_eq!(executor.outcome(&key()), Some(result));
}

#[test]
fn w1_resume_redispatches_an_intent_that_never_started() {
    let mut intents = MemoryIntentLog::default();
    let mut host = FakeHost::default();
    let mut outcomes = MemoryOutcomeStore::default();
    let mut executor = Executor::with_faults(
        &mut intents,
        &mut host,
        &mut outcomes,
        FaultInjector::armed_at(CrashPoint::W1),
    );

    assert!(executor.run(intent(8)).is_err());
    assert_eq!(executor.dispatch_count(), 0);
    assert_eq!(executor.resume(&key()), ResumeAction::ReDispatch);
    assert_eq!(executor.dispatch_count(), 1);
    assert!(executor.outcome(&key()).is_some());
}

#[test]
fn w2_resume_rebinds_without_a_second_dispatch() {
    let mut intents = MemoryIntentLog::default();
    let mut host = FakeHost::default();
    let mut outcomes = MemoryOutcomeStore::default();
    let mut executor = Executor::with_faults(
        &mut intents,
        &mut host,
        &mut outcomes,
        FaultInjector::armed_at(CrashPoint::W2),
    );

    assert!(executor.run(intent(8)).is_err());
    assert_eq!(executor.dispatch_count(), 1);
    assert!(executor.outcome(&key()).is_none());
    drop(executor);

    // A fresh executor has no in-memory run state; it recovers via the host port.
    let mut recovered = Executor::new(&mut intents, &mut host, &mut outcomes);
    assert_eq!(recovered.resume(&key()), ResumeAction::ReBind);
    assert_eq!(recovered.dispatch_count(), 1);
    assert_eq!(recovered.outcome(&key()).unwrap().activation, 8);
}

#[test]
fn activation_distinguishes_reissued_intents_with_the_same_key() {
    let mut intents = MemoryIntentLog::default();
    let mut host = FakeHost::default();
    let mut outcomes = MemoryOutcomeStore::default();
    let mut executor = Executor::new(&mut intents, &mut host, &mut outcomes);

    executor.run(intent(8)).unwrap();
    executor.run(intent(9)).unwrap();
    drop(executor);
    assert_eq!(intents.rows.len(), 2);
    assert_ne!(intents.rows[0].activation, intents.rows[1].activation);
}

#[test]
fn already_bound_resume_returns_the_outcome_without_external_effects() {
    let mut intents = MemoryIntentLog::default();
    let mut host = FakeHost::default();
    let mut outcomes = MemoryOutcomeStore::default();
    let mut executor = Executor::new(&mut intents, &mut host, &mut outcomes);
    let RunStep::Bound(expected) = executor.run(intent(8)).unwrap();

    assert_eq!(
        executor.resume(&key()),
        ResumeAction::AlreadyBound(expected)
    );
    assert_eq!(executor.dispatch_count(), 1);
}

#[test]
fn dispatch_is_reachable_only_after_the_intent_and_started_marker() {
    let mut intents = MemoryIntentLog::default();
    let mut host = FakeHost::default();
    let mut outcomes = MemoryOutcomeStore::default();
    let mut executor = Executor::new(&mut intents, &mut host, &mut outcomes);

    executor.run(intent(8)).unwrap();
    assert!(executor.latest_intent(&key()).is_some());
    assert!(executor.is_started(&key(), 8));
    assert_eq!(executor.dispatch_count(), 1);
}
