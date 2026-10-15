//! Structured event JSON — journal schema v2 (M2 T17).
//!
//! v1 rows carry the Debug repr in `event` and no `event_json` (NULL).
//! v2 rows carry BOTH: the repr (chain-hash original preservation — the
//! v1 hash formula includes it, so it stays byte-identical) and a
//! canonical JSON encoding in `event_json`.
//!
//! HASH PAYLOAD VERSIONING (plan v2, review #2):
//!   v1 hash  = sha256(seq|oid|from|to|event_repr|actor|intent|at|prev)
//!   v2 hash  = sha256(v1 payload + "|" + canonical_json)   [event_json is
//!              IN the hash — tampering with it breaks the chain]
//!   A row's version is derived: event_json IS NULL → v1, else v2.
//!   verify_chain re-derives with the right formula per row.
//!
//! DUAL-WRITE CONSISTENCY (plan v2, review #3):
//!   append_journal encodes JSON, decodes it back, and cross-checks that
//!   JSON-decode(repr) == JSON-decode(json) via the replay engine's
//!   decoder — a mismatch refuses the write (never journals an ambiguous
//!   event).
use crate::sqlite::LedgerRow;
use foundry_domain::OrderEvent;

/// Canonical JSON for an OrderEvent. Canonical = serde_json to_string of a
/// serde-serializable value with a stable field order (derived by the
/// struct/enum definition) — deterministic across runs.
pub fn event_to_json(event: &OrderEvent) -> String {
    serde_json::to_string(&JsonEvent::from(event)).expect("OrderEvent is json-encodable")
}

/// Decode canonical JSON back to an OrderEvent.
pub fn event_from_json(json: &str) -> Option<OrderEvent> {
    let j: JsonEvent = serde_json::from_str(json).ok()?;
    Some(j.into())
}

pub(crate) fn v2_hash_payload(base_payload: &str, canonical_json: &str) -> String {
    format!("{base_payload}|{canonical_json}")
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum JsonEvent {
    Queued,
    DieStarted,
    DieSucceeded {
        attempt: u32,
    },
    DieFailed {
        attempt: u32,
        reason: String,
    },
    InspectionPassed,
    ReviewHeld { reason: String },
    InspectionFailed {
        reason: String,
    },
    Approved {
        intent: String,
        artifact_digest: String,
    },
    Rejected {
        intent: String,
    },
    PublishConfirmed {
        pr_url: String,
    },
    PublishUnknown {
        operation_id: String,
    },
    CancelRequested {
        reason: String,
    },
    CancelConfirmed,
    Canceled {
        reason: String,
    },
}

impl From<&OrderEvent> for JsonEvent {
    fn from(e: &OrderEvent) -> Self {
        match e.clone() {
            OrderEvent::Queued => JsonEvent::Queued,
            OrderEvent::DieStarted => JsonEvent::DieStarted,
            OrderEvent::DieSucceeded { attempt } => JsonEvent::DieSucceeded { attempt },
            OrderEvent::DieFailed { attempt, reason } => JsonEvent::DieFailed { attempt, reason },
            OrderEvent::InspectionPassed => JsonEvent::InspectionPassed,
            OrderEvent::ReviewHeld { reason } => JsonEvent::ReviewHeld { reason },
            OrderEvent::InspectionFailed { reason } => JsonEvent::InspectionFailed { reason },
            OrderEvent::Approved {
                intent,
                artifact_digest,
            } => JsonEvent::Approved {
                intent,
                artifact_digest,
            },
            OrderEvent::Rejected { intent } => JsonEvent::Rejected { intent },
            OrderEvent::PublishConfirmed { pr_url } => JsonEvent::PublishConfirmed { pr_url },
            OrderEvent::PublishUnknown { operation_id } => {
                JsonEvent::PublishUnknown { operation_id }
            }
            OrderEvent::CancelRequested { reason } => JsonEvent::CancelRequested { reason },
            OrderEvent::CancelConfirmed => JsonEvent::CancelConfirmed,
            OrderEvent::Canceled { reason } => JsonEvent::Canceled { reason },
        }
    }
}

impl From<JsonEvent> for OrderEvent {
    fn from(j: JsonEvent) -> Self {
        match j {
            JsonEvent::Queued => OrderEvent::Queued,
            JsonEvent::DieStarted => OrderEvent::DieStarted,
            JsonEvent::DieSucceeded { attempt } => OrderEvent::DieSucceeded { attempt },
            JsonEvent::DieFailed { attempt, reason } => OrderEvent::DieFailed { attempt, reason },
            JsonEvent::InspectionPassed => OrderEvent::InspectionPassed,
            JsonEvent::ReviewHeld { reason } => OrderEvent::ReviewHeld { reason },
            JsonEvent::InspectionFailed { reason } => OrderEvent::InspectionFailed { reason },
            JsonEvent::Approved {
                intent,
                artifact_digest,
            } => OrderEvent::Approved {
                intent,
                artifact_digest,
            },
            JsonEvent::Rejected { intent } => OrderEvent::Rejected { intent },
            JsonEvent::PublishConfirmed { pr_url } => OrderEvent::PublishConfirmed { pr_url },
            JsonEvent::PublishUnknown { operation_id } => {
                OrderEvent::PublishUnknown { operation_id }
            }
            JsonEvent::CancelRequested { reason } => OrderEvent::CancelRequested { reason },
            JsonEvent::CancelConfirmed => OrderEvent::CancelConfirmed,
            JsonEvent::Canceled { reason } => OrderEvent::Canceled { reason },
        }
    }
}

/// Row schema version (derived, never stored): event_json NULL → v1.
pub fn row_version(r: &LedgerRow) -> u8 {
    if r.event_json.as_deref().unwrap_or("").is_empty() {
        1
    } else {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_events() -> Vec<OrderEvent> {
        use OrderEvent::*;
        vec![
            Queued,
            DieStarted,
            DieSucceeded { attempt: 1 },
            DieFailed {
                attempt: 2,
                reason: "boom".into(),
            },
            InspectionPassed,
            InspectionFailed {
                reason: "no-op".into(),
            },
            Approved {
                intent: "gate".into(),
                artifact_digest: "abc123".into(),
            },
            Rejected {
                intent: "expired".into(),
            },
            PublishConfirmed {
                pr_url: "https://x/1".into(),
            },
            PublishUnknown {
                operation_id: "o:1".into(),
            },
            CancelRequested {
                reason: "user".into(),
            },
            CancelConfirmed,
            Canceled {
                reason: "done".into(),
            },
        ]
    }

    #[test]
    fn every_event_roundtrips_through_json() {
        for e in all_events() {
            let j = event_to_json(&e);
            let back = event_from_json(&j).expect("decode");
            assert_eq!(
                format!("{back:?}"),
                format!("{e:?}"),
                "roundtrip mismatch for {e:?}"
            );
        }
    }

    #[test]
    fn canonical_json_is_deterministic() {
        for e in all_events() {
            assert_eq!(event_to_json(&e), event_to_json(&e));
        }
    }

    #[test]
    fn v2_payload_appends_json() {
        let p = v2_hash_payload("base", "{\"type\":\"queued\"}");
        assert_eq!(p, "base|{\"type\":\"queued\"}");
    }
}
