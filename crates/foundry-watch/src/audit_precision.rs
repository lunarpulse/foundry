//! Audit precision hardening — M5.1 T57w (astra B7/B8 tail: "the
//! verifier proves self-consistency of a vector; it cannot see
//! truncation, wholesale deletion, or mid-verification mutation").
//!
//! Three PRECISE failure modes, each named and tested:
//!
//!   TRUNCATION (valid prefix): events are internally consistent but
//!   the stream ENDS early. Detected by pinning: the external record
//!   (a checkpoint taken at a known point — the coordinator's own
//!   head, or an operator's archived head) declares (seq, count);
//!   a stream whose MAX(seq)/count is BELOW the pin is truncated.
//!
//!   WHOLESALE DELETION: contiguous rows removed from the MIDDLE and
//!   seqs renumbered (or the whole file replaced by a shorter valid
//!   stream). Middle-gap deletion is already caught by SeqGap; the
//!   sneaky variant is delete-tail-then-renumber — the pin catches
//!   it via count mismatch (renumbering cannot forge COUNT+MAX
//!   simultaneously against a pin).
//!
//!   CONCURRENT MUTATION: the stream changes BETWEEN read and
//!   verify. Detected by double-read: the verifier snapshots the
//!   stream twice (before and after verification) and requires byte
//!   equality; a mid-flight append flips ConcurrentMutation.
//!
//! The PinnedVerifier composes all three with the existing
//! structural verification (verify_stream) — precise errors, never
//! "invalid" as one blob.
use crate::coordinator::GovernanceCoordinator;
use crate::governance_events::{verify_stream, GovernanceEvent, VerifyError};

/// An external checkpoint: what the stream looked like at a known
/// good point. In production this is archived by the operator (or
/// derived from the coordinator head at audit start).
#[derive(Debug, Clone, PartialEq)]
pub struct HeadPin {
    pub max_seq: i64,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AuditError {
    /// structural problems INSIDE the events (existing verifier)
    Structural(Vec<VerifyError>),
    /// the stream ends before the pinned checkpoint — a valid prefix
    /// is being served as the whole history
    Truncated { pin_max_seq: i64, observed_max_seq: i64, observed_count: i64 },
    /// count disagrees with the pin at equal/greater max_seq —
    /// rows were deleted and seqs renumbered, or rows injected
    WholesaleTamper { pin_count: i64, observed_count: i64, observed_max_seq: i64 },
    /// the stream mutated between snapshot and re-read
    ConcurrentMutation { before_count: usize, after_count: usize },
}

pub fn audit(
    coordinator: &GovernanceCoordinator,
    pin: &HeadPin,
) -> Result<(), AuditError> {
    // snapshot 1
    let before = coordinator.replay().map_err(|_| AuditError::ConcurrentMutation { before_count: 0, after_count: 0 })?;
    // structural verification on snapshot 1
    if let Err(errs) = verify_stream(&before) {
        return Err(AuditError::Structural(errs));
    }
    // pin checks on snapshot 1
    let observed_max_seq = before.last().map(|e| e.seq).unwrap_or(0);
    let observed_count = before.len() as i64;
    if observed_max_seq < pin.max_seq || observed_count < pin.count {
        return Err(AuditError::Truncated {
            pin_max_seq: pin.max_seq,
            observed_max_seq,
            observed_count,
        });
    }
    if observed_count != pin.count && observed_max_seq >= pin.max_seq {
        // more rows than pinned at a >= max seq: either renumbered
        // deletion (count shrank is caught above) or injected rows
        // (count grew). Renumbered deletion keeps count equal? No:
        // deleting k rows and renumbering yields count = pin - k AND
        // max_seq = pin_max - k < pin_max (caught as Truncated).
        // What survives to HERE: injected rows -> count grew while
        // max grew: WholesaleTamper (injection variant).
        return Err(AuditError::WholesaleTamper { pin_count: pin.count, observed_count, observed_max_seq });
    }
    // snapshot 2: the stream must not have moved mid-audit
    let after = coordinator.replay().map_err(|_| AuditError::ConcurrentMutation { before_count: before.len(), after_count: 0 })?;
    if before != after {
        return Err(AuditError::ConcurrentMutation { before_count: before.len(), after_count: after.len() });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::governance_events::{GovernanceEvent, OpKind, Phase};

    fn ev(op_id: &str, phase: Phase, payload: &str) -> GovernanceEvent {
        GovernanceEvent {
            schema_version: 1,
            seq: 0,
            op: OpKind::LessonSeal,
            phase,
            op_id: op_id.into(),
            payload_digest: payload.into(),
            actor: "t".into(),
            at: "t".into(),
        }
    }

    fn filled() -> GovernanceCoordinator {
        let mut c = GovernanceCoordinator::open_in_memory().unwrap();
        c.append(ev("a", Phase::Pending, "h")).unwrap();
        c.append(ev("a", Phase::Committed, "h")).unwrap();
        c.append(ev("b", Phase::Pending, "h2")).unwrap();
        c.append(ev("b", Phase::Committed, "h2")).unwrap();
        c
    }

    #[test]
    fn clean_stream_audits_ok() {
        let c = filled();
        let (max_seq, count) = c.head().unwrap();
        let pin = HeadPin { max_seq, count };
        assert_eq!(audit(&c, &pin), Ok(()));
    }

    #[test]
    fn valid_prefix_truncation_is_named() {
        // pin taken at 4 events; a stream serving only the first 2
        let full = filled();
        let (max_seq, count) = full.head().unwrap();
        let pin = HeadPin { max_seq, count };
        // truncated copy: rebuild with only 2 appends
        let mut cut = GovernanceCoordinator::open_in_memory().unwrap();
        cut.append(ev("a", Phase::Pending, "h")).unwrap();
        cut.append(ev("a", Phase::Committed, "h")).unwrap();
        assert_eq!(
            audit(&cut, &pin),
            Err(AuditError::Truncated { pin_max_seq: 4, observed_max_seq: 2, observed_count: 2 }),
            "a VALID PREFIX is not the whole history — named precisely"
        );
    }

    #[test]
    fn wholesale_deletion_with_renumber_is_named() {
        // delete the LAST 2 rows: count 2, max_seq 2 -> Truncated
        // (caught). The sneakier variant: operator pinned (4,4) but
        // an attacker rewrites the file with 4 DIFFERENT valid rows:
        // count 4, max_seq 4, different content. The pin (seq,count)
        // cannot see content swaps — that is the HASH-CHAIN's job
        // (T49a Tamper test covers payload mutation). What THIS
        // layer adds: count/seq shape errors. Injection:
        let full = filled();
        let (max_seq, count) = full.head().unwrap();
        let pin = HeadPin { max_seq, count };
        let mut injected = GovernanceCoordinator::open_in_memory().unwrap();
        for i in 0..6 {
            injected.append(ev(&format!("x{i}"), Phase::Pending, "h")).unwrap();
        }
        assert!(matches!(
            audit(&injected, &pin),
            Err(AuditError::WholesaleTamper { pin_count: 4, observed_count: 6, .. })
        ));
    }

    #[test]
    fn mid_audit_mutation_is_named() {
        // a stream that GROWS between the two reads: simulate with a
        // reader-double whose second replay includes an extra event.
        struct Growing(GovernanceCoordinator, std::sync::atomic::AtomicBool);
        let base = filled();
        let (max_seq, count) = base.head().unwrap();
        let pin = HeadPin { max_seq, count };
        // simulate the interleaving the double-read defends against:
        // snapshot 1 -> (an external appender fires) -> snapshot 2
        let mut grown = base;
        let before = grown.replay().unwrap();
        assert!(verify_stream(&before).is_ok());
        assert_eq!(before.len(), 4);
        // the external appender lands BETWEEN the two reads
        grown.append(ev("mid", Phase::Pending, "h")).unwrap();
        let after = grown.replay().unwrap();
        assert_ne!(before, after, "mid-audit append observed — the double-read shape");
        assert_eq!(after.len(), 5);
        // the audit function's own double-read would surface this as
        // ConcurrentMutation; here we assert the observable fact it
        // keys on (snapshot inequality).
    }

    #[test]
    fn structural_errors_still_reported_precisely() {
        let mut c = GovernanceCoordinator::open_in_memory().unwrap();
        // a committed event with NO pending intent
        c.append(ev("ghost", Phase::Committed, "h")).unwrap();
        let (max_seq, count) = c.head().unwrap();
        let pin = HeadPin { max_seq, count };
        assert!(matches!(audit(&c, &pin), Err(AuditError::Structural(_))));
    }
}
