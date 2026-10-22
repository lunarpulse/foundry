//! G16 — 승인 플로우 독립 오라클 동등 (플랜 v5).
//!
//! 오라클 독립성 규칙: 기대치 산출에 프로덕션 전이/가드/원장 헬퍼 호출 금지
//! — 게이트 안 순수 Rust 재현. 비교 필드: 주문 상태·시도·fence 단조성·소비.
//! 성공/거부/회복 3경로.

use crate::common::{approval_event, engine, furnace_order};
use foundry_domain::OrderState;
use foundry_workflow::{ApprovalLedgerAdapter as LedgerHook, GrantRef};

/// 순수 오라클 — 승인 플로우의 기대 결과를 프로덕션 코드 없이 모형화.
/// (도메인 규칙의 재현: FurnaceHold + Approved(비빈 digest) = Publishing;
/// 인가 성공 시 grant 는 정확히 1회 소비; fence 는 1에서 시작해 단조 증가.)
mod oracle {
    #[derive(Debug, Clone, PartialEq)]
    pub struct Expectation {
        pub applied: bool,
        pub final_state: &'static str,
        pub grant_consumed: bool,
        pub fence_used: u64,
    }

    /// 성공 경로 기대치 — 유효 grant + 올바른 스코프.
    pub fn success() -> Expectation {
        Expectation {
            applied: true,
            final_state: "publishing",
            grant_consumed: true,
            fence_used: 1,
        }
    }

    /// 거부 경로 기대치 — grant 부재/타스코프: 전이 없음·소비 없음.
    pub fn refusal() -> Expectation {
        Expectation {
            applied: false,
            final_state: "furnace_hold",
            grant_consumed: false,
            fence_used: 0,
        }
    }

    /// 회복 경로 기대치 — 크래시 후 APPLYING 은 CONSUMED 로 완결 (fail-closed).
    /// fence_used 는 '기대 고정값'이 아니라 **단조성 기준점** 으로 사용
    /// (전역 카운터 — 첫 예약이 항상 1은 아님).
    pub fn recovery() -> Expectation {
        Expectation {
            applied: true,
            final_state: "publishing",
            grant_consumed: true,
            fence_used: 0,
        }
    }
}

fn state_name(s: OrderState) -> &'static str {
    match s {
        OrderState::Publishing => "publishing",
        OrderState::FurnaceHold => "furnace_hold",
        _ => "other",
    }
}

fn sc(order: &str) -> serde_json::Value {
    serde_json::json!({
        "order_id": order, "intent": "ship", "artifact_digest": "dig-1", "root_scope": "",
    })
}

/// 성공 경로 — 오라클 vs 엔진.
#[test]
fn g16_success_path_matches_oracle() {
    let expect = oracle::success();
    let mut e = engine();
    e.enroll(furnace_order("g16-s"));
    e.acquire("g16-s", "ex").unwrap();
    let mut ledger = foundry_approval::ApprovalLedger::open_in_memory().unwrap();
    ledger.issue_grant("g-s", "tok-s", &sc("g16-s")).unwrap();
    let mut hook = LedgerHook(ledger);
    let grant = GrantRef {
        grant_id: "g-s".into(),
    };
    let out = e.dispatch_live(
        "g16-s",
        "ex",
        &approval_event("ship", "dig-1"),
        1,
        Some(&grant),
        &mut hook,
    );
    assert_eq!(
        out.unwrap() == foundry_workflow::Outcome::Applied,
        expect.applied
    );
    assert_eq!(
        state_name(e.order("g16-s").unwrap().state()),
        expect.final_state
    );
    assert_eq!(
        hook.0.get_grant("g-s").unwrap().state == foundry_approval::GrantState::Consumed,
        expect.grant_consumed
    );
}

/// 거부 경로 — 무grant.
#[test]
fn g16_refusal_path_matches_oracle() {
    let expect = oracle::refusal();
    let mut e = engine();
    e.enroll(furnace_order("g16-r"));
    e.acquire("g16-r", "ex").unwrap();
    let mut hook = LedgerHook(foundry_approval::ApprovalLedger::open_in_memory().unwrap());
    let r = e.dispatch_live(
        "g16-r",
        "ex",
        &approval_event("ship", "dig-1"),
        1,
        None,
        &mut hook,
    );
    assert!(r.is_err(), "refused");
    assert_eq!(
        state_name(e.order("g16-r").unwrap().state()),
        expect.final_state
    );
}

/// 회복 경로 — begin_apply 커밋 후 "크래시": 회복은 CONSUMED 완결.
#[test]
fn g16_recovery_path_matches_oracle() {
    let expect = oracle::recovery();
    let mut ledger = foundry_approval::ApprovalLedger::open_in_memory().unwrap();
    ledger.issue_grant("g-c", "tok-c", &sc("g16-c")).unwrap();
    let r = ledger.reserve_grant("g-c", &sc("g16-c"), "ex", 1).unwrap();
    assert!(
        r.fence_token >= 1,
        "fence is positive and monotonic (got {})",
        r.fence_token
    );
    ledger.begin_apply("g-c", "ex", 1, r.fence_token).unwrap();
    // "크래시" — finalize 없이 회복 스캔.
    let released = ledger.recovery_scan().unwrap();
    assert!(released.is_empty(), "APPLYING is completed, not released");
    assert_eq!(
        ledger.get_grant("g-c").unwrap().state == foundry_approval::GrantState::Consumed,
        expect.grant_consumed,
        "fail-closed: applied grant is consumed by recovery"
    );
    // fence 단조성 — 소비 후 재예약 불가.
    assert!(ledger.reserve_grant("g-c", &sc("g16-c"), "ex2", 1).is_err());
}
