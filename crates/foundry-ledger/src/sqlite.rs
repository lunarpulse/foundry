//! SqliteLedger — Ledger trait over append-only journal with chain hashing (plan §11).
//! Vendored maos-audit machinery lives in `vendor` (see VENDORED.md).
use chrono::{DateTime, Utc};
use foundry_domain::ports::{Ledger, PortError, PortResult};
use foundry_domain::{ActorId, OrderEvent, OrderState};
use rusqlite::Connection;
use std::path::Path;
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub seq: i64,
    pub order_id: String,
    pub from_state: String,
    pub to_state: String,
    pub event: String,
    pub actor: String,
    pub intent: String,
    pub at: DateTime<Utc>,
    pub prev_hash: String,
    pub hash: String,
    /// Journal schema v2 (T17): canonical JSON of the event; NULL/empty on
    /// legacy (v1) rows. Hash-covered on v2 rows (see event_json.rs).
    pub event_json: Option<String>,
}

pub struct SqliteLedger {
    conn: Mutex<Connection>,
    db_path: std::path::PathBuf,
}

impl SqliteLedger {
    pub fn open(path: &Path) -> Result<Self, PortError> {
        let db_path = path.to_path_buf();
        let conn = Connection::open(path)
            .map_err(|e| PortError::Permanent(format!("open ledger: {e}")))?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS ledger (
                seq        INTEGER PRIMARY KEY AUTOINCREMENT,
                order_id   TEXT NOT NULL,
                from_state TEXT NOT NULL,
                to_state   TEXT NOT NULL,
                event      TEXT NOT NULL,
                actor      TEXT NOT NULL,
                intent     TEXT NOT NULL DEFAULT '',
                at         TEXT NOT NULL,
                prev_hash  TEXT NOT NULL,
                event_json TEXT,
                hash       TEXT NOT NULL
            );
            -- append-only: no UPDATE, no DELETE (plan §11 immutability)
            CREATE TRIGGER IF NOT EXISTS ledger_no_update
              BEFORE UPDATE ON ledger BEGIN
                SELECT RAISE(ABORT, 'ledger is append-only');
            END;
            CREATE TRIGGER IF NOT EXISTS ledger_no_delete
              BEFORE DELETE ON ledger BEGIN
                SELECT RAISE(ABORT, 'ledger is append-only');
            END;
            "#,
        )
        .map_err(|e| PortError::Permanent(format!("init ledger: {e}")))?;
        Ok(Self {
            conn: Mutex::new(conn),
            db_path,
        })
    }

    /// Verify the whole chain (M1 gate: chain-hash check).
    pub fn verify_chain(&self) -> PortResult<bool> {
        let rows = self.read_all()?;
        let mut prev = "GENESIS".to_string();
        for r in &rows {
            if r.prev_hash != prev {
                return Ok(false); // chain broken
            }
            let expect = match crate::event_json::row_version(r) {
                1 => entry_hash(
                    r.seq,
                    &r.order_id,
                    &r.from_state,
                    &r.to_state,
                    &r.event,
                    &r.actor,
                    &r.intent,
                    &r.at,
                    &prev,
                ),
                _ => {
                    // v2: v1 payload + canonical json (event_json hash-covered)
                    let v1_payload = format!(
                        "{}|{}|{}|{}|{}|{}|{}|{}|{}",
                        r.seq,
                        r.order_id,
                        r.from_state,
                        r.to_state,
                        r.event,
                        r.actor,
                        r.intent,
                        r.at,
                        prev
                    );
                    let json = r.event_json.as_deref().unwrap_or("");
                    let payload = crate::event_json::v2_hash_payload(&v1_payload, json);
                    use sha2::{Digest, Sha256};
                    hex::encode(sha2::Sha256::digest(payload.as_bytes()))
                }
            };
            if expect != r.hash {
                return Ok(false);
            }
            prev = r.hash.clone();
        }
        Ok(true)
    }

    /// Verify a row slice's chain (T56 cursor issuance) — same algorithm
    /// as verify_chain but over caller-provided rows (prefix re-verification).
    pub fn verify_rows(rows: &[LedgerRow]) -> bool {
        let mut prev = "GENESIS".to_string();
        for r in rows {
            if r.prev_hash != prev {
                return false;
            }
            let expect = match crate::event_json::row_version(r) {
                1 => entry_hash(
                    r.seq,
                    &r.order_id,
                    &r.from_state,
                    &r.to_state,
                    &r.event,
                    &r.actor,
                    &r.intent,
                    &r.at,
                    &prev,
                ),
                _ => {
                    let v1_payload = format!(
                        "{}|{}|{}|{}|{}|{}|{}|{}|{}",
                        r.seq,
                        r.order_id,
                        r.from_state,
                        r.to_state,
                        r.event,
                        r.actor,
                        r.intent,
                        r.at,
                        prev
                    );
                    let json = r.event_json.as_deref().unwrap_or("");
                    let payload = crate::event_json::v2_hash_payload(&v1_payload, json);
                    use sha2::{Digest, Sha256};
                    hex::encode(sha2::Sha256::digest(payload.as_bytes()))
                }
            };
            if expect != r.hash {
                return false;
            }
            prev = r.hash.clone();
        }
        true
    }

    /// Full journal snapshot, seq-ordered — the read-model's only input
    /// (M1 T11: serve replays this inside ONE read transaction; the
    /// ledger==state invariant makes a separate orders table unnecessary).
    pub fn read_all(&self) -> PortResult<Vec<LedgerRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT seq, order_id, from_state, to_state, event, actor, intent, at, prev_hash, hash, event_json FROM ledger ORDER BY seq",
            )
            .map_err(port_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(LedgerRow {
                    seq: r.get(0)?,
                    order_id: r.get(1)?,
                    from_state: r.get(2)?,
                    to_state: r.get(3)?,
                    event: r.get(4)?,
                    actor: r.get(5)?,
                    intent: r.get(6)?,
                    at: r.get(7)?,
                    prev_hash: r.get(8)?,
                    hash: r.get(9)?,
                    event_json: r.get::<_, Option<String>>(10)?,
                })
            })
            .map_err(port_err)?
            .collect::<Result<_, _>>()
            .map_err(port_err)?;
        Ok(rows)
    }
}

/// One immutable journal row (serve read-model input).
#[derive(Debug, Clone, serde::Serialize)]
pub struct LedgerRow {
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
    /// Journal schema v2 (T17): canonical event JSON; NULL on v1 rows.
    pub event_json: Option<String>,
}

fn port_err(e: rusqlite::Error) -> PortError {
    PortError::Permanent(format!("ledger: {e}"))
}

fn entry_hash(
    seq: i64,
    order_id: &str,
    from: &str,
    to: &str,
    event: &str,
    actor: &str,
    intent: &str,
    at: &str,
    prev: &str,
) -> String {
    use sha2::{Digest, Sha256};
    let payload = format!("{seq}|{order_id}|{from}|{to}|{event}|{actor}|{intent}|{at}|{prev}");
    let digest = Sha256::digest(payload.as_bytes());
    hex::encode(digest)
}

fn event_repr(event: &OrderEvent) -> String {
    format!("{event:?}")
}

fn intent_of(event: &OrderEvent) -> String {
    match event {
        OrderEvent::Approved { intent, .. } | OrderEvent::Rejected { intent } => intent.clone(),
        OrderEvent::Canceled { reason } | OrderEvent::CancelRequested { reason } => reason.clone(),
        OrderEvent::DieFailed { reason, .. } | OrderEvent::InspectionFailed { reason } => {
            reason.clone()
        }
        _ => String::new(),
    }
}

impl Ledger for SqliteLedger {
    fn append(
        &mut self,
        order_id: &str,
        event: &OrderEvent,
        from: OrderState,
        to: OrderState,
        actor: &ActorId,
        at: DateTime<Utc>,
    ) -> PortResult<()> {
        // journal commit FIRST — the mirror is a projection and must never
        // gate the journal (review #1: lag=0 is a goal, not a guarantee;
        // transient mirror failures surface in mirror_state, not here)
        let appended = self.append_journal(order_id, event, from, to, actor, at);
        if appended.is_ok() {
            if let Err(e) = self.project_to_mirror() {
                eprintln!("mirror: projection failed (journal unaffected): {e}");
            }
        }
        appended
    }
}

impl SqliteLedger {
    /// Project the freshest journal row into the JSONL mirror (durable).
    /// Journal stays authoritative; a failed projection only logs.
    fn project_to_mirror(&mut self) -> std::io::Result<()> {
        let rows = self
            .read_all()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("{e:?}")))?;
        let Some(last) = rows.last() else {
            return Ok(());
        };
        let mirror = crate::mirror::Mirror::new(&self.db_path);
        mirror.append_durable(last)
    }

    fn append_journal(
        &mut self,
        order_id: &str,
        event: &OrderEvent,
        from: OrderState,
        to: OrderState,
        actor: &ActorId,
        at: DateTime<Utc>,
    ) -> PortResult<()> {
        let conn = self.conn.lock().unwrap();
        let prev_hash: String = conn
            .query_row(
                "SELECT hash FROM ledger ORDER BY seq DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| "GENESIS".into());
        let seq: i64 = conn
            .query_row("SELECT COALESCE(MAX(seq),0)+1 FROM ledger", [], |r| {
                r.get(0)
            })
            .map_err(port_err)?;
        let event_s = event_repr(event);
        let intent = intent_of(event);
        let at_s = at.to_rfc3339();
        let actor_s = format!("{:?}/{}", actor.kind, actor.id);
        // T17: dual-write — canonical JSON alongside the repr. Consistency
        // gate (review #3): the JSON must decode to the same event the
        // replay engine's repr decoder yields, or the write is refused.
        let json_s = crate::event_json::event_to_json(event);
        {
            let from_repr = crate::replay::decode_for_consistency(&event_s, &intent);
            let from_json = crate::event_json::event_from_json(&json_s)
                .ok_or_else(|| PortError::Permanent("event_json decode failed".into()))?;
            let from_repr = from_repr.ok_or_else(|| {
                PortError::Permanent("event repr decode failed (dual-write divergence)".into())
            })?;
            if format!("{from_repr:?}") != format!("{from_json:?}") {
                return Err(PortError::Permanent(format!(
                    "dual-write divergence: repr decodes {from_repr:?}, json decodes {from_json:?}"
                )));
            }
        }
        // T17: v2 hash — v1 payload + canonical json (review #2: event_json
        // is hash-covered; tampering with it breaks the chain).
        let v1_payload = format!(
            "{seq}|{order_id}|{}|{}|{event_s}|{actor_s}|{intent}|{at_s}|{prev_hash}",
            from.to_string(),
            to.to_string()
        );
        let hash = {
            use sha2::{Digest, Sha256};
            let payload = crate::event_json::v2_hash_payload(&v1_payload, &json_s);
            hex::encode(Sha256::digest(payload.as_bytes()))
        };
        conn.execute(
            "INSERT INTO ledger (order_id, from_state, to_state, event, actor, intent, at, prev_hash, hash, event_json)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            rusqlite::params![
                order_id,
                from.to_string(),
                to.to_string(),
                event_s,
                actor_s,
                intent,
                at_s,
                prev_hash,
                hash,
                json_s
            ],
        )
        .map_err(port_err)?;
        Ok(())
    }
}
