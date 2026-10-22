//! A′ — 승인 소비 회복: 크래시 주입 5지점 + 교차 경쟁 (플랜 v5).
//!
//! 각 지점 후 재시작 3단: 원장 reopen·저널 reopen+verify_chain·신규 엔진
//! rebuild. 단정: "전이∧미소비" 부재 (③ begin_apply 직후만 문서화된
//! fail-closed 손실 — apply_intents 감사 증거와 함께).

use crate::common::{actor, approval_event, file_journal, furnace_order};
use foundry_domain::OrderState;
use foundry_domain::ports::Ledger as _;
use foundry_workflow::recovery::RecoveryRunner;
use foundry_workflow::{ApprovalLedgerAdapter as LedgerHook, GrantRef, Outcome, WorkflowEngine};

fn sc(order: &str) -> serde_json::Value {
    serde_json::json!({
        "order_id": order, "intent": "ship", "artifact_digest": "dig-a", "root_scope": "",
    })
}

/// 파일 기반 원장 헬퍼 (reopen 검증).
fn file_approval(tag: &str) -> (foundry_approval::ApprovalLedger, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!("gate-aprime-{tag}-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    (foundry_approval::ApprovalLedger::open(&path).unwrap(), path)
}

/// 재시작 3단 + 무결성 단정 공용.
fn restart_and_assert(
    ap_path: &std::path::Path,
    jr_path: &std::path::Path,
    allow_consumed_without_transition: bool,
) -> foundry_workflow::recovery::RecoveryReport {
    let mut ap = foundry_approval::ApprovalLedger::open(ap_path).unwrap();
    let jr = foundry_ledger::SqliteLedger::open(jr_path).unwrap();
    assert!(
        jr.verify_chain().unwrap(),
        "journal chain verifies after restart"
    );
    let mut fresh = WorkflowEngine::embedded_default();
    let report = RecoveryRunner::run(&mut ap, &jr, &mut fresh).unwrap();
    assert!(report.journal_verified, "journal verified in report");
    let rows = jr.read_all().unwrap();
    let has_transition = !rows.is_empty();
    if !allow_consumed_without_transition {
        let g = ap.get_grant("g-a").unwrap();
        if g.state == foundry_approval::GrantState::Consumed {
            assert!(
                has_transition,
                "CONSUMED without transition — invariant broken"
            );
        }
    }
    report
}

/// ① reserve 커밋 직후 크래시 — 회복 릴리즈 (전이 없음·Issued 복귀).
#[test]
fn aprime_p1_crash_after_reserve_recovery_releases() {
    let (mut ap, ap_path) = file_approval("p1");
    let (jr, jr_path) = file_journal("ap1");
    let _ = jr;
    ap.issue_grant("g-a", "tok-a", &sc("o-a")).unwrap();
    ap.reserve_grant("g-a", &sc("o-a"), "ex", 1).unwrap();
    drop(ap);
    let report = restart_and_assert(&ap_path, &jr_path, false);
    assert_eq!(
        report.released,
        vec!["g-a".to_string()],
        "pending reservation released"
    );
    let ap = foundry_approval::ApprovalLedger::open(&ap_path).unwrap();
    assert_eq!(
        ap.get_grant("g-a").unwrap().state,
        foundry_approval::GrantState::Issued
    );
}

/// ② precheck 실패(해제 경로) — precheck 결격 디스패치는 grant 를 Issued 로.
#[test]
fn aprime_p2_precheck_failure_leaves_no_reservation() {
    let mut ap = foundry_approval::ApprovalLedger::open_in_memory().unwrap();
    ap.issue_grant("g-a", "tok-a", &sc("o-a2")).unwrap();
    let mut e = WorkflowEngine::embedded_default();
    let mut o = furnace_order("o-a2");
    o.force_state_for_test(OrderState::Received, 1);
    e.enroll(o);
    e.acquire("o-a2", "ex").unwrap();
    let mut hook = LedgerHook(ap);
    let grant = GrantRef {
        grant_id: "g-a".into(),
    };
    let out = e.dispatch_live(
        "o-a2",
        "ex",
        &approval_event("ship", "dig-a"),
        1,
        Some(&grant),
        &mut hook,
    );
    assert_eq!(out.unwrap(), Outcome::NoEdge);
    assert_eq!(
        hook.0.get_grant("g-a").unwrap().state,
        foundry_approval::GrantState::Issued,
        "precheck-ineligible dispatch leaves the grant Issued"
    );
}

/// ③ begin_apply 커밋 직후 크래시 — APPLYING 은 CONSUMED 로 완결 (fail-closed)
/// + apply_intents 감사 증거 유지.
#[test]
fn aprime_p3_crash_after_begin_apply_fails_closed() {
    let (mut ap, ap_path) = file_approval("p3");
    let (jr, jr_path) = file_journal("ap3");
    let _ = jr;
    ap.issue_grant("g-a", "tok-a", &sc("o-a")).unwrap();
    let r = ap.reserve_grant("g-a", &sc("o-a"), "ex", 1).unwrap();
    ap.begin_apply("g-a", "ex", 1, r.fence_token).unwrap();
    drop(ap);
    let _report = restart_and_assert(&ap_path, &jr_path, true);
    let ap = foundry_approval::ApprovalLedger::open(&ap_path).unwrap();
    assert_eq!(
        ap.get_grant("g-a").unwrap().state,
        foundry_approval::GrantState::Consumed,
        "APPLYING completed to CONSUMED (fail-closed)"
    );
    assert!(
        ap.apply_intent("g-a", "ex", 1).unwrap(),
        "audit evidence retained"
    );
}

/// ④ 전이 적용·finalize 직전 크래시 — **실제 저널 append 후** (astra T56
/// MAJOR: begin_apply 반복이 아니라 진짜 전이 커밋 뒤). 재구축 상태 +
/// CONSUMED 단정.
#[test]
fn aprime_p4_crash_before_finalize() {
    let (mut ap, ap_path) = file_approval("p4");
    ap.issue_grant("g-a", "tok-a", &sc("o-a")).unwrap();

    let jr_path = std::env::temp_dir().join(format!("gate-ap4-j-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&jr_path);
    let jr_static: &'static mut foundry_ledger::SqliteLedger = Box::leak(Box::new(
        foundry_ledger::SqliteLedger::open(&jr_path).unwrap(),
    ));
    let mut e = WorkflowEngine::embedded_default();
    e.set_ledger(jr_static, actor("p4"));
    e.set_clock(chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap());
    // 완전한 라이프사이클 — 자율 구간 재생.
    let mut o = furnace_order("o-a");
    o.force_state_for_test(OrderState::Received, 1);
    e.enroll(o);
    e.acquire("o-a", "ex").unwrap();
    use foundry_domain::OrderEvent as OE;
    for ev in [
        OE::Queued,
        OE::DieStarted,
        OE::DieSucceeded { attempt: 1 },
        OE::InspectionPassed,
    ] {
        let att = e.order("o-a").unwrap().attempt().max(1);
        let rows = foundry_ledger::SqliteLedger::open(&jr_path)
            .unwrap()
            .read_all()
            .unwrap();
        let ctx = crate::common::replay_ctx_for("o-a", &rows);
        let jr_ro: &'static mut foundry_ledger::SqliteLedger = Box::leak(Box::new(
            foundry_ledger::SqliteLedger::open(&jr_path).unwrap(),
        ));
        e.dispatch_replay_ctx("o-a", "ex", &ev, att, &ctx, jr_ro)
            .unwrap();
    }
    // 승인 전이 — dispatch_live 는 begin_apply~finalize 를 원자적으로 마는데,
    // 크래시 지점 재현: reserve+begin_apply 만 수동 수행 후 **전이를 엔진이
    // 아니라 저널에만** 심는다(전이 적용됨·finalize 미수행).
    let r = ap.reserve_grant("g-a", &sc("o-a"), "ex", 1).unwrap();
    ap.begin_apply("g-a", "ex", 1, r.fence_token).unwrap();
    // 전이 적용 (도메인 직접 — 엔진을 거치지 않는 크래시 경계 시뮬레이션).
    drop(e); // 엔진 폐기 — 쓰기 핸들 해제.
    let mut dom = furnace_order("o-a"); // FurnaceHold 현재 상태
    let to = dom
        .transition(
            OE::Approved {
                intent: "ship".into(),
                artifact_digest: "dig-a".into(),
            },
            1,
        )
        .unwrap();
    let mut jr_w = foundry_ledger::SqliteLedger::open(&jr_path).unwrap();
    use foundry_domain::ports::Ledger as _;
    jr_w.append(
        "o-a",
        &OE::Approved {
            intent: "ship".into(),
            artifact_digest: "dig-a".into(),
        },
        foundry_domain::OrderState::FurnaceHold,
        to,
        &actor("p4"),
        chrono::Utc::now(),
    )
    .unwrap();
    drop(jr_w);
    drop(ap);
    // 재시작 — 저널 검증·재구축(Publishing)·APPLYING→CONSUMED.
    let mut ap2 = foundry_approval::ApprovalLedger::open(&ap_path).unwrap();
    let jr2 = foundry_ledger::SqliteLedger::open(&jr_path).unwrap();
    assert!(jr2.verify_chain().unwrap());
    let mut fresh = WorkflowEngine::embedded_default();
    let report = RecoveryRunner::run(&mut ap2, &jr2, &mut fresh).unwrap();
    assert_eq!(report.rebuilt_orders, 1);
    assert_eq!(
        fresh.order("o-a").unwrap().state(),
        OrderState::Publishing,
        "transition survived in journal"
    );
    assert_eq!(
        ap2.get_grant("g-a").unwrap().state,
        foundry_approval::GrantState::Consumed,
        "APPLYING completed — no transition-without-consumption"
    );
}

/// ⑤ finalize 커밋 직후 — 이미 CONSUMED: 회복은 무연산 (멱등).
#[test]
fn aprime_p5_crash_after_finalize_idempotent() {
    let (mut ap, ap_path) = file_approval("p5");
    let (jr, jr_path) = file_journal("ap5");
    let _ = jr;
    ap.issue_grant("g-a", "tok-a", &sc("o-a")).unwrap();
    let r = ap.reserve_grant("g-a", &sc("o-a"), "ex", 1).unwrap();
    ap.begin_apply("g-a", "ex", 1, r.fence_token).unwrap();
    ap.finalize_grant("g-a", "ex", 1, r.fence_token).unwrap();
    drop(ap);
    // 이 테스트의 저널은 비어 있다(원장 수명주기만 검증) — 전이 단정은
    // wired 테스트가 소유. 여기선 회복 멱등성만.
    let _ = restart_and_assert(&ap_path, &jr_path, true);
    let ap = foundry_approval::ApprovalLedger::open(&ap_path).unwrap();
    assert_eq!(
        ap.get_grant("g-a").unwrap().state,
        foundry_approval::GrantState::Consumed
    );
    assert!(
        !ap.apply_intent("g-a", "ex", 1).unwrap(),
        "intent cleaned after finalize"
    );
}

/// 교차 경쟁 — X 예약→회복 릴리즈→Y 예약→X 확정 시도 = fence 거부.
#[test]
fn aprime_cross_race_fence_refusal() {
    let mut ap = foundry_approval::ApprovalLedger::open_in_memory().unwrap();
    ap.issue_grant("g-a", "tok-a", &sc("o-a")).unwrap();
    let x = ap.reserve_grant("g-a", &sc("o-a"), "ex-X", 1).unwrap();
    ap.recovery_scan().unwrap();
    let y = ap.reserve_grant("g-a", &sc("o-a"), "ex-Y", 1).unwrap();
    assert_ne!(
        x.fence_token, y.fence_token,
        "new reservation gets a NEW fence"
    );
    let err = ap
        .finalize_grant("g-a", "ex-X", 1, x.fence_token)
        .unwrap_err();
    assert!(
        matches!(
            err,
            foundry_approval::LedgerError::FenceStale { .. }
                | foundry_approval::LedgerError::ReservationConflict { .. }
        ),
        "stale owner fence-refused: {err:?}"
    );
}

/// 배선 엔진 전체 회복 주기 — 승인 전이가 저널에 심기고 재시작이 정리·재구축.
#[test]
fn aprime_wired_engine_full_recovery_cycle() {
    let (mut ap, ap_path) = file_approval("wired");
    ap.issue_grant("g-a", "tok-a", &sc("o-a")).unwrap();

    let jr_path = std::env::temp_dir().join(format!("gate-apw-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&jr_path);
    let jr_static: &'static mut foundry_ledger::SqliteLedger = Box::leak(Box::new(
        foundry_ledger::SqliteLedger::open(&jr_path).unwrap(),
    ));
    let mut e = WorkflowEngine::embedded_default();
    e.set_ledger(jr_static, actor("aprime"));
    e.set_clock(chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap());
    // 완전한 라이프사이클: Received 부터 재구축 가능한 전체 시퀀스.
    use foundry_domain::OrderEvent as OE;
    let mut o = furnace_order("o-a");
    o.force_state_for_test(OrderState::Received, 1);
    e.enroll(o);
    e.acquire("o-a", "ex").unwrap();
    // 자율 구간 — 커서 소유 재생 경로 (저널 배선이 append).
    for ev in [
        OE::Queued,
        OE::DieStarted,
        OE::DieSucceeded { attempt: 1 },
        OE::InspectionPassed,
    ] {
        let att = e.order("o-a").unwrap().attempt().max(1);
        let ctx = crate::common::replay_ctx_for(
            "o-a",
            &foundry_ledger::SqliteLedger::open(&jr_path)
                .unwrap()
                .read_all()
                .unwrap(),
        );
        let journal_ro: &'static mut foundry_ledger::SqliteLedger = Box::leak(Box::new(
            foundry_ledger::SqliteLedger::open(&jr_path).unwrap(),
        ));
        e.dispatch_replay_ctx("o-a", "ex", &ev, att, &ctx, journal_ro)
            .unwrap();
    }
    assert_eq!(
        e.order("o-a").unwrap().state(),
        OrderState::FurnaceHold,
        "autonomous leg done"
    );
    let mut hook = LedgerHook(ap);
    let grant = GrantRef {
        grant_id: "g-a".into(),
    };
    let out = e.dispatch_live(
        "o-a",
        "ex",
        &approval_event("ship", "dig-a"),
        1,
        Some(&grant),
        &mut hook,
    );
    assert_eq!(out.unwrap(), Outcome::Applied);
    assert_eq!(e.order("o-a").unwrap().state(), OrderState::Publishing);
    let rows = foundry_ledger::SqliteLedger::open(&jr_path)
        .unwrap()
        .read_all()
        .unwrap();
    assert!(
        rows.iter().any(|r| r.event.contains("Approved")),
        "approval transition journaled"
    );
    drop(hook);

    let report = restart_and_assert(&ap_path, &jr_path, false);
    assert_eq!(report.rebuilt_orders, 1, "order rebuilt from journal");
}
