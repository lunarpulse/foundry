use foundry_exec::{
    ArtifactTuple, ArtifactTupleKey, Attempt, DispatchHost, DurableReviewRun, EmitLog, EmitReceipt,
    EmitSink, Epoch, ExecutionIntent, HostError, HostResult, IntentLog, OrderId, PromotionReceipt,
    ReviewRunError, ReviewRunPort, ReviewRunRequest, RunId, SqliteTupleStore, TupleStore,
};
use std::sync::{Arc, Mutex};

struct Host {
    result: HostResult,
    refs: Arc<Mutex<Vec<String>>>,
}
impl DispatchHost for Host {
    type Result = HostResult;
    fn dispatch(&mut self, intent: &ExecutionIntent) -> Self::Result {
        self.refs.lock().unwrap().push(intent.external_ref.clone());
        self.result.clone()
    }
    fn dispatch_count(&self) -> u64 {
        1
    }
    fn recover_result(&mut self, _: &ExecutionIntent) -> Option<Self::Result> {
        None
    }
}

#[derive(Default)]
struct Intents(Vec<ExecutionIntent>);
impl IntentLog for Intents {
    fn append(&mut self, i: &ExecutionIntent) {
        self.0.push(i.clone());
    }
    fn mark_started(&mut self, _: &(OrderId, Attempt, RunId), _: u64) {}
    fn latest(&self, _: &(OrderId, Attempt, RunId)) -> Option<ExecutionIntent> {
        self.0.last().cloned()
    }
    fn get(&self, _: &(OrderId, Attempt, RunId), _: u64) -> Option<ExecutionIntent> {
        self.0.last().cloned()
    }
    fn is_started(&self, _: &(OrderId, Attempt, RunId), _: u64) -> bool {
        false
    }
}

#[derive(Default)]
struct Sink {
    receipts: Vec<EmitReceipt>,
}
impl EmitSink for Sink {
    fn publish(&mut self, tuple: &ArtifactTuple) -> EmitReceipt {
        let receipt = EmitReceipt {
            key: tuple.key(),
            event: "publish".into(),
            at: chrono::Utc::now(),
        };
        self.receipts.push(receipt.clone());
        receipt
    }
    fn receipt_of(&self, key: &ArtifactTupleKey) -> Option<EmitReceipt> {
        self.receipts.iter().find(|r| &r.key == key).cloned()
    }
}

#[derive(Default)]
struct Log {
    emitted: Option<EmitReceipt>,
    complete: bool,
}
impl EmitLog for Log {
    fn current_epoch(&self) -> Epoch {
        Epoch(0)
    }
    fn promotion_of(&self, _: &ArtifactTupleKey) -> Option<PromotionReceipt> {
        None
    }
    fn emit_of(&self, key: &ArtifactTupleKey) -> Option<EmitReceipt> {
        self.emitted.as_ref().filter(|r| &r.key == key).cloned()
    }
    fn record_emit(&mut self, receipt: &EmitReceipt) {
        self.emitted = Some(receipt.clone());
    }
    fn mark_complete(&mut self, _: &ArtifactTupleKey) {
        self.complete = true;
    }
    fn is_complete(&self, _: &ArtifactTupleKey) -> bool {
        self.complete
    }
}

struct Db(std::path::PathBuf);
impl Db {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(std::env::temp_dir().join(format!(
            "review-run-{}-{}.db",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }
}
impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(self.0.with_extension("db-wal"));
        let _ = std::fs::remove_file(self.0.with_extension("db-shm"));
    }
}

fn runner(
    exit_code: i32,
    base: &str,
    result: &str,
    refs: Arc<Mutex<Vec<String>>>,
    path: &std::path::Path,
) -> impl ReviewRunPort {
    let host = Host {
        result: HostResult {
            base_commit: base.into(),
            result_commit: result.into(),
            log_path: "review.log".into(),
            exit_code,
            generations: Vec::new(),
            held: None,
        },
        refs,
    };
    DurableReviewRun::new(
        Sink::default(),
        Log::default(),
        Box::new(host),
        Box::new(Intents::default()),
        Arc::new(SqliteTupleStore::open(path).unwrap()) as Arc<dyn TupleStore>,
    )
}

fn request() -> ReviewRunRequest {
    ReviewRunRequest {
        order: OrderId("o-31".into()),
        attempt: Attempt(4),
        run: RunId(22),
        base_commit: "base".into(),
        result_commit: "result".into(),
        artifact_digest: "result".into(),
        epoch: Epoch(3),
        activation: 9,
    }
}

fn generation_runner(
    chain: Vec<foundry_exec::GenerationCommit>,
    final_commit: &str,
    path: &std::path::Path,
) -> impl ReviewRunPort {
    let host = Host {
        result: HostResult {
            base_commit: "base".into(),
            result_commit: final_commit.into(),
            log_path: "review.log".into(),
            exit_code: 0,
            generations: chain,
            held: None,
        },
        refs: Arc::new(Mutex::new(Vec::new())),
    };
    DurableReviewRun::new(
        Sink::default(),
        Log::default(),
        Box::new(host),
        Box::new(Intents::default()),
        Arc::new(SqliteTupleStore::open(path).unwrap()) as Arc<dyn TupleStore>,
    )
}

#[test]
fn successful_review_is_bound_to_die_commits_and_publishes_a_tuple() {
    let db = Db::new();
    let refs = Arc::new(Mutex::new(Vec::new()));
    let mut run = runner(0, "base", "result", refs.clone(), &db.0);
    let evidence = run.run(request()).unwrap();
    let foundry_exec::ReviewRunOutcome::Completed(evidence) = evidence else {
        panic!("expected completed review")
    };
    assert_eq!(evidence.verdict, foundry_exec::ReviewVerdict::Passed);
    assert_eq!(evidence.tuple_key.run, RunId(22));
    assert_eq!(
        refs.lock().unwrap().as_slice(),
        &["review:o-31:4:base..result"]
    );
    let tuples = SqliteTupleStore::open(&db.0).unwrap();
    assert_eq!(
        tuples
            .latest(&OrderId("o-31".into()), Attempt(4))
            .unwrap()
            .unwrap()
            .immutable_commit
            .0,
        "result"
    );
}

#[test]
fn rejection_and_binding_mismatch_never_create_a_tuple() {
    for (code, base, result, expected) in [
        (2, "base", "result", "rejected"),
        (0, "other", "result", "binding"),
    ] {
        let db = Db::new();
        let mut run = runner(code, base, result, Arc::new(Mutex::new(Vec::new())), &db.0);
        let error = run.run(request()).unwrap_err();
        match expected {
            "rejected" => assert!(matches!(error, ReviewRunError::Rejected { .. })),
            _ => assert_eq!(error, ReviewRunError::BindingMismatch),
        }
        let tuples = SqliteTupleStore::open(&db.0).unwrap();
        assert!(
            tuples
                .latest(&OrderId("o-31".into()), Attempt(4))
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn host_error_is_fail_closed() {
    struct Broken;
    impl DispatchHost for Broken {
        type Result = HostResult;
        fn dispatch(&mut self, _: &ExecutionIntent) -> HostResult {
            unreachable!()
        }
        fn try_dispatch(&mut self, _: &ExecutionIntent) -> Result<HostResult, HostError> {
            Err(HostError::Worker("offline".into()))
        }
        fn dispatch_count(&self) -> u64 {
            0
        }
        fn recover_result(&mut self, _: &ExecutionIntent) -> Option<Self::Result> {
            None
        }
    }
    let db = Db::new();
    let tuples = Arc::new(SqliteTupleStore::open(&db.0).unwrap()) as Arc<dyn TupleStore>;
    let mut run = DurableReviewRun::new(
        Sink::default(),
        Log::default(),
        Box::new(Broken),
        Box::new(Intents::default()),
        tuples,
    );
    assert!(matches!(
        run.run(request()),
        Err(ReviewRunError::Dispatch(HostError::Worker(_)))
    ));
}

#[test]
fn generation_chain_binds_tuple_to_final_head_and_rejects_malformed_chains() {
    let db = Db::new();
    let chain = vec![
        foundry_exec::GenerationCommit {
            generation: 0,
            commit: "result".into(),
            fix_log_path: String::new(),
        },
        foundry_exec::GenerationCommit {
            generation: 1,
            commit: "fixed".into(),
            fix_log_path: "fix.log".into(),
        },
    ];
    let mut run = generation_runner(chain.clone(), "fixed", &db.0);
    assert!(matches!(
        run.run(request()).unwrap(),
        foundry_exec::ReviewRunOutcome::Completed(_)
    ));
    let tuple = SqliteTupleStore::open(&db.0)
        .unwrap()
        .latest(&OrderId("o-31".into()), Attempt(4))
        .unwrap()
        .unwrap();
    assert_eq!(tuple.immutable_commit.0, "fixed");
    assert_eq!(tuple.generations, chain);

    let malformed = [
        vec![foundry_exec::GenerationCommit {
            generation: 0,
            commit: "wrong".into(),
            fix_log_path: String::new(),
        }],
        vec![
            chain[0].clone(),
            foundry_exec::GenerationCommit {
                generation: 2,
                commit: "fixed".into(),
                fix_log_path: "fix.log".into(),
            },
        ],
        vec![
            chain[0].clone(),
            foundry_exec::GenerationCommit {
                generation: 1,
                commit: "fixed".into(),
                fix_log_path: String::new(),
            },
        ],
    ];
    for entries in malformed {
        let db = Db::new();
        let mut run = generation_runner(entries, "fixed", &db.0);
        assert_eq!(
            run.run(request()).unwrap_err(),
            ReviewRunError::BindingMismatch
        );
    }
    let db = Db::new();
    let mut run = generation_runner(chain, "different", &db.0);
    assert_eq!(
        run.run(request()).unwrap_err(),
        ReviewRunError::BindingMismatch
    );
}

type Trace = Arc<Mutex<Vec<&'static str>>>;
struct TracedIntents(Trace, Intents);
impl IntentLog for TracedIntents {
    fn append(&mut self, i: &ExecutionIntent) {
        self.0.lock().unwrap().push("intent");
        self.1.append(i);
    }
    fn mark_started(&mut self, key: &(OrderId, Attempt, RunId), a: u64) {
        self.1.mark_started(key, a);
    }
    fn latest(&self, key: &(OrderId, Attempt, RunId)) -> Option<ExecutionIntent> {
        self.1.latest(key)
    }
    fn get(&self, key: &(OrderId, Attempt, RunId), a: u64) -> Option<ExecutionIntent> {
        self.1.get(key, a)
    }
    fn is_started(&self, key: &(OrderId, Attempt, RunId), a: u64) -> bool {
        self.1.is_started(key, a)
    }
}
struct TracedHost(Trace, HostResult);
impl DispatchHost for TracedHost {
    type Result = HostResult;
    fn dispatch(&mut self, _: &ExecutionIntent) -> HostResult {
        self.0.lock().unwrap().push("host");
        self.1.clone()
    }
    fn dispatch_count(&self) -> u64 {
        1
    }
    fn recover_result(&mut self, _: &ExecutionIntent) -> Option<HostResult> {
        None
    }
}
struct TracedSink(Trace, Sink);
impl EmitSink for TracedSink {
    fn publish(&mut self, tuple: &ArtifactTuple) -> EmitReceipt {
        self.0.lock().unwrap().push("publish");
        self.1.publish(tuple)
    }
    fn receipt_of(&self, key: &ArtifactTupleKey) -> Option<EmitReceipt> {
        self.1.receipt_of(key)
    }
}
struct TracedLog(Trace, Log, Epoch);
impl EmitLog for TracedLog {
    fn current_epoch(&self) -> Epoch {
        self.2
    }
    // The runner must supply promotion authorization; this inner log has none.
    fn promotion_of(&self, _: &ArtifactTupleKey) -> Option<PromotionReceipt> {
        None
    }
    fn emit_of(&self, key: &ArtifactTupleKey) -> Option<EmitReceipt> {
        self.1.emit_of(key)
    }
    fn record_emit(&mut self, receipt: &EmitReceipt) {
        self.0.lock().unwrap().push("emit");
        self.1.record_emit(receipt);
    }
    fn mark_complete(&mut self, key: &ArtifactTupleKey) {
        self.0.lock().unwrap().push("complete");
        self.1.mark_complete(key);
    }
    fn is_complete(&self, key: &ArtifactTupleKey) -> bool {
        self.1.is_complete(key)
    }
}
struct TracedTuples {
    trace: Trace,
    inner: SqliteTupleStore,
    fail: &'static str,
}
impl TupleStore for TracedTuples {
    fn record(
        &self,
        tuple: &ArtifactTuple,
    ) -> Result<foundry_exec::Recorded, foundry_exec::PortError> {
        self.trace.lock().unwrap().push("record");
        if self.fail == "record" {
            return Err(foundry_exec::PortError::Store("record unavailable".into()));
        }
        self.inner.record(tuple)
    }
    fn latest(
        &self,
        order: &OrderId,
        attempt: Attempt,
    ) -> Result<Option<ArtifactTuple>, foundry_exec::PortError> {
        self.inner.latest(order, attempt)
    }
    fn latest_promoted(
        &self,
        order: &OrderId,
        attempt: Attempt,
    ) -> Result<Option<ArtifactTuple>, foundry_exec::PortError> {
        self.inner.latest_promoted(order, attempt)
    }
    fn promote(
        &self,
        key: &ArtifactTupleKey,
        receipt: &EmitReceipt,
    ) -> Result<(), foundry_exec::PortError> {
        self.trace.lock().unwrap().push("promote");
        if self.fail == "promote" {
            return Err(foundry_exec::PortError::Store("promote unavailable".into()));
        }
        self.inner.promote(key, receipt)
    }
}

struct FailOncePromotion {
    inner: SqliteTupleStore,
    fail: std::sync::atomic::AtomicBool,
}
impl TupleStore for FailOncePromotion {
    fn record(
        &self,
        tuple: &ArtifactTuple,
    ) -> Result<foundry_exec::Recorded, foundry_exec::PortError> {
        self.inner.record(tuple)
    }
    fn latest(
        &self,
        order: &OrderId,
        attempt: Attempt,
    ) -> Result<Option<ArtifactTuple>, foundry_exec::PortError> {
        self.inner.latest(order, attempt)
    }
    fn latest_promoted(
        &self,
        order: &OrderId,
        attempt: Attempt,
    ) -> Result<Option<ArtifactTuple>, foundry_exec::PortError> {
        self.inner.latest_promoted(order, attempt)
    }
    fn promote(
        &self,
        key: &ArtifactTupleKey,
        receipt: &EmitReceipt,
    ) -> Result<(), foundry_exec::PortError> {
        if self.fail.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Err(foundry_exec::PortError::Store(
                "injected promotion window".into(),
            ));
        }
        self.inner.promote(key, receipt)
    }
}

#[test]
fn promotion_window_is_not_consumable_and_retry_repairs_it_without_republish() {
    struct CountSink(Arc<Mutex<usize>>);
    impl EmitSink for CountSink {
        fn publish(&mut self, tuple: &ArtifactTuple) -> EmitReceipt {
            *self.0.lock().unwrap() += 1;
            EmitReceipt {
                key: tuple.key(),
                event: "publish".into(),
                at: chrono::Utc::now(),
            }
        }
        fn receipt_of(&self, _: &ArtifactTupleKey) -> Option<EmitReceipt> {
            None
        }
    }
    let db = Db::new();
    let inner = SqliteTupleStore::open(&db.0).unwrap();
    let tuples = Arc::new(FailOncePromotion {
        inner,
        fail: std::sync::atomic::AtomicBool::new(true),
    });
    let refs = Arc::new(Mutex::new(Vec::new()));
    let host = Host {
        result: HostResult {
            base_commit: "base".into(),
            result_commit: "result".into(),
            log_path: "review.log".into(),
            exit_code: 0,
            generations: Vec::new(),
            held: None,
        },
        refs: refs.clone(),
    };
    let publishes = Arc::new(Mutex::new(0));
    let mut run = DurableReviewRun::new(
        CountSink(publishes.clone()),
        Log::default(),
        Box::new(host),
        Box::new(Intents::default()),
        tuples.clone(),
    );
    assert!(matches!(
        run.run(request()),
        Err(ReviewRunError::Tuple(foundry_exec::PortError::Store(_)))
    ));
    assert!(
        tuples
            .latest(&request().order, request().attempt)
            .unwrap()
            .is_some()
    );
    assert!(
        tuples
            .latest_promoted(&request().order, request().attempt)
            .unwrap()
            .is_none()
    );
    let foundry_exec::ReviewRunOutcome::Completed(evidence) = run.run(request()).unwrap() else {
        panic!("expected completed review")
    };
    assert_eq!(evidence.verdict, foundry_exec::ReviewVerdict::Passed);
    assert_eq!(
        tuples
            .latest_promoted(&request().order, request().attempt)
            .unwrap()
            .unwrap()
            .immutable_commit
            .0,
        "result"
    );
    assert_eq!(*publishes.lock().unwrap(), 1);
    assert_eq!(refs.lock().unwrap().len(), 2);
}

#[test]
fn pipeline_stops_at_each_failure_and_reuses_emit_receipts_on_retry() {
    for failure in [
        "none", "reject", "base", "result", "stale", "record", "promote",
    ] {
        let db = Db::new();
        let trace = Trace::default();
        let tuples = Arc::new(TracedTuples {
            trace: trace.clone(),
            inner: SqliteTupleStore::open(&db.0).unwrap(),
            fail: failure,
        });
        let mut runner = DurableReviewRun::new(
            TracedSink(trace.clone(), Sink::default()),
            TracedLog(
                trace.clone(),
                Log::default(),
                if failure == "stale" {
                    Epoch(4)
                } else {
                    Epoch(0)
                },
            ),
            Box::new(TracedHost(
                trace.clone(),
                HostResult {
                    base_commit: if failure == "base" { "wrong" } else { "base" }.into(),
                    result_commit: if failure == "result" {
                        "wrong"
                    } else {
                        "result"
                    }
                    .into(),
                    log_path: "review.log".into(),
                    exit_code: if failure == "reject" { 1 } else { 0 },
                    generations: Vec::new(),
                    held: None,
                },
            )),
            Box::new(TracedIntents(trace.clone(), Intents::default())),
            tuples.clone(),
        );
        let result = runner.run(request());
        match failure {
            "none" => {
                let foundry_exec::ReviewRunOutcome::Completed(result) = result.unwrap() else {
                    panic!("expected completed review")
                };
                assert_eq!(result.emit_receipt.key.run, RunId(22));
            }
            "reject" => assert!(matches!(result, Err(ReviewRunError::Rejected { .. }))),
            "base" | "result" => assert_eq!(result, Err(ReviewRunError::BindingMismatch)),
            "stale" => assert_eq!(
                result,
                Err(ReviewRunError::Emit(
                    foundry_exec::EmitOutcome::StaleRejected
                ))
            ),
            _ => assert!(matches!(
                result,
                Err(ReviewRunError::Tuple(foundry_exec::PortError::Store(_)))
            )),
        }
        let expected: &[&str] = match failure {
            "reject" | "base" | "result" | "stale" => &["intent", "host"],
            "record" => &["intent", "host", "publish", "emit", "record"],
            "promote" => &["intent", "host", "publish", "emit", "record", "promote"],
            _ => &[
                "intent", "host", "publish", "emit", "record", "promote", "complete",
            ],
        };
        assert_eq!(trace.lock().unwrap().as_slice(), expected, "{failure}");
        let saved = tuples.latest(&request().order, request().attempt).unwrap();
        assert_eq!(saved.is_some(), failure == "none" || failure == "promote");
        if let Some(saved) = saved {
            assert_eq!(saved.immutable_commit.0, "result");
        }
        if matches!(failure, "none" | "record" | "promote") {
            // Even a failed tuple write must not publish the same key twice.
            let retry = runner.run(request());
            assert_eq!(retry.is_ok(), failure == "none");
            assert_eq!(
                trace
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|e| **e == "publish")
                    .count(),
                1
            );
        }
    }
}
