use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{TimeZone, Utc};
use foundry_exec::host::{DieHostAdapter, DieWorker, HostKey, HostLedger, HostResult};
use foundry_exec::{
    Attempt, DispatchHost, DispatchResult, Epoch, ExecutionIntent, ExecutionOutcome, Executor,
    IntentLog, OrderId, OutcomeStore, ResumeAction, RunId,
};
use foundry_exec::types::Generation;

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "foundry-exec-host-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
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

struct FileLedger {
    dir: PathBuf,
}

impl FileLedger {
    fn new(dir: &Path) -> Self {
        fs::create_dir_all(dir).unwrap();
        Self { dir: dir.to_path_buf() }
    }

    fn path(&self, key: &HostKey, suffix: &str) -> PathBuf {
        let encoded = serde_json::to_vec(key).unwrap();
        let encoded = encoded.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        self.dir.join(format!("{encoded}.{suffix}"))
    }

    fn atomic_write(&self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let temporary = path.with_extension(format!(
            "pending-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        let result = (|| {
            use std::io::Write;
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .map_err(|error| error.to_string())?;
            file.write_all(bytes).map_err(|error| error.to_string())?;
            file.sync_all().map_err(|error| error.to_string())?;
            fs::rename(&temporary, path).map_err(|error| error.to_string())?;
            File::open(&self.dir)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| error.to_string())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}

impl HostLedger for FileLedger {
    fn accept(&mut self, key: &HostKey) -> Result<(), String> {
        let path = self.path(key, "accepted");
        if path.exists() {
            return Ok(());
        }
        self.atomic_write(&path, b"accepted")
    }

    fn is_accepted(&self, key: &HostKey) -> bool {
        self.path(key, "accepted").exists()
    }

    fn save(&mut self, key: &HostKey, result: &HostResult) -> Result<(), String> {
        if !self.is_accepted(key) {
            return Err("result cannot be saved before acceptance".into());
        }
        let path = self.path(key, "result.json");
        if path.exists() {
            return Ok(());
        }
        self.atomic_write(&path, &serde_json::to_vec(result).map_err(|error| error.to_string())?)
    }

    fn load(&self, key: &HostKey) -> Option<HostResult> {
        let path = self.path(key, "result.json");
        match fs::read(path) {
            Ok(bytes) => Some(serde_json::from_slice(&bytes).expect("invalid durable host result")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("durable host result read failed: {error}"),
        }
    }
}

#[derive(Default)]
struct Worker {
    calls: u64,
    fail_once: bool,
    receipt_path: Option<PathBuf>,
    result_path: Option<PathBuf>,
    saw_receipt_before_work: bool,
    saw_no_result_before_work: bool,
}

impl DieWorker for Worker {
    fn run(&mut self, _key: &HostKey, work: &str) -> Result<HostResult, String> {
        self.calls += 1;
        if let Some(path) = &self.receipt_path {
            self.saw_receipt_before_work = path.exists();
        }
        if let Some(path) = &self.result_path {
            self.saw_no_result_before_work = !path.exists();
        }
        if self.fail_once {
            self.fail_once = false;
            return Err("worker fault".into());
        }
        Ok(HostResult {
            base_commit: "base".into(),
            result_commit: format!("result-{work}"),
            log_path: "/work/die.log".into(),
            exit_code: 0,
        })
    }
}

fn intent() -> ExecutionIntent {
    ExecutionIntent {
        order: OrderId("host-order".into()),
        attempt: Attempt(2),
        run: RunId(7),
        activation: 11,
        generation: Generation(3),
        epoch: Epoch(4),
        external_ref: "workdir-A".into(),
        recorded_at: Utc.with_ymd_and_hms(2026, 10, 15, 0, 0, 0).unwrap(),
    }
}

fn host_key(intent: &ExecutionIntent) -> HostKey {
    HostKey {
        order: intent.order.0.clone(),
        attempt: intent.attempt.0,
        run: intent.run.0,
        activation: intent.activation,
    }
}

fn expected_result() -> HostResult {
    HostResult {
        base_commit: "base".into(),
        result_commit: "result-workdir-A".into(),
        log_path: "/work/die.log".into(),
        exit_code: 0,
    }
}

#[test]
fn dispatch_persists_receipt_before_work_and_result_before_return_and_reuses_result() {
    let dir = TestDir::new();
    let work = intent();
    let key = host_key(&work);
    let mut worker = Worker {
        receipt_path: Some(FileLedger::new(&dir.0).path(&key, "accepted")),
        result_path: Some(FileLedger::new(&dir.0).path(&key, "result.json")),
        ..Worker::default()
    };
    {
        let mut ledger = FileLedger::new(&dir.0);
        let mut host = DieHostAdapter::new(&mut worker, &mut ledger);
        assert_eq!(host.dispatch(&work), expected_result());
        assert!(host.accepted(&work));
        assert_eq!(host.recover_result(&work), Some(expected_result()));
        assert_eq!(host.dispatch(&work), expected_result());
        assert_eq!(host.dispatch_count(), 1);
    }
    assert_eq!(worker.calls, 1);
    assert!(worker.saw_receipt_before_work);
    assert!(worker.saw_no_result_before_work);
    let mut ledger = FileLedger::new(&dir.0);
    assert!(ledger.is_accepted(&key));
    let conflicting_result = HostResult { result_commit: "must-not-replace".into(), ..expected_result() };
    ledger.save(&key, &conflicting_result).unwrap();
    assert_eq!(ledger.load(&key), Some(expected_result()));
}

#[test]
fn accept_survives_adapter_drop_and_reopen_without_a_result() {
    let dir = TestDir::new();
    let key = host_key(&intent());
    FileLedger::new(&dir.0).accept(&key).unwrap();
    let reopened = FileLedger::new(&dir.0);
    assert!(reopened.is_accepted(&key));
    assert_eq!(reopened.load(&key), None);
}

#[test]
fn worker_failure_keeps_receipt_without_result_and_retry_runs_worker_again() {
    let dir = TestDir::new();
    let work = intent();
    let key = host_key(&work);
    let mut worker = Worker { fail_once: true, ..Worker::default() };
    {
        let mut ledger = FileLedger::new(&dir.0);
        let mut host = DieHostAdapter::new(&mut worker, &mut ledger);
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| host.dispatch(&work)));
        assert!(failed.is_err());
        assert!(host.accepted(&work));
        assert_eq!(host.recover_result(&work), None);
        assert_eq!(host.dispatch(&work), expected_result());
    }
    assert_eq!(worker.calls, 2);
    let reopened = FileLedger::new(&dir.0);
    assert!(reopened.is_accepted(&key));
    assert_eq!(reopened.load(&key), Some(expected_result()));
}

#[test]
fn saved_result_is_recovered_after_adapter_drop_and_reopen() {
    let dir = TestDir::new();
    let work = intent();
    let mut worker = Worker::default();
    {
        let mut ledger = FileLedger::new(&dir.0);
        assert_eq!(DieHostAdapter::new(&mut worker, &mut ledger).dispatch(&work), expected_result());
    }
    let mut worker_after_restart = Worker::default();
    let mut ledger_after_restart = FileLedger::new(&dir.0);
    let mut host = DieHostAdapter::new(&mut worker_after_restart, &mut ledger_after_restart);
    assert_eq!(host.recover_result(&work), Some(expected_result()));
    assert_eq!(host.dispatch(&work), expected_result());
    assert_eq!(host.dispatch_count(), 0);
    assert_eq!(worker_after_restart.calls, 0);
}

type IntentKey = (OrderId, Attempt, RunId);

#[derive(Default)]
struct MemoryIntents {
    rows: Vec<ExecutionIntent>,
    started: HashSet<(IntentKey, u64)>,
}

impl IntentLog for MemoryIntents {
    fn append(&mut self, intent: &ExecutionIntent) { self.rows.push(intent.clone()); }
    fn mark_started(&mut self, key: &IntentKey, activation: u64) {
        self.started.insert((key.clone(), activation));
    }
    fn latest(&self, key: &IntentKey) -> Option<ExecutionIntent> {
        self.rows.iter().rev().find(|row| intent_key(row) == *key).cloned()
    }
    fn get(&self, key: &IntentKey, activation: u64) -> Option<ExecutionIntent> {
        self.rows.iter().find(|row| intent_key(row) == *key && row.activation == activation).cloned()
    }
    fn is_started(&self, key: &IntentKey, activation: u64) -> bool {
        self.started.contains(&(key.clone(), activation))
    }
}

#[derive(Default)]
struct ResumeHost {
    accepted: bool,
    calls: u64,
}

impl DispatchHost for ResumeHost {
    type Result = DispatchResult;
    fn dispatch(&mut self, _: &ExecutionIntent) -> Self::Result {
        self.calls += 1;
        DispatchResult::default()
    }
    fn dispatch_count(&self) -> u64 { self.calls }
    fn accepted(&self, _: &ExecutionIntent) -> bool { self.accepted }
    fn recover_result(&mut self, _: &ExecutionIntent) -> Option<Self::Result> { None }
}

#[derive(Default)]
struct MemoryOutcomes(HashMap<(IntentKey, u64), ExecutionOutcome>);

impl OutcomeStore for MemoryOutcomes {
    fn commit(&mut self, outcome: &ExecutionOutcome) {
        self.0.entry((outcome.intent_key.clone(), outcome.activation)).or_insert_with(|| outcome.clone());
    }
    fn latest(&self, key: &IntentKey) -> Option<ExecutionOutcome> {
        self.0.iter().filter(|((row_key, _), _)| row_key == key).max_by_key(|((_, activation), _)| activation)
            .map(|(_, outcome)| outcome.clone())
    }
    fn get(&self, key: &IntentKey, activation: u64) -> Option<ExecutionOutcome> {
        self.0.get(&(key.clone(), activation)).cloned()
    }
}

fn intent_key(intent: &ExecutionIntent) -> IntentKey {
    (intent.order.clone(), intent.attempt, intent.run)
}

#[test]
fn resume_waits_for_accepted_host_and_redispatches_when_receipt_is_absent() {
    let work = intent();
    let key = intent_key(&work);
    let mut intents = MemoryIntents::default();
    intents.append(&work);
    intents.mark_started(&key, work.activation);
    let mut host = ResumeHost { accepted: true, ..ResumeHost::default() };
    let mut outcomes = MemoryOutcomes::default();
    let mut executor = Executor::new(&mut intents, &mut host, &mut outcomes);
    assert_eq!(executor.resume(&key), ResumeAction::AwaitResult);
    drop(executor);

    let mut host_without_receipt = ResumeHost::default();
    let mut executor = Executor::new(&mut intents, &mut host_without_receipt, &mut outcomes);
    assert_eq!(executor.resume(&key), ResumeAction::ReDispatch);
    assert_eq!(executor.dispatch_count(), 1);
}
