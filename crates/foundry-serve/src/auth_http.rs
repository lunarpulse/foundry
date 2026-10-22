//! ProductAtomicV1 authentication routes. These handlers are mounted only by
//! the product-mode server and never consult the legacy Identity database.
use axum::{
    Json, Router,
    extract::DefaultBodyLimit,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use foundry_ledger::ProductStore;
use serde::Deserialize;
use std::sync::Arc;

pub struct ProductHttpState {
    pub store: tokio::sync::Mutex<ProductStore>,
    pub projection: tokio::sync::Mutex<foundry_line::product::ProductProjection>,
    /// Same frozen-pack host algorithm backed by a separate ProductStore
    /// handle on the same configured SQLite file. Absent means continuation
    /// is unavailable; Resume must fail before accepting a human decision.
    pub review_host: tokio::sync::Mutex<Option<foundry_line::PackReviewHost>>,
}

impl ProductHttpState {
    pub fn new(store: ProductStore) -> Arc<Self> {
        Arc::new(Self {
            store: tokio::sync::Mutex::new(store),
            projection: tokio::sync::Mutex::new(foundry_line::product::ProductProjection::default()),
            review_host: tokio::sync::Mutex::new(None),
        })
    }

    pub fn with_review_host(store: ProductStore, host: foundry_line::PackReviewHost) -> Arc<Self> {
        Arc::new(Self {
            store: tokio::sync::Mutex::new(store),
            projection: tokio::sync::Mutex::new(foundry_line::product::ProductProjection::default()),
            review_host: tokio::sync::Mutex::new(Some(host)),
        })
    }
}

#[derive(Deserialize)]
pub struct LoginRequest {
    pub actor_id: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct AuthorityRequest {
    pub role: String,
    pub enabled: bool,
}

#[derive(Deserialize)]
pub struct CreateAccountRequest {
    pub actor_id: String,
    pub role: String,
    pub password: String,
}

pub fn router(state: Arc<ProductHttpState>) -> Router {
    Router::new()
        .route("/api/product/login", post(login))
        .route("/api/product/logout", post(logout))
        .route("/api/product/session", get(session))
        .route("/api/product/accounts", post(create_account))
        .route(
            "/api/product/accounts/{actor_id}/authority",
            post(set_authority),
        )
        .layer(DefaultBodyLimit::max(8 * 1024))
        .with_state(state)
}

pub async fn create_account(
    State(state): State<Arc<ProductHttpState>>,
    headers: HeaderMap,
    Json(body): Json<CreateAccountRequest>,
) -> impl IntoResponse {
    let Some(digest) = bearer_digest(&headers) else {
        return StatusCode::UNAUTHORIZED;
    };
    if body.actor_id.trim().is_empty()
        || body.actor_id.len() > 256
        || !matches!(
            body.role.as_str(),
            "owner" | "approver" | "operator" | "viewer"
        )
        || body.password.is_empty()
        || body.password.len() > 1024
    {
        return StatusCode::BAD_REQUEST;
    }
    {
        let store = state.store.lock().await;
        match store.resolve_session(&digest, chrono::Utc::now()) {
            Ok(principal) if principal.role == "owner" => {}
            _ => return StatusCode::FORBIDDEN,
        }
    }
    let actor_id = body.actor_id.clone();
    let role = body.role.clone();
    let phc = match foundry_ledger::product_identity::ProductPasswordKdf::hash(&body.password) {
        Ok(phc) => phc,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR,
    };
    drop(body);
    match state.store.lock().await.create_account(
        &digest,
        &actor_id,
        &role,
        &phc,
        chrono::Utc::now(),
    ) {
        Ok(()) => StatusCode::CREATED,
        Err(_) => StatusCode::CONFLICT,
    }
}

pub async fn set_authority(
    State(state): State<Arc<ProductHttpState>>,
    headers: HeaderMap,
    axum::extract::Path(actor_id): axum::extract::Path<String>,
    Json(body): Json<AuthorityRequest>,
) -> impl IntoResponse {
    let Some(digest) = bearer_digest(&headers) else {
        return StatusCode::UNAUTHORIZED;
    };
    if actor_id.trim().is_empty()
        || actor_id.len() > 256
        || !matches!(
            body.role.as_str(),
            "owner" | "approver" | "operator" | "viewer"
        )
    {
        return StatusCode::BAD_REQUEST;
    }
    match state.store.lock().await.set_account_authority(
        &digest,
        &actor_id,
        &body.role,
        body.enabled,
        chrono::Utc::now(),
    ) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::FORBIDDEN,
    }
}

pub async fn login(
    State(state): State<Arc<ProductHttpState>>,
    Json(body): Json<LoginRequest>,
) -> impl IntoResponse {
    if body.actor_id.trim().is_empty() || body.password.is_empty() || body.password.len() > 1024 {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error":"authentication failed"})),
        )
            .into_response();
    }
    let result = state
        .store
        .lock()
        .await
        .login(&body.actor_id, &body.password, chrono::Utc::now());
    // Do not retain the request body longer than this handler needs it.
    drop(body);
    match result {
        Ok(session) => {
            let mut response = (StatusCode::OK, Json(serde_json::json!({"access_token":session.bearer(),"token_type":"Bearer","expires_in":28800}))).into_response();
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                "no-store".parse().expect("static header"),
            );
            response
        }
        Err(_) => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error":"authentication failed"})),
        )
            .into_response(),
    }
}

pub async fn logout(
    State(state): State<Arc<ProductHttpState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(digest) = bearer_digest(&headers) else {
        return StatusCode::UNAUTHORIZED;
    };
    match state
        .store
        .lock()
        .await
        .revoke_session(&digest, chrono::Utc::now())
    {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::UNAUTHORIZED,
    }
}

pub async fn session(
    State(state): State<Arc<ProductHttpState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(digest) = bearer_digest(&headers) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error":"authentication required"})),
        )
            .into_response();
    };
    match state.store.lock().await.resolve_session(&digest, chrono::Utc::now()) {
        Ok(principal) => (StatusCode::OK, Json(serde_json::json!({"actor_id":principal.actor_id,"role":principal.role,"authority_revision":principal.authority_revision}))).into_response(),
        Err(_) => (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error":"authentication required"}))).into_response(),
    }
}

pub(crate) fn bearer_digest(headers: &HeaderMap) -> Option<String> {
    let token = headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?;
    if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    use sha2::{Digest, Sha256};
    Some(
        Sha256::digest(token.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    )
}
