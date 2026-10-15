use foundry_exec::{
    ArtifactTuple, ArtifactTupleKey, Attempt, DispatchHost, DurableReviewRun, EmitLog, EmitReceipt, EmitSink,
    Epoch, ExecutionIntent, HostError, HostResult, IntentLog, OrderId,
    PromotionReceipt, ReviewRunError, ReviewRunPort, ReviewRunRequest, RunId, SqliteTupleStore,
    TupleStore,
};
use std::sync::{Arc, Mutex};

struct Host { result: HostResult, refs: Arc<Mutex<Vec<String>>> }
impl DispatchHost for Host {
    type Result = HostResult;
    fn dispatch(&mut self, intent: &ExecutionIntent) -> Self::Result {
        self.refs.lock().unwrap().push(intent.external_ref.clone());
        self.result.clone()
    }
    fn dispatch_count(&self) -> u64 { 1 }
    fn recover_result(&mut self, _: &ExecutionIntent) -> Option<Self::Result> { None }
}

#[derive(Default)]
struct Intents(Vec<ExecutionIntent>);
impl IntentLog for Intents {
    fn append(&mut self, i: &ExecutionIntent) { self.0.push(i.clone()); }
    fn mark_started(&mut self, _: &(OrderId, Attempt, RunId), _: u64) {}
    fn latest(&self, _: &(OrderId, Attempt, RunId)) -> Option<ExecutionIntent> { self.0.last().cloned() }
    fn get(&self, _: &(OrderId, Attempt, RunId), _: u64) -> Option<ExecutionIntent> { self.0.last().cloned() }
    fn is_started(&self, _: &(OrderId, Attempt, RunId), _: u64) -> bool { false }
}

#[derive(Default)]
struct Sink { receipts: Vec<EmitReceipt> }
impl EmitSink for Sink {
    fn publish(&mut self, tuple: &ArtifactTuple) -> EmitReceipt {
        let receipt = EmitReceipt { key: tuple.key(), event: "publish".into(), at: chrono::Utc::now() };
        self.receipts.push(receipt.clone()); receipt
    }
    fn receipt_of(&self, key: &ArtifactTupleKey) -> Option<EmitReceipt> { self.receipts.iter().find(|r| &r.key == key).cloned() }
}

#[derive(Default)]
struct Log { emitted: Option<EmitReceipt>, complete: bool }
impl EmitLog for Log {
    fn current_epoch(&self) -> Epoch { Epoch(0) }
    fn promotion_of(&self, _: &ArtifactTupleKey) -> Option<PromotionReceipt> { None }
    fn emit_of(&self, key: &ArtifactTupleKey) -> Option<EmitReceipt> { self.emitted.as_ref().filter(|r| &r.key == key).cloned() }
    fn record_emit(&mut self, receipt: &EmitReceipt) { self.emitted = Some(receipt.clone()); }
    fn mark_complete(&mut self, _: &ArtifactTupleKey) { self.complete = true; }
    fn is_complete(&self, _: &ArtifactTupleKey) -> bool { self.complete }
}

struct Db(std::path::PathBuf);
impl Db {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(std::env::temp_dir().join(format!("review-run-{}-{}.db", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))))
    }
}
impl Drop for Db { fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); let _ = std::fs::remove_file(self.0.with_extension("db-wal")); let _ = std::fs::remove_file(self.0.with_extension("db-shm")); } }

fn runner(exit_code: i32, base: &str, result: &str, refs: Arc<Mutex<Vec<String>>>, path: &std::path::Path) -> impl ReviewRunPort {
    let host = Host { result: HostResult { base_commit: base.into(), result_commit: result.into(), log_path: "review.log".into(), exit_code }, refs };
    DurableReviewRun::new(Sink::default(), Log::default(), Box::new(host), Box::new(Intents::default()),
        Arc::new(SqliteTupleStore::open(path).unwrap()) as Arc<dyn TupleStore>)
}

fn request() -> ReviewRunRequest {
    ReviewRunRequest { order: OrderId("o-31".into()), attempt: Attempt(4), run: RunId(22),
        base_commit: "base".into(), result_commit: "result".into(), artifact_digest: "result".into(),
        epoch: Epoch(3), activation: 9 }
}

#[test]
fn successful_review_is_bound_to_die_commits_and_publishes_a_tuple() {
    let db = Db::new();
    let refs = Arc::new(Mutex::new(Vec::new()));
    let mut run = runner(0, "base", "result", refs.clone(), &db.0);
    let evidence = run.run(request()).unwrap();
    assert_eq!(evidence.verdict, foundry_exec::ReviewVerdict::Passed);
    assert_eq!(evidence.tuple_key.run, RunId(22));
    assert_eq!(refs.lock().unwrap().as_slice(), &["review:o-31:4:base..result"]);
    let tuples = SqliteTupleStore::open(&db.0).unwrap();
    assert_eq!(tuples.latest(&OrderId("o-31".into()), Attempt(4)).unwrap().unwrap().immutable_commit.0, "result");
}

#[test]
fn rejection_and_binding_mismatch_never_create_a_tuple() {
    for (code, base, result, expected) in [(2, "base", "result", "rejected"), (0, "other", "result", "binding")] {
        let db = Db::new();
        let mut run = runner(code, base, result, Arc::new(Mutex::new(Vec::new())), &db.0);
        let error = run.run(request()).unwrap_err();
        match expected {
            "rejected" => assert!(matches!(error, ReviewRunError::Rejected { .. })),
            _ => assert_eq!(error, ReviewRunError::BindingMismatch),
        }
        let tuples = SqliteTupleStore::open(&db.0).unwrap();
        assert!(tuples.latest(&OrderId("o-31".into()), Attempt(4)).unwrap().is_none());
    }
}

#[test]
fn host_error_is_fail_closed() {
    struct Broken;
    impl DispatchHost for Broken {
        type Result = HostResult;
        fn dispatch(&mut self, _: &ExecutionIntent) -> HostResult { unreachable!() }
        fn try_dispatch(&mut self, _: &ExecutionIntent) -> Result<HostResult, HostError> { Err(HostError::Worker("offline".into())) }
        fn dispatch_count(&self) -> u64 { 0 }
        fn recover_result(&mut self, _: &ExecutionIntent) -> Option<Self::Result> { None }
    }
    let db = Db::new();
    let tuples = Arc::new(SqliteTupleStore::open(&db.0).unwrap()) as Arc<dyn TupleStore>;
    let mut run = DurableReviewRun::new(Sink::default(), Log::default(), Box::new(Broken), Box::new(Intents::default()), tuples);
    assert!(matches!(run.run(request()), Err(ReviewRunError::Dispatch(HostError::Worker(_)))));
}

