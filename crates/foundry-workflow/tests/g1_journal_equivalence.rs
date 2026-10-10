//! G1 — 저널 바이트 동등성 (T51b의 첫 관문).
//!
//! M6 계약의 조작적 정의: "모든 M1-M5 주문을 내장 워크플로로 재생 →
//! 저널 바이트열 동일". 본 테스트가 그 실행판이다:
//!
//!   경로 A (직접 — M1-M5의 실제 프로덕션 경로):
//!     Order::transition 승인 → SqliteLedger.append (해시체인 v2)
//!   경로 B (엔진 — 워크플로 축):
//!     WorkflowEngine.dispatch (입장허가=엣지, 권위=transition 위임)
//!     승인 → 동일 SqliteLedger.append
//!
//! 두 경로 모두 승인 판정은 `Order::transition` 하나이고(C-W4),
//! 저널 레코드는 같은 append 파라미터로 산출되므로
//! **저널 파일 전체가 바이트 동일**해야 한다 (seq·event·actor·
//! intent·at·prev_hash·hash·event_json 전 컬럼).
//!
//! 결정성 확보: `at`는 고정 시각(엔진이 시각을 만들지 않는다 —
//! dispatch는 저널 파라미터에서 시각을 받지 않고, 심는 쪽이 준다).

use foundry_domain::order::{ExternalRef, Order, OrderEvent};
use foundry_domain::ports::{ActorId, ActorKind, Ledger};
use foundry_ledger::sqlite::SqliteLedger;
use foundry_workflow::WorkflowEngine;

fn actor() -> ActorId {
    ActorId { kind: ActorKind::Agent, source: "g1".into(), id: "agent-1".into() }
}

fn tmp_path(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("g1-journal-{tag}-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

/// M1-M5 대표 시나리오 전부 — happy path·재시도 back-edge·no-op 종단·
/// 승인 홀드·발행·취소·거부(stale). 전이 승인쌍 (event, from→to) 산출.
fn scenarios() -> Vec<(&'static str, Vec<OrderEvent>)> {
    vec![
        (
            "happy-full",
            vec![
                OrderEvent::Queued,
                OrderEvent::DieStarted,
                OrderEvent::DieSucceeded { attempt: 1 },
                OrderEvent::InspectionPassed,
                OrderEvent::Approved { intent: "ship".into(), artifact_digest: "d1".into() },
                OrderEvent::PublishConfirmed { pr_url: "https://pr/1".into() },
            ],
        ),
        (
            "retry-then-fail",
            vec![
                OrderEvent::Queued,
                OrderEvent::DieStarted,
                OrderEvent::DieFailed { attempt: 1, reason: "transient".into() },
                OrderEvent::DieStarted, // attempt 2
                OrderEvent::DieFailed { attempt: 2, reason: "again".into() },
                OrderEvent::DieStarted, // attempt 3
                OrderEvent::DieFailed { attempt: 3, reason: "last".into() }, // → Failed
            ],
        ),
        (
            "noop-terminal",
            vec![
                OrderEvent::Queued,
                OrderEvent::DieStarted,
                OrderEvent::DieSucceeded { attempt: 1 },
                OrderEvent::InspectionFailed { reason: "no-op: empty diff".into() },
            ],
        ),
        (
            "approve-reject",
            vec![
                OrderEvent::Queued,
                OrderEvent::DieStarted,
                OrderEvent::DieSucceeded { attempt: 1 },
                OrderEvent::InspectionPassed,
                OrderEvent::Rejected { intent: "not needed".into() },
            ],
        ),
        (
            "cancel-early",
            vec![OrderEvent::Queued, OrderEvent::Canceled { reason: "user".into() }],
        ),
        (
            "unknown-then-confirm",
            vec![
                OrderEvent::Queued,
                OrderEvent::DieStarted,
                OrderEvent::DieSucceeded { attempt: 1 },
                OrderEvent::InspectionPassed,
                OrderEvent::Approved { intent: "ship".into(), artifact_digest: "d2".into() },
                OrderEvent::PublishUnknown { operation_id: "op-9".into() },
                OrderEvent::PublishConfirmed { pr_url: "https://pr/2".into() },
            ],
        ),
        (
            "stale-refused-then-ok",
            vec![
                OrderEvent::Queued,
                OrderEvent::DieStarted,
                // stale — 양쪽 경로 모두 거부, 저널에 안 심음
                OrderEvent::DieSucceeded { attempt: 0 },
                OrderEvent::DieSucceeded { attempt: 1 },
                OrderEvent::InspectionPassed,
            ],
        ),
    ]
}

#[test]
fn g1_journal_bytes_identical_direct_vs_engine() {
    let actor = actor();
    let t0 = chrono::Utc::now();

    // 경로 A: 직접 (M1-M5 프로덕션 경로)
    let path_a = tmp_path("direct");
    let mut ledger_a = SqliteLedger::open(&path_a).unwrap();
    let mut clock = 0i64;
    let mut journaled_a: Vec<(String, OrderEvent)> = Vec::new();
    for (tag, events) in scenarios() {
        let mut o = Order::new(format!("o-{tag}"), ExternalRef::cron("a", "b"), t0);
        for ev in &events {
            let from = o.state();
            match o.transition(ev.clone(), attempt_for(ev, &o)) {
                Ok(to) => {
                    ledger_a
                        .append(&o.id, ev, from, to, &actor, at(&mut clock))
                        .unwrap();
                    journaled_a.push((o.id.clone(), ev.clone()));
                }
                Err(_) => { /* refused — not journaled (both paths) */ }
            }
        }
    }
    drop(ledger_a);

    // 경로 B (재작, astra MAJOR): 엔진 경유 — 로컬 Order 폐지.
    // 엔진 내부의 order에서 상태를 직접 읽어 심는다: 저널 B의 내용이
    // 엔진의 실제 산출물임이 이제 구조적으로 보장된다. 시도 주장도
    // 엔진 내부 attempt에서 계산한다.
    let path_b = tmp_path("engine");
    let mut ledger_b = SqliteLedger::open(&path_b).unwrap();
    let mut clock_b = 0i64;
    for (tag, events) in scenarios() {
        let oid = format!("o-{tag}");
        let mut engine = WorkflowEngine::embedded_default();
        engine.enroll(Order::new(oid.clone(), ExternalRef::cron("a", "b"), t0));
        engine.acquire(&oid, "exec-g1").unwrap();
        for ev in &events {
            // from/시도 주장을 모두 엔진 내부 상태에서
            let from = engine.order(&oid).unwrap().state();
            let cur_attempt = engine.order(&oid).unwrap().attempt();
            let claim = match ev {
                OrderEvent::DieStarted => cur_attempt.max(1),
                _ => cur_attempt.max(1),
            };
            match engine.dispatch(&oid, "exec-g1", ev, claim) {
                Ok(foundry_workflow::Outcome::Applied) => {
                    let to = engine.order(&oid).unwrap().state();
                    ledger_b
                        .append(&oid, ev, from, to, &actor, at(&mut clock_b))
                        .unwrap();
                }
                _ => { /* refused/NoEdge — not journaled (both paths) */ }
            }
        }
        // 엔진 내부 상태는 시나리오 종료 상태와 일치해야 한다 (직접 경로와
        // 같은 최종 상태 — 미비교면 "엔진이 다른 곳에 있었어도 통과").
        let final_engine = engine.order(&oid).unwrap().state();
        let mut direct = Order::new(oid.clone(), ExternalRef::cron("a", "b"), t0);
        for ev in &events {
            let claim = match ev {
                OrderEvent::DieStarted => direct.attempt().max(1),
                _ => direct.attempt().max(1),
            };
            let _ = direct.transition(ev.clone(), claim);
        }
        assert_eq!(
            final_engine, direct.state(),
            "engine internal state must equal direct replay state for {oid}"
        );
    }
    drop(ledger_b);

    let bytes_a = std::fs::read(&path_a).unwrap();
    let bytes_b = std::fs::read(&path_b).unwrap();
    assert_eq!(
        bytes_a.len(),
        bytes_b.len(),
        "journal files differ in size: direct {} vs engine {}",
        bytes_a.len(),
        bytes_b.len()
    );
    // 바이트 동일성 — sha256으로도 단정 (full compare는 위 size assert와 함께)
    use sha2::Digest as _;
    let ha = format!("{:x}", sha2::Sha256::digest(&bytes_a));
    let hb = format!("{:x}", sha2::Sha256::digest(&bytes_b));
    assert_eq!(ha, hb, "G1 FAILED: journal bytes differ (direct {ha} vs engine {hb})");
    // 그리고 저널은 비어있지 않다 (진짜 재생이 일어났다)
    let rows = SqliteLedger::open(&path_a).unwrap().read_all().unwrap();
    assert!(rows.len() >= 30, "expected a substantial journal, got {}", rows.len());
    let _ = journaled_a;
}

/// 이벤트별 attempt 주장: 현재 attempt.max(1) (첫 실행 1, 재시도는
/// DieFailed가 이미 증가시킨 값 재주장).
fn attempt_for(ev: &OrderEvent, o: &Order) -> u32 {
    let _ = ev;
    o.attempt().max(1)
}

fn at(clock: &mut i64) -> chrono::DateTime<chrono::Utc> {
    *clock += 1;
    chrono::DateTime::from_timestamp(1_700_000_000 + *clock, 0).unwrap()
}
