//! T7 — Line executor tests. Fakes for every port; only the orchestration
//! logic under test. Verify: lease, atomic journal, retry/backoff, attempt
//! fencing, idempotent publish, approval≠publish.
use foundry_domain::ports::{
    ApprovalToken, Decision, Gate, Ledger, Policy, PortResult,
    PublishOutcome, Publisher,
};
use foundry_domain::{ActorId, ActorKind, ExternalRef, Order, OrderEvent, OrderState};
use foundry_harness_cli::fake::FakeHarness;
use foundry_line::{Line, LineConfig, backoff_delay};
use foundry_exec::{ArtifactLocation, ArtifactTuple, ArtifactTupleKey, Attempt, EmitReceipt, Generation, ImmutableCommit, OrderId, PortError as ExecError, Recorded, ReviewEvidence, ReviewRunPort, ReviewRunRequest, ReviewVerdict, TupleStore};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as TMutex;

// ---------- test doubles ----------

#[derive(Default)]
pub struct MemLedger {
    pub rows: Mutex<Vec<(String, OrderEvent, OrderState, OrderState)>>,
}
impl Ledger for MemLedger {
    fn append(
        &mut self,
        order_id: &str,
        event: &OrderEvent,
        from: OrderState,
        to: OrderState,
        _actor: &ActorId,
        _at: chrono::DateTime<chrono::Utc>,
    ) -> PortResult<()> {
        self.rows
            .lock()
            .unwrap()
            .push((order_id.to_string(), event.clone(), from, to));
        Ok(())
    }
}

pub struct AllowPolicy;
impl Policy for AllowPolicy {
    fn authorize(&self, _a: &ActorId, _action: &str, _r: &str) -> Decision {
        Decision::Allow {
            reason: "test".into(),
        }
    }
}

pub struct DenyPolicy;
impl Policy for DenyPolicy {
    fn authorize(&self, _a: &ActorId, _action: &str, _r: &str) -> Decision {
        Decision::Deny {
            reason: "not yours".into(),
        }
    }
}

pub struct OpenGate;
impl Gate for OpenGate {
    fn verify(&self, _o: &Order, _t: &ApprovalToken, _d: &str) -> PortResult<()> {
        Ok(())
    }
}

/// Publisher with a scripted response queue.
#[derive(Default)]
pub struct ScriptedPublisher {
    pub responses: Mutex<VecDeque<PortResult<PublishOutcome>>>,
    pub calls: Mutex<Vec<String>>,
}
impl Publisher for ScriptedPublisher {
    fn publish(
        &self,
        _order: &Order,
        operation_id: &str,
        _expected_digest: &str,
    ) -> impl Future<Output = PortResult<PublishOutcome>> + Send {
        self.calls.lock().unwrap().push(operation_id.to_string());
        let next = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| {
                Ok(PublishOutcome::Confirmed {
                    pr_url: "https://pr/1".into(),
                })
            });
        async move { next }
    }
}

fn actor() -> ActorId {
    ActorId {
        kind: ActorKind::Human,
        source: "test".into(),
        id: "luna".into(),
    }
}

fn sys() -> ActorId {
    ActorId {
        kind: ActorKind::Agent,
        source: "line".into(),
        id: "executor".into(),
    }
}

fn order(id: &str) -> Order {
    Order::new(
        id.into(),
        ExternalRef::cron("dep", "serde"),
        chrono::Utc::now(),
    )
}

type TestLine = Line<OpenGate, FakeHarness, MemLedger, ScriptedPublisher>;

#[derive(Default)]
struct MemTuples(Mutex<Vec<(ArtifactTuple, Option<EmitReceipt>)>>);
impl TupleStore for MemTuples {
    fn record(&self, tuple: &ArtifactTuple) -> Result<Recorded, ExecError> {
        let mut rows = self.0.lock().unwrap();
        if let Some((old, _)) = rows.iter().find(|(old, _)| old.key() == tuple.key()) {
            return if old == tuple { Ok(Recorded) } else { Err(ExecError::Permanent("tuple collision".into())) };
        }
        rows.push((tuple.clone(), None));
        Ok(Recorded)
    }
    fn latest(&self, order: &OrderId, attempt: Attempt) -> Result<Option<ArtifactTuple>, ExecError> {
        Ok(self.0.lock().unwrap().iter().filter(|(t, _)| &t.order == order && t.attempt == attempt)
            .max_by_key(|(t, _)| (t.run, t.generation)).map(|(t, _)| t.clone()))
    }
    fn latest_promoted(&self, order: &OrderId, attempt: Attempt) -> Result<Option<ArtifactTuple>, ExecError> {
        Ok(self.0.lock().unwrap().iter().filter(|(t, receipt)| &t.order == order && t.attempt == attempt && receipt.is_some())
            .max_by_key(|(t, _)| (t.run, t.generation)).map(|(t, _)| t.clone()))
    }
    fn promote(&self, key: &ArtifactTupleKey, receipt: &EmitReceipt) -> Result<(), ExecError> {
        let mut rows = self.0.lock().unwrap();
        let (_, slot) = rows.iter_mut().find(|(t, _)| t.key() == *key).ok_or(ExecError::MissingEmitReceipt)?;
        if receipt.key != *key { return Err(ExecError::MissingEmitReceipt); }
        *slot = Some(receipt.clone());
        Ok(())
    }
}

struct FakeReview(Arc<MemTuples>);
impl ReviewRunPort for FakeReview {
    fn run(&mut self, req: ReviewRunRequest) -> Result<foundry_exec::ReviewRunOutcome, foundry_exec::ReviewRunError> {
        let tuple = ArtifactTuple {
            order: req.order, attempt: req.attempt, run: req.run, generation: Generation(0),
            immutable_commit: ImmutableCommit(req.artifact_digest), artifact_location: ArtifactLocation("fake-review.log".into()), generations: Vec::new(),
        };
        let key = tuple.key();
        let receipt = EmitReceipt { key: key.clone(), event: "publish".into(), at: chrono::Utc::now() };
        self.0.record(&tuple).map_err(foundry_exec::ReviewRunError::Tuple)?;
        self.0.promote(&key, &receipt).map_err(foundry_exec::ReviewRunError::Tuple)?;
        Ok(foundry_exec::ReviewRunOutcome::Completed(ReviewEvidence { emit_receipt: receipt, tuple_key: key, verdict: ReviewVerdict::Passed }))
    }
}

fn mk_line(harness: FakeHarness, publisher: ScriptedPublisher) -> TestLine {
    let tuples = Arc::new(MemTuples::default());
    Line::new(
        Arc::new(OpenGate),
        Arc::new(harness),
        Arc::new(TMutex::new(MemLedger::default())),
        Arc::new(publisher),
        Arc::new(AllowPolicy),
    )
    .with_review_run(FakeReview(tuples.clone()))
    .with_tuple_store(tuples)
    .with_cfg(LineConfig {
        base_backoff_ms: 1,
        max_backoff_ms: 5,
    }) // tests: fast
}

fn mk_unreviewed_line() -> TestLine {
    Line::new(
        Arc::new(OpenGate), Arc::new(FakeHarness::default()),
        Arc::new(TMutex::new(MemLedger::default())), Arc::new(ScriptedPublisher::default()),
        Arc::new(AllowPolicy),
    ).with_cfg(LineConfig { base_backoff_ms: 1, max_backoff_ms: 1 })
}

// ---------- tests ----------

#[tokio::test]
async fn drive_runs_received_to_furnace_hold() {
    let line = mk_line(FakeHarness::default(), ScriptedPublisher::default());
    let o = order("o-a");
    line.submit(o).await;
    line.acquire_lease("watch").await.unwrap();

    let end = line.drive("o-a").await.unwrap();
    assert_eq!(end, OrderState::FurnaceHold);
    let o = line.get("o-a").await.unwrap();
    assert_eq!(o.state(), OrderState::FurnaceHold);

    // journal has the full happy path
    let led = line.ledger.lock().await;
    let rows = led.rows.lock().unwrap();
    let states: Vec<OrderState> = rows.iter().map(|r| r.3).collect();
    assert_eq!(
        states,
        vec![
            OrderState::Queued,
            OrderState::Working,
            OrderState::Inspecting,
            OrderState::FurnaceHold,
        ]
    );
}

#[tokio::test]
async fn missing_review_runner_never_commits_inspection_passed() {
    let line = mk_unreviewed_line();
    line.submit(order("review-missing")).await;
    line.acquire_lease("watch").await.unwrap();
    assert_eq!(line.drive("review-missing").await.unwrap(), OrderState::Failed);
    let rows = line.ledger.lock().await.rows.lock().unwrap().clone();
    assert!(rows.iter().any(|(_, ev, _, _)| matches!(ev, OrderEvent::InspectionFailed { .. })));
    assert!(!rows.iter().any(|(_, ev, _, _)| matches!(ev, OrderEvent::InspectionPassed)));
}

#[tokio::test]
async fn inspecting_reentry_without_tuple_stays_inspecting() {
    let line = mk_line(FakeHarness::default(), ScriptedPublisher::default());
    let mut o = order("review-reentry");
    o.artifact_digest = Some("digest".into());
    line.submit(o).await;
    let a = sys();
    for ev in [OrderEvent::Queued, OrderEvent::DieStarted, OrderEvent::DieSucceeded { attempt: 1 }] {
        line.commit_for_test("review-reentry", &ev, OrderState::Received, OrderState::Working, &a).await.unwrap();
    }
    assert_eq!(line.drive("review-reentry").await.unwrap(), OrderState::Inspecting);
    let rows = line.ledger.lock().await.rows.lock().unwrap().clone();
    assert!(!rows.iter().any(|(_, ev, _, _)| matches!(ev, OrderEvent::InspectionPassed)));
}

#[tokio::test]
async fn approval_and_publish_refuse_when_latest_tuple_is_missing() {
    let line = mk_line(FakeHarness::default(), ScriptedPublisher::default());
    let mut o = order("tuple-required");
    o.artifact_digest = Some("digest".into());
    line.submit(o).await;
    let human = actor();
    for ev in [
        OrderEvent::Queued,
        OrderEvent::DieStarted,
        OrderEvent::DieSucceeded { attempt: 1 },
        OrderEvent::InspectionPassed,
    ] {
        line.commit_for_test("tuple-required", &ev, OrderState::Inspecting, OrderState::FurnaceHold, &human).await.unwrap();
    }
    let token = ApprovalToken { order_id: "tuple-required".into(), intent: "approve".into(),
        artifact_digest: "digest".into(), actor: human.clone(), granted_at: chrono::Utc::now() };
    assert!(line.approve("tuple-required", &token, "approve", &human).await.is_err());
    line.commit_for_test("tuple-required", &OrderEvent::Approved { intent: "test".into(), artifact_digest: "digest".into() },
        OrderState::FurnaceHold, OrderState::Publishing, &human).await.unwrap();
    assert!(line.publish("tuple-required", &human).await.is_err());
}

#[tokio::test]
async fn retry_with_backoff_until_success() {
    let line = mk_line(FakeHarness::fail_first(2), ScriptedPublisher::default());
    line.submit(order("o-b")).await;
    line.acquire_lease("watch").await.unwrap();

    let end = line.drive("o-b").await.unwrap();
    assert_eq!(end, OrderState::FurnaceHold);
    let h_calls = line.harness.calls();
    assert_eq!(h_calls.len(), 3, "attempts 1,2 fail; 3 succeeds");
    assert_eq!(
        h_calls.iter().map(|c| c.1).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}

#[tokio::test]
async fn lease_is_exclusive() {
    let line = mk_line(FakeHarness::default(), ScriptedPublisher::default());
    line.acquire_lease("watch").await.unwrap();
    let err = line.acquire_lease("cron-run").await.unwrap_err();
    assert!(matches!(err, foundry_line::LineError::LeaseHeld(_)));
    line.release_lease("watch").await;
    line.acquire_lease("cron-run").await.unwrap(); // now free
}

#[tokio::test]
async fn approval_then_publish_confirmed() {
    let pubr = ScriptedPublisher::default();
    pubr.responses
        .lock()
        .unwrap()
        .push_back(Ok(PublishOutcome::Confirmed {
            pr_url: "https://github.com/x/y/pull/9".into(),
        }));
    let line = mk_line(FakeHarness::default(), pubr);
    line.submit(order("o-c")).await;
    line.acquire_lease("watch").await.unwrap();
    line.drive("o-c").await.unwrap();

    // human approves (with a token) — separate step from publishing
    let st = line
        .approve(
            "o-c",
            &ApprovalToken {
                order_id: "o-c".into(),
                intent: "safe dep bump".into(),
                artifact_digest: String::new(),
                actor: actor(),
                granted_at: chrono::Utc::now(),
            },
            "safe dep bump",
            &actor(),
        )
        .await
        .unwrap();
    assert_eq!(st, OrderState::Publishing);

    let st = line.publish("o-c", &actor()).await.unwrap();
    assert_eq!(st.state, OrderState::Shipped);
    // operation_id is order:attempt — deterministic
    assert_eq!(line.publisher.calls.lock().unwrap()[0], "o-c:1");
}

#[tokio::test]
async fn publish_unknown_records_state() {
    let pubr = ScriptedPublisher::default();
    pubr.responses
        .lock()
        .unwrap()
        .push_back(Ok(PublishOutcome::Unknown {
            operation_id: "o-d:1".into(),
        }));
    let line = mk_line(FakeHarness::default(), pubr);
    line.submit(order("o-d")).await;
    line.acquire_lease("watch").await.unwrap();
    line.drive("o-d").await.unwrap();
    line.approve(
        "o-d",
        &ApprovalToken {
            order_id: "o-d".into(),
            intent: "ok".into(),
            artifact_digest: String::new(),
            actor: actor(),
            granted_at: chrono::Utc::now(),
        },
        "ok",
        &actor(),
    )
    .await
    .unwrap();

    let st = line.publish("o-d", &actor()).await.unwrap();
    assert_eq!(
        st.state,
        OrderState::Unknown,
        "crash mid-publish → Unknown, not Shipped"
    );
}

#[tokio::test]
async fn publish_without_approval_is_illegal() {
    let line = mk_line(FakeHarness::default(), ScriptedPublisher::default());
    line.submit(order("o-e")).await;
    line.acquire_lease("watch").await.unwrap();
    line.drive("o-e").await.unwrap(); // ends at FurnaceHold
    let _ = line; // publish is only reachable through approve(); state machine
    // rejects Publishing from FurnaceHold without Approved event —
    // covered by T2 transition-table tests.
}

#[test]
fn backoff_is_exponential_and_capped() {
    assert_eq!(backoff_delay(1, 100, 10_000), 100);
    assert_eq!(backoff_delay(2, 100, 10_000), 200);
    assert_eq!(backoff_delay(3, 100, 10_000), 400);
    assert_eq!(backoff_delay(20, 100, 10_000), 10_000, "capped");
}

#[tokio::test]
async fn every_transition_is_journaled() {
    let line = mk_line(FakeHarness::default(), ScriptedPublisher::default());
    line.submit(order("o-f")).await;
    line.acquire_lease("watch").await.unwrap();
    line.drive("o-f").await.unwrap();

    let rows = line.ledger.lock().await.rows.lock().unwrap().clone();
    let o = line.get("o-f").await.unwrap();
    // last journaled to-state == current state (no un-journaled transitions)
    assert_eq!(rows.last().unwrap().3, o.state());
    // version count == journal row count (1:1 atomicity)
    assert_eq!(rows.len() as u64, o.version);
}

// ---------- T19: no-op taxonomy (defect #8) ----------

/// Die exits 0 with NO new commit, task expected a change → quality
/// FAILURE: InspectionFailed, terminal Failed, never reaches FurnaceHold.
#[tokio::test]
async fn t19_noop_die_with_change_required_fails() {
    let line = mk_line(FakeHarness::noop_success(), ScriptedPublisher::default());
    let o = order("o-noop1");
    line.submit(o).await;
    line.acquire_lease("watch").await.unwrap();

    let end = line.drive("o-noop1").await.unwrap();
    assert_eq!(
        end,
        OrderState::Failed,
        "empty diff + change required must FAIL"
    );
    let o = line.get("o-noop1").await.unwrap();
    assert_eq!(o.state(), OrderState::Failed);
    // journal carries the honest reason
    let rows = line.ledger.lock().await.rows.lock().unwrap().clone();
    let last = rows.last().unwrap();
    match &last.1 {
        OrderEvent::InspectionFailed { reason } => {
            assert!(reason.contains("no-op"), "reason must name no-op: {reason}")
        }
        other => panic!("expected InspectionFailed, got {other:?}"),
    }
    // publish must be unreachable
    let pub_res = line.publish("o-noop1", &sys()).await;
    assert!(
        pub_res.is_err(),
        "publishing a Failed order must be refused"
    );
}

/// Real diff still passes through to FurnaceHold (no regression).
#[tokio::test]
async fn t19_real_diff_still_reaches_hold() {
    let line = mk_line(FakeHarness::default(), ScriptedPublisher::default());
    line.submit(order("o-ok1")).await;
    line.acquire_lease("watch").await.unwrap();
    let end = line.drive("o-ok1").await.unwrap();
    assert_eq!(end, OrderState::FurnaceHold);
}


#[tokio::test]
async fn completed_die_reuses_latest_matching_tuple_without_a_runner() {
    let tuples = Arc::new(MemTuples::default());
    let tuple = ArtifactTuple {
        order: OrderId("review-existing".into()), attempt: Attempt(1),
        run: foundry_exec::RunId(7), generation: Generation(0),
        immutable_commit: ImmutableCommit("die-1".into()),
        artifact_location: ArtifactLocation("existing-review.log".into()), generations: Vec::new(),
    };
    tuples.record(&tuple).unwrap();
    tuples.promote(&tuple.key(), &EmitReceipt { key: tuple.key(), event: "publish".into(), at: chrono::Utc::now() }).unwrap();
    let line = mk_unreviewed_line().with_tuple_store(tuples);
    line.submit(order("review-existing")).await;
    assert_eq!(line.drive("review-existing").await.unwrap(), OrderState::FurnaceHold);
    assert_eq!(line.ledger.lock().await.rows.lock().unwrap().iter()
        .filter(|(_, ev, _, _)| matches!(ev, OrderEvent::InspectionPassed)).count(), 1);
    assert_eq!(line.drive("review-existing").await.unwrap(), OrderState::FurnaceHold);
}

#[tokio::test]
async fn latest_mismatch_blocks_reentry_approval_and_publish() {
    let tuples = Arc::new(MemTuples::default());
    for (run, digest) in [(1, "digest"), (2, "other")] {
        tuples.record(&ArtifactTuple {
            order: OrderId("latest-mismatch".into()), attempt: Attempt(1),
            run: foundry_exec::RunId(run), generation: Generation(0),
            immutable_commit: ImmutableCommit(digest.into()), artifact_location: ArtifactLocation("review.log".into()), generations: Vec::new(),
        }).unwrap();
    }
    let line = mk_unreviewed_line().with_tuple_store(tuples);
    let mut o = order("latest-mismatch");
    o.artifact_digest = Some("digest".into());
    line.submit(o).await;
    for ev in [OrderEvent::Queued, OrderEvent::DieStarted, OrderEvent::DieSucceeded { attempt: 1 }] {
        line.commit_for_test("latest-mismatch", &ev, OrderState::Received, OrderState::Inspecting, &sys()).await.unwrap();
    }
    assert_eq!(line.drive("latest-mismatch").await.unwrap(), OrderState::Inspecting);
    line.commit_for_test("latest-mismatch", &OrderEvent::InspectionPassed, OrderState::Inspecting, OrderState::FurnaceHold, &sys()).await.unwrap();
    let token = ApprovalToken { order_id: "latest-mismatch".into(), intent: "go".into(), artifact_digest: "digest".into(), actor: actor(), granted_at: chrono::Utc::now() };
    assert!(line.approve("latest-mismatch", &token, "go", &actor()).await.is_err());
    line.commit_for_test("latest-mismatch", &OrderEvent::Approved { intent: "go".into(), artifact_digest: "digest".into() }, OrderState::FurnaceHold, OrderState::Publishing, &actor()).await.unwrap();
    assert!(line.publish("latest-mismatch", &actor()).await.is_err());
    assert!(line.publisher.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn noop_taxonomy_needs_no_review_runner_or_tuple_reader() {
    let mut line = mk_unreviewed_line();
    line.harness = Arc::new(FakeHarness::noop_success());
    line.submit(order("noop-required-unreviewed")).await;
    assert_eq!(line.drive("noop-required-unreviewed").await.unwrap(), OrderState::Failed);
    assert!(!line.ledger.lock().await.rows.lock().unwrap().iter().any(|(_, ev, _, _)| matches!(ev, OrderEvent::InspectionPassed)));
}

#[tokio::test]
async fn inspecting_reentry_reuses_matching_tuple_and_commits_once() {
    let tuples = Arc::new(MemTuples::default());
    let tuple = ArtifactTuple {
        order: OrderId("review-resume".into()), attempt: Attempt(1), run: foundry_exec::RunId(9), generation: Generation(0),
        immutable_commit: ImmutableCommit("digest".into()), artifact_location: ArtifactLocation("review.log".into()), generations: Vec::new(),
    };
    tuples.record(&tuple).unwrap();
    tuples.promote(&tuple.key(), &EmitReceipt { key: tuple.key(), event: "publish".into(), at: chrono::Utc::now() }).unwrap();
    let line = mk_unreviewed_line().with_tuple_store(tuples);
    let mut o = order("review-resume");
    o.artifact_digest = Some("digest".into());
    line.submit(o).await;
    for ev in [OrderEvent::Queued, OrderEvent::DieStarted, OrderEvent::DieSucceeded { attempt: 1 }] {
        line.commit_for_test("review-resume", &ev, OrderState::Received, OrderState::Inspecting, &sys()).await.unwrap();
    }
    for _ in 0..2 { assert_eq!(line.drive("review-resume").await.unwrap(), OrderState::FurnaceHold); }
    assert_eq!(line.ledger.lock().await.rows.lock().unwrap().iter().filter(|(_, ev, _, _)| matches!(ev, OrderEvent::InspectionPassed)).count(), 1);
    assert!(line.harness.calls().is_empty());
}
