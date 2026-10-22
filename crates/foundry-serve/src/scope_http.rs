//! Product scope administration and scoped order reads.
//!
//! All authority and data filtering is performed by ProductStore. The HTTP
//! layer only validates bounded typed requests and maps failures to generic
//! public responses.
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use foundry_ledger::{ProductCapability, RepositoryIdentity, ScopeMutation};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRequest {
    pub connector_id: String,
    pub organization: String,
    pub provider: String,
    pub api_origin: String,
    pub stable_repository_id: String,
    pub api_owner: String,
    pub api_repo: String,
    /// Explicit compatibility assertion for a pre-scope source row. It is
    /// never used as repository identity or to construct an API URL.
    pub legacy_repository_locator: String,
    pub scope_query: String,
    pub enabled: bool,
    pub expected_revision: i64,
    pub request_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeChangeRequest {
    pub subject_id: String,
    pub source_instance_id: String,
    pub capability: ProductCapability,
    pub expected_scope_revision: i64,
    pub request_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantQuery {
    pub subject_id: String,
    pub source_instance_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderListQuery {
    pub after: Option<String>,
    pub limit: Option<usize>,
}

pub fn router(state: Arc<crate::auth_http::ProductHttpState>) -> Router {
    Router::new()
        .route(
            "/api/product/sources",
            get(list_sources).post(configure_source),
        )
        .route("/api/product/scope/grants", get(list_grants).post(grant))
        .route("/api/product/scope/grants/revoke", post(revoke))
        .route("/api/product/orders", get(list_orders))
        .route("/api/product/orders/{order_id}", get(order_detail))
        .layer(DefaultBodyLimit::max(8 * 1024))
        .with_state(state)
}

async fn list_sources(
    State(state): State<Arc<crate::auth_http::ProductHttpState>>,
    headers: HeaderMap,
) -> axum::response::Response {
    let Some(session) = crate::auth_http::bearer_digest(&headers) else {
        return error(StatusCode::UNAUTHORIZED, "authentication required");
    };
    match state
        .store
        .lock()
        .await
        .list_scoped_sources(&session, chrono::Utc::now())
    {
        Ok(sources) => {
            (StatusCode::OK, Json(serde_json::json!({"sources":sources}))).into_response()
        }
        Err(_) => error(StatusCode::FORBIDDEN, "source administration unavailable"),
    }
}

async fn configure_source(
    State(state): State<Arc<crate::auth_http::ProductHttpState>>,
    headers: HeaderMap,
    Json(body): Json<SourceRequest>,
) -> axum::response::Response {
    let Some(session) = crate::auth_http::bearer_digest(&headers) else {
        return error(StatusCode::UNAUTHORIZED, "authentication required");
    };
    if body.connector_id.trim().is_empty()
        || body.connector_id.len() > 256
        || body.organization.trim().is_empty()
        || body.organization.len() > 256
        || body.provider.trim().is_empty()
        || body.provider.len() > 64
        || body.api_origin.len() > 512
        || body.stable_repository_id.trim().is_empty()
        || body.stable_repository_id.len() > 256
        || body.api_owner.trim().is_empty()
        || body.api_repo.trim().is_empty()
        || body.api_owner.len() > 256
        || body.api_repo.len() > 256
        || body.legacy_repository_locator.len() > 256
        || body.scope_query.trim().is_empty()
        || body.scope_query.len() > 1024
        || body.request_key.trim().is_empty()
        || body.request_key.len() > 256
        || body.expected_revision < 0
    {
        return error(StatusCode::BAD_REQUEST, "invalid scope request");
    }
    let identity = match RepositoryIdentity::canonicalized(
        body.organization,
        body.provider,
        body.api_origin,
        body.stable_repository_id,
    ) {
        Ok(identity) => identity,
        Err(_) => return error(StatusCode::BAD_REQUEST, "invalid scope request"),
    };
    let configured = {
        let mut store = state.store.lock().await;
        store.configure_scoped_source(
            &session,
            &body.connector_id,
            identity,
            &body.api_owner,
            &body.api_repo,
            &body.legacy_repository_locator,
            &body.scope_query,
            body.enabled,
            body.expected_revision,
            &body.request_key,
            chrono::Utc::now(),
        )
    };
    match configured {
        Ok(source) => {
            let revision = state
                .store
                .lock()
                .await
                .list_scoped_sources(&session, chrono::Utc::now())
                .ok()
                .and_then(|views| {
                    views
                        .into_iter()
                        .find(|view| view.source.source_instance_id == source.source_instance_id)
                })
                .map(|view| view.source_revision);
            match revision {
                Some(source_revision) => (
                    StatusCode::OK,
                    Json(serde_json::json!({"source":source,"source_revision":source_revision})),
                )
                    .into_response(),
                None => error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "source revision unavailable",
                ),
            }
        }
        Err(_) => error(StatusCode::CONFLICT, "source configuration unavailable"),
    }
}

async fn list_grants(
    State(state): State<Arc<crate::auth_http::ProductHttpState>>,
    headers: HeaderMap,
    Query(query): Query<GrantQuery>,
) -> axum::response::Response {
    let Some(session) = crate::auth_http::bearer_digest(&headers) else {
        return error(StatusCode::UNAUTHORIZED, "authentication required");
    };
    match state.store.lock().await.list_product_scope_grants(
        &session,
        &query.subject_id,
        &query.source_instance_id,
        chrono::Utc::now(),
    ) {
        Ok(grants) => (StatusCode::OK, Json(serde_json::json!({"grants":grants}))).into_response(),
        Err(_) => error(StatusCode::FORBIDDEN, "scope grants unavailable"),
    }
}

async fn grant(
    State(state): State<Arc<crate::auth_http::ProductHttpState>>,
    headers: HeaderMap,
    Json(body): Json<ScopeChangeRequest>,
) -> axum::response::Response {
    change_scope(state, headers, body, ScopeMutation::Grant).await
}

async fn revoke(
    State(state): State<Arc<crate::auth_http::ProductHttpState>>,
    headers: HeaderMap,
    Json(body): Json<ScopeChangeRequest>,
) -> axum::response::Response {
    change_scope(state, headers, body, ScopeMutation::Revoke).await
}

async fn change_scope(
    state: Arc<crate::auth_http::ProductHttpState>,
    headers: HeaderMap,
    body: ScopeChangeRequest,
    mutation: ScopeMutation,
) -> axum::response::Response {
    let Some(session) = crate::auth_http::bearer_digest(&headers) else {
        return error(StatusCode::UNAUTHORIZED, "authentication required");
    };
    if body.subject_id.trim().is_empty()
        || body.subject_id.len() > 256
        || body.source_instance_id.trim().is_empty()
        || body.source_instance_id.len() > 256
        || body.request_key.trim().is_empty()
        || body.request_key.len() > 256
        || body.expected_scope_revision < 0
    {
        return error(StatusCode::BAD_REQUEST, "invalid scope request");
    }
    match state.store.lock().await.change_product_scope(
        &session,
        &body.subject_id,
        &body.source_instance_id,
        body.capability,
        mutation,
        body.expected_scope_revision,
        &body.request_key,
        chrono::Utc::now(),
    ) {
        Ok(receipt) => {
            (StatusCode::OK, Json(serde_json::json!({"receipt":receipt}))).into_response()
        }
        Err(_) => error(StatusCode::CONFLICT, "scope change unavailable"),
    }
}

async fn list_orders(
    State(state): State<Arc<crate::auth_http::ProductHttpState>>,
    headers: HeaderMap,
    Query(query): Query<OrderListQuery>,
) -> axum::response::Response {
    let Some(session) = crate::auth_http::bearer_digest(&headers) else {
        return error(StatusCode::UNAUTHORIZED, "authentication required");
    };
    let limit = query.limit.unwrap_or(100);
    if !(1..=100).contains(&limit)
        || query
            .after
            .as_ref()
            .is_some_and(|after| after.is_empty() || after.len() > 256)
    {
        return error(StatusCode::BAD_REQUEST, "invalid order cursor");
    }
    match state.store.lock().await.list_scoped_orders(
        &session,
        query.after.as_deref(),
        limit,
        chrono::Utc::now(),
    ) {
        Ok(orders) => (StatusCode::OK, Json(serde_json::json!({"orders":orders}))).into_response(),
        Err(_) => error(StatusCode::FORBIDDEN, "orders unavailable"),
    }
}

async fn order_detail(
    State(state): State<Arc<crate::auth_http::ProductHttpState>>,
    headers: HeaderMap,
    Path(order_id): Path<String>,
) -> axum::response::Response {
    let Some(session) = crate::auth_http::bearer_digest(&headers) else {
        return error(StatusCode::UNAUTHORIZED, "authentication required");
    };
    match state
        .store
        .lock()
        .await
        .read_scoped_order(&session, &order_id, chrono::Utc::now())
    {
        Ok(order) => (StatusCode::OK, Json(serde_json::json!({"order":order}))).into_response(),
        Err(_) => error(StatusCode::NOT_FOUND, "order unavailable"),
    }
}

fn error(status: StatusCode, message: &'static str) -> axum::response::Response {
    (status, Json(serde_json::json!({"error":message}))).into_response()
}
