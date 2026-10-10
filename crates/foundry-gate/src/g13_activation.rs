//! G13 — 활성화 토큰 결속 4케이스: 무토큰·타 root·타 의도·주문 토큰 재사용.

use foundry_approval::ApprovalLedger;

fn scope(order: &str, intent: &str, digest: &str, root: &str) -> serde_json::Value {
    serde_json::json!({
        "order_id": order, "intent": intent, "artifact_digest": digest, "root_scope": root,
    })
}

fn ledger() -> ApprovalLedger {
    ApprovalLedger::open_in_memory().unwrap()
}

/// ① 무토큰 — 활성화 없이는 결속 없음 (조회 음성).
#[test]
fn g13_case1_no_token_not_activated() {
    let l = ledger();
    assert!(!l.is_activated("root-1", "intent-1").unwrap(), "no token, no activation");
}

/// ② 타 root — 같은 의도라도 다른 root 는 미활성화.
#[test]
fn g13_case2_wrong_root_refused() {
    let mut l = ledger();
    l.activate_workflow("root-1", "intent-1", "tok-13").unwrap();
    assert!(l.is_activated("root-1", "intent-1").unwrap());
    assert!(!l.is_activated("root-2", "intent-1").unwrap(), "different root is NOT activated by this token");
}

/// ③ 타 의도 — 같은 root 라도 다른 의도는 미활성화.
#[test]
fn g13_case3_wrong_intent_refused() {
    let mut l = ledger();
    l.activate_workflow("root-1", "intent-1", "tok-13").unwrap();
    assert!(!l.is_activated("root-1", "intent-2").unwrap(), "different intent is NOT activated");
}

/// ④ 주문 스코프 토큰 재사용 — 워크플로 활성화에 order-scope 토큰 결속 불가.
/// (스코프 불변: grant 의 scope_json 은 주문 스코프 — 활성화 결속과
/// 정규 비교상 불일치. 토큰 유일 활성화 계약으로 재사용 차단.)
#[test]
fn g13_case4_order_token_reuse_refused() {
    let mut l = ledger();
    // 주문 스코프로 발급된 grant 토큰.
    l.issue_grant("g-order", "tok-order", &scope("o-1", "ship", "d1", "")).unwrap();
    // 그 토큰 id 로 워크플로 활성화 시도 — 다른 (root, intent) 결속은
    // 1토큰-1결속 원칙으로 거부된다.
    let err = l.activate_workflow("root-wf", "intent-wf", "tok-order").unwrap_err();
    assert!(
        matches!(err, foundry_approval::LedgerError::ActivationRefused { .. }),
        "order-scope token cannot activate a workflow: {err:?}"
    );
    assert!(!l.is_activated("root-wf", "intent-wf").unwrap());
}
