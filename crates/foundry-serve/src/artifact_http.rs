//! Authenticated ProductAtomicV1 artifact reads. Filesystem paths never enter
//! request DTOs or responses; ProductStore resolves opaque IDs and verifies
//! the complete payload before this module constructs a response.
use crate::auth_http::{ProductHttpState, bearer_digest};
use axum::{
    Json, Router,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use std::sync::Arc;

pub fn router(state: Arc<ProductHttpState>) -> Router {
    Router::new()
        .route(
            "/api/product/orders/{order_id}/results/{result_id}",
            get(read_result),
        )
        .route(
            "/api/product/orders/{order_id}/results/{result_id}/artifacts/{artifact_id}",
            get(read_result_artifact),
        )
        .route("/api/product/orders/{order_id}/artifacts", get(list))
        .route(
            "/api/product/orders/{order_id}/artifacts/{artifact_id}",
            get(read),
        )
        .with_state(state)
}

async fn read_result(
    State(state): State<Arc<ProductHttpState>>,
    headers: HeaderMap,
    Path((order_id, result_id)): Path<(String, String)>,
) -> Response {
    let Some(session) = bearer_digest(&headers) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error":"result unavailable"})),
        )
            .into_response();
    };
    match state
        .store
        .lock()
        .await
        .read_product_result(&session, &order_id, &result_id)
    {
        Ok(info) => {
            let mut response = (StatusCode::OK, Json(info)).into_response();
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Err(
            foundry_ledger::ProductResultError::Storage
            | foundry_ledger::ProductResultError::Unavailable
            | foundry_ledger::ProductResultError::CommitUncertain,
        ) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error":"result unavailable"})),
        )
            .into_response(),
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error":"result unavailable"})),
        )
            .into_response(),
    }
}

async fn read_result_artifact(
    State(state): State<Arc<ProductHttpState>>,
    headers: HeaderMap,
    Path((order_id, result_id, artifact_id)): Path<(String, String, String)>,
) -> Response {
    let Some(session) = bearer_digest(&headers) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error":"artifact unavailable"})),
        )
            .into_response();
    };
    match state.store.lock().await.read_product_result_artifact(
        &session,
        &order_id,
        &result_id,
        &artifact_id,
    ) {
        Ok(content) => {
            let mut response = Response::new(Body::from(content.artifact.bytes));
            *response.status_mut() = StatusCode::OK;
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            );
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            for (name, value) in [
                ("x-foundry-result-id", content.result.result_id.as_str()),
                (
                    "x-foundry-tuple-digest",
                    content.result.tuple_digest.as_str(),
                ),
                (
                    "x-foundry-artifact-id",
                    content.artifact.info.artifact_id.as_str(),
                ),
                (
                    "x-foundry-artifact-sha256",
                    content.artifact.info.sha256.as_str(),
                ),
            ] {
                if let Ok(value) = HeaderValue::from_str(value) {
                    response.headers_mut().insert(name, value);
                }
            }
            response
        }
        Err(
            foundry_ledger::ProductResultError::Storage
            | foundry_ledger::ProductResultError::Unavailable
            | foundry_ledger::ProductResultError::CommitUncertain,
        ) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error":"artifact unavailable"})),
        )
            .into_response(),
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error":"artifact unavailable"})),
        )
            .into_response(),
    }
}

async fn list(
    State(state): State<Arc<ProductHttpState>>,
    headers: HeaderMap,
    Path(order_id): Path<String>,
) -> Response {
    let Some(session) = bearer_digest(&headers) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error":"artifact unavailable"})),
        )
            .into_response();
    };
    match state
        .store
        .lock()
        .await
        .list_product_artifacts(&session, &order_id, chrono::Utc::now())
    {
        Ok(artifacts) => {
            let mut response = (
                StatusCode::OK,
                Json(serde_json::json!({"artifacts":artifacts})),
            )
                .into_response();
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Err(foundry_ledger::ProductError::Database(_)) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error":"artifact unavailable"})),
        )
            .into_response(),
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error":"artifact unavailable"})),
        )
            .into_response(),
    }
}

async fn read(
    State(state): State<Arc<ProductHttpState>>,
    headers: HeaderMap,
    Path((order_id, artifact_id)): Path<(String, String)>,
) -> Response {
    let Some(session) = bearer_digest(&headers) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error":"artifact unavailable"})),
        )
            .into_response();
    };
    match state.store.lock().await.read_product_artifact(
        &session,
        &order_id,
        &artifact_id,
        chrono::Utc::now(),
    ) {
        Ok(artifact) => {
            let mut response = Response::new(Body::from(artifact.bytes));
            *response.status_mut() = StatusCode::OK;
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            );
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            if let Ok(value) = HeaderValue::from_str(&artifact.info.sha256) {
                response
                    .headers_mut()
                    .insert("x-foundry-artifact-sha256", value);
            }
            if let Ok(value) = HeaderValue::from_str(&artifact.info.artifact_id) {
                response
                    .headers_mut()
                    .insert("x-foundry-artifact-id", value);
            }
            response
        }
        Err(foundry_ledger::ProductError::Database(_)) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error":"artifact unavailable"})),
        )
            .into_response(),
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error":"artifact unavailable"})),
        )
            .into_response(),
    }
}
