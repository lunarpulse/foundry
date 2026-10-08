//! CastingRun journal — SQLite table with the same v2 hash-chain
//! discipline as the order ledger (M3 T22).
//!
//! One row per lifecycle transition; hash covers the prior hash, the
//! row payload, AND the canonical event JSON (schema v2 semantics
//! inherited from the order ledger — tampering with any field breaks
//! the chain).
use foundry_domain::casting::{CastingState, Verdict};
use rusqlite::Connection;
use std::path::Path;

pub struct CastingLedger {
    con: Connection,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CastingRow {
    pub seq: i64,
    pub run_id: String,
    pub from_state: String,
    pub to_state: String,
    pub at: String,
    pub prev_hash: String,
    pub hash: String,
    pub event_json: Option<String>,
}

fn state_str(s: &CastingState) -> String {
    match s {
        CastingState::Draft => "draft".into(),
        CastingState::Running => "running".into(),
        CastingState::Judged { verdict } => format!(
            "judged:{}",
            match verdict {
                Verdict::Promote => "promote",
                Verdict::Reject => "reject",
                Verdict::Inconclusive => "inconclusive",
            }
        ),
        CastingState::Promoted => "promoted".into(),
        CastingState::Rejected { .. } => "rejected".into(),
    }
}

fn row_hash(prev: &str, run_id: &str, from: &str, to: &str, at: &str, event_json: &str) -> String {
    use sha2::{Digest, Sha256};
    let payload = format!("casting-v2\n{prev}\n{run_id}\n{from}\n{to}\n{at}\n{event_json}");
    hex::encode(Sha256::digest(payload.as_bytes()))
}

impl CastingLedger {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let con = if path.as_os_str().is_empty() {
            Connection::open_in_memory()?
        } else {
            Connection::open(path)?
        };
        con.execute_batch(
            "CREATE TABLE IF NOT EXISTS casting_runs (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL,
                from_state TEXT NOT NULL,
                to_state TEXT NOT NULL,
                at TEXT NOT NULL,
                prev_hash TEXT NOT NULL,
                hash TEXT NOT NULL,
                event_json TEXT
            );
            CREATE TRIGGER IF NOT EXISTS casting_append_only
            BEFORE UPDATE ON casting_runs
            BEGIN
                SELECT RAISE(ABORT, 'casting journal is append-only');
            END;",
        )?;
        Ok(Self { con })
    }

    pub fn append(
        &self,
        run_id: &str,
        from: &CastingState,
        to: &CastingState,
        at: &str,
    ) -> rusqlite::Result<()> {
        let (prev_seq, prev_hash) = self.tip()?;
        let event_json = serde_json::json!({
            "run_id": run_id,
            "from": state_str(from),
            "to": state_str(to),
            "at": at,
        })
        .to_string();
        let h = row_hash(
            &prev_hash,
            run_id,
            &state_str(from),
            &state_str(to),
            at,
            &event_json,
        );
        self.con.execute(
            "INSERT INTO casting_runs (run_id, from_state, to_state, at, prev_hash, hash, event_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![run_id, state_str(from), state_str(to), at, prev_hash, h, event_json],
        )?;
        let _ = prev_seq;
        Ok(())
    }

    fn tip(&self) -> rusqlite::Result<(i64, String)> {
        self.con
            .query_row(
                "SELECT seq, hash FROM casting_runs ORDER BY seq DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .or_else(|_| Ok((0, "genesis".to_string())))
    }

    pub fn verify_chain(&self) -> rusqlite::Result<bool> {
        let mut stmt = self.con.prepare(
            "SELECT seq, run_id, from_state, to_state, at, prev_hash, hash, event_json FROM casting_runs ORDER BY seq",
        )?;
        let rows: Vec<(
            i64,
            String,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
        )> = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        let mut prev = "genesis".to_string();
        for (i, (seq, run_id, from, to, at, ph, h, ej)) in rows.iter().enumerate() {
            if *ph != prev {
                return Ok(false);
            }
            let recomputed = row_hash(ph, run_id, from, to, at, ej.as_deref().unwrap_or(""));
            if recomputed != *h {
                return Ok(false);
            }
            let _ = (seq, i);
            prev = h.clone();
        }
        Ok(true)
    }

    /// ADVERSARIAL path for gate fixtures: drop the append-only
    /// trigger and rewrite a row, simulating a DB-level attacker.
    /// Only used by tests/examples to prove verify_chain catches it.
    #[doc(hidden)]
    pub fn adversarial_tamper(&self) -> rusqlite::Result<()> {
        self.con
            .execute_batch("DROP TRIGGER IF EXISTS casting_append_only;")?;
        self.con.execute(
            "UPDATE casting_runs SET to_state='promoted' WHERE seq=1",
            [],
        )?;
        Ok(())
    }

    /// Full state of a run by replaying its rows (single consumer rule:
    /// no bespoke parser — this IS the casting replay, engine-style).
    pub fn final_state(&self, run_id: &str) -> Option<CastingState> {
        self.con
            .query_row(
                "SELECT to_state FROM casting_runs WHERE run_id = ?1 ORDER BY seq DESC LIMIT 1",
                [run_id],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .map(|s| match s.as_str() {
                "draft" => CastingState::Draft,
                "running" => CastingState::Running,
                "promoted" => CastingState::Promoted,
                "rejected" => CastingState::Rejected {
                    reason: String::new(),
                },
                other => {
                    if let Some(v) = other.strip_prefix("judged:") {
                        let verdict = match v {
                            "promote" => Verdict::Promote,
                            "reject" => Verdict::Reject,
                            _ => Verdict::Inconclusive,
                        };
                        CastingState::Judged { verdict }
                    } else {
                        CastingState::Draft
                    }
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_and_append_only() {
        let led = CastingLedger::open(Path::new("")).unwrap();
        led.append("cast-1", &CastingState::Draft, &CastingState::Running, "t1")
            .unwrap();
        led.append(
            "cast-1",
            &CastingState::Running,
            &CastingState::Judged {
                verdict: Verdict::Promote,
            },
            "t2",
        )
        .unwrap();
        assert!(led.verify_chain().unwrap());
        assert_eq!(
            led.final_state("cast-1"),
            Some(CastingState::Judged {
                verdict: Verdict::Promote
            })
        );
        // append-only trigger refuses UPDATE
        let res = led.con.execute(
            "UPDATE casting_runs SET to_state='promoted' WHERE seq=1",
            [],
        );
        assert!(res.is_err());
    }

    #[test]
    fn tamper_breaks_chain() {
        let led = CastingLedger::open(Path::new("")).unwrap();
        led.append("cast-1", &CastingState::Draft, &CastingState::Running, "t1")
            .unwrap();
        // bypass trigger: drop it, then tamper (adversarial path)
        led.con
            .execute_batch("DROP TRIGGER casting_append_only;")
            .unwrap();
        led.con
            .execute("UPDATE casting_runs SET to_state='promoted'", [])
            .unwrap();
        assert!(!led.verify_chain().unwrap());
    }
}
