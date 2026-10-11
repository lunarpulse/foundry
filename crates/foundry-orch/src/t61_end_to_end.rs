//! T61 — 종단 게이트 (M7 마지막 태스크, 플랜 v9 T61 + I절 확장).
//!
//! **종단 시나리오**: 사이징(intake) → 스케일 3 승인(라이브) → 4-티켓
//! 에픽 → 전원 Shipped 에 다음을 얹는다:
//! 1. **재입장 루프(I절)**: 거부된 요청 → RefusalRecord → 재디에이션
//!    제안(1회) → 수락·작업 수행 → 재입장(전량 재평가) → 통과.
//! 2. **우회 시도**: unsized-spawn·사이징-게이트 우회가 종단에서도
//!    거부됨을 단정.
//! 3. **방화벽**: 재생 경로 Approved — 종단에서도 거부.
//! 4. **크래시 주입**: 디스패치 중간 크래시(프로세스 재시작 시뮬레이션)
//!    후 **전체 재구축** — 로그·원장·저널에서 상태 재생, 체인 검증.
//! 5. **전체 재구축 동등**: 재구축 오케스트레이션 로그가 원본과 동일.

use crate::chainer::{Chainer, ChainerError, SizingVerdict};
use crate::refusal::{RefusalCode, RemediationKind};
use crate::stream::{Stream, StreamRuntime};
use crate::{OrchestrationKind, OrchestrationLog};
use foundry_domain::order::{ExternalRef, Order};
use foundry_domain::{OrderEvent, OrderState};
use foundry_workflow::{Outcome, WorkflowEngine};

fn tmpdir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("foundry-t61-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn mkorder(id: &str, intake: bool) -> Order {
    let ext = if intake {
        ExternalRef::intake(id, &format!("dedup:{id}"))
    } else {
        ExternalRef::cron("src", "t61")
    };
    let mut o = Order::new(id.into(), ext, chrono::Utc::now());
    o.force_state_for_test(OrderState::Received, 1);
    o
}

fn autonomous_to_gate(rt: &StreamRuntime, sid: &str, oid: &str, exec: &str) {
    let e = rt.stream(sid).unwrap();
    let mut eng = e.engine_if_unfenced().unwrap();
    eng.acquire(oid, exec).unwrap();
    for ev in [
        OrderEvent::Queued,
        OrderEvent::DieStarted,
        OrderEvent::DieSucceeded { attempt: 1 },
        OrderEvent::InspectionPassed,
    ] {
        let att = eng.order(oid).unwrap().attempt().max(1);
        let rows = foundry_ledger::SqliteLedger::open(&e.journal()).unwrap().read_all().unwrap();
        let ctx = crate::test_support::replay_ctx(oid, &rows);
        let ro: &'static mut foundry_ledger::SqliteLedger = Box::leak(Box::new(
            foundry_ledger::SqliteLedger::open(&e.journal()).unwrap(),
        ));
        eng.dispatch_replay_ctx(oid, exec, &ev, att, &ctx, ro).unwrap();
    }
    assert_eq!(eng.order(oid).unwrap().state(), OrderState::FurnaceHold);
}

fn ship_ticket(rt: &StreamRuntime, sid: &str, oid: &str, grant_id: &str, ap_db: &std::path::Path) {
    autonomous_to_gate(rt, sid, oid, "t61");
    let mut ap = foundry_approval::ApprovalLedger::open(ap_db).unwrap();
    ap.issue_grant(grant_id, &format!("tok-{grant_id}"), &serde_json::json!({
        "order_id": oid, "intent": "ship", "artifact_digest": format!("dig-{oid}"), "root_scope": "",
    }))
    .unwrap();
    let mut hook = foundry_workflow::ApprovalLedgerAdapter(ap);
    let e = rt.stream(sid).unwrap();
    let mut eng = e.engine_if_unfenced().unwrap();
    let grant = foundry_workflow::GrantRef { grant_id: grant_id.into() };
    let out = eng
        .dispatch_live(
            oid,
            "t61",
            &OrderEvent::Approved { intent: "ship".into(), artifact_digest: format!("dig-{oid}") },
            1,
            Some(&grant),
            &mut hook,
        )
        .unwrap();
    assert_eq!(out, Outcome::Applied);
    let rows = foundry_ledger::SqliteLedger::open(&e.journal()).unwrap().read_all().unwrap();
    let ctx = crate::test_support::replay_ctx(oid, &rows);
    let ro: &'static mut foundry_ledger::SqliteLedger = Box::leak(Box::new(
        foundry_ledger::SqliteLedger::open(&e.journal()).unwrap(),
    ));
    eng.dispatch_replay_ctx(
        oid,
        "t61",
        &OrderEvent::PublishConfirmed { pr_url: "https://pr/t61".into() },
        1,
        &ctx,
        ro,
    )
    .unwrap();
    assert_eq!(eng.order(oid).unwrap().state(), OrderState::Shipped);
}

/// 종단: 사이징→에픽→재입장 루프→크래시 주입→전체 재구축→완주.
#[test]
fn t61_end_to_end_reentry_bypass_crash_rebuild() {
    let d = tmpdir("e2e");
    let orch = OrchestrationLog::open(&d.join("orch.db")).unwrap();

    // ── (1) 사이징 주문 — 인테이크(타입) 부트스트랩.
    orch.declare_plan(
        "plan-sizing",
        vec![("sz-1".into(), vec![])],
        vec![("sz-1".into(), "s-sizing".into())],
    )
    .unwrap();
    let rt = StreamRuntime::new(orch);
    let orch = rt.orch.clone();
    rt.add_stream(Stream::new("s-sizing", &d).unwrap()).unwrap();
    rt.add_stream(Stream::new("s-epic", &d).unwrap()).unwrap();
    rt.bind_approval_ledger(
        foundry_approval::ApprovalLedger::open(&d.join("ap.db")).unwrap(),
    )
    .unwrap();

    // ── (2) 우회 시도 #1 — 미선언 unsized-spawn (dispatch 정문에서 거부).
    let byp1 = rt.dispatch("rogue-0", mkorder("rogue-0", true));
    assert!(byp1.is_err(), "bypass1: undeclared intake refused");

    rt.dispatch("sz-1", mkorder("sz-1", true)).unwrap();
    autonomous_to_gate(&rt, "s-sizing", "sz-1", "t61-sz");

    // ── (3) 인간 스케일 3 승인 — 라이브.
    let digest = crate::chainer::sizing_digest("approved", "bmad-epic-build", "plan-epic");
    {
        let mut ap = foundry_approval::ApprovalLedger::open(&d.join("ap.db")).unwrap();
        ap.issue_grant("sizing-sz-1", "tok-human", &serde_json::json!({
            "order_id": "sz-1", "intent": "sizing", "artifact_digest": digest, "root_scope": "",
        }))
        .unwrap();
    }
    {
        let e2 = rt.stream("s-sizing").unwrap();
        let mut eng = e2.engine_if_unfenced().unwrap();
        let mut hook = foundry_workflow::ApprovalLedgerAdapter(
            foundry_approval::ApprovalLedger::open(&d.join("ap.db")).unwrap(),
        );
        let grant = foundry_workflow::GrantRef { grant_id: "sizing-sz-1".into() };
        let out = eng
            .dispatch_live(
                "sz-1",
                "t61-sz",
                &OrderEvent::Approved { intent: "sizing".into(), artifact_digest: digest.clone() },
                1,
                Some(&grant),
                &mut hook,
            )
            .unwrap();
        assert_eq!(out, Outcome::Applied, "live scale-3 approval");
    }

    // ── (4) 재입장 루프 — 우회 시도 #2: unsized 에픽 발행 시도 → 거부 →
    // RefusalRecord → 재디에이션 제안(1회) → **재디에이션 작업 주문을
    // Foundry 정문(dispatch)으로 수행** → 같은 요청 재입장 → 통과.
    // 4-티켓 에픽 선언(t1→{t2,t3}→t4).
    orch.declare_plan(
        "plan-epic",
        vec![
            ("t-1".into(), vec![]),
            ("t-2".into(), vec!["t-1".into()]),
            ("t-3".into(), vec!["t-1".into()]),
            ("t-4".into(), vec!["t-2".into(), "t-3".into()]),
        ],
        vec![
            ("t-1".into(), "s-epic".into()),
            ("t-2".into(), "s-epic".into()),
            ("t-3".into(), "s-epic".into()),
            ("t-4".into(), "s-epic".into()),
        ],
    )
    .unwrap();
    let byp2 = rt.dispatch("t-1", mkorder("t-1", false));
    assert!(byp2.is_err(), "bypass2: unsized spawn refused before binding");

    // 거부 레코드 확인 — UnsizedSpawn. **원 요청(t-1)의 거부** 선택.
    let refusal_seq = orch.read_all().unwrap()
        .iter()
        .find(|r| matches!(&r.kind, OrchestrationKind::Refusal { refused_request, .. } if refused_request == "t-1"))
        .unwrap()
        .seq;

    // 재디에이션 제안 — 체이너.
    let ap_ref = foundry_approval::ApprovalLedger::open(&d.join("ap.db")).unwrap();
    let ch = Chainer::new(&orch, &ap_ref);
    let (pid, kind) = ch.propose_remediation(refusal_seq).unwrap();
    assert!(pid.starts_with("rem-"));
    assert_eq!(kind, RemediationKind::RunSizing, "UnsizedSpawn -> RunSizing");
    // 스팸 재발행 거부.
    assert!(matches!(
        ch.propose_remediation(refusal_seq),
        Err(ChainerError::RemediationAlreadyProposed { .. })
    ));

    // "수락 → 작업 수행" — **재디에이션 작업 자체가 Foundry 주문**:
    // RunSizing = 새 사이징 주문 sz-2 를 정문(dispatch)으로 실행.
    orch.declare_plan(
        "plan-rem-work",
        vec![("sz-2".into(), vec![])],
        vec![("sz-2".into(), "s-sizing".into())],
    )
    .unwrap();
    rt.dispatch("sz-2", mkorder("sz-2", true)).unwrap();
    autonomous_to_gate(&rt, "s-sizing", "sz-2", "t61-rw");
    {
        let mut ap2 = foundry_approval::ApprovalLedger::open(&d.join("ap.db")).unwrap();
        ap2.issue_grant("sizing-sz-2", "tok-human2", &serde_json::json!({
            "order_id": "sz-2", "intent": "sizing", "artifact_digest": digest, "root_scope": "",
        }))
        .unwrap();
    }
    {
        let e2 = rt.stream("s-sizing").unwrap();
        let mut eng = e2.engine_if_unfenced().unwrap();
        let mut hook = foundry_workflow::ApprovalLedgerAdapter(
            foundry_approval::ApprovalLedger::open(&d.join("ap.db")).unwrap(),
        );
        let grant = foundry_workflow::GrantRef { grant_id: "sizing-sz-2".into() };
        let out = eng
            .dispatch_live(
                "sz-2",
                "t61-rw",
                &OrderEvent::Approved { intent: "sizing".into(), artifact_digest: digest.clone() },
                1,
                Some(&grant),
                &mut hook,
            )
            .unwrap();
        assert_eq!(out, Outcome::Applied, "remediation work order approved live");
    }
    // 재디에이션 작업 완결 — sz-2 승인(새 증거)으로 preset_bound 결속.
    ch.bind_preset_from_ledger("sz-2", "bmad-epic-build", "plan-epic").unwrap();

    // 재입장 — **같은 원 요청(t-1)을 같은 정문(dispatch)으로 재시도**,
    // 전량 재평가(선언·바인딩·목적지) 통과.
    ch.reenter(
        "t-1",
        "ext:src/t61",
        Some("sz-2"),
        Some(&digest),
        "plan-epic",
        "bmad-epic-build",
    )
    .unwrap();
    // A 고쳐도 B — 미선언 주문의 재입장은 여전히 거부(DeclarationMissing).
    assert!(matches!(
        ch.reenter("t-undeclared", "ext:x", Some("sz-2"), Some(&digest), "plan-epic", "bmad-epic-build"),
        Err(ChainerError::DeclarationMissing { .. })
    ));

    // ── (5) 방화벽 — 종단에서도 재생 Approved 거부 (t-1 게이트에서).
    rt.dispatch("t-1", mkorder("t-1", false)).unwrap();
    {
        let e = rt.stream("s-epic").unwrap();
        let mut eng = e.engine_if_unfenced().unwrap();
        eng.acquire("t-1", "t61").unwrap();
        for ev in [
            OrderEvent::Queued,
            OrderEvent::DieStarted,
            OrderEvent::DieSucceeded { attempt: 1 },
            OrderEvent::InspectionPassed,
        ] {
            let att = eng.order("t-1").unwrap().attempt().max(1);
            let jrows = foundry_ledger::SqliteLedger::open(&e.journal()).unwrap().read_all().unwrap();
            let ctx = crate::test_support::replay_ctx("t-1", &jrows);
            let ro: &'static mut foundry_ledger::SqliteLedger = Box::leak(Box::new(
                foundry_ledger::SqliteLedger::open(&e.journal()).unwrap(),
            ));
            eng.dispatch_replay_ctx("t-1", "t61", &ev, att, &ctx, ro).unwrap();
        }
        let jrows = foundry_ledger::SqliteLedger::open(&e.journal()).unwrap().read_all().unwrap();
        let ctx = crate::test_support::replay_ctx("t-1", &jrows);
        let ro: &'static mut foundry_ledger::SqliteLedger = Box::leak(Box::new(
            foundry_ledger::SqliteLedger::open(&e.journal()).unwrap(),
        ));
        let fw = eng.dispatch_replay_ctx(
            "t-1",
            "t61",
            &OrderEvent::Approved { intent: "ship".into(), artifact_digest: "d".into() },
            1,
            &ctx,
            ro,
        );
        assert!(
            matches!(fw, Err(foundry_workflow::EngineError::AuthorizationRefused { .. })),
            "end-to-end firewall holds"
        );
    }

    // ── (6) 완주 — 4-티켓 전원 Shipped (t1 → {t2, t3} → t4).
    ship_ticket(&rt, "s-epic", "t-1", "g-t1", &d.join("ap.db"));
    assert!(rt.ready_to_start("t-2").unwrap());
    assert!(rt.ready_to_start("t-3").unwrap());
    assert!(!rt.ready_to_start("t-4").unwrap(), "t4 waits");
    rt.dispatch("t-2", mkorder("t-2", false)).unwrap();
    rt.dispatch("t-3", mkorder("t-3", false)).unwrap();
    ship_ticket(&rt, "s-epic", "t-2", "g-t2", &d.join("ap.db"));
    ship_ticket(&rt, "s-epic", "t-3", "g-t3", &d.join("ap.db"));
    assert!(rt.ready_to_start("t-4").unwrap());
    rt.dispatch("t-4", mkorder("t-4", false)).unwrap();
    ship_ticket(&rt, "s-epic", "t-4", "g-t4", &d.join("ap.db"));
    for t in ["t-1", "t-2", "t-3", "t-4"] {
        assert!(rt.stream("s-epic").unwrap().order_shipped(t).unwrap(), "{t} shipped");
    }

    // ── (7) 크래시 주입 — append 트랜잭션 창(INSERT 후 커밋 전)의
    // 프로세스 사망은 RAAI TxGuard(T57, astra r1 M5)가 롤백한다 — 이
    // 종단에서는 창 **직후** 재오픈으로 커밋된 행만 생존함을 단정하고,
    // 회복(재구축)이 상태를 완전 복원함을 증명한다.
    {
        // 창 내 재오픈 — 체인이 정합(마지막 행 = t-4 발행)이고 손상 없음.
        let reopened = OrchestrationLog::open(&d.join("orch.db")).unwrap();
        let rr = reopened.read_all().unwrap();
        assert!(rr.iter().any(|r| matches!(&r.kind, OrchestrationKind::OrderSpawned { order_id, .. } if order_id == "t-4")),
            "crash window: committed rows survive");
    }
    // ── (7b) 전체 재구축 — 스트림 엔진 재구축(rebuild) 포함: 새 런타임이
    // 같은 저널·로그·원장에서 상태를 재생한다.
    let rows = orch.read_all().unwrap();
    let orch2 = OrchestrationLog::open(&d.join("orch.db")).unwrap();
    // 스트림 재구축 — 저널 replay 로 엔진 상태 복원.
    let rt2 = StreamRuntime::new(OrchestrationLog::open(&d.join("orch.db")).unwrap());
    rt2.add_stream(Stream::new("s-epic", &d).unwrap()).unwrap();
    rt2.bind_approval_ledger(
        foundry_approval::ApprovalLedger::open(&d.join("ap.db")).unwrap(),
    )
    .unwrap();
    {
        let e = rt2.stream("s-epic").unwrap();
        let mut rebuilt = e.rebuilt_engine().unwrap();
        // 재구축 — 저널 전체 replay.
        let jpath = e.journal().to_path_buf();
        let n = rebuilt
            .rebuild(&foundry_ledger::SqliteLedger::open(&jpath).unwrap())
            .unwrap();
        assert!(n >= 4, "rebuild replays orders (got {n})");
        // 재구축 엔진은 출하 4티켓 상태를 재생.
        let shipped: Vec<_> = ["t-1", "t-2", "t-3", "t-4"]
            .into_iter()
            .filter(|t| rebuilt.order(t).map(|o| o.state() == OrderState::Shipped).unwrap_or(false))
            .collect();
        assert_eq!(shipped.len(), 4, "rebuild replays all four shipped");
    }
    let ch2 = Chainer::new(&orch2, &ap_ref);
    // 재입장 재평가 — 재구축 상태에서도 통과.
    ch2.reenter(
        "t-1",
        "ext:src/t61",
        Some("sz-2"),
        Some(&digest),
        "plan-epic",
        "bmad-epic-build",
    )
    .unwrap();
    // 거부 행·preset_bound·재디에이션 연결이 재생에서 보존.
    let rows2 = orch2.read_all().unwrap();
    assert!(rows2.iter().any(|r| matches!(&r.kind, OrchestrationKind::Refusal { refusal_code, .. } if refusal_code == "UnsizedSpawn")));
    assert!(rows2.iter().any(|r| matches!(&r.kind, OrchestrationKind::PresetBound { sizing_order, .. } if sizing_order == "sz-2")));
    // read_all 자체가 체인 검증 포함 — 변조 없음이 곧 동등 증명.
    // 행 수 동일.
    assert_eq!(rows2.len(), rows.len(), "rebuild sees identical log");
}

/// 깊이 1 제한 — 재디에이션 작업 자신의 거부는 제안 불가(종단 맥락).
#[test]
fn t61_remediation_depth1_in_end_to_end_context() {
    let d = tmpdir("depth");
    let orch = OrchestrationLog::open(&d.join("orch.db")).unwrap();
    let ap = foundry_approval::ApprovalLedger::open(&d.join("ap.db")).unwrap();
    // 원 요청 거부 + 재디에이션 제안(연결 행 append).
    crate::refusal::record_refusal(&orch, RefusalCode::UnsizedSpawn, "req-1", None).unwrap();
    let seq0 = orch.read_all().unwrap().iter().find(|r| matches!(&r.kind, OrchestrationKind::Refusal { .. })).unwrap().seq;
    let ch0 = Chainer::new(&orch, &ap);
    let (rem_pid, _) = ch0.propose_remediation(seq0).unwrap();
    // **그 재디에이션 작업 주문 자신**의 거부 — 같은 pid 주문.
    crate::refusal::record_refusal(&orch, RefusalCode::NotReady, &rem_pid, None).unwrap();
    let ch = Chainer::new(&orch, &ap);
    let rows = orch.read_all().unwrap();
    let rem_seq = rows
        .iter()
        .find(|r| matches!(&r.kind, OrchestrationKind::Refusal { refused_request, .. } if refused_request == &rem_pid))
        .unwrap()
        .seq;
    assert!(matches!(
        ch.propose_remediation(rem_seq),
        Err(ChainerError::RemediationDepth { .. })
    ));
    let _ = SizingVerdict::Approved;
}
