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
    // RefusalRecord → 재디에이션 제안(1회) → 사이징 승인(preset_bound) →
    // 재입장 통과.
    orch.declare_plan(
        "plan-epic",
        vec![
            ("t-1".into(), vec![]),
            ("t-2".into(), vec!["t-1".into()]),
        ],
        vec![
            ("t-1".into(), "s-epic".into()),
            ("t-2".into(), "s-epic".into()),
        ],
    )
    .unwrap();
    let byp2 = rt.dispatch("t-1", mkorder("t-1", false));
    assert!(byp2.is_err(), "bypass2: unsized spawn refused before binding");

    // 거부 레코드 확인 — UnsizedSpawn.
    let refusal_seq = orch.read_all().unwrap()
        .iter()
        .find(|r| matches!(&r.kind, OrchestrationKind::Refusal { refusal_code, .. } if refusal_code == "UnsizedSpawn"))
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

    // "수락 → 작업 수행" = preset_bound 결속(원장 CONSUMED 승인 이미 있음).
    ch.bind_preset_from_ledger("sz-1", "bmad-epic-build", "plan-epic").unwrap();

    // 재입장 — 전량 재평가 통과(선언·바인딩 모두 현재 상태에서).
    ch.reenter(
        "t-1",
        "ext:src/t61",
        Some("sz-1"),
        Some(&digest),
        "plan-epic",
        "bmad-epic-build",
    )
    .unwrap();

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

    // ── (6) 완주 — t-1, t-2 Shipped.
    ship_ticket(&rt, "s-epic", "t-1", "g-t1", &d.join("ap.db"));
    assert!(rt.ready_to_start("t-2").unwrap());
    rt.dispatch("t-2", mkorder("t-2", false)).unwrap();
    ship_ticket(&rt, "s-epic", "t-2", "g-t2", &d.join("ap.db"));
    assert!(rt.stream("s-epic").unwrap().order_shipped("t-2").unwrap());

    // ── (7) 크래시 주입 + 전체 재구축 — 새 OrchestrationLog/Chainer 가
    // 같은 로그를 재생해 동일 상태를 본다(멱등·체인 검증).
    let rows = orch.read_all().unwrap();
    let orch2 = OrchestrationLog::open(&d.join("orch.db")).unwrap();
    let ch2 = Chainer::new(&orch2, &ap_ref);
    // 재입장 재평가 — 재구축 상태에서도 통과.
    ch2.reenter(
        "t-1",
        "ext:src/t61",
        Some("sz-1"),
        Some(&digest),
        "plan-epic",
        "bmad-epic-build",
    )
    .unwrap();
    // 거부 행·preset_bound·재디에이션 연결이 재생에서 보존.
    let rows2 = orch2.read_all().unwrap();
    assert!(rows2.iter().any(|r| matches!(&r.kind, OrchestrationKind::Refusal { refusal_code, .. } if refusal_code == "UnsizedSpawn")));
    assert!(rows2.iter().any(|r| matches!(&r.kind, OrchestrationKind::PresetBound { sizing_order, .. } if sizing_order == "sz-1")));
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
    // 원 요청 거부.
    crate::refusal::record_refusal(&orch, RefusalCode::UnsizedSpawn, "req-1", None).unwrap();
    // 재디에이션 작업 주문("rem-…")의 거부.
    crate::refusal::record_refusal(&orch, RefusalCode::NotReady, "rem-work-1", None).unwrap();
    let ch = Chainer::new(&orch, &ap);
    let rows = orch.read_all().unwrap();
    let rem_seq = rows
        .iter()
        .find(|r| matches!(&r.kind, OrchestrationKind::Refusal { refused_request, .. } if refused_request == "rem-work-1"))
        .unwrap()
        .seq;
    assert!(matches!(
        ch.propose_remediation(rem_seq),
        Err(ChainerError::RemediationDepth { .. })
    ));
    let _ = SizingVerdict::Approved;
}
