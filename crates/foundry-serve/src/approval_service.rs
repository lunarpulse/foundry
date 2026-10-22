//! Product approval facade. Identity and final approval both resolve through
//! ProductStore; no legacy ApprovalToken or caller role is accepted here.
use crate::auth_http::{ProductHttpState, bearer_digest};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::post,
};
use foundry_ledger::ProductApprovalIntent;
use rand_core::RngCore as _;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct GrantRequest {
    pub order_id: String,
}

#[derive(Deserialize)]
pub struct ApproveRequest {
    pub grant_id: String,
    pub grant_token: String,
    pub expected_source_revision: i64,
    pub expected_attempt: i64,
    pub run_id: i64,
    pub generation: i64,
    pub artifact_digest: String,
    pub tuple_digest: String,
    pub intent: String,
}

pub fn router(state: Arc<ProductHttpState>) -> Router {
    Router::new()
        .route("/api/product/approval-grants", post(issue_grant))
        .route("/api/product/orders/{id}/approve", post(approve))
        .layer(DefaultBodyLimit::max(8 * 1024))
        .with_state(state)
}

pub async fn issue_grant(
    State(state): State<Arc<ProductHttpState>>,
    headers: HeaderMap,
    Json(body): Json<GrantRequest>,
) -> impl IntoResponse {
    let Some(session) = bearer_digest(&headers) else {
        return unauthorized();
    };
    if body.order_id.trim().is_empty() || body.order_id.len() > 256 {
        return bad_request();
    }
    let mut raw = [0u8; 32];
    if rand_core::OsRng.try_fill_bytes(&mut raw).is_err() {
        return internal_error();
    }
    let token = raw.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let mut id_bytes = [0u8; 16];
    if rand_core::OsRng.try_fill_bytes(&mut id_bytes).is_err() {
        return internal_error();
    }
    let grant_id = id_bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let digest = sha256_hex(token.as_bytes());
    let result = state.store.lock().await.issue_approval_grant(
        &session,
        &grant_id,
        &digest,
        &body.order_id,
        chrono::Utc::now(),
    );
    match result {
        Ok(()) => {
            let mut response = (
                StatusCode::CREATED,
                Json(serde_json::json!({"grant_id":grant_id,"grant_token":token})),
            )
                .into_response();
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                "no-store".parse().expect("static header"),
            );
            response
        }
        Err(_) => unauthorized(),
    }
}

pub async fn approve(
    State(state): State<Arc<ProductHttpState>>,
    headers: HeaderMap,
    Path(order_id): Path<String>,
    Json(body): Json<ApproveRequest>,
) -> impl IntoResponse {
    let Some(session) = bearer_digest(&headers) else {
        return unauthorized();
    };
    let Some(request_key) = headers.get("idempotency-key").and_then(|v| v.to_str().ok()) else {
        return bad_request();
    };
    if request_key.trim().is_empty()
        || request_key.len() > 200
        || body.grant_token.len() != 64
        || !body.grant_token.bytes().all(|b| b.is_ascii_hexdigit())
        || body.grant_id.trim().is_empty()
        || body.grant_id.len() > 200
        || !matches!(body.artifact_digest.len(), 40 | 64)
        || !body.artifact_digest.bytes().all(|b| b.is_ascii_hexdigit())
        || body.tuple_digest.len() != 64
        || !body.tuple_digest.bytes().all(|b| b.is_ascii_hexdigit())
        || body.intent.trim().is_empty()
        || body.intent.len() > 4096
    {
        return bad_request();
    }
    let now = chrono::Utc::now();
    let intent = ProductApprovalIntent {
        request_key: request_key.to_owned(),
        grant_id: body.grant_id.clone(),
        token_digest: sha256_hex(body.grant_token.as_bytes()),
        order_id,
        expected_source_revision: body.expected_source_revision,
        expected_attempt: body.expected_attempt,
        run_id: body.run_id,
        generation: body.generation,
        artifact_digest: body.artifact_digest.clone(),
        tuple_digest: body.tuple_digest.clone(),
        intent: body.intent.clone(),
        now,
    };
    let mut store = state.store.lock().await;
    let result = store
        .prepare_approval(&session, &intent)
        .and_then(|proof| store.approve_product(&session, &intent, &proof));
    drop(store);
    match result {
        Ok(receipt) => {
            // The transaction and immutable receipt are already committed.
            // A projection/mirror failure must never retry approval.
            let _projection_result = {
                let mut projection = state.projection.lock().await;
                let mut store = state.store.lock().await;
                projection.apply_committed_approval(&mut store, &receipt)
            };
            (StatusCode::OK, Json(serde_json::json!({"receipt":receipt}))).into_response()
        }
        Err(_) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error":"approval unavailable"})),
        )
            .into_response(),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn unauthorized() -> axum::response::Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({"error":"authentication required"})),
    )
        .into_response()
}
fn bad_request() -> axum::response::Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({"error":"invalid request"})),
    )
        .into_response()
}
fn internal_error() -> axum::response::Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({"error":"request failed"})),
    )
        .into_response()
}
