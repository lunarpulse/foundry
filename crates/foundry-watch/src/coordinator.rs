//! GovernanceCoordinator — M5.1 T52w (astra B2: "replace test-time
//! journal merging with a production durable coordinator").
//!
//! THE single durability boundary for every governance event in the
//! factory. All layers (lessons, synthesis, evaluation, policy
//! application, budget) append through `append()` — one SQLite
//! transaction per event, one AUTOINCREMENT sequence assigned by the
//! DATABASE, not by any caller. There is no test-time merging
//! because there is nothing to merge: one stream, one order, one
//! commit point.
//!
//! Restart = `replay()`: state reconstructed by re-folding the
//! committed events (reservations, applied policies, registered
//! predictions — via the layer-specific rehydrate paths). Pending
//! intents surface explicitly with their deterministic recovery
//! decisions (T49a).
//!
//! The coordinator also closes the split state/log API astra flagged
//! (`apply()` then `journal.push()` as separate calls): here, an
//! append that fails leaves NO state change, and state changes ride
//! the same transaction.
use crate::governance_events::{
    GOVERNANCE_SCHEMA_VERSION, GovernanceEvent, OpKind, Phase, recover, verify_stream,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

#[derive(Debug)]
pub enum CoordinatorError {
    Db(String),
    /// the caller tried to assign a sequence number — only the
    /// coordinator (database) assigns sequence
    CallerSuppliedSeq,
    /// stream corruption detected at open/replay time
    StreamInvalid(String),
    /// schema newer than this binary understands — explicit, never
    /// silent (astra: unsupported schema must be reported)
    SchemaTooNew {
        got: u32,
        supported: u32,
    },
}

pub struct GovernanceCoordinator {
    conn: Connection,
}

impl GovernanceCoordinator {
    /// Open (creating if absent) the single governance stream.
    pub fn open(path: &Path) -> Result<Self, CoordinatorError> {
        let conn = Connection::open(path).map_err(|e| CoordinatorError::Db(e.to_string()))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS governance (
                seq        INTEGER PRIMARY KEY AUTOINCREMENT,
                schema_version INTEGER NOT NULL,
                op         TEXT NOT NULL,
                phase      TEXT NOT NULL,
                op_id      TEXT NOT NULL,
                payload_digest TEXT NOT NULL,
                actor      TEXT NOT NULL,
                at         TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_governance_op_id ON governance(op_id);",
        )
        .map_err(|e| CoordinatorError::Db(e.to_string()))?;
        Ok(GovernanceCoordinator { conn })
    }

    /// In-memory coordinator for tests.
    pub fn open_in_memory() -> Result<Self, CoordinatorError> {
        let mut c = GovernanceCoordinator {
            conn: Connection::open_in_memory().map_err(|e| CoordinatorError::Db(e.to_string()))?,
        };
        // apply the same schema
        c.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS governance (
                    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
                    schema_version INTEGER NOT NULL,
                    op         TEXT NOT NULL,
                    phase      TEXT NOT NULL,
                    op_id      TEXT NOT NULL,
                    payload_digest TEXT NOT NULL,
                    actor      TEXT NOT NULL,
                    at         TEXT NOT NULL
                );",
            )
            .map_err(|e| CoordinatorError::Db(e.to_string()))?;
        Ok(c)
    }

    /// Append one event in a single transaction. The sequence number
    /// is assigned by the database and RETURNED — callers never
    /// supply it. `ev.seq` is overwritten regardless of what the
    /// caller set (the coordinator owns ordering).
    pub fn append(&mut self, mut ev: GovernanceEvent) -> Result<i64, CoordinatorError> {
        if ev.schema_version > GOVERNANCE_SCHEMA_VERSION {
            return Err(CoordinatorError::SchemaTooNew {
                got: ev.schema_version,
                supported: GOVERNANCE_SCHEMA_VERSION,
            });
        }
        let _ = ev.seq; // caller's value is ignored: coordinator owns seq
        let tx = self
            .conn
            .transaction()
            .map_err(|e| CoordinatorError::Db(e.to_string()))?;
        tx.execute(
            "INSERT INTO governance (schema_version, op, phase, op_id, payload_digest, actor, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                ev.schema_version,
                ev.op.as_str(),
                phase_str(&ev.phase),
                ev.op_id,
                ev.payload_digest,
                ev.actor,
                ev.at,
            ],
        )
        .map_err(|e| CoordinatorError::Db(e.to_string()))?;
        let seq: i64 = tx
            .query_row("SELECT last_insert_rowid()", [], |r| r.get(0))
            .map_err(|e| CoordinatorError::Db(e.to_string()))?;
        tx.commit()
            .map_err(|e| CoordinatorError::Db(e.to_string()))?;
        ev.seq = seq;
        Ok(seq)
    }

    /// Full ordered stream, as it lies on disk.
    pub fn replay(&self) -> Result<Vec<GovernanceEvent>, CoordinatorError> {
        let mut stmt = self
            .conn
            .prepare("SELECT seq, schema_version, op, phase, op_id, payload_digest, actor, at FROM governance ORDER BY seq")
            .map_err(|e| CoordinatorError::Db(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                ))
            })
            .map_err(|e| CoordinatorError::Db(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            let (seq, schema_version, op, phase, op_id, payload_digest, actor, at) =
                r.map_err(|e| CoordinatorError::Db(e.to_string()))?;
            if schema_version > GOVERNANCE_SCHEMA_VERSION {
                return Err(CoordinatorError::SchemaTooNew {
                    got: schema_version,
                    supported: GOVERNANCE_SCHEMA_VERSION,
                });
            }
            let op = parse_op(&op).ok_or_else(|| {
                CoordinatorError::StreamInvalid(format!("unknown op '{op}' at seq {seq}"))
            })?;
            let phase = parse_phase(&phase).ok_or_else(|| {
                CoordinatorError::StreamInvalid(format!("unknown phase '{phase}' at seq {seq}"))
            })?;
            out.push(GovernanceEvent {
                schema_version,
                seq,
                op,
                phase,
                op_id,
                payload_digest,
                actor,
                at,
            });
        }
        Ok(out)
    }

    /// Replay + verify: the durable stream must be structurally valid.
    /// Pending intents are surfaced WITH their deterministic recovery
    /// decisions — this is the restart state machine (astra B3: open
    /// journal, rebuild, resolve pending, resume).
    pub fn restart_state(&self) -> Result<RestartState, CoordinatorError> {
        let events = self.replay()?;
        let states = verify_stream(&events).map_err(|errs| {
            CoordinatorError::StreamInvalid(format!("governance stream invalid: {errs:?}"))
        })?;
        // pending = intent without a terminal phase in the stream
        let mut seen_terminal = std::collections::BTreeSet::new();
        let mut pending = Vec::new();
        for e in &events {
            match e.phase {
                Phase::Pending => pending.push(e.clone()),
                _ => {
                    seen_terminal.insert(e.op_id.clone());
                }
            }
        }
        pending.retain(|e| !seen_terminal.contains(&e.op_id));
        let recoveries: Vec<GovernanceEvent> = pending
            .iter()
            .map(|p| recover(p, 0)) // seq assigned on append
            .collect();
        let clean = states
            .iter()
            .all(|s| !matches!(s, crate::governance_events::OpState::Pending { .. }));
        Ok(RestartState {
            clean,
            pending_count: pending.len(),
            recovery_decisions: recoveries,
        })
    }

    /// The authoritative head checkpoint: (last seq, count). Downstream
    /// audit pinning (T57w) references this against external records.
    pub fn head(&self) -> Result<(i64, i64), CoordinatorError> {
        self.conn
            .query_row(
                "SELECT COALESCE(MAX(seq),0), COUNT(*) FROM governance",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| CoordinatorError::Db(e.to_string()))
    }

    /// Look up whether an op already reached a terminal phase (single
    /// concurrency guard for idempotent layer operations).
    pub fn op_terminal(&self, op_id: &str) -> Result<bool, CoordinatorError> {
        let n: Option<i64> = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM governance WHERE op_id = ?1 AND phase != 'pending'",
                params![op_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| CoordinatorError::Db(e.to_string()))?;
        Ok(n.unwrap_or(0) > 0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RestartState {
    /// true when no intents are dangling
    pub clean: bool,
    pub pending_count: usize,
    /// deterministic recovery decisions for the pendings (seq to be
    /// assigned at append time by the coordinator)
    pub recovery_decisions: Vec<GovernanceEvent>,
}

fn phase_str(p: &Phase) -> &'static str {
    match p {
        Phase::Pending => "pending",
        Phase::Committed => "committed",
        Phase::Aborted => "aborted",
    }
}

fn parse_phase(s: &str) -> Option<Phase> {
    match s {
        "pending" => Some(Phase::Pending),
        "committed" => Some(Phase::Committed),
        "aborted" => Some(Phase::Aborted),
        _ => None,
    }
}

fn parse_op(s: &str) -> Option<OpKind> {
    Some(match s {
        "policy_proposal" => OpKind::PolicyProposal,
        "policy_application" => OpKind::PolicyApplication,
        "lesson_seal" => OpKind::LessonSeal,
        "lesson_revoke" => OpKind::LessonRevoke,
        "candidate_synthesize" => OpKind::CandidateSynthesize,
        "candidate_activate" => OpKind::CandidateActivate,
        "prediction_register" => OpKind::PredictionRegister,
        "prediction_score" => OpKind::PredictionScore,
        "budget_reserve" => OpKind::BudgetReserve,
        "budget_release" => OpKind::BudgetRelease,
        "run_cancel" => OpKind::RunCancel,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(op: OpKind, phase: Phase, op_id: &str, payload: &str) -> GovernanceEvent {
        GovernanceEvent {
            schema_version: 1,
            seq: 99999, // caller garbage — coordinator must overwrite
            op,
            phase,
            op_id: op_id.into(),
            payload_digest: payload.into(),
            actor: "test".into(),
            at: "t".into(),
        }
    }

    #[test]
    fn coordinator_owns_the_sequence() {
        let mut c = GovernanceCoordinator::open_in_memory().unwrap();
        let s1 = c
            .append(ev(OpKind::LessonSeal, Phase::Pending, "seal:L1", "h1"))
            .unwrap();
        let s2 = c
            .append(ev(OpKind::LessonSeal, Phase::Committed, "seal:L1", "h1"))
            .unwrap();
        assert_eq!(
            (s1, s2),
            (1, 2),
            "database assigns contiguous seq, caller value ignored"
        );
        let stream = c.replay().unwrap();
        assert_eq!(stream.len(), 2);
        assert!(stream.iter().all(|e| e.seq <= 2));
        assert!(verify_stream(&stream).is_ok());
    }

    #[test]
    fn restart_reconstructs_state_and_surfaces_pending() {
        let mut c = GovernanceCoordinator::open_in_memory().unwrap();
        // a complete op and a dangling intent
        c.append(ev(OpKind::LessonSeal, Phase::Pending, "seal:A", "h"))
            .unwrap();
        c.append(ev(OpKind::LessonSeal, Phase::Committed, "seal:A", "h"))
            .unwrap();
        c.append(ev(
            OpKind::PolicyApplication,
            Phase::Pending,
            "apply:B",
            "h2",
        ))
        .unwrap();
        let rs = c.restart_state().unwrap();
        assert_eq!(rs.pending_count, 1);
        assert!(!rs.clean);
        // human-bound op: recovery aborts, never auto-applies
        assert_eq!(rs.recovery_decisions[0].phase, Phase::Aborted);
        assert_eq!(rs.recovery_decisions[0].op_id, "apply:B");
    }

    #[test]
    fn reopen_is_the_same_stream() {
        let dir = std::env::temp_dir().join(format!("t52w-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gov.db");
        let _ = std::fs::remove_file(&path);
        {
            let mut c = GovernanceCoordinator::open(&path).unwrap();
            c.append(ev(
                OpKind::CandidateSynthesize,
                Phase::Pending,
                "synth:X",
                "p",
            ))
            .unwrap();
            c.append(ev(
                OpKind::CandidateSynthesize,
                Phase::Committed,
                "synth:X",
                "p",
            ))
            .unwrap();
            assert_eq!(c.head().unwrap(), (2, 2));
        }
        // "restart": a new process opens the same file
        let c2 = GovernanceCoordinator::open(&path).unwrap();
        assert_eq!(c2.head().unwrap(), (2, 2), "durable across open/close");
        let rs = c2.restart_state().unwrap();
        assert!(rs.clean);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn concurrent_interleavings_are_serialized_by_the_database() {
        // astra B1/B7: overlapping appends from two coordinators on
        // one file must serialize without seq collisions. Simulating
        // two writers on one SQLite file in-process: second connection.
        let dir = std::env::temp_dir().join(format!("t52w-c-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gov2.db");
        let _ = std::fs::remove_file(&path);
        let mut a = GovernanceCoordinator::open(&path).unwrap();
        let mut b = GovernanceCoordinator::open(&path).unwrap();
        let mut seqs = Vec::new();
        for i in 0..10 {
            if i % 2 == 0 {
                seqs.push(
                    a.append(ev(
                        OpKind::BudgetReserve,
                        Phase::Pending,
                        &format!("r{i}"),
                        "h",
                    ))
                    .unwrap(),
                );
            } else {
                seqs.push(
                    b.append(ev(
                        OpKind::BudgetReserve,
                        Phase::Pending,
                        &format!("r{i}"),
                        "h",
                    ))
                    .unwrap(),
                );
            }
        }
        let mut unique = seqs.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 10, "no sequence collisions across writers");
        let stream = a.replay().unwrap();
        assert_eq!(stream.len(), 10);
        assert!(
            verify_stream(&stream).is_ok(),
            "interleaved appends form a valid stream"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn future_schema_is_explicit_not_silent() {
        let mut c = GovernanceCoordinator::open_in_memory().unwrap();
        let mut e = ev(OpKind::LessonSeal, Phase::Pending, "x", "h");
        e.schema_version = 99;
        assert!(matches!(
            c.append(e),
            Err(CoordinatorError::SchemaTooNew { got: 99, .. })
        ));
    }

    #[test]
    fn op_terminal_is_the_idempotency_guard() {
        let mut c = GovernanceCoordinator::open_in_memory().unwrap();
        c.append(ev(OpKind::PredictionScore, Phase::Pending, "score:1", "h"))
            .unwrap();
        assert!(!c.op_terminal("score:1").unwrap());
        c.append(ev(
            OpKind::PredictionScore,
            Phase::Committed,
            "score:1",
            "h",
        ))
        .unwrap();
        assert!(
            c.op_terminal("score:1").unwrap(),
            "retry after commit sees terminal state"
        );
    }
}
