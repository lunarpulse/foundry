//! Journal replay engine — the SINGLE source of truth for replaying the
//! append-only journal into orders (M2 T16, plan v2).
//!
//! Before T16, three consumers each had their own replay/parser:
//!   - foundry-serve::replay (cards — string state + best-effort attempt)
//!   - foundry-line::rehydrate (orders via transition() — #7 fix)
//!   - foundry status/doctor (ad-hoc queries)
//! A duplicated parser is a reserved defect; this module is the one place.
//!
//! Contract (plan v2, review #4):
//!   INPUT PRECONDITION — rows must be chain-hash-verified and strictly
//!   increasing by seq, no duplicates. The engine CHECKS this itself and
//!   fails with RowIdGap / DuplicateEvent / HashUnverifiedInput rather
//!   than trusting the caller. UnknownSchemaVersion guards T17 v2 rows.
//!   REPLAY — every transition goes through the REAL `Order::transition`
//!   (no state bypass): an illegal journal row fails loudly (IllegalRow).
//!   MULTI-LIFECYCLE — one order_id can span several lifecycles
//!   (re-orders restart from Received); a fresh Order starts at every
//!   `from_state == received` boundary (defect #7, live-ledger measured).
use crate::LedgerRow;
use foundry_domain::{Order, OrderEvent, OrderState};

#[derive(Debug, PartialEq)]
pub enum ReplayError {
    /// seq jumped (e.g. 41 → 43): rows missing or out of order.
    RowIdGap { expected: i64, got: i64 },
    /// the same seq appeared twice.
    DuplicateEvent { seq: i64 },
    /// caller passed rows whose chain hashes do not verify (or that were
    /// not verified at all) — replay refuses unverified input.
    HashUnverifiedInput { at_seq: i64 },
    /// a journal row's event could not be decoded (unknown repr/json mix).
    UnknownSchemaVersion { at_seq: i64, detail: String },
    /// decoded fine but the state machine rejects the transition.
    IllegalRow {
        order_id: String,
        seq: i64,
        err: String,
    },
}

/// One replayed lifecycle of an order (an id re-ordered N times yields N
/// entries; `lifecycle` is 0-based).
#[derive(Debug)]
pub struct ReplayedOrder {
    pub order_id: String,
    pub lifecycle: u32,
    pub order: Order,
    /// seq of the row that STARTED this lifecycle (its Queued row).
    pub started_seq: i64,
    /// seq of the last row of this lifecycle (terminal or current).
    pub last_seq: i64,
}

/// Display-string → OrderState (reverse of the Display impl; replaces the
/// hardcoded 12-arm match tables — the kanban column list stays 1:1).
pub fn state_from_display(s: &str) -> Option<OrderState> {
    Some(match s {
        "received" => OrderState::Received,
        "queued" => OrderState::Queued,
        "working" => OrderState::Working,
        "review" => OrderState::Inspecting,
        "waiting" => OrderState::FurnaceHold,
        "publishing" => OrderState::Publishing,
        "unknown" => OrderState::Unknown,
        "shipped" => OrderState::Shipped,
        "failed" => OrderState::Failed,
        "rejected" => OrderState::Rejected,
        "canceling" => OrderState::Canceling,
        "canceled" => OrderState::Canceled,
        _ => return None,
    })
}

/// Replay verified journal rows into order lifecycles. `verify_chain` is
/// called by the engine itself on the input — callers must NOT pass raw
/// untrusted rows. (The closure seam keeps this module ledger-agnostic
/// and unit-testable without SQLite.)
pub fn replay(
    rows: &[LedgerRow],
    verify_chain: impl FnOnce(&[LedgerRow]) -> bool,
) -> Result<Vec<ReplayedOrder>, ReplayError> {
    if !verify_chain(rows) {
        let at = rows.first().map(|r| r.seq).unwrap_or(0);
        return Err(ReplayError::HashUnverifiedInput { at_seq: at });
    }
    // seq strictly increasing, no gaps within the provided window
    for w in rows.windows(2) {
        if w[1].seq == w[0].seq {
            return Err(ReplayError::DuplicateEvent { seq: w[1].seq });
        }
        if w[1].seq != w[0].seq + 1 {
            return Err(ReplayError::RowIdGap {
                expected: w[0].seq + 1,
                got: w[1].seq,
            });
        }
    }

    let mut out: Vec<ReplayedOrder> = Vec::new();
    let mut pending: std::collections::HashMap<String, ReplayedOrder> = Default::default();
    for r in rows {
        // lifecycle boundary: journal restarts an id from `received` while a
        // previous lifecycle of the SAME id is still pending → close it.
        if r.from_state == "received" {
            if let Some(prev) = pending.remove(&r.order_id) {
                out.push(prev);
            }
        }
        let cur = pending
            .entry(r.order_id.clone())
            .or_insert_with(|| ReplayedOrder {
                order_id: r.order_id.clone(),
                lifecycle: lifecycle_count(&out, &r.order_id),
                order: new_order(r),
                started_seq: r.seq,
                last_seq: r.seq,
            });
        let ev = decode_event(&r.event, &r.intent, cur.order.attempt());
        // live lifecycle: enforce via the real state machine
        let ev = match ev {
            Some(ev) => ev,
            None => {
                return Err(ReplayError::UnknownSchemaVersion {
                    at_seq: r.seq,
                    detail: r.event.clone(),
                });
            }
        };
        let ea = event_attempt(&r.event).unwrap_or_else(|| cur.order.attempt().max(1));
        match cur.order.transition(ev, ea) {
            Ok(_) => {
                cur.last_seq = r.seq;
            }
            Err(e) => {
                return Err(ReplayError::IllegalRow {
                    order_id: r.order_id.clone(),
                    seq: r.seq,
                    err: e.to_string(),
                });
            }
        }
    }
    let mut remaining: Vec<ReplayedOrder> = pending.into_values().collect();
    remaining.sort_by_key(|o| o.started_seq);
    out.extend(remaining);
    out.sort_by_key(|o| (o.started_seq, o.lifecycle));
    Ok(out)
}

fn lifecycle_count(done: &[ReplayedOrder], oid: &str) -> u32 {
    done.iter().filter(|o| o.order_id == oid).count() as u32
}

fn new_order(r: &LedgerRow) -> Order {
    let external = external_for(&r.order_id);
    Order::new(r.order_id.clone(), external, chrono::Utc::now())
}

/// M0 id convention (unchanged from rehydrate #7):
/// `o-<issue#>:<timestamp>` = GitHub intake; bare `o-<tail>` = cron/legacy.
pub fn external_for(order_id: &str) -> foundry_domain::ExternalRef {
    let tail = order_id.strip_prefix("o-").unwrap_or(order_id);
    if tail.contains(':') {
        if let Some(n) = tail.split(':').next().unwrap_or("").parse::<u64>().ok() {
            return foundry_domain::ExternalRef::github(
                "lunarpulse/foundary-demo",
                &n.to_string(),
                format!("issues#{n}"),
            );
        }
    }
    foundry_domain::ExternalRef::cron("replay", tail)
}

/// Decode the journal's event repr (Debug format; T17 adds JSON — this
/// parser gains a JSON-first branch then) into an OrderEvent. Carries the
/// four live-ledger edges from defect #7:
///   (a) DieStarted has no attempt in repr — caller supplies context
///   (b) Approved's digest lives inside the repr — quoted-field extract
pub(crate) fn decode_event(repr: &str, intent: &str, ctx_attempt: u32) -> Option<OrderEvent> {
    let name = repr
        .split(|c: char| c == ' ' || c == '{')
        .next()
        .unwrap_or("");
    Some(match name {
        "Queued" => OrderEvent::Queued,
        "DieStarted" => OrderEvent::DieStarted,
        "DieSucceeded" => OrderEvent::DieSucceeded {
            attempt: event_attempt(repr).unwrap_or(ctx_attempt.max(1)),
        },
        "DieFailed" => OrderEvent::DieFailed {
            attempt: event_attempt(repr).unwrap_or(ctx_attempt.max(1)),
            reason: repr.to_string(),
        },
        "InspectionPassed" => OrderEvent::InspectionPassed,
        "InspectionFailed" => OrderEvent::InspectionFailed {
            reason: repr.to_string(),
        },
        "Approved" => OrderEvent::Approved {
            intent: intent.to_string(),
            artifact_digest: extract_quoted(repr, "artifact_digest").unwrap_or_default(),
        },
        "Rejected" => OrderEvent::Rejected {
            intent: intent.to_string(),
        },
        "PublishConfirmed" => OrderEvent::PublishConfirmed {
            pr_url: extract_quoted(repr, "pr_url").unwrap_or_default(),
        },
        "PublishUnknown" => OrderEvent::PublishUnknown {
            operation_id: extract_quoted(repr, "operation_id").unwrap_or_else(|| repr.to_string()),
        },
        "CancelRequested" => OrderEvent::CancelRequested {
            reason: repr.to_string(),
        },
        "CancelConfirmed" => OrderEvent::CancelConfirmed,
        "Canceled" => OrderEvent::Canceled {
            reason: repr.to_string(),
        },
        _ => return None,
    })
}

/// Dual-write consistency helper (T17): decode via the repr path.
pub(crate) fn decode_for_consistency(repr: &str, intent: &str) -> Option<OrderEvent> {
    decode_event(repr, intent, 1)
}

/// First digit run after "attempt" (Debug repr `attempt: 1`).
pub(crate) fn event_attempt(repr: &str) -> Option<u32> {
    let i = repr.find("attempt")? + "attempt".len();
    let rest = &repr[i..];
    let digits: String = rest
        .trim_start_matches(|c: char| !c.is_ascii_digit())
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// Extract `field: "value"` from a Debug repr.
pub(crate) fn extract_quoted(repr: &str, field: &str) -> Option<String> {
    let pat = format!("{field}: ");
    let i = repr.find(&pat)? + pat.len();
    let mut rest = &repr[i..];
    if rest.starts_with('"') {
        rest = &rest[1..];
    }
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(seq: i64, oid: &str, from: &str, to: &str, event: &str) -> LedgerRow {
        LedgerRow {
            seq,
            order_id: oid.into(),
            from_state: from.into(),
            to_state: to.into(),
            event: event.into(),
            actor: "Agent/executor".into(),
            intent: String::new(),
            at: "2026-10-08T00:00:00Z".into(),
            prev_hash: String::new(),
            hash: String::new(),
            event_json: None,
        }
    }

    fn ok_chain(_: &[LedgerRow]) -> bool {
        true
    }

    #[test]
    fn replays_a_full_lifecycle() {
        let rows = vec![
            row(1, "o-1:ts", "received", "queued", "Queued"),
            row(2, "o-1:ts", "queued", "working", "DieStarted"),
            row(
                3,
                "o-1:ts",
                "working",
                "review",
                "DieSucceeded { attempt: 1 }",
            ),
            row(4, "o-1:ts", "review", "waiting", "InspectionPassed"),
        ];
        let out = replay(&rows, ok_chain).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].order.state(), OrderState::FurnaceHold);
        assert_eq!(out[0].order.attempt(), 1);
    }

    #[test]
    fn re_order_starts_a_new_lifecycle() {
        let rows = vec![
            row(1, "o-1:ts", "received", "queued", "Queued"),
            row(2, "o-1:ts", "queued", "working", "DieStarted"),
            row(
                3,
                "o-1:ts",
                "working",
                "review",
                "DieSucceeded { attempt: 1 }",
            ),
            row(4, "o-1:ts", "review", "waiting", "InspectionPassed"),
            row(
                5,
                "o-1:ts",
                "waiting",
                "publishing",
                "Approved { intent: \"x\", artifact_digest: \"d\" }",
            ),
            row(
                6,
                "o-1:ts",
                "publishing",
                "shipped",
                "PublishConfirmed { pr_url: \"https://x/1\" }",
            ),
            row(7, "o-1:ts", "received", "queued", "Queued"),
            row(8, "o-1:ts", "queued", "working", "DieStarted"),
        ];
        let out = replay(&rows, ok_chain).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].order.state(), OrderState::Shipped);
        assert_eq!(out[1].order.state(), OrderState::Working);
        assert_eq!(out[1].lifecycle, 1);
    }

    #[test]
    fn interleaved_orders_track_independently() {
        let rows = vec![
            row(1, "o-1:ts", "received", "queued", "Queued"),
            row(2, "o-2:ts", "received", "queued", "Queued"),
            row(3, "o-1:ts", "queued", "working", "DieStarted"),
            row(4, "o-2:ts", "queued", "working", "DieStarted"),
        ];
        let out = replay(&rows, ok_chain).unwrap();
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|o| o.order.state() == OrderState::Working));
    }

    #[test]
    fn row_gap_is_detected() {
        let rows = vec![
            row(1, "o-1:ts", "received", "queued", "Queued"),
            row(3, "o-1:ts", "queued", "working", "DieStarted"),
        ];
        match replay(&rows, ok_chain) {
            Err(ReplayError::RowIdGap { expected, got }) => {
                assert_eq!((expected, got), (2, 3));
            }
            other => panic!("expected RowIdGap, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_seq_is_detected() {
        let rows = vec![
            row(1, "o-1:ts", "received", "queued", "Queued"),
            row(1, "o-1:ts", "queued", "working", "DieStarted"),
        ];
        match replay(&rows, ok_chain) {
            Err(ReplayError::DuplicateEvent { seq }) => assert_eq!(seq, 1),
            other => panic!("expected DuplicateEvent, got {other:?}"),
        }
    }

    #[test]
    fn unverified_input_is_refused() {
        let rows = vec![row(1, "o-1:ts", "received", "queued", "Queued")];
        match replay(&rows, |_: &[LedgerRow]| false) {
            Err(ReplayError::HashUnverifiedInput { at_seq }) => assert_eq!(at_seq, 1),
            other => panic!("expected HashUnverifiedInput, got {other:?}"),
        }
    }

    #[test]
    fn unknown_event_repr_fails_loudly() {
        let rows = vec![row(
            1,
            "o-1:ts",
            "received",
            "queued",
            "Teleported { to: \"mars\" }",
        )];
        match replay(&rows, ok_chain) {
            Err(ReplayError::UnknownSchemaVersion { at_seq, .. }) => assert_eq!(at_seq, 1),
            other => panic!("expected UnknownSchemaVersion, got {other:?}"),
        }
    }

    #[test]
    fn display_roundtrip_covers_all_states() {
        for s in [
            "received",
            "queued",
            "working",
            "review",
            "waiting",
            "publishing",
            "unknown",
            "shipped",
            "failed",
            "rejected",
            "canceling",
            "canceled",
        ] {
            let st = state_from_display(s).unwrap_or_else(|| panic!("{s} unmapped"));
            assert_eq!(st.to_string(), s, "display({st:?}) != {s}");
        }
    }
}
