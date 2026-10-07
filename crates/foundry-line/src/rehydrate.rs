//! Order rehydration — plan M1 (defect #7, found by the E2E).
//!
//! Orders live in the Line's in-memory map; when the watch process exits,
//! FurnaceHold orders vanish from RAM — the ledger keeps the journal, but
//! `foundry approve` in a NEW process hit "unknown order". Fix: replay the
//! journal's events through the REAL state machine (no bypass — the same
//! transition table enforces legality on replay), then re-bind the
//! artifact digest for actionable states (waiting/FurnaceHold).
//!
//! Digest source: the Die workdir HEAD (identical to what the Die bound
//! in-memory). If the workdir is gone, rehydrate refuses — an order must
//! be re-run rather than approved blind (digest-결속 invariant, plan §11).
use foundry_domain::{ExternalRef, Order, OrderEvent, OrderState};
use foundry_ledger::{LedgerRow, SqliteLedger};
use std::path::Path;

#[derive(Debug)]
pub enum RehydrateError {
    Ledger(String),
    /// Journal replay hit an illegal transition (should be impossible for
    /// a journal the Line itself wrote).
    Replay {
        order_id: String,
        row_seq: i64,
        err: String,
    },
    /// Order is actionable but its workdir (and thus digest) is gone.
    DigestUnavailable {
        order_id: String,
    },
}

pub fn rehydrate(
    ledger: &SqliteLedger,
    _workdir_root: &Path,
) -> Result<Vec<Order>, RehydrateError> {
    let rows = ledger
        .read_all()
        .map_err(|e| RehydrateError::Ledger(format!("{e:?}")))?;

    let mut by_order: std::collections::BTreeMap<String, Vec<&LedgerRow>> = Default::default();
    for r in &rows {
        by_order.entry(r.order_id.clone()).or_default().push(r);
    }

    let mut out = Vec::new();
    for (oid, rs) in by_order {
        // A single order_id may span MULTIPLE lifecycles: re-orders reuse
        // the id and restart from Received after a terminal state. Start a
        // fresh Order at every `from_state == received` boundary.
        let mut order = Order::new(oid.clone(), external_for(&oid), chrono::Utc::now());
        for r in &rs {
            if r.from_state == "received" && order.state() != OrderState::Received {
                out.push(order);
                order = Order::new(oid.clone(), external_for(&oid), chrono::Utc::now());
            }
            let ev = parse_event(&r.event, &r.intent);
            let mut ea = event_attempt(&r.event);
            if r.event.trim() == "DieStarted" {
                ea = order.attempt().max(1);
            }
            match order.transition(ev, ea) {
                Ok(_) => {}
                Err(e) => {
                    return Err(RehydrateError::Replay {
                        order_id: oid,
                        row_seq: r.seq,
                        err: e.to_string(),
                    });
                }
            }
        }
        // digest for actionable states — bind on the LAST lifecycle of this id
        if order.state() == OrderState::FurnaceHold {
            let attempt = order.attempt().max(1);
            let repo = foundry_domain::repo_path(&oid, attempt);
            if repo.join(".git").exists() {
                if let Ok(h) = git_head(&repo) {
                    order.artifact_digest = Some(h);
                }
            }
            if order.artifact_digest.is_none() {
                return Err(RehydrateError::DigestUnavailable { order_id: oid });
            }
        }
        out.push(order);
    }
    Ok(out)
}

/// Parse the journal's event repr back into an OrderEvent. The reprs are
/// `{event:?}` Debug strings written by the ledger (M0 contract).
fn parse_event(repr: &str, intent: &str) -> OrderEvent {
    let name = repr
        .split(|c: char| c == ' ' || c == '{')
        .next()
        .unwrap_or("");
    match name {
        "Queued" => OrderEvent::Queued,
        // DieStarted carries no attempt in its repr — the state machine
        // expects the CURRENT attempt (max(1)); reconstruct contextually.
        "DieStarted" => OrderEvent::DieStarted,
        "DieSucceeded" => OrderEvent::DieSucceeded {
            attempt: event_attempt(repr).max(1),
        },
        "DieFailed" => OrderEvent::DieFailed {
            attempt: event_attempt(repr).max(1),
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
            pr_url: repr.to_string(),
        },
        "PublishUnknown" => OrderEvent::PublishUnknown {
            operation_id: repr.to_string(),
        },
        "CancelRequested" => OrderEvent::CancelRequested {
            reason: repr.to_string(),
        },
        "CancelConfirmed" => OrderEvent::CancelConfirmed,
        "Canceled" => OrderEvent::Canceled {
            reason: repr.to_string(),
        },
        _ => OrderEvent::Queued, // unknown repr — should be impossible
    }
}

/// Extract `field: "value"` from a Debug repr.
fn extract_quoted(repr: &str, field: &str) -> Option<String> {
    let pat = format!("{field}: ");
    let i = repr.find(&pat)? + pat.len();
    let mut rest = &repr[i..];
    if rest.starts_with('"') {
        rest = &rest[1..];
    }
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn event_attempt(repr: &str) -> u32 {
    // Debug repr "attempt: 1" — take the first digit run after "attempt"
    let i = match repr.find("attempt") { Some(i) => i + "attempt".len(), None => return 0 };
    let rest = &repr[i..];
    let digits: String = rest
        .trim_start_matches(|c: char| !c.is_ascii_digit())
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().unwrap_or(0)
}

/// M0 id convention: `o-<dedup tail>`; intake dedup_key = `issues#<n>`.
fn external_for(oid: &str) -> ExternalRef {
    let tail = oid.strip_prefix("o-").unwrap_or(oid);
    // GitHub-intake ids are `o-<issue#>:<timestamp>` (colon mandatory);
    // bare `o-<timestamp>` ids are cron/legacy — never treat as issues.
    if tail.contains(':') {
        if let Some(n) = tail.split(':').next().unwrap_or("").parse::<u64>().ok() {
            return ExternalRef::github(
                "lunarpulse/foundary-demo",
                &n.to_string(),
                format!("issues#{n}"),
            );
        }
    }
    ExternalRef::cron("rehydrate", tail)
}

fn git_head(repo: &Path) -> std::io::Result<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("rev-parse")
        .arg("HEAD")
        .output()?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "git rev-parse failed",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_attempt_parses_debug_repr() {
        assert_eq!(event_attempt("DieSucceeded { attempt: 2 }"), 2);
        assert_eq!(event_attempt("DieStarted"), 0);
        assert_eq!(event_attempt("DieFailed { attempt: 1, reason: x }"), 1);
    }

    #[test]
    fn external_recovers_issue_number() {
        let e = external_for("o-137:2026-10-07T13:59:47Z");
        assert_eq!(e.dedup_key, "issues#137");
        let c = external_for("o-1760123456789");
        assert!(c.dedup_key.starts_with("cron:"));
    }
}
