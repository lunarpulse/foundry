//! ProductAtomicV1 pseudonymized export. Authorization and S1 snapshot come
//! from the same ProductStore configured by product mode.
use crate::auth_http::{ProductHttpState, bearer_digest};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use rand_core::RngCore as _;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedReadQuery {
    limit: Option<usize>,
}

fn new_export_context() -> Result<(crate::privacy::ExportKey, String), ()> {
    let key = crate::privacy::ExportKey::generate().map_err(|_| ())?;
    let mut bytes = [0_u8; 16];
    if rand_core::OsRng.try_fill_bytes(&mut bytes).is_err() {
        bytes.fill(0);
        return Err(());
    }
    let context = crate::identity_hex(&bytes);
    bytes.fill(0);
    Ok((key, context))
}

pub fn router(state: Arc<ProductHttpState>) -> Router {
    Router::new()
        .route("/api/product/export", get(export))
        .route("/api/product/audit", get(audit))
        .with_state(state)
}

async fn audit(
    State(state): State<Arc<ProductHttpState>>,
    headers: HeaderMap,
    Query(query): Query<ScopedReadQuery>,
) -> impl IntoResponse {
    let verification_started = std::time::Instant::now();
    let Some(session) = bearer_digest(&headers) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error":"authentication required"})),
        )
            .into_response();
    };
    let limit = query.limit.unwrap_or(100);
    if !(1..=100).contains(&limit) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error":"audit unavailable"})),
        )
            .into_response();
    }
    let now = chrono::Utc::now();
    let snapshot = match state.store.lock().await.scoped_evidence_snapshot(
        &session,
        foundry_ledger::ProductCapability::ReadOperationalAudit,
        limit,
        now,
    ) {
        Ok(snapshot) => snapshot,
        Err(foundry_ledger::ProductError::Database(_))
        | Err(foundry_ledger::ProductError::ContractMismatch) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error":"audit unavailable"})),
            )
                .into_response();
        }
        Err(_) => {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error":"audit unavailable"})),
            )
                .into_response();
        }
    };
    let (key, context) = match new_export_context() {
        Ok(context) => context,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error":"audit unavailable"})),
            )
                .into_response();
        }
    };
    let mut report =
        match crate::audit_report::scoped_product_inventory_v3(&snapshot, now, &context, &key) {
            Ok(report) => report,
            Err(_) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({"error":"audit unavailable"})),
                )
                    .into_response();
            }
        };
    report.projection.verification_duration_micros = Some(
        verification_started
            .elapsed()
            .as_micros()
            .min(u64::MAX as u128) as u64,
    );
    let mut response = (StatusCode::OK, Json(report)).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-store".parse().expect("static header"),
    );
    response
}

async fn export(
    State(state): State<Arc<ProductHttpState>>,
    headers: HeaderMap,
    Query(query): Query<ScopedReadQuery>,
) -> impl IntoResponse {
    let verification_started = std::time::Instant::now();
    let Some(session) = bearer_digest(&headers) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error":"authentication required"})),
        )
            .into_response();
    };
    let limit = query.limit.unwrap_or(100);
    if !(1..=100).contains(&limit) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error":"export unavailable"})),
        )
            .into_response();
    }
    let snapshot = match state.store.lock().await.scoped_evidence_snapshot(
        &session,
        foundry_ledger::ProductCapability::ExportEvidence,
        limit,
        chrono::Utc::now(),
    ) {
        Ok(snapshot) => snapshot,
        Err(foundry_ledger::ProductError::Database(_))
        | Err(foundry_ledger::ProductError::ContractMismatch) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error":"export unavailable"})),
            )
                .into_response();
        }
        Err(_) => {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error":"export unavailable"})),
            )
                .into_response();
        }
    };
    let (key, context) = match new_export_context() {
        Ok(context) => context,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error":"export unavailable"})),
            )
                .into_response();
        }
    };
    let mut document = match crate::privacy::scoped_product_export(&snapshot, &context, &key) {
        Ok(document) => document,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error":"export unavailable"})),
            )
                .into_response();
        }
    };
    document.verification_duration_micros = Some(
        verification_started
            .elapsed()
            .as_micros()
            .min(u64::MAX as u128) as u64,
    );
    let mut response = (StatusCode::OK, Json(document)).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-store".parse().expect("static header"),
    );
    response
}
