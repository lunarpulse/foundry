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

fn make_actor() -> ActorId {
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
    let actor_id = make_actor();
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
                        .append(&o.id, ev, from, to, &actor_id, at(&mut clock))
                        .unwrap();
                    journaled_a.push((o.id.clone(), ev.clone()));
                }
                Err(_) => { /* refused — not journaled (both paths) */ }
            }
        }
    }
    drop(ledger_a);

    // 경로 B (T56 P1b 마이그레이션): 엔진이 **자체 저널**에 append 한다
    // (P0 배선 — 관측이 아니라 Ledger::append). 무커서 공개 dispatch는
    // 제거되었으므로 재생 입구는 커서 발급 경로뿐이다. borrow 단순화:
    // 엔진 1개가 전 시나리오를 담당한다 (원래 시나리오별 독립 엔진과
    // 동치 — 저널 append만 전역 공유).
    let path_b = tmp_path("engine");
    {
        let ledger_b: &'static mut SqliteLedger =
            Box::leak(Box::new(SqliteLedger::open(&path_b).unwrap()));
        let mut engine = WorkflowEngine::embedded_default();
        engine.set_ledger(ledger_b, make_actor());
        // 직접 경로의 at()과 동일 계열: 1_700_000_000+n초 (n≥1).
        engine.set_clock(chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap());
        for (tag, events) in scenarios() {
            let oid = format!("o-{tag}");
            engine.enroll(Order::new(oid.clone(), ExternalRef::cron("a", "b"), t0));
            engine.acquire(&oid, "exec-g1").unwrap();
            // 커서 발급 (신규 실행 의미론: 재생은 커서 소유 경로만).
            // 커밋먼트는 현재 저널 시점 (빈 저널 = GENESIS 핀).
            // 읽기는 별도 연결 — 엔진의 쓰기 핸들과 무관.
            let rows_now = SqliteLedger::open(&path_b).unwrap().read_all().unwrap();
            let commit = foundry_workflow::replay::TerminalCommitment {
                max_seq: rows_now.last().map(|r| r.seq).unwrap_or(0),
                count: rows_now.len() as i64,
                chain_head: rows_now.last().map(|r| r.hash.clone()).unwrap_or_else(|| "GENESIS".into()),
            };
            // 재생 컨텍스트 — order 결속 (seq 0 = GENESIS 시점).
            let ctx = foundry_workflow::replay::ReplayContext {
                cursor: foundry_workflow::replay::VerifiedReplayCursor {
                    order_id: oid.clone(),
                    seq: 0,
                    row_digest: "GENESIS".into(),
                    pin_digest: commit.chain_head.clone(),
                },
            };
            for ev in &events {
                let cur_attempt = engine.order(&oid).unwrap().attempt();
                let claim = cur_attempt.max(1);
                // 이벤트마다 커서 갱신 — 직전에 커밋된 **이 주문의** 행을
                // 가리킨다 (첫 이벤트는 GENESIS). 발급은 커밋먼트 경유.
                let rows_now2 = SqliteLedger::open(&path_b).unwrap().read_all().unwrap();
                let last_mine = rows_now2.iter().filter(|r| r.order_id == oid).next_back();
                let ctx = foundry_workflow::replay::ReplayContext {
                    cursor: match last_mine {
                        Some(r) => foundry_workflow::replay::VerifiedReplayCursor {
                            order_id: oid.clone(),
                            seq: r.seq,
                            row_digest: r.hash.clone(),
                            pin_digest: commit.chain_head.clone(),
                        },
                        None => foundry_workflow::replay::VerifiedReplayCursor {
                            order_id: oid.clone(),
                            seq: 0,
                            row_digest: "GENESIS".into(),
                            pin_digest: commit.chain_head.clone(),
                        },
                    },
                };
                // 읽기 전용 재오픈 — 결속 대조용 (엔진의 쓰기 핸들과 무관).
                let jr_ro = SqliteLedger::open(&path_b).unwrap();
                let _ = engine.dispatch_replay_ctx(&oid, "exec-g1", ev, claim, &ctx, &jr_ro);
                drop(jr_ro);
                // refused/NoEdge는 저널에 안 심긴다 (양 경로 동일 규율 —
                // P0 배선은 Applied만 append).
            }
            // 엔진 내부 상태 == 직접 경로 최종 상태.
            let final_engine = engine.order(&oid).unwrap().state();
            let mut direct = Order::new(oid.clone(), ExternalRef::cron("a", "b"), t0);
            for ev in &events {
                let _ = direct.transition(ev.clone(), direct.attempt().max(1));
            }
            assert_eq!(
                final_engine, direct.state(),
                "engine internal state must equal direct replay state for {oid}"
            );
        }
    }

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
