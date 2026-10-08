//! M4 T29: approval endpoint now REQUIRES an authenticated session.
//! The M3-era "local serve = human" assumption is retired (review #2/#3).
//!
//! Flow: Bearer session token -> resolve CURRENT account state ->
//! authorize(Approve) via the C1 matrix -> legacy digest/order checks ->
//! the SINGLE ApprovalToken mint -> atomic consume (single use) ->
//! line.approve. Also wires the legacy compat boundary: an order
//! approved before M4 keeps its journal rows; NEW approvals always
//! need identity.
use axum::extract::{FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use crate::identity::{Capability, IdentityError};
use std::sync::Arc;

#[derive(serde::Deserialize)]
pub struct ApproveBody {
    pub intent: String,
}

#[derive(serde::Serialize)]
pub struct ApproveResponse {
    pub order_id: String,
    pub state: String,
    pub height: i64,
    pub approved_by: String,
}

pub struct WithParts {
    bearer: Option<String>,
}

impl<S: Send + Sync> FromRequestParts<S> for WithParts {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let bearer = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(|t| t.to_string());
        Ok(WithParts { bearer })
    }
}

pub async fn approve(
    State(st): State<Arc<crate::ServeState>>,
    parts: WithParts,
    Path(id): Path<String>,
    body: Option<Json<ApproveBody>>,
) -> impl IntoResponse {
    let intent = match body {
        Some(Json(b)) if !b.intent.trim().is_empty() => b.intent,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "intent required"})),
            )
                .into_response()
        }
    };

    // ---- identity: session token -> CURRENT user (C3: no role in token)
    let token = match parts.bearer.clone() {
        Some(t) => t,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "authentication required"})),
            )
                .into_response()
        }
    };
    // helper closure can't borrow st mutably in async fn easily; inline:
    let user = {
        let ident = st.identity.lock().await;
        match ident.resolve_session(&token) {
            Ok(u) => u,
            Err(e) => return auth_error(e),
        }
    };

    // ---- C1 capability matrix at the domain boundary
    {
        let ident = st.identity.lock().await;
        if let Err(e) = ident.authorize(&user, Capability::Approve) {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": e.to_string(), "username": user.username})),
            )
                .into_response();
        }
    }

    // rehydrate if needed (defect #7 recovery)
    if st.line.get(&id).await.is_none() {
        let rows = st.ledger.read_all().unwrap_or_default();
        if !rows.iter().any(|r| r.order_id == id) {
            return not_found(&id);
        }
        match foundry_line::rehydrate::rehydrate(&st.ledger, std::path::Path::new("/tmp")) {
            Ok(orders) => {
                for o in orders {
                    st.line.submit(o).await;
                }
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": "rehydrate failed", "detail": format!("{e:?}")})),
                )
                    .into_response()
            }
        }
    }

    // order must be holding
    match st.line.get(&id).await {
        Some(o) if o.state() == foundry_domain::OrderState::FurnaceHold => {}
        Some(o) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "order is not awaiting approval",
                    "state": o.state().to_string(),
                })),
            )
                .into_response()
        }
        None => return not_found(&id),
    }

    // ---- SINGLE token mint, bound to the AUTHENTICATED actor (C4)
    let token = foundry_domain::ApprovalToken {
        order_id: id.clone(),
        intent: intent.clone(),
        artifact_digest: st
            .line
            .get(&id)
            .await
            .and_then(|o| o.artifact_digest)
            .unwrap_or_default(),
        actor: foundry_domain::ActorId {
            kind: foundry_domain::ActorKind::Human,
            source: "serve".into(),
            id: user.username.clone(),
        },
        granted_at: chrono::Utc::now(),
    };

    // ---- consume exactly once (atomic), THEN commit the transition
    {
        let mut ident = st.identity.lock().await;
        if let Err(e) = ident.consume_approval(&token) {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    }

    let actor = foundry_domain::ActorId {
        kind: foundry_domain::ActorKind::Human,
        source: "serve".into(),
        id: user.username.clone(),
    };
    match st.line.approve(&id, &token, &intent, &actor).await {
        Ok(state) => {
            let height = st.ledger.read_all().map(|r| r.len() as i64).unwrap_or(-1);
            (
                StatusCode::OK,
                [(header::CACHE_CONTROL, "no-store")],
                Json(ApproveResponse {
                    order_id: id,
                    state: state.to_string(),
                    height,
                    approved_by: user.username,
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



fn auth_error(e: IdentityError) -> axum::response::Response {
    let code = match &e {
        IdentityError::UnknownSession | IdentityError::SessionExpired => StatusCode::UNAUTHORIZED,
        IdentityError::Disabled => StatusCode::UNAUTHORIZED,
        _ => StatusCode::UNAUTHORIZED,
    };
    (code, Json(serde_json::json!({"error": e.to_string()}))).into_response()
}

fn not_found(id: &str) -> axum::response::Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"error": "unknown order", "order_id": id})),
    )
        .into_response()
}
