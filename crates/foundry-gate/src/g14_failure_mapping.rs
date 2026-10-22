//! G14 — 실행기 실패 매핑 3분기 (T55a).
//!
//! 재시도 가능 → back-edge (attempt 는 도메인 소유: DieFailed 증가·
//! DieStarted 클레임) · 소진 → MaxAttempts 경로 · 매핑 불능 → 엔진 래치·
//! 주문 상태 불변.

use crate::common::{engine, file_journal, replay_event};
use foundry_domain::OrderState;
use foundry_domain::order::{ExternalRef, Order};
use foundry_workflow::executor_failures::ExecutorErrorKind;
use foundry_workflow::{EngineError, Outcome};

fn working_order(id: &str, attempt: u32) -> Order {
    let mut o = Order::new(
        id.to_string(),
        ExternalRef::cron("s", "r"),
        chrono::Utc::now(),
    );
    o.force_state_for_test(OrderState::Working, attempt);
    o
}

fn failed_state_order(id: &str) -> Order {
    let mut o = Order::new(
        id.to_string(),
        ExternalRef::cron("s", "r"),
        chrono::Utc::now(),
    );
    o.force_state_for_test(OrderState::Failed, foundry_domain::order::MAX_ATTEMPTS);
    o
}

/// ① Transient@Working — DieFailed back-edge 로 재시도 (상태 Queued 복귀).
#[test]
fn g14_transient_retries_via_backedge() {
    let mut e = engine();
    let (journal, _p) = file_journal("g14-t");
    e.enroll(working_order("g14-t", 1));
    e.acquire("g14-t", "ex").unwrap();
    replay_event(
        &mut e,
        &journal,
        "g14-t",
        "ex",
        &foundry_domain::OrderEvent::DieStarted,
        1,
    )
    .unwrap();
    let r = e
        .report_executor_failure(
            "g14-t",
            "ex",
            &ExecutorErrorKind::Transient,
            "node-a",
            "conn reset",
        )
        .unwrap();
    assert_eq!(
        r,
        Outcome::Applied,
        "transient maps to the existing back-edge"
    );
    assert_eq!(
        e.order("g14-t").unwrap().state(),
        OrderState::Queued,
        "retry returns to Queued"
    );
    // attempt 는 DieFailed 가 증가시켰다 (도메인 소유).
    assert!(
        e.order("g14-t").unwrap().attempt() >= 2,
        "attempt incremented by the domain"
    );
}

/// ② 소진 — MaxAttempts 경로 (재시도 불가 → Failed 종단).
#[test]
fn g14_exhaustion_via_max_attempts() {
    let mut e = engine();
    e.enroll(failed_state_order("g14-x"));
    e.acquire("g14-x", "ex").unwrap();
    // Failed 상태에서 소진 보고 — 합법 엣지 없음 → 매핑 불능과 동일 규율?
    // 아니: 플랜은 "소진(attempt>=MAX) → 기존 MaxAttempts 경로".
    // Working@MAX 로 소진 재현:
    let mut o = working_order("g14-x2", foundry_domain::order::MAX_ATTEMPTS);
    let (journal, _p) = file_journal("g14-x2");
    e.enroll(o.clone());
    e.acquire("g14-x2", "ex").unwrap();
    replay_event(
        &mut e,
        &journal,
        "g14-x2",
        "ex",
        &foundry_domain::OrderEvent::DieStarted,
        foundry_domain::order::MAX_ATTEMPTS,
    )
    .unwrap();
    let r = e
        .report_executor_failure(
            "g14-x2",
            "ex",
            &ExecutorErrorKind::Permanent,
            "node-a",
            "hard fail",
        )
        .unwrap();
    assert_eq!(
        r,
        Outcome::Applied,
        "exhaustion maps to the MaxAttempts path"
    );
    assert_eq!(
        e.order("g14-x2").unwrap().state(),
        OrderState::Failed,
        "exhausted order reaches Failed"
    );
}

/// ③ 매핑 불능 — FurnaceHold 등 합법 실패 엣지 없는 상태: 래치·상태 불변.
#[test]
fn g14_unmappable_latches_state_frozen() {
    let mut e = engine();
    let mut o = Order::new(
        "g14-u".into(),
        ExternalRef::cron("s", "r"),
        chrono::Utc::now(),
    );
    o.force_state_for_test(OrderState::FurnaceHold, 1);
    e.enroll(o);
    e.acquire("g14-u", "ex").unwrap();
    let r = e.report_executor_failure(
        "g14-u",
        "ex",
        &ExecutorErrorKind::Permanent,
        "node-a",
        "exploded",
    );
    match &r {
        Err(EngineError::UnmappableFailure { node, .. }) if node == "node-a" => {}
        other => panic!("expected UnmappableFailure: {other:?}"),
    }
    assert_eq!(
        e.order("g14-u").unwrap().state(),
        OrderState::FurnaceHold,
        "state UNCHANGED"
    );
    // 재디스패치 — 같은 래치 사유 재생 (최초 사유 보존).
    let (journal, _p) = file_journal("g14-u");
    let r2 = replay_event(
        &mut e,
        &journal,
        "g14-u",
        "ex",
        &foundry_domain::OrderEvent::Queued,
        1,
    );
    assert!(
        matches!(r2, Err(EngineError::UnmappableFailure { .. })),
        "latch replays on redispatch"
    );
}
