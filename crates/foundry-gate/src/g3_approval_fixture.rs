//! G3 — 승인 문서 대항 픽스처 4케이스 (플랜 v3/v5 정의).
//!
//! ① Waive 주입 작성 거부 ② 무grant 라이브 승인 → AuthorizationRefused·
//! 상태 불변·관측 승인 0·원장 무기록 ③ 유효-타스코프 grant 거부 ④ 유효
//! 인간 승인 → Applied·CONSUMED.

use crate::common::{actor, approval_event, engine, furnace_order};
use foundry_domain::OrderState;
use foundry_workflow::dsl::{PolicyLevel, WorkflowDoc};
use foundry_workflow::{ApprovalLedgerAdapter as LedgerHook, EngineError, GrantRef, Outcome};

fn v3_doc_with_waive() -> WorkflowDoc {
    // 최소 v3 문서 — Waive 를 policy 에 주입한 변형.
    let mut doc = foundry_workflow::dsl::load_preset(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent().unwrap().parent().unwrap()
            .join("presets/bmad-intake-pool.v3.json"),
    )
    .unwrap();
    doc.policy.as_mut().unwrap().approvals[0].level = PolicyLevel::Waive;
    doc
}

/// ① Waive 선언 — 검증기가 작성 시점에 거부.
#[test]
fn g3_case1_waive_rejected_at_authoring() {
    let doc = v3_doc_with_waive();
    let errs = foundry_workflow::validator::validate(
        &doc,
        &foundry_domain::workflow_encoding::EmbeddedWorkflow::frozen_v1(),
    )
    .unwrap_err();
    assert!(
        errs.iter().any(|d| matches!(d, foundry_workflow::validator::Diagnostic::Error { code, .. } if code.contains("waive"))),
        "waive must be refused: {errs:?}"
    );
}

/// ② 무grant 라이브 승인 — 인가 거부·상태 불변·관측 Applied 0·원장 무기록.
#[test]
fn g3_case2_no_grant_refused_state_frozen_ledger_empty() {
    let mut e = engine();
    e.enroll(furnace_order("g3-o"));
    e.acquire("g3-o", "ex").unwrap();
    let mut hook = LedgerHook(foundry_approval::ApprovalLedger::open_in_memory().unwrap());
    let r = e.dispatch_live("g3-o", "ex", &approval_event("ship", "dig-1"), 1, None, &mut hook);
    match &r {
        Err(EngineError::AuthorizationRefused { grant_id: None, .. }) => {}
        other => panic!("expected AuthorizationRefused(None): {other:?}"),
    }
    assert_eq!(e.order("g3-o").unwrap().state(), OrderState::FurnaceHold, "state frozen");
    // acquire 관측(wf.lock_*)은 남지만 승인 Applied 는 0이어야 한다 —
    // 이벤트 패밀리로 필터.
    let applied = e
        .observations()
        .iter()
        .filter(|o| o.exec_id == "ex" && matches!(o.outcome, Outcome::Applied) && o.event_family == "Approved")
        .count();
    assert_eq!(applied, 0, "no Applied observation for the approval event");
    // 원장 무기록 — grant 자체가 없으므로 예약/소비 기록 부재.
    assert!(hook.0.recovery_scan().unwrap().is_empty(), "no ledger writes");
}

/// ③ 유효하지만 타스코프의 grant — 거부·상태 불변.
#[test]
fn g3_case3_wrong_scope_grant_refused() {
    let mut e = engine();
    e.enroll(furnace_order("g3-o"));
    e.acquire("g3-o", "ex").unwrap();
    let mut ledger = foundry_approval::ApprovalLedger::open_in_memory().unwrap();
    let wrong = serde_json::json!({
        "order_id": "OTHER-order", "intent": "ship", "artifact_digest": "dig-1", "root_scope": "",
    });
    ledger.issue_grant("g-w", "tok-w", &wrong).unwrap();
    let mut hook = LedgerHook(ledger);
    let grant = GrantRef { grant_id: "g-w".into() };
    let r = e.dispatch_live("g3-o", "ex", &approval_event("ship", "dig-1"), 1, Some(&grant), &mut hook);
    assert!(matches!(r, Err(EngineError::AuthorizationRefused { .. })), "{r:?}");
    assert_eq!(e.order("g3-o").unwrap().state(), OrderState::FurnaceHold);
}

/// ④ 유효 인간 승인 — Applied·CONSUMED (원장 소비 단정).
#[test]
fn g3_case4_valid_grant_applies_and_consumes() {
    let mut e = engine();
    e.enroll(furnace_order("g3-o"));
    e.acquire("g3-o", "ex").unwrap();
    let mut ledger = foundry_approval::ApprovalLedger::open_in_memory().unwrap();
    let right = serde_json::json!({
        "order_id": "g3-o", "intent": "ship", "artifact_digest": "dig-1", "root_scope": "",
    });
    ledger.issue_grant("g-r", "tok-r", &right).unwrap();
    let mut hook = LedgerHook(ledger);
    let grant = GrantRef { grant_id: "g-r".into() };
    let out = e.dispatch_live("g3-o", "ex", &approval_event("ship", "dig-1"), 1, Some(&grant), &mut hook).unwrap();
    assert_eq!(out, Outcome::Applied);
    assert_eq!(e.order("g3-o").unwrap().state(), OrderState::Publishing);
    assert_eq!(hook.0.get_grant("g-r").unwrap().state, foundry_approval::GrantState::Consumed, "CONSUMED");
    // apply_intents 정리 — finalize 가 증거를 finalize_records 로.
    assert!(!hook.0.apply_intent("g-r", "ex", 1).unwrap(), "intent cleaned");
}
