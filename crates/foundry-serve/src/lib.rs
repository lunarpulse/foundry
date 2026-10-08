//! foundry-serve — the M1 read-model API (plan T11).
//!
//! The ledger is the ONLY source of truth: every request replays the journal
//! snapshot in one logical read (review #2: a consistent snapshot contract).
//! Responses carry `height`, `head_hash`, `read_at` so a consumer always
//! knows which ledger state it is looking at. No orders table — the
//! journal==state invariant is structural, not maintained.

pub mod approve;
pub mod approve_v2;
pub mod fake_noop;
pub mod health;
pub mod http;
pub mod identity;
use foundry_ledger::{LedgerRow, SqliteLedger};
use foundry_line::line::Line;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Order state names, exactly the kanban columns (T12 consumes 1:1).
pub const STATE_NAMES: &[&str] = &[
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
];

#[derive(serde::Serialize)]
pub struct SnapshotMeta {
    pub height: i64,
    pub head_hash: String,
    pub read_at: String,
}

#[derive(serde::Serialize)]
pub struct OrdersResponse {
    pub height: i64,
    pub head_hash: String,
    pub read_at: String,
    pub counts: HashMap<String, usize>,
    pub cards: Vec<Card>,
}

#[derive(serde::Serialize)]
pub struct Card {
    pub order_id: String,
    pub state: String,
    pub attempt: u32,
    pub last_event: String,
    pub at: String,
}

#[derive(serde::Serialize)]
pub struct OrderDetail {
    pub height: i64,
    pub head_hash: String,
    pub read_at: String,
    pub order_id: String,
    pub state: String,
    pub journal: Vec<LedgerRow>,
}

#[derive(serde::Serialize)]
pub struct VerifyResponse {
    pub height: i64,
    pub head_hash: String,
    pub read_at: String,
    pub chain_valid: bool,
}

/// Replay the journal into per-order cards. T16: delegates to the SINGLE
/// replay engine (foundry_ledger::replay) — the bespoke card parser was a
/// duplicated decoder of the Debug repr (reserved defect). Cards are the
/// LAST lifecycle of each order id (kanban semantics: what is it NOW).
/// Chain verification runs through the ledger's own verifier.
pub fn replay_cards(
    rows: &[LedgerRow],
    verify_chain: impl FnOnce(&[LedgerRow]) -> bool,
) -> Result<HashMap<String, Card>, String> {
    let replayed =
        foundry_ledger::replay::replay(rows, verify_chain).map_err(|e| format!("{e:?}"))?;
    let mut cards: HashMap<String, Card> = HashMap::new();
    for ro in replayed {
        // later lifecycles overwrite earlier ones (seq order is preserved)
        cards.insert(
            ro.order_id.clone(),
            Card {
                order_id: ro.order_id.clone(),
                state: ro.order.state().to_string(),
                attempt: ro.order.attempt(),
                last_event: String::new(), // per-card event tail kept simple; detail view has the journal
                at: String::new(),
            },
        );
    }
    Ok(cards)
}

pub type AppLine =
    Line<fake_noop::ServeGate, fake_noop::NoopHarness, SqliteLedger, fake_noop::NoopPublisher>;

pub struct ServeState {
    pub ledger: Arc<SqliteLedger>,
    /// M4 T29: identity store — sessions, capability matrix, token
    /// consumption. Guarded; every privileged op re-resolves state.
    pub identity: tokio::sync::Mutex<identity::Identity>,
    /// T27 UI-1: the shared AppLine so approve actions go through the
    /// SAME gate/ledger path as the CLI (single token mint per action).
    /// Fake harness — serve never executes Dies; it only records human
    /// decisions on already-executed orders.
    pub line: Arc<AppLine>,
}

impl ServeState {
    pub fn new(ledger_path: impl Into<PathBuf>) -> Result<Self, String> {
        let path: PathBuf = ledger_path.into();
        let ledger = SqliteLedger::open(&path).map_err(|e| format!("open ledger: {e:?}"))?;
        let ledger2 = SqliteLedger::open(&path).map_err(|e| format!("open ledger: {e:?}"))?;
        let line: AppLine = Line::new(
            Arc::new(fake_noop::ServeGate),
            Arc::new(fake_noop::NoopHarness),
            Arc::new(tokio::sync::Mutex::new(ledger2)),
            Arc::new(fake_noop::NoopPublisher),
            Arc::new(fake_noop::AllowHumanPolicy),
        );
        let ident = identity::Identity::open(std::path::Path::new(
            &path.with_extension("identity.db"),
        ))
        .map_err(|e| format!("identity open: {e}"))?;
        Ok(Self {
            ledger: Arc::new(ledger),
            identity: tokio::sync::Mutex::new(ident),
            line: Arc::new(line),
        })
    }

    fn snapshot_meta(&self, rows: &[LedgerRow]) -> SnapshotMeta {
        SnapshotMeta {
            height: rows.len() as i64,
            head_hash: rows
                .last()
                .map(|r| r.hash.clone())
                .unwrap_or_else(|| "GENESIS".into()),
            read_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    pub fn orders(&self, state_filter: Option<&str>, limit: usize) -> OrdersResponse {
        // ONE consistent read: read_all is a single locked query pass.
        let rows = self.ledger.read_all().unwrap_or_default();
        let meta = self.snapshot_meta(&rows);
        let cards = replay_cards(&rows, |rs| {
            rs.iter()
                .zip(rs.iter().skip(1))
                .all(|(a, b)| b.prev_hash == a.hash)
        })
        .unwrap_or_default();
        let mut counts: HashMap<String, usize> = HashMap::new();
        for name in STATE_NAMES {
            counts.insert(name.to_string(), 0);
        }
        let mut list: Vec<Card> = cards
            .into_values()
            .filter(|c| state_filter.map_or(true, |f| c.state == f))
            .collect();
        for c in &list {
            *counts.entry(c.state.clone()).or_default() += 1;
        }
        list.sort_by(|a, b| a.at.cmp(&b.at));
        list.truncate(limit);
        OrdersResponse {
            height: meta.height,
            head_hash: meta.head_hash,
            read_at: meta.read_at,
            counts,
            cards: list,
        }
    }

    pub fn order_detail(&self, order_id: &str) -> Option<OrderDetail> {
        let rows = self.ledger.read_all().unwrap_or_default();
        let meta = self.snapshot_meta(&rows);
        let mine: Vec<LedgerRow> = rows
            .iter()
            .filter(|r| r.order_id == order_id)
            .cloned()
            .collect();
        let state = mine.last().map(|r| r.to_state.clone())?;
        Some(OrderDetail {
            height: meta.height,
            head_hash: meta.head_hash,
            read_at: meta.read_at,
            order_id: order_id.to_string(),
            state,
            journal: mine,
        })
    }

    pub fn verify(&self) -> VerifyResponse {
        let rows = self.ledger.read_all().unwrap_or_default();
        let meta = self.snapshot_meta(&rows);
        let chain_valid = self.ledger.verify_chain().unwrap_or(false);
        VerifyResponse {
            height: meta.height,
            head_hash: meta.head_hash,
            read_at: meta.read_at,
            chain_valid,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use foundry_domain::ports::Ledger;
    use foundry_domain::{Order, OrderEvent};

    fn mk_order(id: &str) -> Order {
        Order::new(
            id.into(),
            foundry_domain::ExternalRef::github("o/r", "1", "k".into()),
            chrono::Utc::now(),
        )
    }

    #[test]
    fn replay_groups_by_order_and_tracks_state() {
        let db = std::env::temp_dir().join(format!("serve-t-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        let mut led = SqliteLedger::open(&db).unwrap();
        let o = mk_order("o-1");
        let actor = foundry_domain::ActorId {
            kind: foundry_domain::ActorKind::Agent,
            source: "t".into(),
            id: "t".into(),
        };
        led.append(
            "o-1",
            &OrderEvent::Queued,
            foundry_domain::OrderState::Received,
            foundry_domain::OrderState::Queued,
            &actor,
            chrono::Utc::now(),
        )
        .unwrap();
        led.append(
            "o-1",
            &OrderEvent::DieStarted,
            foundry_domain::OrderState::Queued,
            foundry_domain::OrderState::Working,
            &actor,
            chrono::Utc::now(),
        )
        .unwrap();
        led.append(
            "o-1",
            &OrderEvent::DieSucceeded { attempt: 1 },
            foundry_domain::OrderState::Working,
            foundry_domain::OrderState::Inspecting,
            &actor,
            chrono::Utc::now(),
        )
        .unwrap();
        let rows = led.read_all().unwrap();
        let cards = replay_cards(&rows, |rs| {
            rs.iter()
                .zip(rs.iter().skip(1))
                .all(|(a, b)| b.prev_hash == a.hash)
        })
        .unwrap_or_default();
        assert_eq!(cards["o-1"].state, "review");
        assert_eq!(cards["o-1"].attempt, 1);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn orders_response_has_consistent_meta() {
        let db = std::env::temp_dir().join(format!("serve-m-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        let mut led = SqliteLedger::open(&db).unwrap();
        let actor = foundry_domain::ActorId {
            kind: foundry_domain::ActorKind::Agent,
            source: "t".into(),
            id: "t".into(),
        };
        led.append(
            "o-9",
            &OrderEvent::Queued,
            foundry_domain::OrderState::Received,
            foundry_domain::OrderState::Queued,
            &actor,
            chrono::Utc::now(),
        )
        .unwrap();
        let state = ServeState::new(&db).unwrap();
        let resp = state.orders(None, 50);
        assert_eq!(resp.height, 1);
        assert_ne!(resp.head_hash, "GENESIS");
        assert_eq!(resp.counts["queued"], 1);
        assert_eq!(resp.cards.len(), 1);
        let v = state.verify();
        assert!(v.chain_valid);
        let _ = std::fs::remove_file(&db);
    }
}
