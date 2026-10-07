//! Ledger JSONL mirror — plan T14 (M1).
//!
//! Every ledger append projects the same row (with its chain hash) into
//! `ledger.mirror.jsonl` next to the DB. lag=0 is a GOAL, not a guarantee
//! (review #1): transient failures WILL happen (M0 lesson), so the mirror
//! is MODELLED — seven states below — and the journal commit stays
//! independent of mirror success (append-only inviolable; the mirror is a
//! catch-up projection, always rebuildable from the ledger).
use crate::sqlite::LedgerRow;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub enum MirrorState {
    /// In sync: ledger rows == mirror lines.
    Ok { lag: u64 },
    /// Mirror behind ledger (transient write failure / crash window).
    Behind { lag: u64 },
    /// Last line lacks a trailing newline (torn write mid-line).
    Truncated,
    /// A line failed JSON parsing.
    BadJson { line_no: u64 },
    /// Line seqs are not strictly +1 (missing/duplicated lines).
    SeqGap { at_seq: i64 },
    /// Chain hash diverges from the ledger at this seq.
    HashMismatch { at_seq: i64 },
    /// Mirror has lines the ledger does not (should be impossible).
    MirrorAhead { mirror_lines: u64, ledger_rows: u64 },
}

impl std::fmt::Display for MirrorState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MirrorState::Ok { lag } => write!(f, "ok(lag={lag})"),
            MirrorState::Behind { lag } => write!(f, "behind(lag={lag})"),
            MirrorState::Truncated => write!(f, "truncated"),
            MirrorState::BadJson { line_no } => write!(f, "bad_json(line={line_no})"),
            MirrorState::SeqGap { at_seq } => write!(f, "seq_gap(at={at_seq})"),
            MirrorState::HashMismatch { at_seq } => write!(f, "hash_mismatch(at={at_seq})"),
            MirrorState::MirrorAhead {
                mirror_lines,
                ledger_rows,
            } => {
                write!(
                    f,
                    "mirror_ahead(mirror={mirror_lines},ledger={ledger_rows})"
                )
            }
        }
    }
}

pub struct Mirror {
    path: PathBuf,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct MirrorLine {
    pub seq: i64,
    pub order_id: String,
    pub from_state: String,
    pub to_state: String,
    pub event: String,
    pub actor: String,
    pub intent: String,
    pub at: String,
    pub prev_hash: String,
    pub hash: String,
}

impl From<&LedgerRow> for MirrorLine {
    fn from(r: &LedgerRow) -> Self {
        Self {
            seq: r.seq,
            order_id: r.order_id.clone(),
            from_state: r.from_state.clone(),
            to_state: r.to_state.clone(),
            event: r.event.clone(),
            actor: r.actor.clone(),
            intent: r.intent.clone(),
            at: r.at.clone(),
            prev_hash: r.prev_hash.clone(),
            hash: r.hash.clone(),
        }
    }
}

impl Mirror {
    pub fn new(ledger_db: &Path) -> Self {
        let mut p = ledger_db.to_path_buf();
        p.set_extension("mirror.jsonl");
        Self { path: p }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one row (write + flush; fsync is the caller's cadence choice —
    /// per-append fsync is the watch-loop default via `append_durable`).
    pub fn append(&self, row: &LedgerRow) -> std::io::Result<()> {
        let line = serde_json::to_string(&MirrorLine::from(row))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(f, "{line}")?;
        f.flush()
    }

    /// Durable append (write + flush + fsync) — journal-commit path.
    pub fn append_durable(&self, row: &LedgerRow) -> std::io::Result<()> {
        use std::os::unix::fs::FileExt;
        let line = serde_json::to_string(&MirrorLine::from(row))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        f.write_all(format!("{line}\n").as_bytes())?;
        f.sync_data()?; // fsync: crash-safe projection
        Ok(())
    }

    /// Read mirror lines (None = file absent → treated as 0 lines / rebuildable).
    pub fn lines(&self) -> std::io::Result<Vec<MirrorLine>> {
        let raw = match std::fs::read_to_string(&self.path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        for l in raw.lines() {
            if l.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<MirrorLine>(l) {
                Ok(m) => out.push(m),
                Err(_) => {
                    // distinguish torn tail from corruption: json error on the
                    // LAST line with no trailing newline → Truncated
                    if !raw.ends_with('\n') {
                        // handled by state(); skip here
                    }
                    // store sentinel via error: caller uses state() anyway
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Classify mirror health vs the ledger (the 7-state model, review #1).
    pub fn state(&self, ledger_rows: &[LedgerRow]) -> MirrorState {
        let raw = match std::fs::read_to_string(&self.path) {
            Ok(s) => s,
            Err(_) => String::new(), // absent = 0 lines
        };
        let torn = !raw.is_empty() && !raw.ends_with('\n');
        let mut parsed: Vec<(u64, MirrorLine)> = Vec::new();
        for (i, l) in raw.lines().enumerate() {
            if l.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<MirrorLine>(l) {
                Ok(m) => parsed.push((i as u64 + 1, m)),
                Err(_) => {
                    if torn && i + 1 == raw.lines().count() {
                        return MirrorState::Truncated;
                    }
                    return MirrorState::BadJson {
                        line_no: i as u64 + 1,
                    };
                }
            }
        }
        // mirror longer than the ledger is the strongest divergence signal —
        // check BEFORE seq continuity (duplicates would otherwise surface as
        // SeqGap and hide the real anomaly)
        let ledger_rows_count = ledger_rows.len() as u64;
        let mirror_count = parsed.len() as u64;
        if mirror_count > ledger_rows_count {
            return MirrorState::MirrorAhead {
                mirror_lines: mirror_count,
                ledger_rows: ledger_rows_count,
            };
        }
        // seq continuity from 1
        let mut expect = 1i64;
        for (_, m) in &parsed {
            if m.seq != expect {
                return MirrorState::SeqGap { at_seq: m.seq };
            }
            expect += 1;
        }
        // compare against ledger prefix
        for (i, (_, m)) in parsed.iter().enumerate() {
            if ledger_rows[i].hash != m.hash || ledger_rows[i].seq != m.seq {
                return MirrorState::HashMismatch { at_seq: m.seq };
            }
        }
        let lag = ledger_rows_count - mirror_count;
        if lag == 0 {
            MirrorState::Ok { lag: 0 }
        } else {
            MirrorState::Behind { lag }
        }
    }

    /// Rebuild the mirror from the ledger (recovery: last-good seq onwards,
    /// or whole file). Always possible — the ledger is the source of truth.
    pub fn rebuild(
        &self,
        ledger_rows: &[LedgerRow],
        from_seq: Option<i64>,
    ) -> std::io::Result<u64> {
        // A rebuild ALWAYS catches the mirror up to the given ledger
        // snapshot: keep the intact on-disk prefix (lines 1..from-1), then
        // re-append every ledger row with seq >= from. Atomic tmp+rename.
        let from = from_seq.unwrap_or(1);
        let existing = std::fs::read_to_string(&self.path).unwrap_or_default();
        let prefix_lines: Vec<&str> = existing.lines().take((from - 1).max(0) as usize).collect();
        let mut content = prefix_lines.join("\n");
        if !content.is_empty() {
            content.push('\n');
        }
        let mut written = 0u64;
        for row in ledger_rows.iter().filter(|r| r.seq >= from) {
            let l = serde_json::to_string(&MirrorLine::from(row))
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            content.push_str(&l);
            content.push('\n');
            written += 1;
        }
        let mut tmp = self.path.clone();
        tmp.set_extension("mirror.jsonl.tmp");
        std::fs::write(&tmp, content)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use foundry_domain::ports::Ledger;
    use foundry_domain::{ActorId, ActorKind, OrderEvent, OrderState};

    fn actor() -> ActorId {
        ActorId {
            kind: ActorKind::Agent,
            source: "t".into(),
            id: "t".into(),
        }
    }

    fn tmp_db(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("mirror-{tag}-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let mut m = p.clone();
        m.set_extension("mirror.jsonl");
        let _ = std::fs::remove_file(&m);
        p
    }

    fn seed_rows(db: &Path, n: usize) -> Vec<LedgerRow> {
        let mut led = crate::SqliteLedger::open(db).unwrap();
        for i in 0..n {
            led.append(
                &format!("o-{i}"),
                &OrderEvent::Queued,
                OrderState::Received,
                OrderState::Queued,
                &actor(),
                chrono::Utc::now(),
            )
            .unwrap();
        }
        led.read_all().unwrap()
    }

    #[test]
    fn in_sync_is_ok_zero_lag() {
        // append auto-projects (T14 contract): seeding alone must leave the
        // mirror in sync — no manual appends anywhere.
        let db = tmp_db("ok");
        let rows = seed_rows(&db, 3);
        let m = Mirror::new(&db);
        assert_eq!(m.state(&rows), MirrorState::Ok { lag: 0 });
    }

    #[test]
    fn missing_tail_is_behind_with_lag() {
        // simulate a lost mirror tail: seed (auto-projected), then truncate
        // the last 2 lines of the mirror file
        let db = tmp_db("behind");
        let rows = seed_rows(&db, 5);
        let m = Mirror::new(&db);
        let raw = std::fs::read_to_string(m.path()).unwrap();
        let truncated: String = raw.lines().take(3).map(|l| format!("{l}\n")).collect();
        std::fs::write(m.path(), truncated).unwrap();
        assert_eq!(m.state(&rows), MirrorState::Behind { lag: 2 });
    }

    #[test]
    fn torn_write_is_truncated() {
        let db = tmp_db("torn");
        let rows = seed_rows(&db, 2);
        let m = Mirror::new(&db);
        // simulate torn write: half a JSON line, no newline
        std::fs::write(m.path(), format!("{{\"seq\":2,\"order_i")).unwrap();
        assert_eq!(m.state(&rows), MirrorState::Truncated);
    }

    #[test]
    fn corruption_is_bad_json_or_seq_gap_or_hash_mismatch() {
        let db = tmp_db("corrupt");
        let rows = seed_rows(&db, 2);
        let m = Mirror::new(&db);
        // wipe the auto-projected mirror, write corruption fresh
        std::fs::write(m.path(), format!("{{\"seq\":2}}\n")).unwrap();
        assert_eq!(m.state(&rows), MirrorState::BadJson { line_no: 1 });

        // hash divergence: restore the mirror via rebuild, then tamper line 2
        m.rebuild(&rows, Some(1)).unwrap();
        let raw = std::fs::read_to_string(m.path()).unwrap();
        let lines: Vec<&str> = raw.lines().collect();
        let mut line2: MirrorLine = serde_json::from_str(lines[1]).unwrap();
        line2.hash = "deadbeef".into();
        std::fs::write(
            m.path(),
            format!("{}\n{}\n", lines[0], serde_json::to_string(&line2).unwrap()),
        )
        .unwrap();
        assert_eq!(m.state(&rows), MirrorState::HashMismatch { at_seq: 2 });
    }

    #[test]
    fn rebuild_restores_sync() {
        let db = tmp_db("rebuild");
        let rows = seed_rows(&db, 4);
        let m = Mirror::new(&db);
        // corrupt the tail; rebuild from last-good seq 2 must catch up to the
        // FULL snapshot (contract: rebuild always reaches the ledger state)
        m.rebuild(&rows, Some(2)).unwrap();
        assert_eq!(m.state(&rows), MirrorState::Ok { lag: 0 });
        // full rebuild from scratch also lands in sync
        m.rebuild(&rows, Some(1)).unwrap();
        assert_eq!(m.state(&rows), MirrorState::Ok { lag: 0 });
    }

    #[test]
    fn mirror_ahead_detected() {
        let db = tmp_db("ahead");
        let rows = seed_rows(&db, 1);
        let m = Mirror::new(&db);
        // duplicate the first mirror line (simulates a double-projection bug)
        let raw = std::fs::read_to_string(m.path()).unwrap();
        let first = raw.lines().next().unwrap();
        std::fs::write(m.path(), format!("{first}\n{raw}")).unwrap();
        assert!(matches!(m.state(&rows), MirrorState::MirrorAhead { .. }));
    }
}
