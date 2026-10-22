//! HTTP layer — axum routes over ServeState (plan T11).
//!
//! Read-only by construction: no handler holds a &mut to anything; every
//! response embeds the snapshot meta (review #2). Bind 127.0.0.1 by default
//! — UI-1 approve actions open up in M3 with Identity (plan §4).
use crate::ServeState;
use axum::routing::{get, post};
use axum::{Json, Router};
use axum::{
    extract::{Request, State},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use std::sync::Arc;

pub fn router(state: Arc<ServeState>) -> Router {
    Router::new()
        .route("/api/orders", get(crate::read_guard::orders_handler))
        .route("/api/orders/{id}", get(crate::read_guard::detail_handler))
        // UI-1: human approve action (T27) — single token mint per POST
        .route("/api/orders/{id}/approve", post(crate::approve_v2::approve))
        .route(
            "/api/orders/{id}/review/hold/{hold_id}/resume",
            post(crate::review_hold::resume),
        )
        .route(
            "/api/orders/{id}/review/hold/{hold_id}/commit-pass",
            post(crate::review_hold::commit_pass),
        )
        .route(
            "/api/orders/{id}/review/hold/{hold_id}/commit-fail",
            post(crate::review_hold::commit_fail),
        )
        .route(
            "/api/orders/{id}/review/hold/{hold_id}/abort",
            post(crate::review_hold::abort),
        )
        .route("/api/audit/verify", get(crate::read_guard::verify_handler))
        .route("/api/status", get(status))
        // M5.2 T60b: the production policy-application entry (N1
        // ordering, N2 rebinding, session-resolved roles)
        .route("/api/policy/apply", post(crate::policy_http::apply))
        // UI-0: single static board, no build step (plan T12)
        .route("/", get(board))
        .with_state(state)
}

/// ProductAtomicV1 routes. The caller opens ProductStore for the selected
/// database contract; no StandaloneV1 identity or approval state is mounted.
pub fn product_router(state: Arc<crate::auth_http::ProductHttpState>) -> Router {
    let guard_state = state.clone();
    crate::auth_http::router(state.clone())
        .merge(crate::artifact_http::router(state.clone()))
        .merge(crate::approval_service::router(state.clone()))
        .merge(crate::review_hold::product_router(state.clone()))
        .merge(crate::scope_http::router(state.clone()))
        .merge(crate::export_http::router(state))
        .route("/", get(board))
        .route_layer(middleware::from_fn_with_state(
            guard_state,
            require_product_scope,
        ))
}

async fn require_product_scope(
    State(state): State<Arc<crate::auth_http::ProductHttpState>>,
    request: Request,
    next: Next,
) -> Response {
    if !matches!(state.store.lock().await.scope_policy_enforced(), Ok(true)) {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error":"product scope setup or migration required"})),
        )
            .into_response();
    }
    next.run(request).await
}

async fn board() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("../assets/index.html"),
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
