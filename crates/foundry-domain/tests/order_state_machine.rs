// T2 — Order state machine (plan v3 §9)
use chrono::{DateTime, Utc};
use foundry_domain::order::{ExternalRef, Order, OrderEvent, OrderState};
use std::str::FromStr;

fn order() -> Order {
    Order::new(
        "o-1".into(),
        ExternalRef::cron("dep-bump", "tokio"),
        DateTime::<Utc>::MIN_UTC,
    )
}

// ---- 정상 경로 (happy path) ----

#[test]
fn full_happy_path_received_to_shipped() {
    let mut o = order();
    assert_eq!(o.state(), OrderState::Received);
    o.transition(OrderEvent::Queued, 0).unwrap();
    o.transition(OrderEvent::DieStarted, 1).unwrap();
    assert_eq!(o.state(), OrderState::Working);
    o.transition(OrderEvent::DieSucceeded { attempt: 1 }, 1).unwrap();
    assert_eq!(o.state(), OrderState::Inspecting);
    o.transition(OrderEvent::InspectionPassed, 1).unwrap();
    assert_eq!(o.state(), OrderState::FurnaceHold);
    o.transition(
        OrderEvent::Approved {
            intent: "looks good".into(),
            artifact_digest: "abc".into(),
        },
        1,
    )
    .unwrap();
    assert_eq!(o.state(), OrderState::Publishing);
    o.transition(OrderEvent::PublishConfirmed { pr_url: "u".into() }, 1).unwrap();
    assert_eq!(o.state(), OrderState::Shipped);
}

#[test]
fn retry_path_working_back_to_queued_then_failed_on_exhaustion() {
    let mut o = order();
    o.transition(OrderEvent::Queued, 0).unwrap();
    o.transition(OrderEvent::DieStarted, 1).unwrap();
    o.transition(
        OrderEvent::DieFailed { attempt: 1, reason: "timeout".into() },
        1,
    )
    .unwrap();
    assert_eq!(o.state(), OrderState::Queued); // retry: back to queue
    assert_eq!(o.attempt(), 2);

    o.transition(OrderEvent::DieStarted, 2).unwrap();
    o.transition(
        OrderEvent::DieFailed { attempt: 2, reason: "timeout".into() },
        2,
    )
    .unwrap();
    assert_eq!(o.state(), OrderState::Queued);
    assert_eq!(o.attempt(), 3);

    o.transition(OrderEvent::DieStarted, 3).unwrap();
    o.transition(
        OrderEvent::DieFailed { attempt: 3, reason: "final".into() },
        3,
    )
    .unwrap();
    // max attempts (3) exhausted
    assert_eq!(o.state(), OrderState::Failed);
}

// ---- attempt fencing ----

#[test]
fn stale_die_result_is_rejected() {
    let mut o = order();
    o.transition(OrderEvent::Queued, 0).unwrap();
    o.transition(OrderEvent::DieStarted, 1).unwrap();
    // Die retried (attempt 2 now) but attempt-1 result arrives late
    o.transition(
        OrderEvent::DieFailed { attempt: 1, reason: "late".into() },
        1,
    )
    .unwrap(); // legal: this IS the retry transition
    assert_eq!(o.attempt(), 2);
    // late success report from attempt 1 must NOT be accepted
    let err = o.transition(OrderEvent::DieSucceeded { attempt: 1 }, 2);
    assert!(err.is_err(), "stale attempt-1 success must be rejected");
    assert_eq!(o.state(), OrderState::Queued); // unchanged
}

#[test]
fn die_succeeded_from_wrong_attempt_rejected_in_working() {
    let mut o = order();
    o.transition(OrderEvent::Queued, 0).unwrap();
    o.transition(OrderEvent::DieStarted, 1).unwrap();
    let err = o.transition(OrderEvent::DieSucceeded { attempt: 2 }, 1);
    assert!(err.is_err());
    assert_eq!(o.state(), OrderState::Working);
}

// ---- 비합법 전이 (부정 테이블) ----

#[test]
fn illegal_transitions_are_rejected() {
    struct Case(OrderEvent, &'static str);
    let cases = vec![
        // FurnaceHold → Shipped 직행 금지 (v3 핵심 수정)
        Case(OrderEvent::PublishConfirmed { pr_url: "u".into() }, "skip approval"),
    ];
    for Case(ev, why) in cases {
        let mut o = order();
        o.transition(OrderEvent::Queued, 0).unwrap();
        o.transition(OrderEvent::DieStarted, 1).unwrap();
        o.transition(OrderEvent::DieSucceeded { attempt: 1 }, 1).unwrap();
        o.transition(OrderEvent::InspectionPassed, 1).unwrap();
        assert_eq!(o.state(), OrderState::FurnaceHold);
        let err = o.transition(ev.clone(), 1);
        assert!(err.is_err(), "must reject: {why}");
        assert_eq!(o.state(), OrderState::FurnaceHold);
    }
}

#[test]
fn terminal_states_accept_nothing() {
    for final_setup in 0..3 {
        let mut o = order();
        o.transition(OrderEvent::Queued, 0).unwrap();
        match final_setup {
            0 => {
                // reach Failed
                for a in 1..=3 {
                    o.transition(OrderEvent::DieStarted, a).unwrap();
                    o.transition(OrderEvent::DieFailed { attempt: a, reason: "x".into() }, a).unwrap();
                }
                assert_eq!(o.state(), OrderState::Failed);
            }
            1 => {
                // reach Rejected
                o.transition(OrderEvent::DieStarted, 1).unwrap();
                o.transition(OrderEvent::DieSucceeded { attempt: 1 }, 1).unwrap();
                o.transition(OrderEvent::InspectionPassed, 1).unwrap();
                o.transition(OrderEvent::Rejected { intent: "no".into() }, 1).unwrap();
                assert_eq!(o.state(), OrderState::Rejected);
            }
            _ => {
                o.transition(OrderEvent::Canceled { reason: "closed".into() }, 0).unwrap();
                assert_eq!(o.state(), OrderState::Canceled);
            }
        }
        let s = o.state();
        let err = o.transition(OrderEvent::Queued, o.attempt());
        assert!(err.is_err(), "terminal {s:?} must not transition");
        assert_eq!(o.state(), s);
    }
}

#[test]
fn approval_requires_digest() {
    let mut o = order();
    o.transition(OrderEvent::Queued, 0).unwrap();
    o.transition(OrderEvent::DieStarted, 1).unwrap();
    o.transition(OrderEvent::DieSucceeded { attempt: 1 }, 1).unwrap();
    o.transition(OrderEvent::InspectionPassed, 1).unwrap();
    // empty digest = no provenance binding → reject
    let err = o.transition(
        OrderEvent::Approved { intent: "ok".into(), artifact_digest: String::new() },
        1,
    );
    assert!(err.is_err(), "approval without artifact digest must be rejected");
}

// ---- Unknown / reconcile ----

#[test]
fn publishing_unknown_then_reconciled() {
    let mut o = order();
    o.transition(OrderEvent::Queued, 0).unwrap();
    o.transition(OrderEvent::DieStarted, 1).unwrap();
    o.transition(OrderEvent::DieSucceeded { attempt: 1 }, 1).unwrap();
    o.transition(OrderEvent::InspectionPassed, 1).unwrap();
    o.transition(OrderEvent::Approved { intent: "ok".into(), artifact_digest: "d".into() }, 1).unwrap();
    // publish attempt crashed before response
    o.transition(OrderEvent::PublishUnknown { operation_id: "op-1".into() }, 1).unwrap();
    assert_eq!(o.state(), OrderState::Unknown);
    // reconcile finds the PR actually exists
    o.transition(OrderEvent::PublishConfirmed { pr_url: "https://x".into() }, 1).unwrap();
    assert_eq!(o.state(), OrderState::Shipped);
}

// ---- Canceling ----

#[test]
fn canceling_from_working_then_canceled() {
    let mut o = order();
    o.transition(OrderEvent::Queued, 0).unwrap();
    o.transition(OrderEvent::DieStarted, 1).unwrap();
    o.transition(OrderEvent::CancelRequested { reason: "issue closed".into() }, 1).unwrap();
    assert_eq!(o.state(), OrderState::Canceling);
    o.transition(OrderEvent::CancelConfirmed, 1).unwrap();
    assert_eq!(o.state(), OrderState::Canceled);
}

#[test]
fn direct_cancel_allowed_from_received_and_queued_only() {
    let mut o = order();
    o.transition(OrderEvent::Canceled { reason: "r".into() }, 0).unwrap();
    assert_eq!(o.state(), OrderState::Canceled);

    let mut o2 = order();
    o2.transition(OrderEvent::Queued, 0).unwrap();
    o2.transition(OrderEvent::Canceled { reason: "r".into() }, 0).unwrap();
    assert_eq!(o2.state(), OrderState::Canceled);

    // Working must go through Canceling
    let mut o3 = order();
    o3.transition(OrderEvent::Queued, 0).unwrap();
    o3.transition(OrderEvent::DieStarted, 1).unwrap();
    assert!(o3.transition(OrderEvent::Canceled { reason: "r".into() }, 1).is_err());
}

// ---- state parsing (칸반 칼럼용) ----

#[test]
fn state_names_round_trip() {
    for s in [
        OrderState::Received, OrderState::Queued, OrderState::Working,
        OrderState::Inspecting, OrderState::FurnaceHold, OrderState::Publishing,
        OrderState::Unknown, OrderState::Shipped, OrderState::Failed,
        OrderState::Rejected, OrderState::Canceling, OrderState::Canceled,
    ] {
        assert_eq!(OrderState::from_str(&s.to_string()).unwrap(), s);
    }
}
