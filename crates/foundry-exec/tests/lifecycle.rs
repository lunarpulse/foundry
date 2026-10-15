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

    fn get(&self, key: &Key, activation: u64) -> Option<ExecutionIntent> {
        self.rows.iter().find(|intent| {
            (intent.order.clone(), intent.attempt, intent.run) == *key
                && intent.activation == activation
        }).cloned()
    }
}

#[derive(Default)]
struct FakeHost {
    calls: u64,
    recovered: Option<(Key, u64, DispatchResult)>,
    accepted: bool,
}

impl DispatchHost for FakeHost {
    type Result = DispatchResult;

    fn dispatch(&mut self, intent: &ExecutionIntent) -> Self::Result {
        self.calls += 1;
        self.accepted = true;
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

    fn accepted(&self, _intent: &ExecutionIntent) -> bool {
        self.accepted
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
    rows: HashMap<(Key, u64), ExecutionOutcome>,
    latest_activations: HashMap<Key, u64>,
}

impl OutcomeStore for MemoryOutcomeStore {
    fn commit(&mut self, outcome: &ExecutionOutcome) {
        if self.rows.contains_key(&(outcome.intent_key.clone(), outcome.activation)) {
            return;
        }
        self.latest_activations.insert(outcome.intent_key.clone(), outcome.activation);
        self.rows
            .entry((outcome.intent_key.clone(), outcome.activation))
            .or_insert_with(|| outcome.clone());
    }

    fn latest(&self, key: &Key) -> Option<ExecutionOutcome> {
        self.get(key, *self.latest_activations.get(key)?)
    }

    fn get(&self, key: &Key, activation: u64) -> Option<ExecutionOutcome> {
        self.rows.get(&(key.clone(), activation)).cloned()
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

#[test]
fn review_w1_of_new_activation_does_not_return_the_previous_outcome() {
    let mut intents = MemoryIntentLog::default();
    let mut host = FakeHost::default();
    let mut outcomes = MemoryOutcomeStore::default();
    Executor::new(&mut intents, &mut host, &mut outcomes).run(intent(8)).unwrap();
    let mut executor = Executor::with_faults(
        &mut intents, &mut host, &mut outcomes, FaultInjector::armed_at(CrashPoint::W1),
    );
    assert!(executor.run(intent(9)).is_err());
    assert_eq!(executor.resume(&key()), ResumeAction::ReDispatch);
    assert_eq!(executor.outcome(&key()).unwrap().activation, 9);
    assert_eq!(executor.dispatch_count(), 2);
}

#[test]
fn review_w2_of_new_activation_recovers_its_own_result() {
    let mut intents = MemoryIntentLog::default();
    let mut host = FakeHost::default();
    let mut outcomes = MemoryOutcomeStore::default();
    Executor::new(&mut intents, &mut host, &mut outcomes).run(intent(8)).unwrap();
    let mut executor = Executor::with_faults(
        &mut intents, &mut host, &mut outcomes, FaultInjector::armed_at(CrashPoint::W2),
    );
    assert!(executor.run(intent(9)).is_err());
    assert_eq!(executor.resume(&key()), ResumeAction::ReBind);
    assert_eq!(executor.outcome(&key()).unwrap().activation, 9);
    assert_eq!(executor.dispatch_count(), 2);
}

#[test]
fn review_repeated_run_returns_the_original_outcome_without_dispatch() {
    let mut intents = MemoryIntentLog::default();
    let mut host = FakeHost::default();
    let mut outcomes = MemoryOutcomeStore::default();
    let mut executor = Executor::new(&mut intents, &mut host, &mut outcomes);
    let expected = executor.run(intent(8)).unwrap();
    assert_eq!(executor.run(intent(8)).unwrap(), expected);
    assert_eq!(executor.dispatch_count(), 1);
}

#[test]
fn review_unbound_activation_cannot_be_hidden_by_a_new_one() {
    let mut intents = MemoryIntentLog::default();
    let mut host = FakeHost::default();
    let mut outcomes = MemoryOutcomeStore::default();
    let mut executor = Executor::with_faults(
        &mut intents, &mut host, &mut outcomes, FaultInjector::armed_at(CrashPoint::W2),
    );
    assert!(executor.run(intent(8)).is_err());
    assert!(executor.run(intent(9)).is_err());
    assert_eq!(executor.latest_intent(&key()).unwrap().activation, 8);
    assert_eq!(executor.resume(&key()), ResumeAction::ReBind);
    assert_eq!(executor.dispatch_count(), 1);
}

#[test]
fn review_same_activation_cannot_change_external_execution_identity() {
    let mut intents = MemoryIntentLog::default();
    let mut host = FakeHost::default();
    let mut outcomes = MemoryOutcomeStore::default();
    let mut executor = Executor::new(&mut intents, &mut host, &mut outcomes);
    executor.run(intent(8)).unwrap();
    let mut conflicting = intent(8);
    conflicting.external_ref = "another-external-execution".into();
    assert!(executor.run(conflicting).is_err());
    assert_eq!(executor.dispatch_count(), 1);
}

#[test]
fn review_started_without_a_result_waits_and_never_dispatches() {
    let mut intents = MemoryIntentLog::default();
    let mut host = FakeHost::default();
    let mut outcomes = MemoryOutcomeStore::default();
    intents.append(&intent(8));
    intents.mark_started(&key(), 8);
    host.accepted = true;
    let mut executor = Executor::new(&mut intents, &mut host, &mut outcomes);
    assert_eq!(executor.resume(&key()), ResumeAction::AwaitResult);
    assert_eq!(executor.resume(&key()), ResumeAction::AwaitResult);
    assert_eq!(executor.run(intent(8)), Err(foundry_exec::LifecycleError::AwaitResult(8)));
    assert!(executor.outcome(&key()).is_none());
    assert_eq!(executor.dispatch_count(), 0);
    drop(executor);
    host.recovered = Some((key(), 8, DispatchResult {
        cursor: Some("completed-later".into()),
        ..DispatchResult::default()
    }));
    let mut executor = Executor::new(&mut intents, &mut host, &mut outcomes);
    assert_eq!(executor.resume(&key()), ResumeAction::ReBind);
    assert_eq!(executor.outcome(&key()).unwrap().cursor.as_deref(), Some("completed-later"));
    assert_eq!(executor.dispatch_count(), 0);
}

#[test]
fn review_delayed_duplicate_returns_its_own_receipt_without_hiding_new_activation() {
    let mut intents = MemoryIntentLog::default();
    let mut host = FakeHost::default();
    let mut outcomes = MemoryOutcomeStore::default();
    let mut executor = Executor::new(&mut intents, &mut host, &mut outcomes);
    let original = executor.run(intent(8)).unwrap();
    executor.run(intent(2)).unwrap(); // IDs need not be monotonic.
    assert_eq!(executor.run(intent(8)).unwrap(), original);
    assert_eq!(executor.latest_intent(&key()).unwrap().activation, 2);
    assert_eq!(executor.outcome(&key()).unwrap().activation, 2);
    assert_eq!(executor.dispatch_count(), 2);
    drop(executor);
    assert_eq!(intents.rows.len(), 2);
}

#[test]
fn review_run_retries_both_crash_windows_without_a_second_intent_or_dispatch() {
    for point in [CrashPoint::W1, CrashPoint::W2] {
        let mut intents = MemoryIntentLog::default();
        let mut host = FakeHost::default();
        let mut outcomes = MemoryOutcomeStore::default();
        let mut executor = Executor::with_faults(
            &mut intents, &mut host, &mut outcomes, FaultInjector::armed_at(point),
        );
        assert!(executor.run(intent(8)).is_err());
        let mut retried = intent(8);
        retried.recorded_at += chrono::Duration::seconds(1);
        executor.run(retried).unwrap();
        assert_eq!(executor.outcome(&key()).unwrap().activation, 8);
        assert_eq!(executor.dispatch_count(), 1);
        drop(executor);
        assert_eq!(intents.rows.len(), 1);
    }
}

#[test]
fn review_resume_can_crash_at_w2_and_recover_without_redispatch() {
    let mut intents = MemoryIntentLog::default();
    let mut host = FakeHost::default();
    let mut outcomes = MemoryOutcomeStore::default();
    intents.append(&intent(8));
    let mut executor = Executor::with_faults(
        &mut intents, &mut host, &mut outcomes, FaultInjector::armed_at(CrashPoint::W2),
    );
    assert_eq!(executor.resume(&key()), ResumeAction::Interrupted(
        foundry_exec::LifecycleError::Crash(CrashPoint::W2),
    ));
    assert!(executor.outcome(&key()).is_none());
    assert_eq!(executor.resume(&key()), ResumeAction::ReBind);
    assert_eq!(executor.dispatch_count(), 1);
}

#[test]
fn review_no_intent_requires_a_fresh_run_without_external_effects() {
    let mut intents = MemoryIntentLog::default();
    let mut host = FakeHost::default();
    let mut outcomes = MemoryOutcomeStore::default();
    let mut executor = Executor::new(&mut intents, &mut host, &mut outcomes);
    assert_eq!(executor.resume(&key()), ResumeAction::NeedsFreshRun);
    assert_eq!(executor.dispatch_count(), 0);
    assert!(executor.outcome(&key()).is_none());
}

// A host predating receipt support cannot prove dispatch never reached it.
struct LegacyHost { calls: u64 }
impl DispatchHost for LegacyHost {
    type Result = DispatchResult;
    fn dispatch(&mut self, _: &ExecutionIntent) -> DispatchResult {
        self.calls += 1;
        DispatchResult::default()
    }
    fn dispatch_count(&self) -> u64 { self.calls }
    fn recover_result(&mut self, _: &ExecutionIntent) -> Option<DispatchResult> { None }
}

#[test]
fn b3a_legacy_host_without_receipts_waits_on_run_and_resume() {
    let mut intents = MemoryIntentLog::default();
    intents.append(&intent(8));
    intents.mark_started(&key(), 8);
    let mut host = LegacyHost { calls: 1 }; // Work may already have happened.
    let mut outcomes = MemoryOutcomeStore::default();
    let mut executor = Executor::new(&mut intents, &mut host, &mut outcomes);
    assert_eq!(executor.run(intent(8)), Err(foundry_exec::LifecycleError::AwaitResult(8)));
    assert_eq!(executor.resume(&key()), ResumeAction::AwaitResult);
    assert_eq!(executor.dispatch_count(), 1);
    assert!(executor.outcome(&key()).is_none());
}
