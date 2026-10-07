//! Inspector — the verdict layer between Die and FurnaceHold.
//!
//! T19 (M2, defect #8): a Die that "succeeds" with an EMPTY diff used to
//! pass through to FurnaceHold and break publish with 422 (live E2E
//! finding). The no-op taxonomy (plan v2, reviews #5/#6) splits the two
//! meanings of "nothing happened":
//!
//!   PreflightNoWork        — there was nothing to do (dep already at
//!                            crates.io latest). NOT a quality failure.
//!                            → OrderRejected { stage, reason: NoOp } →
//!                              terminal Rejected. Does not pollute
//!                              failure metrics.
//!   ExpectedChangeButEmptyDiff — change was REQUIRED, the Die produced
//!                            none. A real quality failure of the worker.
//!                            → InspectionFailed { reason: no-op } →
//!                              terminal Failed. Counted in regression
//!                              metrics (M3 Casting must see this axis).
//!
//! `Inspector::inspect` is pure over (order, DieOutput, diff_stats) so
//! benchmark replay re-derives the same verdict deterministically.
use crate::{Order, OrderEvent};

/// Diff evidence the Inspector rules on (harness fills; replay derives).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffStats {
    pub files_changed: u32,
    pub lines_changed: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    /// Nothing to do — normal terminal, Rejected (not a failure metric).
    PreflightNoWork {
        note: String,
    },
    /// Change required but empty diff — quality failure, terminal Failed.
    ExpectedChangeButEmptyDiff,
    /// Inspector found the diff unacceptable for other reasons.
    Fail {
        reason: String,
    },
}

/// Pure verdict function — no I/O, deterministic.
pub fn inspect(diff: &DiffStats, change_required: bool) -> Verdict {
    if diff.lines_changed == 0 && diff.files_changed == 0 {
        if change_required {
            Verdict::ExpectedChangeButEmptyDiff
        } else {
            Verdict::PreflightNoWork {
                note: "nothing to do (already at target)".into(),
            }
        }
    } else {
        Verdict::Pass
    }
}

/// Map a verdict to the journal event the line commits.
pub fn event_for(v: &Verdict) -> OrderEvent {
    match v {
        Verdict::Pass => OrderEvent::InspectionPassed,
        Verdict::PreflightNoWork { note } => OrderEvent::Rejected {
            intent: format!("no-op: {note}"),
        },
        Verdict::ExpectedChangeButEmptyDiff => OrderEvent::InspectionFailed {
            reason: "no-op: change required but diff empty".into(),
        },
        Verdict::Fail { reason } => OrderEvent::InspectionFailed {
            reason: reason.clone(),
        },
    }
}

/// Whether this order's task REQUIRES a change (dep-bump where the dep is
/// already at latest → false). M0 dep-bump: derived from the task string
/// at intake; unknown tasks default to true (fail-safe: an empty diff is
/// then a FAILURE, not a silent pass).
pub fn change_required(_order: &Order) -> bool {
    true // T19: intake will carry preflight results; default fail-safe
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(files: u32, lines: u32) -> DiffStats {
        DiffStats {
            files_changed: files,
            lines_changed: lines,
        }
    }

    #[test]
    fn taxonomy_two_kinds_of_nothing() {
        // nothing to do → Rejected (not a failure)
        assert_eq!(
            inspect(&d(0, 0), false),
            Verdict::PreflightNoWork {
                note: "nothing to do (already at target)".into()
            }
        );
        // change required, nothing produced → Failed (quality failure)
        assert_eq!(inspect(&d(0, 0), true), Verdict::ExpectedChangeButEmptyDiff);
    }

    #[test]
    fn real_diff_passes() {
        assert_eq!(inspect(&d(1, 2), true), Verdict::Pass);
        assert_eq!(inspect(&d(1, 2), false), Verdict::Pass);
    }

    #[test]
    fn events_follow_the_taxonomy() {
        let ev = event_for(&Verdict::PreflightNoWork { note: "x".into() });
        assert!(matches!(ev, OrderEvent::Rejected { .. }));
        let ev = event_for(&Verdict::ExpectedChangeButEmptyDiff);
        assert!(matches!(ev, OrderEvent::InspectionFailed { .. }));
    }

    #[test]
    fn default_is_fail_safe() {
        // unknown task type: change_required=true → empty diff is a failure
        let o = Order::new(
            "o-t".into(),
            crate::ExternalRef::cron("t", "t"),
            chrono::Utc::now(),
        );
        assert!(change_required(&o));
    }
}
