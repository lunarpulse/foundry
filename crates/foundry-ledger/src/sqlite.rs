//! SqliteLedger — Ledger trait over append-only journal with chain hashing (plan §11).
//! Vendored maos-audit machinery lives in `vendor` (see VENDORED.md).
#![allow(clippy::collapsible_if)] // W6a clippy debt: preserve explicit branch and short-circuit ordering
#![allow(clippy::io_other_error)] // W6a clippy debt: preserve the existing I/O error construction path
#![allow(clippy::to_string_in_format_args)] // W6a clippy debt: preserve the existing implementation and observable behavior
#![allow(clippy::too_many_arguments)] // W6a clippy debt: preserve the established operation binding and call shape

use chrono::{DateTime, Utc};
use foundry_domain::ports::{Ledger, PortError, PortResult};
use foundry_domain::{ActorId, OrderEvent, OrderState};
use rusqlite::{Connection, OptionalExtension};
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
        let has_contract: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='foundry_contract')",
            [], |r| r.get(0),
        ).map_err(port_err)?;
        if has_contract {
            let contract: String = conn
                .query_row(
                    "SELECT contract FROM foundry_contract WHERE singleton=1",
                    [],
                    |r| r.get(0),
                )
                .map_err(port_err)?;
            if contract != "StandaloneV1" {
                return Err(PortError::Permanent("ledger contract mismatch".into()));
            }
        } else {
            let is_product: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='product_meta_v1')", [], |r| r.get(0)).map_err(port_err)?;
            if is_product {
                return Err(PortError::Permanent("ledger contract mismatch".into()));
            }
        }
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
            CREATE TABLE IF NOT EXISTS foundry_contract(singleton INTEGER PRIMARY KEY CHECK(singleton=1), contract TEXT NOT NULL);
            INSERT OR IGNORE INTO foundry_contract(singleton,contract) VALUES(1,'StandaloneV1');
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
            let expect = expected_row_hash(r, &prev);
            if expect != r.hash {
                return Ok(false);
            }
            prev = r.hash.clone();
        }
        Ok(true)
    }

    /// (T56 r4) rows[..=idx] 접두사를 재해시해 마지막 행의 기대 해시 산출 —
    /// 커서 결속에서 저장 해시 보존형 페이로드 변조 감지용.
    pub fn row_hash_at(rows: &[LedgerRow]) -> Option<String> {
        let last = rows.last()?;
        let expect = match crate::event_json::row_version(last) {
            1 => entry_hash(
                last.seq,
                &last.order_id,
                &last.from_state,
                &last.to_state,
                &last.event,
                &last.actor,
                &last.intent,
                &last.at,
                &last.prev_hash,
            ),
            _ => {
                let v1_payload = format!(
                    "{}|{}|{}|{}|{}|{}|{}|{}|{}",
                    last.seq,
                    last.order_id,
                    last.from_state,
                    last.to_state,
                    last.event,
                    last.actor,
                    last.intent,
                    last.at,
                    last.prev_hash
                );
                let json = last.event_json.as_deref().unwrap_or("");
                let payload = crate::event_json::v2_hash_payload(&v1_payload, json);
                use sha2::Digest;
                hex::encode(sha2::Sha256::digest(payload.as_bytes()))
            }
        };
        Some(expect)
    }

    /// Verify a row slice's chain (T56 cursor issuance) — same algorithm
    /// as verify_chain but over caller-provided rows (prefix re-verification).
    pub fn verify_rows(rows: &[LedgerRow]) -> bool {
        let mut prev = "GENESIS".to_string();
        for r in rows {
            if r.prev_hash != prev {
                return false;
            }
            let expect = expected_row_hash(r, &prev);
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

/// Incremental equivalent of the chain-hash portion of `verify_rows` for a
/// seq-ordered SQLite cursor. Sequence continuity is checked here as replay
/// would check it for a materialized slice. This verifier retains only the
/// prior hash and sequence, never prior ledger rows.
pub(crate) struct ChainVerifier {
    previous_hash: String,
    previous_seq: Option<i64>,
}

impl ChainVerifier {
    pub(crate) fn new() -> Self {
        Self {
            previous_hash: "GENESIS".to_owned(),
            previous_seq: None,
        }
    }

    pub(crate) fn push(&mut self, row: &LedgerRow) -> bool {
        if self
            .previous_seq
            .is_some_and(|seq| seq.checked_add(1) != Some(row.seq))
            || row.prev_hash != self.previous_hash
            || expected_row_hash(row, &self.previous_hash) != row.hash
        {
            return false;
        }
        self.previous_seq = Some(row.seq);
        self.previous_hash.clone_from(&row.hash);
        true
    }
}

pub(crate) fn expected_row_hash(row: &LedgerRow, previous_hash: &str) -> String {
    match crate::event_json::row_version(row) {
        1 => entry_hash(
            row.seq,
            &row.order_id,
            &row.from_state,
            &row.to_state,
            &row.event,
            &row.actor,
            &row.intent,
            &row.at,
            previous_hash,
        ),
        _ => {
            // v2: v1 payload + canonical json (event_json hash-covered)
            let v1_payload = format!(
                "{}|{}|{}|{}|{}|{}|{}|{}|{}",
                row.seq,
                row.order_id,
                row.from_state,
                row.to_state,
                row.event,
                row.actor,
                row.intent,
                row.at,
                previous_hash
            );
            let json = row.event_json.as_deref().unwrap_or("");
            let payload = crate::event_json::v2_hash_payload(&v1_payload, json);
            use sha2::Digest;
            hex::encode(sha2::Sha256::digest(payload.as_bytes()))
        }
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
        OrderEvent::ReviewHeld { reason } => reason.clone(),
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
        // (M7 G.B4) head 읽기 + INSERT 를 단일 트랜잭션으로 — 단일 연결
        // 소유 전제 하에서도 원자성을 자체 보장한다 (방어 강화).
        // BEGIN IMMEDIATE: 쓰기 잠금을 트랜잭션 시작 시점에 획득해
        // head-읽기와 INSERT 사이에 다른 쓰기가 끼어들 여지를 제거.
        let _tx_guard = TxGuard::begin_immediate(&conn)?;
        let prev_hash: Option<String> = conn
            .query_row(
                "SELECT hash FROM ledger ORDER BY seq DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(port_err)?;
        let prev_hash = prev_hash.unwrap_or_else(|| "GENESIS".into());
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
        // (M7 G.B4) 트랜잭션 확정 — 실패는 호출자에 전파(호출자는 append
        // 실패=스냅샷 롤백 계약을 따름). 가드를 먼저 소진시켜 Drop 의
        // 이중 커밋을 막는다.
        let guard = _tx_guard;
        guard.commit()?;
        Ok(())
    }
}

/// Append transaction guard. Only an explicit successful `commit` persists;
/// every other exit rolls back so an error path cannot accidentally commit.
struct TxGuard<'c> {
    conn: &'c rusqlite::Connection,
    active: bool,
}

impl<'c> TxGuard<'c> {
    fn begin_immediate(conn: &'c rusqlite::Connection) -> PortResult<Self> {
        conn.execute_batch("BEGIN IMMEDIATE").map_err(port_err)?;
        Ok(Self { conn, active: true })
    }

    /// Commit explicitly. Keep the guard active until SQLite reports success;
    /// on error, attempt rollback before returning the original commit error.
    fn commit(mut self) -> PortResult<()> {
        match self.conn.execute_batch("COMMIT") {
            Ok(()) => {
                self.active = false;
                Ok(())
            }
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                self.active = false;
                Err(port_err(error))
            }
        }
    }
}

impl Drop for TxGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }
}

#[cfg(test)]
mod tx_guard_tests {
    use super::TxGuard;
    use rusqlite::Connection;

    #[test]
    fn failed_commit_rolls_back_and_connection_remains_writable() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute_batch(
            "CREATE TABLE parent (id INTEGER PRIMARY KEY);
             CREATE TABLE child (
                 parent_id INTEGER REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED
             );",
        )
        .unwrap();

        let tx = TxGuard::begin_immediate(&conn).unwrap();
        conn.execute("INSERT INTO child(parent_id) VALUES (7)", [])
            .unwrap();
        assert!(tx.commit().is_err(), "deferred FK must fail at COMMIT");
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM child", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0,
            "failed commit must leave no row"
        );

        conn.execute("INSERT INTO parent(id) VALUES (7)", [])
            .unwrap();
        let tx = TxGuard::begin_immediate(&conn).unwrap();
        conn.execute("INSERT INTO child(parent_id) VALUES (7)", [])
            .unwrap();
        tx.commit().unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM child", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1,
            "connection must accept a later committed transaction"
        );
    }

    #[test]
    fn dropping_uncommitted_guard_rolls_back_and_keeps_connection_writable() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE values_table (value INTEGER NOT NULL);")
            .unwrap();
        {
            let _tx = TxGuard::begin_immediate(&conn).unwrap();
            conn.execute("INSERT INTO values_table VALUES (1)", [])
                .unwrap();
        }
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM values_table", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0,
            "drop must roll back rather than commit"
        );
        let tx = TxGuard::begin_immediate(&conn).unwrap();
        conn.execute("INSERT INTO values_table VALUES (2)", [])
            .unwrap();
        tx.commit().unwrap();
    }
}

#[cfg(test)]
mod incremental_verifier_tests {
    use super::{ChainVerifier, LedgerRow, SqliteLedger, expected_row_hash};

    fn row(seq: i64, prev_hash: &str, event_json: Option<&str>) -> LedgerRow {
        let mut row = LedgerRow {
            seq,
            order_id: format!("order-{seq}"),
            from_state: "received".into(),
            to_state: "queued".into(),
            event: "Queued".into(),
            actor: "synthetic".into(),
            intent: String::new(),
            at: "2026-10-18T00:00:00Z".into(),
            prev_hash: prev_hash.into(),
            hash: String::new(),
            event_json: event_json.map(str::to_owned),
        };
        row.hash = expected_row_hash(&row, prev_hash);
        row
    }

    fn verify_incrementally(rows: &[LedgerRow]) -> bool {
        let mut verifier = ChainVerifier::new();
        rows.iter().all(|row| verifier.push(row))
    }

    #[test]
    fn incremental_hashing_matches_legacy_verifier_for_v1_v2_and_tampering() {
        let first = row(1, "GENESIS", None);
        let second = row(2, &first.hash, Some(r#"{"type":"queued"}"#));
        let valid = vec![first.clone(), second.clone()];
        assert!(SqliteLedger::verify_rows(&valid));
        assert!(verify_incrementally(&valid));

        let mut v1_tampered = valid.clone();
        v1_tampered[0].actor.push_str("-changed");
        assert!(!SqliteLedger::verify_rows(&v1_tampered));
        assert_eq!(
            verify_incrementally(&v1_tampered),
            SqliteLedger::verify_rows(&v1_tampered)
        );

        let mut v2_tampered = valid.clone();
        v2_tampered[1].event_json = Some(r#"{"type":"changed"}"#.into());
        assert!(!SqliteLedger::verify_rows(&v2_tampered));
        assert_eq!(
            verify_incrementally(&v2_tampered),
            SqliteLedger::verify_rows(&v2_tampered)
        );

        let mut gap = valid;
        gap[1].seq = 3;
        assert!(
            !verify_incrementally(&gap),
            "stream scan enforces the replay sequence gap rule"
        );
    }
}
