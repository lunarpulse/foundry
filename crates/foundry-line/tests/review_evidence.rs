use foundry_domain::{ActorId, ActorKind, ExternalRef, Order, OrderEvent, OrderState};
use foundry_domain::ports::Ledger;
use foundry_exec::{ArtifactLocation, ArtifactTuple, Attempt, EmitReceipt, Generation, ImmutableCommit, OrderId, RunId, SqliteTupleStore, TupleStore};
use foundry_line::rehydrate::{rehydrate, RehydrateError};

#[test]
fn rehydrate_requires_latest_evidence_for_publishing_unknown_and_shipped() {
    let root = std::env::temp_dir().join(format!("t31-rehydrate-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    for terminal in [OrderState::Publishing, OrderState::Unknown, OrderState::Shipped] {
        let path = root.join(format!("{terminal}.db"));
        let mut ledger = foundry_ledger::SqliteLedger::open(&path).unwrap();
        let mut order = Order::new("review-rehydrate".into(), ExternalRef::cron("review", "test"), chrono::Utc::now());
        let actor = ActorId { kind: ActorKind::Agent, source: "test".into(), id: "review".into() };
        let mut events = vec![OrderEvent::Queued, OrderEvent::DieStarted,
            OrderEvent::DieSucceeded { attempt: 1 }, OrderEvent::InspectionPassed,
            OrderEvent::Approved { intent: "verified".into(), artifact_digest: "digest".into() }];
        if terminal == OrderState::Unknown {
            events.push(OrderEvent::PublishUnknown { operation_id: "review-rehydrate:1".into() });
        } else if terminal == OrderState::Shipped {
            events.push(OrderEvent::PublishConfirmed { pr_url: "https://example.test/pr/1".into() });
        }
        for event in events {
            let from = order.state();
            let to = order.transition(event.clone(), 1).unwrap();
            ledger.append(&order.id, &event, from, to, &actor, chrono::Utc::now()).unwrap();
        }
        let tuples = SqliteTupleStore::open(root.join(format!("{terminal}-tuples.db"))).unwrap();
        assert!(matches!(rehydrate(&ledger, &root, &tuples), Err(RehydrateError::ReviewEvidenceUnavailable { .. })));
        let tuple = |run, digest: &str| ArtifactTuple {
            order: OrderId(order.id.clone()), attempt: Attempt(1), run: RunId(run), generation: Generation(0),
            immutable_commit: ImmutableCommit(digest.into()), artifact_location: ArtifactLocation("review.log".into()),
        };
        let approved = tuple(1, "digest");
        tuples.record(&approved).unwrap();
        tuples.promote(&approved.key(), &EmitReceipt { key: approved.key(), event: "publish".into(), at: chrono::Utc::now() }).unwrap();
        let restored = rehydrate(&ledger, &root, &tuples).unwrap();
        assert_eq!(restored[0].state(), terminal);
        tuples.record(&tuple(2, "other")).unwrap();
        let newer = tuple(2, "other");
        tuples.promote(&newer.key(), &EmitReceipt { key: newer.key(), event: "publish".into(), at: chrono::Utc::now() }).unwrap();
        assert!(matches!(rehydrate(&ledger, &root, &tuples), Err(RehydrateError::ReviewEvidenceUnavailable { .. })));
    }
    std::fs::remove_dir_all(&root).unwrap();
}
