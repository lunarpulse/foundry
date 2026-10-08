//! No-op port impls for serve's AppLine (T27).
//!
//! serve NEVER executes a Die and NEVER publishes — its line exists
//! solely so the approve action goes through the SAME
//! gate -> policy -> ledger-commit path as the CLI. Every capability
//! that must not happen from serve returns a Permanent error instead
//! of silently succeeding.
use foundry_domain::Order;
use foundry_domain::ports::Decision;
use foundry_domain::ports::Policy;
use foundry_domain::ports::{DieOutput, Harness, PortResult, PublishOutcome, Publisher};
use std::future::Future;
use std::path::Path;

pub struct ServeGate;
impl foundry_domain::ports::Gate for ServeGate {
    fn verify(&self, _o: &Order, _t: &foundry_domain::ApprovalToken, _d: &str) -> PortResult<()> {
        Ok(()) // same semantics as the CLI DigestGate (M0)
    }
}

pub struct NoopHarness;
impl Harness for NoopHarness {
    fn run(
        &self,
        _order: &Order,
        _attempt: u32,
        _workdir: &Path,
    ) -> impl Future<Output = PortResult<DieOutput>> + Send {
        async {
            Err(foundry_domain::ports::PortError::Permanent(
                "serve cannot execute Dies — use watch".into(),
            ))
        }
    }
}

pub struct NoopPublisher;
impl Publisher for NoopPublisher {
    fn publish(
        &self,
        _order: &Order,
        _operation_id: &str,
        _digest: &str,
    ) -> impl Future<Output = PortResult<PublishOutcome>> + Send {
        async {
            Err(foundry_domain::ports::PortError::Permanent(
                "serve does not publish — use watch/CLI publish".into(),
            ))
        }
    }
}

pub struct AllowHumanPolicy;
impl Policy for AllowHumanPolicy {
    fn authorize(&self, actor: &foundry_domain::ActorId, action: &str, _order: &str) -> Decision {
        if action == "approve" && actor.kind == foundry_domain::ActorKind::Human {
            Decision::Allow {
                reason: "human approve on serve".into(),
            }
        } else {
            Decision::Deny {
                reason: format!(
                    "serve policy: only human approve, got {action} from {:?}",
                    actor.kind
                ),
            }
        }
    }
}
