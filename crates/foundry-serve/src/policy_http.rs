//! T60b (M5.2, astra B1/N1): the REAL HTTP entry point for policy
//! application. `POST /api/policy/apply` — the axum route that calls
//! apply_policy_via_serve. This is the registered handler astra
//! asked for: not a library function, a route on the production
//! router.
//!
//! Request: Authorization: Bearer <session-token>
//!          body: the ApplyCommand JSON (op_id, proposal_digest,
//!                prediction, base_policy_version, issued_at,
//!                expires_at — approver/role are IGNORED, the server
//!                rebinds them to the session principal, N2).
//!
//! The handler owns the pipeline+layer+coordinator trio through
//! ServeState.policy (T60b wiring): there is NO other production
//! path that constructs these — inventory.rs is the record.
use crate::serve_wiring::apply_policy_via_serve;
use crate::ServeState;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use foundry_watch::policy_apply::{ApplyCommand, PolicyApplicationLayer};
use foundry_watch::retro::InsightPipeline;
use std::sync::Arc;

/// The governance half of ServeState: pipeline (with a REAL reader,
/// T60f), the apply layer, and the durable coordinator. Constructed
/// at server startup — handlers only borrow it.
pub struct PolicyState {
    pub pipeline: InsightPipeline,
    pub layer: tokio::sync::Mutex<PolicyApplicationLayer>,
    pub coordinator: tokio::sync::Mutex<foundry_watch::coordinator::GovernanceCoordinator>,
    pub proposer: String,
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(axum::http::header::AUTHORIZATION)?.to_str().ok()?;
    v.strip_prefix("Bearer ").map(|s| s.to_string())
}

/// POST /api/policy/apply — the production policy-application entry.
pub async fn apply(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(cmd): Json<ApplyCommand>,
) -> impl IntoResponse {
    let token = match bearer_token(&headers) {
        Some(t) => t,
        None => return (StatusCode::UNAUTHORIZED, "missing bearer session".to_string()),
    };
    let mut identity = st.identity.lock().await;
    let mut layer = st.policy.layer.lock().await;
    let mut coord = st.policy.coordinator.lock().await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match apply_policy_via_serve(
        &mut identity,
        &st.policy.pipeline,
        &mut layer,
        &mut coord,
        &token,
        &cmd,
        &st.policy.proposer,
        now,
    ) {
        Ok(msg) => (StatusCode::OK, msg),
        Err(e) => {
            let (code, msg) = match e {
                crate::serve_wiring::ServeWiringError::Unauthenticated(d) => (StatusCode::UNAUTHORIZED, d),
                crate::serve_wiring::ServeWiringError::Forbidden { current_role } => {
                    (StatusCode::FORBIDDEN, format!("current role {current_role} may not apply policy"))
                }
                _ => (StatusCode::CONFLICT, format!("{e:?}")),
            };
            (code, msg)
        }
    }
}
