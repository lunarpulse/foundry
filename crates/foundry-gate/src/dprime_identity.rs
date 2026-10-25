//! D′ — 토큰 스코프·정체성 단정: 정규 비교(키 순서·NFKC 충돌)·재발급 불가(token_id PK)·전 스코프 불일치 음성.

#![allow(dead_code)] // W6a clippy debt: retain conditional helpers used by other feature and test configurations
#![allow(unused_imports)] // W6a clippy debt: retain the existing feature and test configuration import set

use foundry_approval::{ApprovalLedger, LedgerError};

fn scope_full(order: &str, intent: &str, digest: &str, root: &str) -> serde_json::Value {
    serde_json::json!({
        "order_id": order, "intent": intent, "artifact_digest": digest, "root_scope": root,
    })
}

#[test]
fn dprime_canonical_scope_key_order_insensitive() {
    let mut l = ApprovalLedger::open_in_memory().unwrap();
    let a = serde_json::json!({ "order_id": "o-1", "intent": "ship", "artifact_digest": "d", "root_scope": "r" });
    let b_raw =
        r#"{ "root_scope": "r", "artifact_digest": "d", "intent": "ship", "order_id": "o-1" }"#;
    let b: serde_json::Value = serde_json::from_str(b_raw).unwrap();
    l.issue_grant("g-1", "tok-1", &a).unwrap();
    let r = l.reserve_grant("g-1", &b, "ex-1", 1);
    assert!(
        r.is_ok(),
        "key order must not affect scope comparison: {:?}",
        r.err()
    );
}

#[test]
fn dprime_nfkc_collision_refused() {
    let mut l = ApprovalLedger::open_in_memory().unwrap();
    l.issue_grant("g-1", "tok-A", &scope_full("o-1", "s", "d", "r"))
        .unwrap();
    let err = l
        .issue_grant("g-2", "tok-Ａ", &scope_full("o-2", "s", "d", "r"))
        .unwrap_err();
    assert!(
        matches!(err, LedgerError::TokenAlreadyIssued { .. }),
        "{err:?}"
    );
}

#[test]
fn dprime_token_id_primary_key_no_reissue() {
    let mut l = ApprovalLedger::open_in_memory().unwrap();
    l.issue_grant("g-1", "tok-1", &scope_full("o-1", "s", "d", "r"))
        .unwrap();
    let err = l
        .issue_grant("g-OTHER", "tok-1", &scope_full("o-2", "s", "d", "r"))
        .unwrap_err();
    assert!(
        matches!(err, LedgerError::TokenAlreadyIssued { .. }),
        "{err:?}"
    );
}

#[test]
fn dprime_whole_scope_mismatch_negative() {
    let mut l = ApprovalLedger::open_in_memory().unwrap();
    l.issue_grant("g-1", "tok-1", &scope_full("o-1", "ship", "d", "root-A"))
        .unwrap();
    let r = l.reserve_grant(
        "g-1",
        &scope_full("o-1", "deploy", "d", "root-A"),
        "ex-1",
        1,
    );
    assert!(
        matches!(r, Err(LedgerError::GrantNotFound { .. })),
        "partial scope must not match: {r:?}"
    );
    let r2 = l.reserve_grant("g-1", &scope_full("o-1", "ship", "d", "root-B"), "ex-1", 1);
    assert!(
        matches!(r2, Err(LedgerError::GrantNotFound { .. })),
        "root mismatch must not match"
    );
}
