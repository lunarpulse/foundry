use chrono::{TimeZone, Utc};
use foundry_exec::budget::{Budget, ReservationOutcome, reserve};
use foundry_exec::types::*;

#[test]
fn carry_over_deducts_settlement_from_carried_funds() {
    let budget = Budget {
        reserved_nano: 10,
        settled_nano: 11,
        carried_nano: 2,
    };
    assert_eq!(budget.carry_over().carried_nano, 1);

    let exhausted = Budget { settled_nano: 13, ..budget };
    assert_eq!(reserve(&exhausted.carry_over(), 1), ReservationOutcome::Exhausted);
}

#[test]
fn budget_subtracts_spend_before_clamping_the_total() {
    let budget = Budget {
        reserved_nano: u128::MAX,
        settled_nano: u128::MAX,
        carried_nano: u128::MAX,
    };
    assert_eq!(
        reserve(&budget, u128::MAX),
        ReservationOutcome::Granted { remaining_nano: 0 }
    );
}

#[test]
fn carry_over_preserves_available_budget_at_boundaries() {
    let values = [0, 1, 2, u128::MAX - 1, u128::MAX];
    for reserved_nano in values {
        for settled_nano in values {
            for carried_nano in values {
                let budget = Budget { reserved_nano, settled_nano, carried_nano };
                assert_eq!(
                    reserve(&budget, 0),
                    reserve(&budget.carry_over(), 0),
                    "availability changed after carry-over: {budget:?}"
                );
                assert_eq!(budget.carry_over().carry_over(), budget.carry_over());
            }
        }
    }
}

fn session_receipt(restored_from: Option<String>) -> SessionReceipt {
    SessionReceipt {
        session_id: "session".into(),
        order: OrderId("order-1".into()),
        run: RunId(2),
        restored_from,
        at: Utc.with_ymd_and_hms(2026, 10, 14, 12, 0, 0).unwrap(),
    }
}

#[test]
fn session_digest_distinguishes_absent_and_empty_restore_source() {
    let absent = session_receipt(None);
    let empty = session_receipt(Some(String::new()));
    assert_ne!(absent, empty);
    assert_ne!(absent.digest(), empty.digest());
    assert_eq!(empty.digest(), empty.clone().digest());
}

#[test]
fn receipt_wrapper_rejects_unknown_fields_inside_an_outcome() {
    let mut value = serde_json::to_value(ExecutionOutcome {
        intent_key: (OrderId("order-1".into()), Attempt(1), RunId(2)),
        cursor: None,
        next_activation: None,
        receipts: vec![Receipt::Session(session_receipt(None))],
    }).unwrap();
    value["receipts"][0]["unrecognized"] = serde_json::json!(true);
    assert!(serde_json::from_value::<ExecutionOutcome>(value).is_err());
}
