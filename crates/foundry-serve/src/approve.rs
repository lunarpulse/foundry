//! UI-1 approve action — T27 (M3). POST /api/orders/{id}/approve.
//!
//! This is the FIRST write endpoint on the read-model API, so the
//! safety posture is explicit:
//!
//!   1. SINGLE-TOKEN: the handler mints exactly one ApprovalToken per
//!      POST (bound to the order's artifact_digest) and hands it to
//!      AppLine::approve — the same DigestGate the CLI goes through.
//!      There is no second mint path; replaying the POST cannot
//!      double-approve (FurnaceHold -> Publishing is a one-way state).
//!   2. INTENT REQUIRED (empty intent => 400) — the human states WHY.
//!   3. LOCALHOST ONLY by default: serve binds 127.0.0.1; remote
//!      approval requires an explicit bind flag + Identity (M4).
//!   4. Ledger discipline unchanged: approve goes through the same
//!      AppLine.commit => journal append => hash chain.
//!
//! The line is rehydrated from the journal on ServeState::new (same
//! defect-#7 recovery the CLI uses), so a restarted serve can still
//! approve orders recorded by watch.
#![allow(clippy::collapsible_if)] // W6a clippy debt: preserve explicit branch and short-circuit ordering

use crate::ServeState;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use foundry_exec::SqliteTupleStore;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct ApproveBody {
    /// human-stated reason — REQUIRED (empty => 400)
    pub intent: String,
}

#[derive(serde::Serialize)]
pub struct ApproveResponse {
    pub order_id: String,
    pub state: String,
    /// journal height AFTER the approval event landed
    pub height: i64,
}

pub async fn approve(
    State(st): State<Arc<ServeState>>,
    Path(id): Path<String>,
    body: Option<Json<ApproveBody>>,
) -> impl IntoResponse {
    let intent = match body {
        Some(Json(b)) if !b.intent.trim().is_empty() => b.intent,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "intent required",
                    "detail": "state WHY this order is approved — the journal records it"
                })),
            )
                .into_response();
        }
    };

    let tuple_path =
        std::env::var("FOUNDRY_TUPLES").unwrap_or_else(|_| "~/.foundry/tuples.db".into());
    let tuple_path = expand_tilde(&tuple_path);
    let tuples = match SqliteTupleStore::open(&tuple_path) {
        Ok(store) => Arc::new(store),
        Err(e) => return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":"tuple store unavailable","detail":format!("{e:?}")})),
        )
            .into_response(),
    };
    st.line.set_tuple_store(tuples.clone());

    // Rehydrate the in-memory line if the order lives only in the
    // journal (watch restarted / serve restarted).
    if st.line.get(&id).await.is_none() {
        let rows = st.ledger.read_all().unwrap_or_default();
        let exists = rows.iter().any(|r| r.order_id == id);
        if !exists {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "unknown order", "order_id": id})),
            )
                .into_response();
        }
        match foundry_line::rehydrate::rehydrate(
            &st.ledger,
            std::path::Path::new("/tmp"),
            tuples.as_ref(),
        ) {
            Ok(orders) => {
                for o in orders {
                    st.line.submit(o).await;
                }
            }
            Err(e) => return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "rehydrate failed", "detail": format!("{e:?}")})),
            )
                .into_response(),
        }
    }

    // order must currently be holding for a decision
    match st.line.get(&id).await {
        Some(o) if o.state() == foundry_domain::OrderState::FurnaceHold => {}
        Some(o) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "order is not awaiting approval",
                    "order_id": id,
                    "state": o.state().to_string(),
                })),
            )
                .into_response();
        }
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "unknown order", "order_id": id})),
            )
                .into_response();
        }
    }

    // ---- the SINGLE token mint (per POST) ----
    let token = foundry_domain::ApprovalToken {
        order_id: id.clone(),
        intent: intent.clone(),
        artifact_digest: st
            .line
            .get(&id)
            .await
            .and_then(|o| o.artifact_digest)
            .unwrap_or_default(),
        actor: human_actor(),
        granted_at: chrono::Utc::now(),
    };

    match st.line.approve(&id, &token, &intent, &human_actor()).await {
        Ok(state) => {
            let height = st.ledger.read_all().map(|r| r.len() as i64).unwrap_or(-1);
            (
                StatusCode::OK,
                Json(ApproveResponse {
                    order_id: id,
                    state: state.to_string(),
                    height,
                }),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "approve refused", "detail": format!("{e:?}")})),
        )
            .into_response(),
    }
}

fn expand_tilde(raw: &str) -> std::path::PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return std::path::PathBuf::from(home).join(rest);
        }
    }
    raw.into()
}

fn human_actor() -> foundry_domain::ActorId {
    foundry_domain::ActorId {
        kind: foundry_domain::ActorKind::Human,
        source: "serve".into(),
        id: "ui-1".into(),
    }
}
