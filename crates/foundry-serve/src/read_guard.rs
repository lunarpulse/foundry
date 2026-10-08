//! M4 T30: read endpoints (board, detail, verify, status) now go
//! through the scope guard. "Authenticate reads as well as writes"
//! (review #4). Public read of the board was an M1-M3 convenience —
//! organization deployments require a session.
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use std::sync::Arc;

use crate::identity::AuthenticatedUser;
use crate::scope::ScopeError;

/// Resolve the Bearer session -> current user, or None.
pub async fn resolve_user(
    st: &Arc<crate::ServeState>,
    bearer: Option<String>,
) -> Option<AuthenticatedUser> {
    let token = bearer?;
    let ident = st.identity.lock().await;
    ident.resolve_session(&token).ok()
}

pub fn bearer_of(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t.to_string())
}

pub fn scope_denied(e: ScopeError) -> axum::response::Response {
    let code = match &e {
        ScopeError::Unauthenticated => StatusCode::UNAUTHORIZED,
        _ => StatusCode::FORBIDDEN,
    };
    (code, Json(serde_json::json!({"error": e.to_string()}))).into_response()
}

// ---- guarded wrappers over ServeState reads ----

pub async fn orders_guarded(
    st: Arc<crate::ServeState>,
    user: Option<AuthenticatedUser>,
    state_filter: Option<String>,
    limit: usize,
) -> axum::response::Response {
    // guard BEFORE serialization (review #7)
    {
        let scope = st.scope.lock().await;
        if let Err(e) = scope.guard_read(user.as_ref(), None) {
            return scope_denied(e);
        }
    }
    let resp = st.orders(state_filter.as_deref(), limit);
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, "no-store")],
        Json(resp),
    )
        .into_response()
}

pub async fn detail_guarded(
    st: Arc<crate::ServeState>,
    user: Option<AuthenticatedUser>,
    order_id: &str,
) -> axum::response::Response {
    {
        let scope = st.scope.lock().await;
        if let Err(e) = scope.guard_read(user.as_ref(), Some(order_id)) {
            return scope_denied(e);
        }
    }
    match st.order_detail(order_id) {
        Some(d) => (
            StatusCode::OK,
            [(header::CACHE_CONTROL, "no-store")],
            Json(d),
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "unknown order", "order_id": order_id})),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
pub struct OrdersQuery {
    state: Option<String>,
    limit: Option<usize>,
}

pub async fn orders_handler(
    State(st): State<Arc<crate::ServeState>>,
    headers: axum::http::HeaderMap,
    Query(q): Query<OrdersQuery>,
) -> impl IntoResponse {
    let user = resolve_user(&st, bearer_of(&headers)).await;
    let limit = q.limit.unwrap_or(200).min(500);
    orders_guarded(st, user, q.state, limit).await
}

pub async fn detail_handler(
    State(st): State<Arc<crate::ServeState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let user = resolve_user(&st, bearer_of(&headers)).await;
    detail_guarded(st, user, &id).await
}

pub async fn verify_handler(
    State(st): State<Arc<crate::ServeState>>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let user = resolve_user(&st, bearer_of(&headers)).await;
    {
        let scope = st.scope.lock().await;
        if let Err(e) = scope.guard_read(user.as_ref(), None) {
            return scope_denied(e);
        }
    }
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(st.verify()),
    )
        .into_response()
}
