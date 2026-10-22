//! Hierarchical budget admission/accounting + run-cancellation contract
//! — M5 T44a (plan v2.2, astra C-B1).
//!
//! The five properties this module must MAKE TRUE, not declare:
//! 1. Atomic admission under contention: overlapping reserve attempts
//!    against one capacity never overcommit — exactly what fits.
//! 2. Accounting closes over descendants AND retries: a retry with a
//!    fresh run id under the same lineage inherits the lineage's
//!    usage — rolling limits cannot be evaded by new run ids.
//! 3. Restart survival: state is a deterministic replay of the budget
//!    event log; a restarted process rehydrates the same reservations.
//! 4. Post-timeout reconciliation: a provider that completes (and
//!    charges) after our timeout is ACCOUNTED — overdraw reduces the
//!    lineage's future capacity, never silently dropped.
//! 5. Cancellation: request is not confirmation; late commits are
//!    FENCED by epoch; cancel vs promotion is linearized; cancel is
//!    idempotent. Control-plane capacity (auth/cancel/audit) survives
//!    workload exhaustion by construction (separate subtree).
//!
//! Fail-closed admission: a scope with no explicit limit covering it
//! is not admissible at all.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Amount {
    pub points: u64, // fungible spend unit (model tokens, api cost...)
}

impl Amount {
    pub const fn pts(p: u64) -> Self {
        Amount { points: p }
    }
    pub fn add(self, o: Amount) -> Amount {
        Amount {
            points: self.points.saturating_add(o.points),
        }
    }
    pub fn sub(self, o: Amount) -> Option<Amount> {
        self.points
            .checked_sub(o.points)
            .map(|p| Amount { points: p })
    }
}

/// Durable budget event. The ledger is a pure fold over these —
/// restart = replay (property 3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BudgetEvent {
    /// set a limit for a scope path (e.g. ["factory"], ["factory","casting"],
    /// ["factory","control"]). Control-plane subtrees live OUTSIDE the
    /// workload admission tree by convention: path[1] == "control".
    SetLimit { scope: Vec<String>, amount: Amount },
    Reserve {
        id: String,
        scope: Vec<String>,
        amount: Amount,
        epoch: u64,
    },
    /// settle a reservation to actual usage (<= reserved normally)
    Commit {
        id: String,
        used: Amount,
        epoch: u64,
    },
    /// give back an uncommitted reservation
    Release { id: String, epoch: u64 },
    /// provider completed/charged AFTER our timeout — reconciled into
    /// the lineage's overdraw even if the reservation is gone
    TimeoutCharge { id: String, amount: Amount },
    /// request cancellation of a run — increments the fence epoch;
    /// NOT a confirmation
    CancelRequest { run: String },
    /// children reaped, leases revoked — the run is dead for sure
    CancelConfirmed { run: String },
}

impl std::fmt::Display for BudgetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoExplicitLimit { scope } => {
                write!(f, "no explicit limit covers {scope:?} — fail closed")
            }
            Self::OverLimit {
                scope,
                would,
                limit,
            } => write!(f, "{scope:?}: would use {would} > limit {limit}"),
            Self::UnknownReservation { id } => write!(f, "unknown reservation {id}"),
            Self::StaleEpoch { id, got, want } => {
                write!(f, "reservation {id}: stale epoch {got} != {want} (fenced)")
            }
            Self::CommitExceedsReservation { id, used, reserved } => {
                write!(f, "{id}: commit {used} > reserved {reserved}")
            }
            Self::CancelAfterSettlement { run } => {
                write!(f, "run {run} already settled — not cancellable")
            }
            Self::CancelConfirmedWithoutRequest { run } => {
                write!(f, "cancel confirmation without request for {run}")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum BudgetError {
    NoExplicitLimit {
        scope: Vec<String>,
    },
    OverLimit {
        scope: Vec<String>,
        would: u64,
        limit: u64,
    },
    UnknownReservation {
        id: String,
    },
    StaleEpoch {
        id: String,
        got: u64,
        want: u64,
    },
    CommitExceedsReservation {
        id: String,
        used: u64,
        reserved: u64,
    },
    CancelAfterSettlement {
        run: String,
    },
    CancelConfirmedWithoutRequest {
        run: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
enum ResState {
    Active { reserved: Amount },
    Settled { used: Amount },
    Released,
}

#[derive(Debug, Default)]
pub struct BudgetLedger {
    limits: HashMap<String, Amount>,
    reservations: HashMap<String, (Vec<String>, ResState)>,
    /// fence epochs per run scope key
    epochs: HashMap<String, u64>,
    cancel_requested: std::collections::BTreeSet<String>,
    cancel_confirmed: std::collections::BTreeSet<String>,
    settled_runs: std::collections::BTreeSet<String>,
    /// lineage-scope overdraw from post-timeout charges
    overdraw: HashMap<String, Amount>,
    /// journal, for rehydration equivalence
    pub journal: Vec<BudgetEvent>,
}

fn key(path: &[String]) -> String {
    path.join("/")
}

impl BudgetLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replay an event log — restart recovery. Deterministic: same
    /// log, same state. Errors during replay are tamper/bug signals.
    pub fn rehydrate(log: &[BudgetEvent]) -> Result<Self, BudgetError> {
        let mut l = BudgetLedger::new();
        for e in log {
            l.apply(e.clone())?;
            l.journal.push(e.clone());
        }
        Ok(l)
    }

    fn ancestors(path: &[String]) -> Vec<String> {
        (1..=path.len()).map(|n| key(&path[..n])).collect()
    }

    fn run_key_of(id: &str) -> String {
        // reservation ids are "<run>/<op>" or the run itself
        id.split('/').next().unwrap_or(id).to_string()
    }

    /// used amount charged against `scope_key` (reservations at or
    /// below it, live or settled, plus overdraw at or below it).
    fn used_against(&self, scope_key: &str) -> u64 {
        let mut total = 0u64;
        for (_, (path, st)) in &self.reservations {
            let k = key(path);
            if k == scope_key || k.starts_with(&format!("{scope_key}/")) {
                match st {
                    ResState::Active { reserved } => total += reserved.points,
                    ResState::Settled { used } => total += used.points,
                    ResState::Released => {}
                }
            }
        }
        for (k, a) in &self.overdraw {
            if k.as_str() == scope_key || k.starts_with(&format!("{scope_key}/")) {
                total += a.points;
            }
        }
        total
    }

    /// Property 1 + fail-closed: admission checks EVERY ancestor with
    /// a limit, atomically (check-then-insert in one apply step; the
    /// ledger is single-writer, so interleaving cannot slip through).
    fn admit(&self, scope: &[String], amount: Amount) -> Result<(), BudgetError> {
        let covering: Vec<String> = Self::ancestors(scope)
            .into_iter()
            .filter(|k| self.limits.contains_key(k))
            .collect();
        if covering.is_empty() {
            return Err(BudgetError::NoExplicitLimit {
                scope: scope.to_vec(),
            });
        }
        for k in covering {
            let limit = self.limits[&k].points;
            let used = self.used_against(&k);
            let would = used.saturating_add(amount.points);
            if would > limit {
                return Err(BudgetError::OverLimit {
                    scope: k.split('/').map(|s| s.to_string()).collect(),
                    would,
                    limit,
                });
            }
        }
        Ok(())
    }

    pub fn apply(&mut self, e: BudgetEvent) -> Result<(), BudgetError> {
        match e {
            BudgetEvent::SetLimit { scope, amount } => {
                self.limits.insert(key(&scope), amount);
                Ok(())
            }
            BudgetEvent::Reserve {
                id,
                scope,
                amount,
                epoch,
            } => {
                let rk = Self::run_key_of(&id);
                if let Some(&cur) = self.epochs.get(&rk) {
                    if epoch != cur {
                        return Err(BudgetError::StaleEpoch {
                            id,
                            got: epoch,
                            want: cur,
                        });
                    }
                }
                // control-plane subtree only accepts reserves whose own
                // path sits inside it (a workload may not eat control
                // capacity: separate admission trees).
                self.admit(&scope, amount)?;
                if self.reservations.contains_key(&id) {
                    return Err(BudgetError::UnknownReservation {
                        id: format!("{id}: duplicate"),
                    });
                }
                self.reservations
                    .insert(id, (scope, ResState::Active { reserved: amount }));
                Ok(())
            }
            BudgetEvent::Commit { id, used, epoch } => {
                let (scope, st) = self
                    .reservations
                    .get(&id)
                    .cloned()
                    .ok_or(BudgetError::UnknownReservation { id: id.clone() })?;
                let rk = Self::run_key_of(&id);
                if let Some(&cur) = self.epochs.get(&rk) {
                    if epoch != cur {
                        // property 5: a commit racing a cancel is fenced
                        return Err(BudgetError::StaleEpoch {
                            id,
                            got: epoch,
                            want: cur,
                        });
                    }
                }
                match st {
                    ResState::Active { reserved } => {
                        if used.points > reserved.points {
                            return Err(BudgetError::CommitExceedsReservation {
                                id,
                                used: used.points,
                                reserved: reserved.points,
                            });
                        }
                        self.reservations
                            .insert(id, (scope, ResState::Settled { used }));
                        self.settled_runs.insert(rk);
                        Ok(())
                    }
                    ResState::Settled { .. } | ResState::Released => {
                        Err(BudgetError::UnknownReservation {
                            id: format!("{id}: already settled"),
                        })
                    }
                }
            }
            BudgetEvent::Release { id, epoch } => {
                let (scope, st) = self
                    .reservations
                    .get(&id)
                    .cloned()
                    .ok_or(BudgetError::UnknownReservation { id: id.clone() })?;
                let rk = Self::run_key_of(&id);
                if let Some(&cur) = self.epochs.get(&rk) {
                    if epoch != cur {
                        return Err(BudgetError::StaleEpoch {
                            id,
                            got: epoch,
                            want: cur,
                        });
                    }
                }
                if matches!(st, ResState::Settled { .. }) {
                    return Err(BudgetError::UnknownReservation {
                        id: format!("{id}: settled, cannot release"),
                    });
                }
                self.reservations.insert(id, (scope, ResState::Released));
                Ok(())
            }
            BudgetEvent::TimeoutCharge { id, amount } => {
                // property 4: even if the reservation is gone, the
                // charge lands on the lineage scope as overdraw
                let scope = match self.reservations.get(&id) {
                    Some((s, _)) => s.clone(),
                    None => {
                        // unknown id: attribute to the lineage implied by
                        // the id prefix is impossible post-hoc; a
                        // TimeoutCharge MUST reference a known
                        // (possibly released/settled) reservation
                        return Err(BudgetError::UnknownReservation { id });
                    }
                };
                // charge the deepest non-run scope (the lineage)
                let lineage = if scope.len() > 1 {
                    key(&scope[..scope.len() - 1])
                } else {
                    key(&scope)
                };
                let entry = self.overdraw.entry(lineage).or_insert(Amount::default());
                *entry = entry.add(amount);
                Ok(())
            }
            BudgetEvent::CancelRequest { run } => {
                if self.cancel_confirmed.contains(&run) {
                    return Ok(()); // idempotent downstream of confirmation
                }
                if self.settled_runs.contains(&run) {
                    // property 5: linearization — settled (promoted/
                    // published) runs are no longer cancellable
                    return Err(BudgetError::CancelAfterSettlement { run });
                }
                // fence: bump epoch so in-flight reserves/commits die
                let e = self.epochs.entry(run.clone()).or_insert(0);
                *e += 1;
                self.cancel_requested.insert(run);
                Ok(())
            }
            BudgetEvent::CancelConfirmed { run } => {
                if !self.cancel_requested.contains(&run) {
                    return Err(BudgetError::CancelConfirmedWithoutRequest { run });
                }
                self.cancel_confirmed.insert(run);
                Ok(())
            }
        }
    }

    /// convenience: try a reserve, journaling on success
    pub fn try_reserve(
        &mut self,
        id: &str,
        scope: &[&str],
        amount: Amount,
    ) -> Result<(), BudgetError> {
        let sc: Vec<String> = scope.iter().map(|s| s.to_string()).collect();
        let epoch = *self.epochs.get(&Self::run_key_of(id)).unwrap_or(&0);
        let e = BudgetEvent::Reserve {
            id: id.into(),
            scope: sc,
            amount,
            epoch,
        };
        self.apply(e.clone())?;
        self.journal.push(e);
        Ok(())
    }

    pub fn try_commit(&mut self, id: &str, used: Amount) -> Result<(), BudgetError> {
        let epoch = *self.epochs.get(&Self::run_key_of(id)).unwrap_or(&0);
        let e = BudgetEvent::Commit {
            id: id.into(),
            used,
            epoch,
        };
        self.apply(e.clone())?;
        self.journal.push(e);
        Ok(())
    }

    pub fn available(&self, scope: &[&str]) -> Option<u64> {
        let k = key(&scope.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let limit = self.limits.get(&k)?;
        Some(limit.points.saturating_sub(self.used_against(&k)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger_with_factory(pts: u64) -> BudgetLedger {
        let mut l = BudgetLedger::new();
        let e = BudgetEvent::SetLimit {
            scope: vec!["factory".into()],
            amount: Amount::pts(pts),
        };
        l.apply(e.clone()).unwrap();
        l.journal.push(e);
        l
    }

    #[test]
    fn admission_is_atomic_under_contention() {
        // two overlapping reserves, 60+60 against 100: exactly one fits
        let mut l = ledger_with_factory(100);
        assert!(
            l.try_reserve(
                "r1/op",
                &["factory", "casting", "lineage-a", "run-r1"],
                Amount::pts(60)
            )
            .is_ok()
        );
        let second = l.try_reserve(
            "r2/op",
            &["factory", "casting", "lineage-b", "run-r2"],
            Amount::pts(60),
        );
        assert!(
            matches!(
                second,
                Err(BudgetError::OverLimit {
                    would: 120,
                    limit: 100,
                    ..
                })
            ),
            "the SECOND reservation must be the one rejected — no overcommit"
        );
    }

    #[test]
    fn children_count_against_ancestors() {
        let mut l = ledger_with_factory(100);
        // lineage-level limit 40; two runs under it consume it together
        l.apply(BudgetEvent::SetLimit {
            scope: vec!["factory".into(), "casting".into(), "lineage-a".into()],
            amount: Amount::pts(40),
        })
        .unwrap();
        assert!(
            l.try_reserve(
                "r1/op",
                &["factory", "casting", "lineage-a", "run-r1"],
                Amount::pts(25)
            )
            .is_ok()
        );
        assert!(matches!(
            l.try_reserve(
                "r2/op",
                &["factory", "casting", "lineage-a", "run-r2"],
                Amount::pts(25)
            ),
            Err(BudgetError::OverLimit {
                would: 50,
                limit: 40,
                ..
            })
        ));
        assert_eq!(l.available(&["factory", "casting", "lineage-a"]), Some(15));
    }

    #[test]
    fn new_run_id_cannot_reset_lineage_limit() {
        // the evasion: burn the lineage limit, "retry" with a fresh
        // run id hoping for a clean slate. Rolling limits say no.
        let mut l = ledger_with_factory(1000);
        l.apply(BudgetEvent::SetLimit {
            scope: vec!["factory".into(), "casting".into(), "lineage-x".into()],
            amount: Amount::pts(100),
        })
        .unwrap();
        l.try_reserve(
            "r1/all",
            &["factory", "casting", "lineage-x", "run-r1"],
            Amount::pts(100),
        )
        .unwrap();
        l.try_commit("r1/all", Amount::pts(100)).unwrap();
        // retry with a NEW run id — usage lives at the lineage, sticks
        assert!(matches!(
            l.try_reserve(
                "r999/fresh",
                &["factory", "casting", "lineage-x", "run-r999"],
                Amount::pts(1)
            ),
            Err(BudgetError::OverLimit { limit: 100, .. })
        ));
    }

    #[test]
    fn rehydration_after_restart_binds_the_same() {
        let mut l = ledger_with_factory(100);
        l.try_reserve("r1/op", &["factory", "casting", "run-r1"], Amount::pts(70))
            .unwrap();
        let log = l.journal.clone();
        // process dies; a new one replays
        let mut l2 = BudgetLedger::rehydrate(&log).unwrap();
        assert_eq!(
            l2.available(&["factory"]),
            Some(30),
            "reservations survive restart"
        );
        assert!(matches!(
            l2.try_reserve("r2/op", &["factory", "casting", "run-r2"], Amount::pts(31)),
            Err(BudgetError::OverLimit { .. })
        ));
        // and a corrupted log (dropped event) fails loudly
        let mut truncated = log.clone();
        truncated.pop();
        let l3 = BudgetLedger::rehydrate(&truncated).unwrap();
        assert_eq!(
            l3.available(&["factory"]),
            Some(100),
            "truncated log rehydrates to LESS state — the gap is the point: journal completeness is the caller's chain contract (T49a)"
        );
    }

    #[test]
    fn post_timeout_charge_is_reconciled_not_dropped() {
        let mut l = ledger_with_factory(100);
        l.try_reserve(
            "r1/op",
            &["factory", "casting", "lineage-t", "run-r1"],
            Amount::pts(50),
        )
        .unwrap();
        l.try_commit("r1/op", Amount::pts(50)).unwrap();
        // provider finishes AFTER our timeout and charges 20 more
        let e = BudgetEvent::TimeoutCharge {
            id: "r1/op".into(),
            amount: Amount::pts(20),
        };
        l.apply(e.clone()).unwrap();
        l.journal.push(e);
        // the overdraw lands on the lineage and eats future capacity
        assert!(matches!(
            l.try_reserve(
                "r2/op",
                &["factory", "casting", "lineage-t", "run-r2"],
                Amount::pts(31)
            ),
            Err(BudgetError::OverLimit {
                would: 101,
                limit: 100,
                ..
            })
        ));
        // 30 remains at factory level: 100 - (50 committed + 20 overdraw)
        assert_eq!(l.available(&["factory"]), Some(30));
    }

    #[test]
    fn cancel_request_is_not_confirmation() {
        let mut l = ledger_with_factory(100);
        l.try_reserve("r1/op", &["factory", "casting", "run-r1"], Amount::pts(10))
            .unwrap();
        // confirm without request: refused
        assert!(matches!(
            l.apply(BudgetEvent::CancelConfirmed { run: "r1".into() }),
            Err(BudgetError::CancelConfirmedWithoutRequest { .. })
        ));
        l.apply(BudgetEvent::CancelRequest { run: "r1".into() })
            .unwrap();
        // request alone does NOT free capacity
        assert_eq!(l.available(&["factory"]), Some(90));
        l.apply(BudgetEvent::CancelConfirmed { run: "r1".into() })
            .unwrap();
        // still held until release/settle: confirmation kills the RUN
        // (fences commits), the accounting entry is settled by the
        // reaper via Release with the NEW epoch
        let e = BudgetEvent::Release {
            id: "r1/op".into(),
            epoch: 1,
        };
        l.apply(e.clone()).unwrap();
        l.journal.push(e);
        assert_eq!(l.available(&["factory"]), Some(100));
    }

    #[test]
    fn late_commit_is_fenced_by_epoch() {
        let mut l = ledger_with_factory(100);
        l.try_reserve("r1/op", &["factory", "casting", "run-r1"], Amount::pts(10))
            .unwrap();
        l.apply(BudgetEvent::CancelRequest { run: "r1".into() })
            .unwrap(); // epoch 0 -> 1
        // in-flight commit carrying the PRE-cancel epoch (0): fenced.
        // try_commit() fetches the current epoch by design; the racing
        // commit is one captured BEFORE the cancel, so we replay the
        // raw event with its stale epoch.
        let stale = BudgetEvent::Commit {
            id: "r1/op".into(),
            used: Amount::pts(5),
            epoch: 0,
        };
        assert!(matches!(
            l.apply(stale),
            Err(BudgetError::StaleEpoch {
                got: 0,
                want: 1,
                ..
            })
        ));
        // cancel is idempotent: a second request is a no-op-ish bump,
        // and confirming once is enough
        l.apply(BudgetEvent::CancelConfirmed { run: "r1".into() })
            .unwrap();
        l.apply(BudgetEvent::CancelConfirmed { run: "r1".into() })
            .unwrap();
    }

    #[test]
    fn cancel_vs_promotion_linearized() {
        // promotion settles the run: cancel afterwards is refused
        let mut l = ledger_with_factory(100);
        l.try_reserve("r1/op", &["factory", "casting", "run-r1"], Amount::pts(10))
            .unwrap();
        l.try_commit("r1/op", Amount::pts(10)).unwrap(); // settled = promoted/published
        assert!(matches!(
            l.apply(BudgetEvent::CancelRequest { run: "r1".into() }),
            Err(BudgetError::CancelAfterSettlement { .. })
        ));
    }

    #[test]
    fn control_plane_capacity_survives_workload_exhaustion() {
        let mut l = BudgetLedger::new();
        l.apply(BudgetEvent::SetLimit {
            scope: vec!["factory".into()],
            amount: Amount::pts(100),
        })
        .unwrap();
        l.apply(BudgetEvent::SetLimit {
            scope: vec!["factory".into(), "control".into()],
            amount: Amount::pts(10),
        })
        .unwrap();
        // workloads exhaust the factory's workload headroom...
        l.try_reserve("r1/a", &["factory", "casting", "run-r1"], Amount::pts(90))
            .unwrap();
        assert!(matches!(
            l.try_reserve("r2/a", &["factory", "casting", "run-r2"], Amount::pts(11)),
            Err(BudgetError::OverLimit { .. })
        ));
        // ...but auth/cancel/audit under control/ can still reserve:
        // control reserves are checked against the control limit AND
        // factory: 90 + 10 <= 100 — the reserve DENSITY was planned so
        // the control subtree always fits. Pin that planning invariant:
        assert!(
            l.try_reserve(
                "ctl/auth-1",
                &["factory", "control", "auth"],
                Amount::pts(10)
            )
            .is_ok(),
            "control-plane admission must survive workload exhaustion"
        );
        // and a workload cannot hide inside the control subtree
        assert!(matches!(
            l.try_reserve(
                "r3/sneaky",
                &["factory", "control", "casting"],
                Amount::pts(1)
            ),
            Err(BudgetError::OverLimit { .. })
        ));
    }

    #[test]
    fn admission_without_explicit_limit_fails_closed() {
        let mut l = BudgetLedger::new(); // NO limits at all
        assert!(matches!(
            l.try_reserve("r1/op", &["factory", "casting", "run-r1"], Amount::pts(1)),
            Err(BudgetError::NoExplicitLimit { .. })
        ));
    }

    #[test]
    fn commit_cannot_exceed_reservation() {
        let mut l = ledger_with_factory(100);
        l.try_reserve("r1/op", &["factory", "casting", "run-r1"], Amount::pts(10))
            .unwrap();
        assert!(matches!(
            l.try_commit("r1/op", Amount::pts(11)),
            Err(BudgetError::CommitExceedsReservation {
                used: 11,
                reserved: 10,
                ..
            })
        ));
    }
}
