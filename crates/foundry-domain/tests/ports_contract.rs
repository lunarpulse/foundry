//! T3 — port trait contracts (compile-time assertions + decision semantics).
use foundry_domain::ports::*;
use foundry_domain::{Order, OrderState};

// Policy must distinguish Allow/Deny with reasons (critique fix: no bare bool).
struct OwnerPolicy;
impl Policy for OwnerPolicy {
    fn authorize(&self, actor: &ActorId, _action: &str, _resource: &str) -> Decision {
        if actor.kind == ActorKind::Human {
            Decision::Allow { reason: "local owner".into() }
        } else {
            Decision::Deny { reason: "agent self-approval forbidden".into() }
        }
    }
}

#[test]
fn policy_forbids_agent_self_approval() {
    let p = OwnerPolicy;
    let human = ActorId { kind: ActorKind::Human, source: "local".into(), id: "luna".into() };
    let agent = ActorId { kind: ActorKind::Agent, source: "rustain".into(), id: "die-1".into() };
    assert!(matches!(p.authorize(&human, "approve", "o-1"), Decision::Allow { .. }));
    assert!(matches!(p.authorize(&agent, "approve", "o-1"), Decision::Deny { .. }));
}

// PortError taxonomy: Retryable vs Permanent is the outbox policy driver.
#[test]
fn port_error_taxonomy() {
    let r: PortResult<()> = Err(PortError::Retryable("timeout".into()));
    let p: PortResult<()> = Err(PortError::Permanent("bad request".into()));
    assert!(matches!(r, Err(PortError::Retryable(_))));
    assert!(matches!(p, Err(PortError::Permanent(_))));
}

// PublishOutcome models the crash-between-call-and-response case (critique #2).
#[test]
fn publish_outcome_has_unknown_variant() {
    let confirmed = PublishOutcome::Confirmed { pr_url: "https://gh/pr/1".into() };
    let unknown = PublishOutcome::Unknown { operation_id: "op-1".into() };
    assert!(matches!(confirmed, PublishOutcome::Confirmed { .. }));
    assert!(matches!(unknown, PublishOutcome::Unknown { .. }));
}

// ApprovalToken binds order+intent+digest+actor (plan §11 integrity).
#[test]
fn approval_token_binds_provenance() {
    let t = ApprovalToken {
        order_id: "o-1".into(),
        intent: "safe dep bump".into(),
        artifact_digest: "deadbeef".into(),
        actor: ActorId { kind: ActorKind::Human, source: "local".into(), id: "luna".into() },
        granted_at: foundry_domain::order::now(),
    };
    assert_eq!(t.artifact_digest, "deadbeef");
}

// DieOutput carries provenance, not just exit code (plan §1 [MAJOR] fix).
#[test]
fn die_output_carries_provenance() {
    let d = DieOutput {
        attempt: 1,
        base_commit: "aaa".into(),
        result_commit: "bbb".into(),
        log_path: "/tmp/log".into(),
        exit_code: 0,
    };
    assert_eq!(d.attempt, 1);
    assert!(!d.result_commit.is_empty());
}

// Order state still compiles against the ports module.
#[test]
fn order_sanity() {
    let o = Order::new(
        "o-9".into(),
        foundry_domain::ExternalRef::cron("dep", "serde"),
        foundry_domain::order::now(),
    );
    assert_eq!(o.state(), OrderState::Received);
}
