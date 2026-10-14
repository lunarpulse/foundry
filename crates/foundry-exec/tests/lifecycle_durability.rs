use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{TimeZone, Utc};
use foundry_exec::{
    ActivationRef, Attempt, CrashPoint, DispatchHost, DispatchResult, Epoch, ExecutionIntent,
    ExecutionOutcome, Executor, FaultInjector, IntentLog, OrderId, OutcomeStore, Receipt,
    ResumeAction, RunId, SessionReceipt,
};
use foundry_exec::types::Generation;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

type Key = (OrderId, Attempt, RunId);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = PathBuf::from("/tmp/b2a_review").join(format!(
            "durability-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn read<T: DeserializeOwned + Default>(path: &Path) -> T {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => T::default(),
        Err(error) => panic!("durable read failed: {error}"),
    }
}

fn write<T: Serialize>(path: &Path, value: &T) {
    let temporary = path.with_extension("pending");
    fs::write(&temporary, serde_json::to_vec(value).unwrap()).unwrap();
    File::open(&temporary).unwrap().sync_all().unwrap();
    fs::rename(&temporary, path).unwrap();
    File::open(path.parent().unwrap()).unwrap().sync_all().unwrap();
}

#[derive(Default, Serialize, Deserialize)]
struct Intents {
    rows: Vec<ExecutionIntent>,
    started: Vec<(Key, u64)>,
}

struct DiskIntentLog(PathBuf);

impl IntentLog for DiskIntentLog {
    fn append(&mut self, intent: &ExecutionIntent) {
        let mut state: Intents = read(&self.0);
        state.rows.push(intent.clone());
        write(&self.0, &state);
    }

    fn mark_started(&mut self, key: &Key, activation: u64) {
        let mut state: Intents = read(&self.0);
        assert!(state.rows.iter().any(|intent| intent_key(intent) == *key
            && intent.activation == activation));
        state.started.push((key.clone(), activation));
        write(&self.0, &state);
    }

    fn latest(&self, key: &Key) -> Option<ExecutionIntent> {
        let state: Intents = read(&self.0);
        state.rows.into_iter().rev().find(|intent| intent_key(intent) == *key)
    }

    fn get(&self, key: &Key, activation: u64) -> Option<ExecutionIntent> {
        let state: Intents = read(&self.0);
        state.rows.into_iter().find(|intent| intent_key(intent) == *key
            && intent.activation == activation)
    }

    fn is_started(&self, key: &Key, activation: u64) -> bool {
        read::<Intents>(&self.0).started.contains(&(key.clone(), activation))
    }
}

struct DiskOutcomeStore(PathBuf);

impl OutcomeStore for DiskOutcomeStore {
    fn commit(&mut self, outcome: &ExecutionOutcome) {
        let mut rows: Vec<ExecutionOutcome> = read(&self.0);
        if !rows.iter().any(|row| row.intent_key == outcome.intent_key
            && row.activation == outcome.activation)
        {
            rows.push(outcome.clone());
            write(&self.0, &rows);
        }
    }

    fn latest(&self, key: &Key) -> Option<ExecutionOutcome> {
        read::<Vec<ExecutionOutcome>>(&self.0).into_iter().rev()
            .find(|row| row.intent_key == *key)
    }

    fn get(&self, key: &Key, activation: u64) -> Option<ExecutionOutcome> {
        read::<Vec<ExecutionOutcome>>(&self.0).into_iter()
            .find(|row| row.intent_key == *key && row.activation == activation)
    }
}

#[derive(Default, Serialize, Deserialize)]
struct HostState {
    calls: u64,
    results: Vec<ExecutionOutcome>,
}

struct DiskHost {
    path: PathBuf,
    intents_path: PathBuf,
}

impl DispatchHost for DiskHost {
    type Result = DispatchResult;

    fn dispatch(&mut self, intent: &ExecutionIntent) -> DispatchResult {
        // Observe the actual durable files at the external-effect boundary.
        let log = DiskIntentLog(self.intents_path.clone());
        assert_eq!(log.get(&intent_key(intent), intent.activation), Some(intent.clone()));
        assert!(log.is_started(&intent_key(intent), intent.activation));
        let result = expected(intent);
        let mut state: HostState = read(&self.path);
        state.calls += 1;
        state.results.push(result.clone());
        write(&self.path, &state);
        dispatch_result(result)
    }

    fn dispatch_count(&self) -> u64 {
        read::<HostState>(&self.path).calls
    }

    fn recover_result(&mut self, intent: &ExecutionIntent) -> Option<DispatchResult> {
        read::<HostState>(&self.path).results.into_iter()
            .find(|row| row.intent_key == intent_key(intent) && row.activation == intent.activation)
            .map(dispatch_result)
    }
}

fn dispatch_result(outcome: ExecutionOutcome) -> DispatchResult {
    DispatchResult {
        cursor: outcome.cursor,
        next_activation: outcome.next_activation,
        receipts: outcome.receipts,
    }
}

fn intent_key(intent: &ExecutionIntent) -> Key {
    (intent.order.clone(), intent.attempt, intent.run)
}

fn intent() -> ExecutionIntent {
    ExecutionIntent {
        order: OrderId("durable-order".into()),
        attempt: Attempt(1),
        run: RunId(2),
        activation: 8,
        generation: Generation(3),
        epoch: Epoch(4),
        external_ref: "host-work".into(),
        recorded_at: Utc.with_ymd_and_hms(2026, 10, 15, 0, 0, 0).unwrap(),
    }
}

fn expected(intent: &ExecutionIntent) -> ExecutionOutcome {
    ExecutionOutcome {
        intent_key: intent_key(intent),
        activation: intent.activation,
        cursor: Some("durable-cursor".into()),
        next_activation: Some(ActivationRef {
            node: "after-dispatch".into(),
            inputs_digest: "durable-inputs".into(),
        }),
        receipts: vec![Receipt::Session(SessionReceipt {
            session_id: "durable-session".into(),
            order: intent.order.clone(),
            run: intent.run,
            restored_from: Some("origin-session".into()),
            at: intent.recorded_at,
        })],
    }
}

fn ports(path: &Path) -> (DiskIntentLog, DiskHost, DiskOutcomeStore) {
    let intents_path = path.join("intents.json");
    (
        DiskIntentLog(intents_path.clone()),
        DiskHost { path: path.join("host.json"), intents_path },
        DiskOutcomeStore(path.join("outcomes.json")),
    )
}

#[test]
fn all_ports_reopen_after_w1_and_w2_and_bind_the_complete_outcome_once() {
    for point in [CrashPoint::W1, CrashPoint::W2] {
        let dir = TestDir::new();
        let work = intent();
        let key = intent_key(&work);
        {
            let (mut log, mut host, mut store) = ports(&dir.0);
            let mut executor = Executor::with_faults(
                &mut log, &mut host, &mut store, FaultInjector::armed_at(point),
            );
            assert!(executor.run(work.clone()).is_err());
            assert!(executor.outcome(&key).is_none());
            assert!(!dir.0.join("outcomes.json").exists());
        }
        // Recreate every adapter; no in-memory executor, host or storage state survives.
        {
            let (mut log, mut host, mut store) = ports(&dir.0);
            let mut executor = Executor::new(&mut log, &mut host, &mut store);
            assert_eq!(executor.resume(&key), match point {
                CrashPoint::W1 => ResumeAction::ReDispatch,
                CrashPoint::W2 => ResumeAction::ReBind,
            });
            assert_eq!(executor.outcome(&key), Some(expected(&work)));
            assert_eq!(executor.resume(&key), ResumeAction::AlreadyBound(expected(&work)));
            assert_eq!(executor.dispatch_count(), 1);
        }
        let (log, mut host, store) = ports(&dir.0);
        assert!(log.is_started(&key, work.activation));
        assert_eq!(store.latest(&key), Some(expected(&work)));
        assert_eq!(host.recover_result(&work), Some(dispatch_result(expected(&work))));
        assert_eq!(host.recover_result(&work), Some(dispatch_result(expected(&work))));
        assert_eq!(host.dispatch_count(), 1);
    }
}

#[test]
fn failed_durable_intent_write_stops_before_any_host_effect() {
    let dir = TestDir::new();
    let (mut log, mut host, mut store) = ports(&dir.0);
    // Block the append's staging file after reads succeed, without permission assumptions.
    let obstruction = log.0.with_extension("pending");
    fs::create_dir(&obstruction).unwrap();
    let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Executor::new(&mut log, &mut host, &mut store).run(intent())
    }));
    assert!(stopped.is_err());
    assert_eq!(host.dispatch_count(), 0);
    assert!(!host.path.exists());
    assert!(!store.0.exists());
}
