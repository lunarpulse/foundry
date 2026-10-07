//! PublishUnknown re-adjudication — T21 (M2).
//!
//! Publish can fail ambiguously (network drop mid-POST): the journal
//! records PublishUnknown and the order parks in `unknown`. The
//! operation_id ("{order_id}:{attempt}") is idempotent at GitHub, so
//! the truth EXISTS — we just have to ask. This module re-adjudicates:
//!
//!   PR open with our operation_id  -> PublishConfirmed -> Shipped
//!   PR closed/merged               -> PublishConfirmed -> Shipped
//!   PR not found (404)             -> the POST never landed; safe to
//!                                     re-publish (idempotent op id)
//!   query itself fails             -> stays Unknown (never guess)
//!
//! Real GitHub adapter comes in M3-prep; T21 ships the DECISION LOGIC
//! plus a mock transport so the taxonomy is testable now (plan v2,
//! review: mock-limited scope).
use foundry_domain::OrderEvent;

/// What the transport reported about an operation_id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrLookup {
    Found {
        pr_url: String,
        merged: bool,
    },
    NotFound,
    /// query failed — do NOT adjudicate
    QueryFailed {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readjudication {
    /// PR exists (merged or open) — the publish DID land.
    Confirm { pr_url: String },
    /// No PR anywhere with our operation_id — publish never landed;
    /// caller may re-attempt publish (same operation_id = idempotent).
    Republish,
    /// Unknown persists. Never guess on a failed query.
    StayUnknown { reason: String },
}

/// Pure decision from a lookup result.
pub fn readjudicate(lookup: &PrLookup) -> Readjudication {
    match lookup {
        PrLookup::Found { pr_url, .. } => Readjudication::Confirm {
            pr_url: pr_url.clone(),
        },
        PrLookup::NotFound => Readjudication::Republish,
        PrLookup::QueryFailed { reason } => Readjudication::StayUnknown {
            reason: reason.clone(),
        },
    }
}

/// The journal event for a confirmation (merged or open — both are
/// "the publish happened"; merge state is a downstream concern).
pub fn confirm_event(r: &Readjudication) -> Option<OrderEvent> {
    match r {
        Readjudication::Confirm { pr_url } => Some(OrderEvent::PublishConfirmed {
            pr_url: pr_url.clone(),
        }),
        _ => None,
    }
}

/// Mock transport for tests/benchmark (T21 ships decision logic only).
pub struct MockGithub {
    pub state: PrLookup,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn found_confirms() {
        let l = PrLookup::Found {
            pr_url: "https://github.com/o/r/pull/138".into(),
            merged: true,
        };
        assert_eq!(
            readjudicate(&l),
            Readjudication::Confirm {
                pr_url: "https://github.com/o/r/pull/138".into()
            }
        );
        let ev = confirm_event(&readjudicate(&l)).unwrap();
        assert!(matches!(ev, OrderEvent::PublishConfirmed { .. }));
    }

    #[test]
    fn not_found_republish() {
        assert_eq!(readjudicate(&PrLookup::NotFound), Readjudication::Republish);
        // no event: the caller re-attempts publish instead
        assert!(confirm_event(&Readjudication::Republish).is_none());
    }

    #[test]
    fn query_failure_never_guesses() {
        let r = readjudicate(&PrLookup::QueryFailed {
            reason: "502".into(),
        });
        assert!(matches!(r, Readjudication::StayUnknown { .. }));
        assert!(confirm_event(&r).is_none());
    }
}
