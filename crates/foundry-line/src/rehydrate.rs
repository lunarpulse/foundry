//! Order rehydration — thin wrapper over the SINGLE replay engine
//! (`foundry_ledger::replay`, M2 T16). The bespoke replay that lived here
//! (defect #7 fix) is now the engine's job; this module keeps the
//! approve-path ergonomics: read → verify → replay → bind digests for
//! actionable (FurnaceHold) lifecycles from the workdir HEAD.
use foundry_domain::OrderState;
use foundry_exec::{Attempt, OrderId, TupleStore};
use foundry_ledger::SqliteLedger;
use std::path::Path;

#[derive(Debug)]
pub enum RehydrateError {
    Ledger(String),
    Replay(String),
    /// Order is actionable but its workdir (and thus digest) is gone.
    /// Re-run the order rather than approve blind (digest-bound invariant).
    DigestUnavailable {
        order_id: String,
    },
    /// The approval gate has no matching durable review tuple.
    ReviewEvidenceUnavailable {
        order_id: String,
    },
}

pub fn rehydrate(
    ledger: &SqliteLedger,
    _workdir_root: &Path,
    tuples: &dyn TupleStore,
) -> Result<Vec<foundry_domain::Order>, RehydrateError> {
    let rows = ledger
        .read_all()
        .map_err(|e| RehydrateError::Ledger(format!("{e:?}")))?;
    if !ledger
        .verify_chain()
        .map_err(|e| RehydrateError::Ledger(format!("{e:?}")))?
    {
        return Err(RehydrateError::Replay(
            "chain hash verification failed".into(),
        ));
    }
    let replayed = foundry_ledger::replay::replay(&rows, |rows| {
        // the engine requires verified input; re-verify inside the seam
        rows.iter()
            .zip(rows.iter().skip(1))
            .all(|(a, b)| b.prev_hash == a.hash)
            && rows
                .first()
                .map(|r| !r.prev_hash.is_empty())
                .unwrap_or(true)
    })
    .map_err(|e| RehydrateError::Replay(format!("{e:?}")))?;

    let mut out = Vec::new();
    for ro in replayed {
        let mut order = ro.order;
        if order.state() == OrderState::FurnaceHold {
            let attempt = order.attempt().max(1);
            let repo = foundry_domain::repo_path(&ro.order_id, attempt);
            if repo.join(".git").exists()
                && let Ok(h) = git_head(&repo)
            {
                order.artifact_digest = Some(h);
            }
            if order.artifact_digest.is_none() {
                return Err(RehydrateError::DigestUnavailable {
                    order_id: ro.order_id.clone(),
                });
            }
        }
        if matches!(
            order.state(),
            OrderState::FurnaceHold
                | OrderState::Publishing
                | OrderState::Unknown
                | OrderState::Shipped
        ) {
            let attempt = order.attempt();
            let digest = order.artifact_digest.as_deref().unwrap_or_default();
            let tuple = tuples
                .latest_promoted(&OrderId(order.id.clone()), Attempt(attempt as u64))
                .map_err(|e| RehydrateError::Replay(format!("tuple read failed: {e:?}")))?;
            if digest.is_empty() || !tuple.is_some_and(|tuple| tuple.immutable_commit.0 == digest) {
                return Err(RehydrateError::ReviewEvidenceUnavailable {
                    order_id: ro.order_id.clone(),
                });
            }
        }
        out.push(order);
    }
    Ok(out)
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
        Err(std::io::Error::other("git rev-parse failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rehydrate_runs_on_a_seeded_ledger() {
        let db = std::env::temp_dir().join(format!("rehy-t-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        let mut led = SqliteLedger::open(&db).unwrap();
        let oid = "o-999:2026-10-08T00:00:00Z";
        let order = foundry_domain::Order::new(
            oid.into(),
            foundry_domain::ExternalRef::github("o/r", "999", "issues#999".into()),
            chrono::Utc::now(),
        );
        use foundry_domain::ports::Ledger as _;
        // drive to FurnaceHold through real transitions, journalling each
        let mut o = order;
        for ev in [
            foundry_domain::OrderEvent::Queued,
            foundry_domain::OrderEvent::DieStarted,
            foundry_domain::OrderEvent::DieSucceeded { attempt: 1 },
            foundry_domain::OrderEvent::InspectionPassed,
        ] {
            let from = o.state();
            o.transition(ev.clone(), 1).unwrap();
            led.append(
                oid,
                &ev,
                from,
                o.state(),
                &foundry_domain::ActorId {
                    kind: foundry_domain::ActorKind::Agent,
                    source: "foundry".into(),
                    id: "executor".into(),
                },
                chrono::Utc::now(),
            )
            .unwrap();
        }
        drop(led);
        let led = SqliteLedger::open(&db).unwrap();
        // FurnaceHold without a workdir → DigestUnavailable (by design)
        struct EmptyTuples;
        impl TupleStore for EmptyTuples {
            fn record(
                &self,
                _: &foundry_exec::ArtifactTuple,
            ) -> Result<foundry_exec::Recorded, foundry_exec::PortError> {
                Ok(foundry_exec::Recorded)
            }
            fn latest(
                &self,
                _: &OrderId,
                _: Attempt,
            ) -> Result<Option<foundry_exec::ArtifactTuple>, foundry_exec::PortError> {
                Ok(None)
            }
            fn latest_promoted(
                &self,
                _: &OrderId,
                _: Attempt,
            ) -> Result<Option<foundry_exec::ArtifactTuple>, foundry_exec::PortError> {
                Ok(None)
            }
            fn promote(
                &self,
                _: &foundry_exec::ArtifactTupleKey,
                _: &foundry_exec::EmitReceipt,
            ) -> Result<(), foundry_exec::PortError> {
                Ok(())
            }
        }
        match rehydrate(&led, Path::new("/tmp"), &EmptyTuples) {
            Err(RehydrateError::DigestUnavailable { order_id }) => assert_eq!(order_id, oid),
            other => panic!("expected DigestUnavailable, got {other:?}"),
        }
    }
}
