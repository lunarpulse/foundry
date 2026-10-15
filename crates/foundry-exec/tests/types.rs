use chrono::{TimeZone, Utc};
use foundry_exec::budget::{Budget, ReservationOutcome, reserve};
use foundry_exec::hold::{HoldCasOutcome, HoldCommand, HoldInstanceId, HoldOperation, cas_hold};
use foundry_exec::types::*;

fn tuple() -> ArtifactTuple {
    ArtifactTuple {
        order: OrderId("order-1".into()),
        attempt: Attempt(1),
        run: RunId(2),
        generation: Generation(3),
        immutable_commit: ImmutableCommit("commit-abc".into()),
        artifact_location: ArtifactLocation("artifacts/result".into()),
    }
}

#[test]
fn tuple_digest_is_stable_and_covers_every_field() {
    let original = tuple();
    assert_eq!(original.digest(), original.clone().digest());

    let mut changed = original.clone();
    changed.order = OrderId("order-2".into());
    assert_ne!(original.digest(), changed.digest());
    let mut changed = original.clone();
    changed.attempt = Attempt(4);
    assert_ne!(original.digest(), changed.digest());
    let mut changed = original.clone();
    changed.run = RunId(5);
    assert_ne!(original.digest(), changed.digest());
    let mut changed = original.clone();
    changed.generation = Generation(6);
    assert_ne!(original.digest(), changed.digest());
    let mut changed = original.clone();
    changed.immutable_commit = ImmutableCommit("commit-def".into());
    assert_ne!(original.digest(), changed.digest());
    let mut changed = original.clone();
    changed.artifact_location = ArtifactLocation("artifacts/other".into());
    assert_ne!(original.digest(), changed.digest());
}

#[test]
fn fence_classifies_old_tokens_as_stale_and_current_or_future_as_current() {
    assert_eq!(fence(Epoch(5), Epoch(5)), FenceVerdict::Current);
    assert_eq!(fence(Epoch(5), Epoch(4)), FenceVerdict::Stale(Epoch(4)));
    assert_eq!(fence(Epoch(5), Epoch(6)), FenceVerdict::Current);
}

#[test]
fn budget_reserves_and_carries_without_refunding_settled_spend() {
    let budget = Budget {
        reserved_nano: 10,
        settled_nano: 3,
        carried_nano: 2,
    };
    assert_eq!(
        reserve(&budget, 8),
        ReservationOutcome::Granted { remaining_nano: 1 }
    );
    assert_eq!(reserve(&budget, 10), ReservationOutcome::Exhausted);
    assert_eq!(
        budget.carry_over(),
        Budget {
            reserved_nano: 0,
            settled_nano: 0,
            carried_nano: 9
        }
    );
    assert_eq!(
        reserve(
            &Budget {
                reserved_nano: u128::MAX,
                settled_nano: 0,
                carried_nano: u128::MAX
            },
            1
        ),
        ReservationOutcome::Granted {
            remaining_nano: u128::MAX - 1
        },
    );
}

fn hold_instance(run: u64) -> HoldInstanceId {
    HoldInstanceId {
        order: OrderId("order-1".into()),
        attempt: Attempt(1),
        run: RunId(run),
        hold_node: "approval".into(),
        entry: 0,
    }
}

#[test]
fn hold_cas_applies_matching_instance_and_reports_conflict() {
    let expected = hold_instance(2);
    let matching = HoldCommand {
        instance: expected.clone(),
        op: HoldOperation::Approve,
        human_resolution: Some("approved".into()),
        evidence: "evidence".into(),
    };
    assert_eq!(cas_hold(&expected, &matching), HoldCasOutcome::Applied);
    let stale = HoldCommand {
        instance: hold_instance(3),
        ..matching
    };
    assert_eq!(
        cas_hold(&expected, &stale),
        HoldCasOutcome::Conflict {
            observed_instance: hold_instance(3)
        },
    );
}

#[test]
fn serde_round_trips_execution_outcome_tuple_and_session_receipt() {
    let at = Utc.with_ymd_and_hms(2026, 10, 14, 12, 0, 0).unwrap();
    let tuple = tuple();
    let outcome = ExecutionOutcome {
        intent_key: (OrderId("order-1".into()), Attempt(1), RunId(2)),
        activation: 0,
        cursor: Some("cursor".into()),
        next_activation: Some(ActivationRef {
            node: "next".into(),
            inputs_digest: "abc".into(),
        }),
        receipts: vec![Receipt::Session(SessionReceipt {
            session_id: "session".into(),
            order: OrderId("order-1".into()),
            run: RunId(2),
            restored_from: Some("prior".into()),
            at,
        })],
    };
    let receipt = SessionReceipt {
        session_id: "session".into(),
        order: OrderId("order-1".into()),
        run: RunId(2),
        restored_from: None,
        at,
    };
    assert_eq!(
        serde_json::from_str::<ExecutionOutcome>(&serde_json::to_string(&outcome).unwrap())
            .unwrap(),
        outcome
    );
    assert_eq!(
        serde_json::from_str::<ArtifactTuple>(&serde_json::to_string(&tuple).unwrap()).unwrap(),
        tuple
    );
    assert_eq!(
        serde_json::from_str::<SessionReceipt>(&serde_json::to_string(&receipt).unwrap()).unwrap(),
        receipt
    );
}

#[test]
fn receipt_digests_have_separate_domains() {
    let t = tuple();
    let key = t.key();
    let at = Utc.with_ymd_and_hms(2026, 10, 14, 12, 0, 0).unwrap();
    assert!(
        EmitReceipt {
            key: key.clone(),
            event: "done".into(),
            at
        }
        .digest()
        .starts_with("fx-emit-")
    );
    assert!(
        PromotionReceipt {
            tuple: key,
            promoted_at: at
        }
        .digest()
        .starts_with("fx-promo-")
    );
    assert!(
        SessionReceipt {
            session_id: "s".into(),
            order: OrderId("order-1".into()),
            run: RunId(2),
            restored_from: None,
            at,
        }
        .digest()
        .starts_with("fx-sess-")
    );
}

#[test]
fn unknown_fields_are_rejected() {
    let outcome = ExecutionOutcome {
        intent_key: (OrderId("order-1".into()), Attempt(1), RunId(2)),
        activation: 0,
        cursor: None,
        next_activation: None,
        receipts: vec![],
    };
    let mut value = serde_json::to_value(outcome).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("evil".into(), serde_json::json!(true));

    assert!(serde_json::from_value::<ExecutionOutcome>(value).is_err());
}

#[test]
fn tuple_key_distinguishes_content_from_identity() {
    let original = tuple();
    let mut changed_content = original.clone();
    changed_content.immutable_commit = ImmutableCommit("commit-def".into());
    changed_content.artifact_location = ArtifactLocation("artifacts/other".into());

    assert_eq!(original.key(), changed_content.key());
    assert_ne!(original.digest(), changed_content.digest());
}

#[test]
fn carry_over_is_idempotent() {
    let budget = Budget {
        reserved_nano: 37,
        settled_nano: 12,
        carried_nano: 9,
    };

    assert_eq!(budget.carry_over().carry_over(), budget.carry_over());
}

#[test]
fn reserve_exact_boundary_grants_zero_remaining() {
    let budget = Budget {
        reserved_nano: 10,
        settled_nano: 3,
        carried_nano: 2,
    };

    assert_eq!(
        reserve(&budget, 9),
        ReservationOutcome::Granted { remaining_nano: 0 }
    );
}

#[test]
fn over_settled_budget_saturates_to_zero() {
    let budget = Budget {
        reserved_nano: 10,
        settled_nano: 13,
        carried_nano: 2,
    };

    assert_eq!(reserve(&budget, 1), ReservationOutcome::Exhausted);
}
