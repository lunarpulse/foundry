//! HTTP layer — axum routes over ServeState (plan T11).
//!
//! Read-only by construction: no handler holds a &mut to anything; every
//! response embeds the snapshot meta (review #2). Bind 127.0.0.1 by default
//! — UI-1 approve actions open up in M3 with Identity (plan §4).
use crate::ServeState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use std::sync::Arc;

pub fn router(state: Arc<ServeState>) -> Router {
    Router::new()
        .route("/api/orders", get(orders))
        .route("/api/orders/{id}", get(order_detail))
        // UI-1: human approve action (T27) — single token mint per POST
        .route("/api/orders/{id}/approve", post(crate::approve_v2::approve))
        .route("/api/audit/verify", get(verify))
        .route("/api/status", get(status))
        // UI-0: single static board, no build step (plan T12)
        .route("/", get(board))
        .with_state(state)
}

async fn board() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("../assets/index.html"),
    )
}

#[derive(Deserialize)]
struct OrdersQuery {
    state: Option<String>,
    limit: Option<usize>,
}

async fn orders(
    State(st): State<Arc<ServeState>>,
    Query(q): Query<OrdersQuery>,
) -> impl IntoResponse {
    // cap limit: unbounded card lists are an ops trap (review #2)
    let limit = q.limit.unwrap_or(200).min(500);
    let resp = st.orders(q.state.as_deref(), limit);
    (
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        Json(resp),
    )
}

async fn order_detail(
    State(st): State<Arc<ServeState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match st.order_detail(&id) {
        Some(d) => (
            StatusCode::OK,
            [(axum::http::header::CACHE_CONTROL, "no-store")],
            Json(d),
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            [(axum::http::header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({"error": "unknown order", "order_id": id})),
        )
            .into_response(),
    }
}

async fn verify(State(st): State<Arc<ServeState>>) -> impl IntoResponse {
    (
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        Json(st.verify()),
    )
}

async fn status(State(st): State<Arc<ServeState>>) -> impl IntoResponse {
    // review #5: file existence/permission bits only — no paths, no values.
    let checks = crate::health::credential_checks();
    let v = st.verify();
    (
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "height": v.height,
            "head_hash": v.head_hash,
            "read_at": v.read_at,
            "chain_valid": v.chain_valid,
            "tmp_usage_pct": foundry_watch_gc::tmp_usage_pct(),
            "credentials": checks,
        })),
    )
}

// gc의 tmp_usage_pct 재노출 (의존역방지용 로컬 shim)
mod foundry_watch_gc {
    pub fn tmp_usage_pct() -> Option<u32> {
        crate::health::tmp_usage_pct()
    }
}
