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

fn note_path(path: &Path) {
    if let Some(inventory) = std::env::var_os("FOUNDRY_EXEC_TEST_INVENTORY") {
        use std::io::Write;
        let mut file = fs::OpenOptions::new().create(true).append(true).open(inventory).unwrap();
        writeln!(file, "{}", path.display()).unwrap();
    }
}

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
        note_path(&path);
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fn record_tree(path: &Path) {
            note_path(path);
            if path.is_dir() {
                for entry in fs::read_dir(path).unwrap() { record_tree(&entry.unwrap().path()); }
            }
        }
        record_tree(&self.0);
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

    fn exists(path: &Path) -> bool {
        path.try_exists().unwrap_or_else(|error| panic!("durable host lookup failed: {error}"))
    }

    fn crash_at(path: &Path, phase: &str) {
        let target = if path.extension().is_some_and(|suffix| suffix == "accepted") { "accept" } else { "result" };
        if std::env::var("FOUNDRY_EXEC_CRASH_STAGE").ok().as_deref() == Some(&format!("{target}:{phase}")) {
            std::process::exit(73);
        }
    }

    fn atomic_write(&self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let temporary = path.with_extension(format!(
            "pending-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        note_path(&temporary);
        note_path(path);
        let result = (|| {
            use std::io::Write;
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .map_err(|error| error.to_string())?;
            file.write_all(bytes).map_err(|error| error.to_string())?;
            file.sync_all().map_err(|error| error.to_string())?;
            Self::crash_at(path, "before-rename");
            fs::rename(&temporary, path).map_err(|error| error.to_string())?;
            Self::crash_at(path, "after-rename");
            File::open(&self.dir)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| error.to_string())?;
            Self::crash_at(path, "after-dir-sync");
            Ok(())
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
        if Self::exists(&path) {
            return Ok(());
        }
        self.atomic_write(&path, b"accepted")
    }

    fn is_accepted(&self, key: &HostKey) -> bool {
        Self::exists(&self.path(key, "accepted"))
    }

    fn save(&mut self, key: &HostKey, result: &HostResult) -> Result<(), String> {
        if !self.is_accepted(key) {
            return Err("result cannot be saved before acceptance".into());
        }
        let path = self.path(key, "result.json");
        if Self::exists(&path) {
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
    effects_path: Option<PathBuf>,
}

impl DieWorker for Worker {
    fn run(&mut self, _key: &HostKey, work: &str) -> Result<HostResult, String> {
        self.calls += 1;
        if let Some(path) = &self.effects_path {
            note_path(path);
            use std::io::Write;
            let mut file = fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
            file.write_all(b"effect\n").unwrap();
            file.sync_all().unwrap();
            File::open(path.parent().unwrap()).unwrap().sync_all().unwrap();
        }
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
        assert_eq!(host.dispatch(&work), DispatchResult::from(expected_result()));
        assert!(host.accepted(&work));
        assert_eq!(host.recover_result(&work), Some(DispatchResult::from(expected_result())));
        assert_eq!(host.dispatch(&work), DispatchResult::from(expected_result()));
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
        assert_eq!(host.dispatch(&work), DispatchResult::from(expected_result()));
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
        assert_eq!(DieHostAdapter::new(&mut worker, &mut ledger).dispatch(&work), DispatchResult::from(expected_result()));
    }
    let mut worker_after_restart = Worker::default();
    let mut ledger_after_restart = FileLedger::new(&dir.0);
    let mut host = DieHostAdapter::new(&mut worker_after_restart, &mut ledger_after_restart);
    assert_eq!(host.recover_result(&work), Some(DispatchResult::from(expected_result())));
    assert_eq!(host.dispatch(&work), DispatchResult::from(expected_result()));
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

#[test]
fn b3a_adapter_drives_executor_and_preserves_complete_result_without_repeat_effects() {
    let dir = TestDir::new();
    let work = intent();
    let key = intent_key(&work);
    let mut intents = MemoryIntents::default();
    let mut outcomes = MemoryOutcomes::default();
    let mut worker = Worker::default();
    let mut ledger = FileLedger::new(&dir.0);
    let mut host = DieHostAdapter::new(&mut worker, &mut ledger);
    let mut executor = Executor::new(&mut intents, &mut host, &mut outcomes);
    let first = executor.run(work.clone()).unwrap();
    let foundry_exec::RunStep::Bound(outcome) = &first;
    assert_eq!(outcome.intent_key, key);
    assert_eq!(outcome.activation, work.activation);
    assert!(outcome.cursor.is_none());
    assert!(outcome.next_activation.is_none());
    let json = serde_json::to_value(outcome).unwrap();
    assert_eq!(json["receipts"], serde_json::json!([{
        "kind": "host", "receipt": expected_result(),
    }]));
    assert_eq!(serde_json::from_value::<ExecutionOutcome>(json).unwrap(), *outcome);
    assert_eq!(executor.run(work), Ok(first.clone()));
    assert_eq!(executor.resume(&key), ResumeAction::AlreadyBound(outcome.clone()));
    assert_eq!(executor.dispatch_count(), 1);
    drop(executor);
    drop(host);
    assert_eq!(worker.calls, 1);
    let mut restarted_worker = Worker::default();
    let mut restarted_ledger = FileLedger::new(&dir.0);
    let mut host = DieHostAdapter::new(&mut restarted_worker, &mut restarted_ledger);
    let mut executor = Executor::new(&mut intents, &mut host, &mut outcomes);
    assert_eq!(executor.run(intent()), Ok(first));
    assert!(matches!(executor.resume(&key), ResumeAction::AlreadyBound(_)));
    drop(executor);
    drop(host);
    assert_eq!(restarted_worker.calls, 0);
}

#[derive(Clone, Copy, Debug)]
enum Failure { AcceptBefore, AcceptAfter, SaveBefore, SaveAfter }
struct FailingLedger { inner: FileLedger, failure: Option<Failure> }
impl HostLedger for FailingLedger {
    fn accept(&mut self, key: &HostKey) -> Result<(), String> {
        if matches!(self.failure, Some(Failure::AcceptBefore)) {
            self.failure = None;
            return Err("accept before write".into());
        }
        self.inner.accept(key)?;
        if matches!(self.failure, Some(Failure::AcceptAfter)) {
            self.failure = None;
            return Err("accept after write".into());
        }
        Ok(())
    }
    fn is_accepted(&self, key: &HostKey) -> bool { self.inner.is_accepted(key) }
    fn save(&mut self, key: &HostKey, result: &HostResult) -> Result<(), String> {
        if matches!(self.failure, Some(Failure::SaveBefore)) {
            self.failure = None;
            return Err("save before write".into());
        }
        self.inner.save(key, result)?;
        if matches!(self.failure, Some(Failure::SaveAfter)) {
            self.failure = None;
            return Err("save after write".into());
        }
        Ok(())
    }
    fn load(&self, key: &HostKey) -> Option<HostResult> { self.inner.load(key) }
}

fn assert_ledger_error_interrupts(failure: Failure, via_resume: bool) {
    let dir = TestDir::new();
    let work = intent();
    let key = intent_key(&work);
    let mut worker = Worker::default();
    let mut ledger = FailingLedger { inner: FileLedger::new(&dir.0), failure: Some(failure) };
    let mut intents = MemoryIntents::default();
    let mut outcomes = MemoryOutcomes::default();
    if via_resume { intents.append(&work); }
    let mut host = DieHostAdapter::new(&mut worker, &mut ledger);
    let mut executor = Executor::new(&mut intents, &mut host, &mut outcomes);
    let returned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if via_resume {
            match executor.resume(&key) {
                ResumeAction::Interrupted(error) => Err(error),
                other => panic!("expected interruption, got {other:?}"),
            }
        } else { executor.run(work.clone()) }
    }));
    assert!(returned.is_ok(), "{failure:?} escaped as panic");
    let error = returned.unwrap().unwrap_err();
    let stage = match failure {
        Failure::AcceptBefore | Failure::AcceptAfter => "Acceptance",
        Failure::SaveBefore | Failure::SaveAfter => "Persistence",
    };
    assert!(format!("{error:?}").contains(stage));
    assert!(executor.outcome(&key).is_none());
    assert!(executor.is_started(&key, work.activation));
    let expected = match failure {
        Failure::AcceptBefore => ResumeAction::ReDispatch,
        Failure::AcceptAfter | Failure::SaveBefore => ResumeAction::AwaitResult,
        Failure::SaveAfter => ResumeAction::ReBind,
    };
    assert_eq!(executor.resume(&key), expected);
    if expected == ResumeAction::AwaitResult {
        assert!(matches!(executor.run(work), Err(foundry_exec::LifecycleError::AwaitResult(11))));
        assert_eq!(executor.resume(&key), ResumeAction::AwaitResult);
    } else {
        assert!(matches!(executor.resume(&key), ResumeAction::AlreadyBound(_)));
    }
    drop(executor);
    drop(host);
    assert_eq!(worker.calls, if matches!(failure, Failure::AcceptAfter) { 0 } else { 1 });
}

#[test]
fn b3a_accept_before_error_interrupts() {
    for via_resume in [false, true] { assert_ledger_error_interrupts(Failure::AcceptBefore, via_resume); }
}
#[test]
fn b3a_accept_after_error_interrupts() {
    for via_resume in [false, true] { assert_ledger_error_interrupts(Failure::AcceptAfter, via_resume); }
}
#[test]
fn b3a_save_before_error_interrupts() {
    for via_resume in [false, true] { assert_ledger_error_interrupts(Failure::SaveBefore, via_resume); }
}
#[test]
fn b3a_save_after_error_interrupts() {
    for via_resume in [false, true] { assert_ledger_error_interrupts(Failure::SaveAfter, via_resume); }
}

#[test]
fn b3a_worker_error_interrupts_without_panic_and_restart_waits() {
    let dir = TestDir::new();
    let work = intent();
    let key = intent_key(&work);
    let mut worker = Worker { fail_once: true, ..Worker::default() };
    let mut ledger = FileLedger::new(&dir.0);
    let mut intents = MemoryIntents::default();
    let mut outcomes = MemoryOutcomes::default();
    {
        let mut host = DieHostAdapter::new(&mut worker, &mut ledger);
        let mut executor = Executor::new(&mut intents, &mut host, &mut outcomes);
        let returned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| executor.run(work.clone())));
        assert!(returned.is_ok(), "worker Err escaped as panic");
        assert!(format!("{:?}", returned.unwrap().unwrap_err()).contains("Worker"));
        assert!(executor.outcome(&key).is_none());
    }
    assert_eq!(worker.calls, 1);
    let mut fresh_worker = Worker::default();
    let mut fresh_ledger = FileLedger::new(&dir.0);
    let mut host = DieHostAdapter::new(&mut fresh_worker, &mut fresh_ledger);
    let mut executor = Executor::new(&mut intents, &mut host, &mut outcomes);
    assert_eq!(executor.resume(&key), ResumeAction::AwaitResult);
    assert_eq!(executor.run(work), Err(foundry_exec::LifecycleError::AwaitResult(11)));
    drop(executor);
    drop(host);
    assert_eq!(fresh_worker.calls, 0);
}

#[test]
fn b3a_unreadable_receipt_is_not_reported_absent() {
    let dir = TestDir::new();
    let storage = dir.0.join("ledger");
    let ledger = FileLedger::new(&storage);
    fs::remove_dir(&storage).unwrap();
    fs::write(&storage, b"not a directory").unwrap();
    let returned = std::panic::catch_unwind(|| ledger.is_accepted(&host_key(&intent())));
    assert!(returned.is_err(), "I/O error was silently converted to no receipt");
}

#[test]
fn b3a_save_without_acceptance_writes_nothing_and_receipt_repeats_preserve_result() {
    let dir = TestDir::new();
    let key = host_key(&intent());
    let mut ledger = FileLedger::new(&dir.0);
    assert!(ledger.save(&key, &expected_result()).is_err());
    assert!(!ledger.is_accepted(&key));
    assert_eq!(ledger.load(&key), None);
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 0);
    ledger.accept(&key).unwrap();
    ledger.save(&key, &expected_result()).unwrap();
    ledger.accept(&key).unwrap();
    assert_eq!(ledger.load(&key), Some(expected_result()));
}

#[test]
fn b3a_actual_adapter_partitions_receipts_and_results_by_every_identity_field() {
    let dir = TestDir::new();
    let work = intent();
    let mut variants = vec![work.clone(); 4];
    variants[0].order.0.push_str("-other");
    variants[1].attempt.0 += 1;
    variants[2].run.0 += 1;
    variants[3].activation += 1;
    let mut worker = Worker::default();
    let mut ledger = FileLedger::new(&dir.0);
    let mut host = DieHostAdapter::new(&mut worker, &mut ledger);
    host.dispatch(&work);
    for other in variants {
        assert!(!host.accepted(&other));
        assert_eq!(host.recover_result(&other), None);
        assert_eq!(host.dispatch(&other), DispatchResult::from(expected_result()));
        assert!(host.accepted(&other));
        assert_eq!(host.recover_result(&other), Some(DispatchResult::from(expected_result())));
    }
    assert_eq!(host.dispatch_count(), 5);
}

// Invoked in a separate process; exit at a publication boundary skips all Drop.
#[test]
fn b3a_crash_child() {
    let Some(dir) = std::env::var_os("FOUNDRY_EXEC_CRASH_DIR") else { return; };
    let dir = PathBuf::from(dir);
    let mut worker = Worker { effects_path: Some(dir.join("effects.log")), ..Worker::default() };
    let mut ledger = FileLedger::new(&dir);
    DieHostAdapter::new(&mut worker, &mut ledger).dispatch(&intent());
}

fn effect_count(dir: &Path) -> usize {
    match fs::read_to_string(dir.join("effects.log")) {
        Ok(text) => text.lines().count(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => panic!("effect log read failed: {error}"),
    }
}

#[test]
fn b3a_process_crashes_observe_only_published_receipts_and_results_without_repeat_effects() {
    for (target, phase, accepted, saved, effects) in [
        ("accept", "before-rename", false, false, 0),
        ("accept", "after-rename", true, false, 0),
        ("accept", "after-dir-sync", true, false, 0),
        ("result", "before-rename", true, false, 1),
        ("result", "after-rename", true, true, 1),
        ("result", "after-dir-sync", true, true, 1),
    ] {
        let dir = TestDir::new();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "b3a_crash_child", "--nocapture"])
            .env("FOUNDRY_EXEC_CRASH_DIR", &dir.0)
            .env("FOUNDRY_EXEC_CRASH_STAGE", format!("{target}:{phase}"))
            .output().unwrap();
        assert_eq!(status.status.code(), Some(73), "child missed {target}:{phase}: {}",
            String::from_utf8_lossy(&status.stdout));
        let work = intent();
        let host_key = host_key(&work);
        let mut ledger = FileLedger::new(&dir.0);
        assert_eq!(ledger.is_accepted(&host_key), accepted, "{target}:{phase}");
        assert_eq!(ledger.load(&host_key), saved.then(expected_result));
        assert_eq!(effect_count(&dir.0), effects);
        let pending: Vec<_> = fs::read_dir(&dir.0).unwrap().map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().unwrap_or_default().to_string_lossy().starts_with("pending-"))
            .collect();
        assert_eq!(pending.len(), usize::from(phase == "before-rename"));
        // A crash may leave even an undecodable partial temporary. Reads must
        // ignore it rather than publish it as an acceptance or result.
        for path in pending { fs::write(path, b"{partial").unwrap(); }
        let key = intent_key(&work);
        let mut intents = MemoryIntents::default();
        intents.append(&work);
        intents.mark_started(&key, work.activation);
        let mut outcomes = MemoryOutcomes::default();
        let mut worker = Worker { effects_path: Some(dir.0.join("effects.log")), ..Worker::default() };
        let mut host = DieHostAdapter::new(&mut worker, &mut ledger);
        let mut executor = Executor::new(&mut intents, &mut host, &mut outcomes);
        let expected = if saved { ResumeAction::ReBind }
            else if accepted { ResumeAction::AwaitResult } else { ResumeAction::ReDispatch };
        assert_eq!(executor.resume(&key), expected, "{target}:{phase}");
        if expected == ResumeAction::AwaitResult {
            assert_eq!(executor.run(work), Err(foundry_exec::LifecycleError::AwaitResult(11)));
            assert_eq!(executor.resume(&key), ResumeAction::AwaitResult);
            assert!(executor.outcome(&key).is_none());
        } else {
            let foundry_exec::RunStep::Bound(bound) = executor.run(work).unwrap();
            assert_eq!(bound.receipts, DispatchResult::from(expected_result()).receipts);
            assert_eq!(executor.resume(&key), ResumeAction::AlreadyBound(bound));
        }
        drop(executor);
        drop(host);
        assert_eq!(worker.calls, u64::from(!accepted));
        assert_eq!(effect_count(&dir.0), if accepted { effects } else { 1 });
    }
}

#[test]
fn b3a_actual_adapter_run_and_resume_cover_all_reachable_receipt_states() {
    for via_resume in [false, true] {
        // Receipt/result with no started marker is unreachable: Executor durably
        // marks started before the adapter can accept, and these facts persist.
        for (started, accepted, saved) in [
            (false, false, false), (true, false, false),
            (true, true, false), (true, true, true),
        ] {
            let dir = TestDir::new();
            let work = intent();
            let key = intent_key(&work);
            let mut intents = MemoryIntents::default();
            intents.append(&work);
            if started { intents.mark_started(&key, work.activation); }
            let mut ledger = FileLedger::new(&dir.0);
            if accepted { ledger.accept(&host_key(&work)).unwrap(); }
            if saved { ledger.save(&host_key(&work), &expected_result()).unwrap(); }
            let mut outcomes = MemoryOutcomes::default();
            let mut worker = Worker::default();
            let mut host = DieHostAdapter::new(&mut worker, &mut ledger);
            let mut executor = Executor::new(&mut intents, &mut host, &mut outcomes);
            if via_resume {
                assert_eq!(executor.resume(&key), if saved { ResumeAction::ReBind }
                    else if accepted { ResumeAction::AwaitResult } else { ResumeAction::ReDispatch });
            } else if accepted && !saved {
                assert_eq!(executor.run(work.clone()), Err(foundry_exec::LifecycleError::AwaitResult(11)));
            } else {
                assert!(matches!(executor.run(work.clone()), Ok(foundry_exec::RunStep::Bound(_))));
            }
            if accepted && !saved {
                assert_eq!(executor.resume(&key), ResumeAction::AwaitResult);
                assert_eq!(executor.run(work), Err(foundry_exec::LifecycleError::AwaitResult(11)));
                assert!(executor.outcome(&key).is_none());
            } else {
                let bound = executor.outcome(&key).unwrap();
                assert_eq!(bound.receipts, DispatchResult::from(expected_result()).receipts);
                assert_eq!(executor.resume(&key), ResumeAction::AlreadyBound(bound));
                assert!(executor.run(work).is_ok());
            }
            drop(executor);
            drop(host);
            assert_eq!(worker.calls, u64::from(!accepted));
            assert_eq!(intents.rows.len(), 1);
        }
    }
}

#[test]
fn b3a_real_host_w1_w2_recovery_reopens_ledger_and_repeats_no_completed_work() {
    for point in [foundry_exec::CrashPoint::W1, foundry_exec::CrashPoint::W2] {
        let dir = TestDir::new();
        let work = intent();
        let key = intent_key(&work);
        let mut intents = MemoryIntents::default();
        let mut outcomes = MemoryOutcomes::default();
        let mut worker = Worker::default();
        {
            let mut ledger = FileLedger::new(&dir.0);
            let mut host = DieHostAdapter::new(&mut worker, &mut ledger);
            let mut executor = Executor::with_faults(&mut intents, &mut host, &mut outcomes,
                foundry_exec::FaultInjector::armed_at(point));
            assert_eq!(executor.run(work.clone()), Err(foundry_exec::LifecycleError::Crash(point)));
            assert!(executor.outcome(&key).is_none());
        }
        assert_eq!(worker.calls, u64::from(point == foundry_exec::CrashPoint::W2));
        let mut fresh_worker = Worker::default();
        let mut reopened = FileLedger::new(&dir.0);
        let mut host = DieHostAdapter::new(&mut fresh_worker, &mut reopened);
        let mut executor = Executor::new(&mut intents, &mut host, &mut outcomes);
        assert_eq!(executor.resume(&key), if point == foundry_exec::CrashPoint::W1 {
            ResumeAction::ReDispatch
        } else { ResumeAction::ReBind });
        let bound = executor.outcome(&key).unwrap();
        assert_eq!(bound.receipts, DispatchResult::from(expected_result()).receipts);
        assert_eq!(executor.resume(&key), ResumeAction::AlreadyBound(bound));
        assert!(executor.run(work).is_ok());
        drop(executor);
        drop(host);
        assert_eq!(fresh_worker.calls, u64::from(point == foundry_exec::CrashPoint::W1));
    }
}

#[test]
fn b3a_conversion_retains_nonzero_exit_and_all_metadata_without_routing_changes() {
    let result = HostResult { exit_code: 137, ..expected_result() };
    let dispatch = DispatchResult::from(result.clone());
    assert_eq!(dispatch.receipts, vec![foundry_exec::Receipt::Host(result)]);
    assert_eq!(dispatch.cursor, None);
    assert_eq!(dispatch.next_activation, None);
}

#[test]
fn b3a_host_receipt_rejects_unknown_fields_inside_an_outcome() {
    let work = intent();
    let outcome = ExecutionOutcome {
        intent_key: intent_key(&work), activation: work.activation,
        cursor: None, next_activation: None,
        receipts: vec![foundry_exec::Receipt::Host(expected_result())],
    };
    let mut json = serde_json::to_value(outcome).unwrap();
    json["receipts"][0]["receipt"]["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<ExecutionOutcome>(json).is_err());
}
