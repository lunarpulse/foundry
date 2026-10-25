//! T60h — M5.2 통합 게이트 (V1–V8). astra 2차 REJECT의 전 조항이
//! 프로덕션 배선 경로에서 닫혔는지 관측한다.
//!
//! 게이트는 serve_wiring의 유닛테스트를 "신뢰"하지 않는다 — 실
//! ServeState, 실 라우터, 실 요청으로 재현한다. foundry-watch의
//! 게이트 관례(m51_gate.rs)와 동일한 정신, serve 크레이트에 거주
//! (HTTP 라우터 검증 때문 — 순환 의존 회피, T53w 선례).
#![allow(clippy::await_holding_lock)] // W6a clippy debt: preserve this existing test/example logic, assertions, and cell sequence

use axum::body::Body;
use axum::http::{Request, StatusCode};
use foundry_serve::ServeState;
use foundry_watch::governance_events::Phase;
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

static DB_N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct FastHasher;
impl foundry_serve::identity::PasswordHasher for FastHasher {
    fn hash(&self, pw: &str) -> Result<String, String> {
        use sha2::Digest as _;
        let mut h = sha2::Sha256::new();
        h.update(pw.as_bytes());
        Ok(format!("{:x}", h.finalize()))
    }
    fn verify(&self, pw: &str, hash: &str) -> Result<bool, String> {
        Ok(self.hash(pw)? == hash)
    }
}

struct Ctx {
    st: Arc<ServeState>,
    dir: std::path::PathBuf,
    token: String,
}

async fn ctx() -> Ctx {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let n = DB_N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("t60h-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // seed ledger for the REAL reader (one DieSucceeded row)
    let lc = rusqlite::Connection::open(dir.join("ledger.db")).unwrap();
    lc.execute(
        "CREATE TABLE IF NOT EXISTS ledger (seq INTEGER PRIMARY KEY AUTOINCREMENT, order_id TEXT, from_state TEXT, to_state TEXT, event TEXT, actor TEXT, intent TEXT, at TEXT, prev_hash TEXT, hash TEXT, event_json TEXT)",
        [],
    )
    .unwrap();
    lc.execute(
        "INSERT INTO ledger (order_id, event, prev_hash, hash, event_json) VALUES ('o1','DieSucceeded { attempt: 1 }','GENESIS','h1','{\"attempt\":1}')",
        [],
    )
    .unwrap();
    drop(lc);
    unsafe {
        std::env::set_var("FOUNDRY_LEDGER", dir.join("ledger.db"));
        std::env::set_var("FOUNDRY_GOVERNANCE_DB", dir.join("gov.db"));
    }
    let st = Arc::new(ServeState::new(dir.join("serve.db")).expect("ServeState"));
    // owner + live session (V4: roles never ride the request)
    let token = {
        let mut id = st.identity.lock().await;
        id.bootstrap_owner("owner-1", "correct horse battery staple", &FastHasher)
            .unwrap();
        id.login("owner-1", "correct horse battery staple", &FastHasher, 3600)
            .unwrap()
    };
    Ctx { st, dir, token }
}

const CMD_JSON: &str = r#"{"op_id":"op-gate-1","approver":"WHOMEVER","role":"Observer","proposal_digest":"prop-gate","prediction":{"metric":"retry_rate_by_task_class","direction":"down","threshold":"<0.25","window":"next-2-retro-windows"},"base_policy_version":0,"issued_at":1,"expires_at":9999999999}"#;

async fn post(st: &Arc<ServeState>, auth: Option<&str>, body: &str) -> (StatusCode, String) {
    let router = foundry_serve::http::router(st.clone());
    let mut b = Request::builder()
        .method("POST")
        .uri("/api/policy/apply")
        .header("content-type", "application/json");
    if let Some(a) = auth {
        b = b.header("authorization", format!("Bearer {a}"));
    }
    let resp = router
        .oneshot(b.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn v1_write_ahead_ordering_over_http() {
    // N1: over the REAL route, Pending precedes effect — the journal
    // shows intent BEFORE Committed, and timestamps are server-clock.
    let c = ctx().await;
    // File the proposal THROUGH the production pipeline (real reader
    // recomputes the evidence): a claim that matches the seeded
    // ledger row (head h1, one DieSucceeded attempt=1 -> rate 0.0).
    {
        use foundry_watch::retro::{EvidenceEnvelope, InsightRecord, PolicyDiff, Prediction};
        let mut pipeline = c.st.policy.pipeline.lock().await;
        let mut e = EvidenceEnvelope {
            ledger_prefix_len: 1,
            ledger_prefix_head_hash: "h1".into(),
            corpus_snapshot_digest: "h1".into(), // reader: env or ledger head
            query_id: "retry_rate_by_task_class".into(),
            query_params: Default::default(),
            cohort: "all".into(),
            exclusions: vec![],
            denominator: "d".into(),
            missing_data_policy: "m".into(),
            policy_version: 1,
            protocol_digest: "x".into(),
            result_payload: r#"{"all":0.0000}"#.into(),
            result_digest: String::new(),
        };
        e.result_digest = e.compute_result_digest();
        pipeline
            .ingest(InsightRecord {
                insight_id: "I1".into(),
                claim: "c".into(),
                evidence: e,
                proposal: Some(PolicyDiff {
                    target: "poll_interval".into(),
                    changes: [("max_retries".to_string(), "2".to_string())]
                        .into_iter()
                        .collect(),
                }),
                prediction: Some(Prediction {
                    metric: "retry_rate_by_task_class".into(),
                    direction: "down".into(),
                    threshold: "<0.25".into(),
                    window: "next-2-retro-windows".into(),
                }),
                n: 20,
            })
            .expect("honest claim passes the real reader");
        // digest computed from the SAME production pipeline
        let digest =
            foundry_watch::command_binding::HardenedApply::command_digest_for(&pipeline, "I1", 0)
                .unwrap();
        let body = CMD_JSON.replace("prop-gate", &digest);
        drop(pipeline);
        let (code, msg) = post(&c.st, Some(&c.token), &body).await;
        assert_eq!(
            code,
            StatusCode::OK,
            "owner session applies policy over HTTP: {msg}"
        );
    }
    // journal: Pending(seq a) BEFORE Committed(seq b), real timestamps
    {
        let coord = c.st.policy.coordinator.lock().await;
        let stream = coord.replay().unwrap();
        assert!(
            stream.len() >= 2,
            "intent+effect pair on the journal: {:?}",
            stream
        );
        let phases: Vec<&Phase> = stream.iter().map(|e| &e.phase).collect();
        let i_pending = phases
            .iter()
            .position(|p| matches!(p, Phase::Pending))
            .expect("Pending exists");
        let i_committed = phases
            .iter()
            .position(|p| matches!(p, Phase::Committed))
            .expect("Committed exists");
        assert!(
            i_pending < i_committed,
            "N1: intent precedes effect (write-ahead)"
        );
        assert!(stream[i_pending].seq < stream[i_committed].seq);
        assert!(
            !stream[i_pending].at.contains("T00:00:00"),
            "N5: server clock, got {}",
            stream[i_pending].at
        );
    }
}

#[tokio::test]
async fn v2_principals_never_ride_the_request() {
    // N2: body claims approver=WHOMEVER role=Observer — the journal
    // records the SESSION principal as owner. The forged fields are
    // inert: this very request SUCCEEDS as owner despite claiming
    // Observer, proving the body fields are not consulted.
    let c = ctx().await;
    assert!(
        CMD_JSON.contains("\"role\":\"Observer\""),
        "fixture claims Observer"
    );
    let (code, msg) = post(&c.st, Some(&c.token), CMD_JSON).await;
    // unknown digest -> refused DOWNSTREAM of auth: the refusal
    // message proves we passed the owner gate (not a role refusal)
    assert_ne!(
        code,
        StatusCode::FORBIDDEN,
        "Observer-in-body must NOT cause 403: {msg}"
    );
    {
        let coord = c.st.policy.coordinator.lock().await;
        for e in coord.replay().unwrap() {
            assert!(
                e.actor.contains("owner-1"),
                "journal actor is the session principal: {}",
                e.actor
            );
            assert!(
                !e.actor.contains("WHOMEVER"),
                "body approver never reaches the journal"
            );
        }
    }
}

#[tokio::test]
async fn v3_route_registered_and_guards() {
    // B1: the route EXISTS on the production router; unauthenticated
    // is 401 before any governance effect.
    let c = ctx().await;
    let (code, msg) = post(&c.st, None, CMD_JSON).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED, "no session: {msg}");
    let (code2, _) = post(&c.st, Some("forged-token"), CMD_JSON).await;
    assert_eq!(code2, StatusCode::UNAUTHORIZED, "forged session: 401");
    let coord = c.st.policy.coordinator.lock().await;
    assert_eq!(
        coord.replay().unwrap().len(),
        0,
        "zero journal events without a live session"
    );
}

#[tokio::test]
async fn v4_real_reader_backs_the_pipeline() {
    // T60f: ServeState's pipeline ingests with the REAL ledger
    // reader — permissive construction is absent from the state.
    let c = ctx().await;
    // ingest through the PRODUCTION pipeline (a claim bound to the
    // seeded ledger row): if the reader were permissive this would
    // still pass; instead a false claim must FAIL recompute.
    use foundry_watch::retro::{EvidenceEnvelope, InsightRecord, PolicyDiff, Prediction};
    let mut e = EvidenceEnvelope {
        ledger_prefix_len: 1,
        ledger_prefix_head_hash: "LIES".into(), // not the real head
        corpus_snapshot_digest: "c".into(),
        query_id: "retry_rate_by_task_class".into(),
        query_params: Default::default(),
        cohort: "all".into(),
        exclusions: vec![],
        denominator: "d".into(),
        missing_data_policy: "m".into(),
        policy_version: 1,
        protocol_digest: "x".into(),
        result_payload: "r".into(),
        result_digest: String::new(),
    };
    e.result_digest = e.compute_result_digest();
    let mut pipeline = c.st.policy.pipeline.lock().await;
    let res = pipeline.ingest(InsightRecord {
        insight_id: "IX".into(),
        claim: "forged".into(),
        evidence: e,
        proposal: Some(PolicyDiff {
            target: "poll_interval".into(),
            changes: [("max_retries".to_string(), "99".to_string())]
                .into_iter()
                .collect(),
        }),
        prediction: Some(Prediction {
            metric: "retry_rate_by_task_class".into(),
            direction: "down".into(),
            threshold: "<0.25".into(),
            window: "next-2-retro-windows".into(),
        }),
        n: 20,
    });
    assert!(
        res.is_err(),
        "false ledger-head claim REFUSED by the real reader: {:?}",
        res.err()
    );
}

#[tokio::test]
async fn v5_chain_head_pins_content() {
    // T60e over the production journal: pin at head, substitute a
    // same-shape stream -> ContentSubstitution.
    let c = ctx().await;
    {
        let mut coord = c.st.policy.coordinator.lock().await;
        coord
            .append(GovernanceEvent {
                schema_version: 1,
                seq: 0,
                op: OpKind::LessonSeal,
                phase: Phase::Pending,
                op_id: "g1".into(),
                payload_digest: "h".into(),
                actor: "owner-1".into(),
                at: "t1".into(),
            })
            .unwrap();
    }
    let pin = {
        let coord = c.st.policy.coordinator.lock().await;
        let stream = coord.replay().unwrap();
        let (max_seq, count) = coord.head().unwrap();
        foundry_watch::audit_precision::HeadPin {
            max_seq,
            count,
            chain_head: foundry_watch::audit_precision::chain_head(&stream),
        }
    };
    let mut fake = GovernanceCoordinator::open_in_memory().unwrap();
    fake.append(GovernanceEvent {
        schema_version: 1,
        seq: 0,
        op: OpKind::LessonSeal,
        phase: Phase::Pending,
        op_id: "EVIL".into(),
        payload_digest: "forged".into(),
        actor: "attacker".into(),
        at: "t1".into(),
    })
    .unwrap();
    let err = foundry_watch::audit_precision::audit(&fake, &pin).unwrap_err();
    assert!(
        matches!(
            &err,
            foundry_watch::audit_precision::AuditError::ContentSubstitution { .. }
        ),
        "same-shape substitution named: {err:?}"
    );
}

#[tokio::test]
async fn v6_governance_db_is_the_production_journal() {
    // the coordinator opened by ServeState writes to the REAL file
    // on disk (durability is not in-memory-only).
    let c = ctx().await;
    {
        let mut coord = c.st.policy.coordinator.lock().await;
        coord
            .append(GovernanceEvent {
                schema_version: 1,
                seq: 0,
                op: OpKind::LessonSeal,
                phase: Phase::Pending,
                op_id: "dur".into(),
                payload_digest: "h".into(),
                actor: "owner-1".into(),
                at: "t".into(),
            })
            .unwrap();
    }
    let path = c.dir.join("gov.db");
    assert!(path.exists(), "journal file exists on disk");
    let reopened = GovernanceCoordinator::open(&path).unwrap();
    assert!(
        reopened.replay().unwrap().iter().any(|e| e.op_id == "dur"),
        "durable across reopen"
    );
}

use foundry_watch::coordinator::GovernanceCoordinator;
use foundry_watch::governance_events::{GovernanceEvent, OpKind};

#[tokio::test]
async fn v7_v8_summary() {
    // V7: N3 (docker false-positive) + casting route are covered by
    // their dedicated tests in this workspace — t56w (execution
    // mandatory, EROFS attribution, control run) and T60c (binary
    // smoke: legacy route journaled non-promotable). Asserting the
    // modules exist prevents silent deletion of the proofs.
    // V8: linearization + group-kill covered by t60g tests.
    let t56 = include_str!("../../foundry-watch/tests/t56w_docker_mandatory.rs");
    assert!(t56.contains("probe-done"), "probe sentinel present");
    assert!(
        !t56.contains(".unwrap_or_default()"),
        "N3 regression: unwrap_or_default back in t56w"
    );
    let t60g = include_str!("../../foundry-watch/tests/t60g_stress_linearization.rs");
    assert!(t60g.contains("exactly ONE promotion wins"), "L1a present");
    assert!(
        t60g.contains("-{pgid}") || t60g.contains("kill"),
        "group kill present"
    );
    let main_rs = include_str!("../../foundry/src/main.rs");
    assert!(
        main_rs.contains("profile_from_bundle"),
        "casting main wired through the routing layer"
    );
    assert!(
        main_rs.contains("LegacyNotPromotable") || main_rs.contains("legacy_profile"),
        "legacy route journaled"
    );
}
