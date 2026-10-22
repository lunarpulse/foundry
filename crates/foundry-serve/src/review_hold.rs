use crate::{ServeState, identity::Capability};
use axum::{
    Json,
    extract::{FromRequestParts, Path, State},
    http::{StatusCode, header, request::Parts},
    response::IntoResponse,
};
use std::sync::Arc;

/// ProductAtomicV1 review endpoints are mounted separately from the legacy
/// ServeState handlers below. They never resolve identity or mutate the
/// StandaloneV1 AppLine.
pub fn product_router(state: Arc<crate::auth_http::ProductHttpState>) -> axum::Router {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/api/product/review/holds", get(product_hold_list))
        .route(
            "/api/product/orders/{order_id}/review/holds",
            get(product_holds),
        )
        .route(
            "/api/product/orders/{order_id}/review/holds/{hold_id}/accept",
            post(accept_product_hold),
        )
        .route(
            "/api/product/review/operations/{operation_id}",
            get(product_progress),
        )
        .layer(axum::extract::DefaultBodyLimit::max(8 * 1024))
        .with_state(state)
}

#[derive(serde::Deserialize)]
pub struct ProductHoldDecisionBody {
    pub expected: foundry_ledger::ProductHoldExpectation,
    pub decision: String,
    pub target: Option<String>,
}

async fn current_product_reviewer(
    state: &crate::auth_http::ProductHttpState,
    session: &str,
) -> Result<(), axum::http::StatusCode> {
    match state
        .store
        .lock()
        .await
        .resolve_session(session, chrono::Utc::now())
    {
        Ok(principal) if principal.role == "owner" || principal.role == "approver" => Ok(()),
        Ok(_) => Err(axum::http::StatusCode::FORBIDDEN),
        Err(_) => Err(axum::http::StatusCode::UNAUTHORIZED),
    }
}

async fn product_holds(
    State(state): State<Arc<crate::auth_http::ProductHttpState>>,
    headers: axum::http::HeaderMap,
    Path(order_id): Path<String>,
) -> axum::response::Response {
    use axum::{Json, http::StatusCode, response::IntoResponse};
    let Some(session) = crate::auth_http::bearer_digest(&headers) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error":"authentication required"})),
        )
            .into_response();
    };
    if let Err(status) = current_product_reviewer(&state, &session).await {
        return (
            status,
            Json(serde_json::json!({"error":"review holds unavailable"})),
        )
            .into_response();
    }
    let result = state
        .store
        .lock()
        .await
        .list_product_review_holds(&session, chrono::Utc::now());
    match result {
        Ok(holds) => {
            let selected: Vec<_> = holds
                .into_iter()
                .filter(|hold| hold.order_id == order_id)
                .collect();
            (StatusCode::OK, Json(serde_json::json!({"holds": selected}))).into_response()
        }
        Err(_) => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error":"review holds unavailable"})),
        )
            .into_response(),
    }
}

async fn product_hold_list(
    State(state): State<Arc<crate::auth_http::ProductHttpState>>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    use axum::{Json, http::StatusCode, response::IntoResponse};
    let Some(session) = crate::auth_http::bearer_digest(&headers) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error":"authentication required"})),
        )
            .into_response();
    };
    if let Err(status) = current_product_reviewer(&state, &session).await {
        return (
            status,
            Json(serde_json::json!({"error":"review holds unavailable"})),
        )
            .into_response();
    }
    match state
        .store
        .lock()
        .await
        .list_product_review_holds(&session, chrono::Utc::now())
    {
        Ok(holds) => (StatusCode::OK, Json(serde_json::json!({"holds":holds}))).into_response(),
        Err(_) => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error":"review holds unavailable"})),
        )
            .into_response(),
    }
}

async fn product_progress(
    State(state): State<Arc<crate::auth_http::ProductHttpState>>,
    headers: axum::http::HeaderMap,
    Path(operation_id): Path<String>,
) -> axum::response::Response {
    use axum::{Json, http::StatusCode, response::IntoResponse};
    let Some(session) = crate::auth_http::bearer_digest(&headers) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error":"authentication required"})),
        )
            .into_response();
    };
    if let Err(status) = current_product_reviewer(&state, &session).await {
        return (
            status,
            Json(serde_json::json!({"error":"review progress unavailable"})),
        )
            .into_response();
    }
    match state.store.lock().await.product_review_progress(
        &session,
        &operation_id,
        chrono::Utc::now(),
    ) {
        Ok(progress) => (
            StatusCode::OK,
            Json(serde_json::json!({"progress":progress})),
        )
            .into_response(),
        Err(_) => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error":"review progress unavailable"})),
        )
            .into_response(),
    }
}

async fn accept_product_hold(
    State(state): State<Arc<crate::auth_http::ProductHttpState>>,
    headers: axum::http::HeaderMap,
    Path((order_id, hold_id)): Path<(String, String)>,
    Json(body): Json<ProductHoldDecisionBody>,
) -> axum::response::Response {
    use axum::{Json, http::StatusCode, response::IntoResponse};
    let Some(session) = crate::auth_http::bearer_digest(&headers) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error":"authentication required"})),
        )
            .into_response();
    };
    let Some(request_key) = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error":"invalid request"})),
        )
            .into_response();
    };
    if request_key.trim().is_empty()
        || request_key.len() > 256
        || order_id.len() > 256
        || hold_id.len() > 256
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error":"invalid request"})),
        )
            .into_response();
    }
    match state
        .store
        .lock()
        .await
        .resolve_session(&session, chrono::Utc::now())
    {
        Ok(principal) if principal.role == "owner" || principal.role == "approver" => {}
        Ok(_) => {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error":"review decision forbidden"})),
            )
                .into_response();
        }
        Err(_) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error":"authentication required"})),
            )
                .into_response();
        }
    }
    let decision = match (body.decision.as_str(), body.target) {
        ("resume", Some(target)) if !target.trim().is_empty() && target.len() <= 256 => {
            foundry_ledger::ProductHoldDecision::Resume(target)
        }
        ("commit_pass", None) => foundry_ledger::ProductHoldDecision::CommitPass,
        ("commit_fail", None) => foundry_ledger::ProductHoldDecision::CommitFail,
        ("abort", None) => foundry_ledger::ProductHoldDecision::Abort,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error":"invalid request"})),
            )
                .into_response();
        }
    };

    let active_hold = state
        .store
        .lock()
        .await
        .list_product_review_holds(&session, chrono::Utc::now())
        .ok()
        .is_some_and(|holds| {
            holds
                .iter()
                .any(|view| view.order_id == order_id && view.hold_instance_id == hold_id)
        });
    // The runtime and its frozen pack must be available before accepting a
    // NEW decision. Exact retries of an already committed decision must still
    // reveal the durable receipt if runtime availability changed afterwards.
    if active_hold {
        let host = state.review_host.lock().await;
        let Some(host) = host.as_ref() else {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error":"product review runtime unavailable"})),
            )
                .into_response();
        };
        if matches!(&decision, foundry_ledger::ProductHoldDecision::Resume(_))
            && host.ensure_product_executor_available().is_err()
        {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error":"product review runtime unavailable"})),
            )
                .into_response();
        }
    }

    let accepted = state.store.lock().await.accept_product_review_hold(
        &session,
        &order_id,
        &hold_id,
        &body.expected,
        request_key,
        decision,
        chrono::Utc::now(),
    );
    let receipt = match accepted {
        Ok(receipt) => receipt,
        Err(_) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error":"review decision unavailable"})),
            )
                .into_response();
        }
    };

    // Acceptance is already durable. Continuation errors return that receipt
    // and current phase so a client never has to guess whether to re-approve.
    let phase_before = state.store.lock().await.product_review_progress(
        &session,
        &receipt.operation_id,
        chrono::Utc::now(),
    );
    let should_continue = phase_before
        .as_ref()
        .is_ok_and(|p| !matches!(p.phase.as_str(), "completed" | "recovery_required"));
    let continuation_error = if should_continue {
        let mut host = state.review_host.lock().await;
        match host.as_mut() {
            Some(host) => host
                .continue_accepted_product_hold(&session, &receipt.operation_id, chrono::Utc::now())
                .is_err(),
            None => true,
        }
    } else {
        false
    };
    {
        let mut projection = state.projection.lock().await;
        let mut store = state.store.lock().await;
        let _ = projection.catch_up(&mut store);
    }
    let progress = state.store.lock().await.product_review_progress(
        &session,
        &receipt.operation_id,
        chrono::Utc::now(),
    );
    match progress {
        Ok(progress) => {
            let status = if continuation_error || progress.phase != "completed" { StatusCode::ACCEPTED } else { StatusCode::OK };
            (status, Json(serde_json::json!({"receipt":receipt,"progress":progress}))).into_response()
        }
        Err(_) => (StatusCode::ACCEPTED, Json(serde_json::json!({"receipt":receipt,"continuation":"accepted; progress unavailable"}))).into_response(),
    }
}

pub struct ReviewBearer(Option<String>);
impl<S: Send + Sync> FromRequestParts<S> for ReviewBearer {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(
            parts
                .headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .map(str::to_owned),
        ))
    }
}
#[derive(serde::Deserialize)]
pub struct ResumeBody {
    pub target: String,
}

pub async fn resume(
    State(st): State<Arc<ServeState>>,
    auth: ReviewBearer,
    Path((id, hold)): Path<(String, String)>,
    Json(body): Json<ResumeBody>,
) -> impl IntoResponse {
    resolve(
        st,
        auth,
        id,
        hold,
        foundry_exec::ReviewHoldAction::Resume {
            target: body.target,
        },
    )
    .await
}
pub async fn commit_pass(
    State(st): State<Arc<ServeState>>,
    auth: ReviewBearer,
    Path((id, hold)): Path<(String, String)>,
) -> impl IntoResponse {
    resolve(
        st,
        auth,
        id,
        hold,
        foundry_exec::ReviewHoldAction::CommitPass,
    )
    .await
}
pub async fn commit_fail(
    State(st): State<Arc<ServeState>>,
    auth: ReviewBearer,
    Path((id, hold)): Path<(String, String)>,
) -> impl IntoResponse {
    resolve(
        st,
        auth,
        id,
        hold,
        foundry_exec::ReviewHoldAction::CommitFail,
    )
    .await
}
pub async fn abort(
    State(st): State<Arc<ServeState>>,
    auth: ReviewBearer,
    Path((id, hold)): Path<(String, String)>,
) -> impl IntoResponse {
    resolve(st, auth, id, hold, foundry_exec::ReviewHoldAction::Abort).await
}

async fn resolve(
    st: Arc<ServeState>,
    auth: ReviewBearer,
    id: String,
    hold: String,
    action: foundry_exec::ReviewHoldAction,
) -> axum::response::Response {
    let Some(token) = auth.0 else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error":"bearer session required"})),
        )
            .into_response();
    };
    let user =
        {
            let identity = st.identity.lock().await;
            match identity.resolve_session(&token) {
                Ok(user) => user,
                Err(e) => return (
                    StatusCode::UNAUTHORIZED,
                    Json(
                        serde_json::json!({"error":crate::privacy::sanitize_error(&e.to_string())}),
                    ),
                )
                    .into_response(),
            }
        };
    {
        let identity = st.identity.lock().await;
        if let Err(e) = identity.authorize(&user, Capability::ResolveReviewHold) {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error":crate::privacy::sanitize_error(&e.to_string())})),
            )
                .into_response();
        }
    }
    if st.line.get(&id).await.is_none()
        && let Err(error) = restore_order(&st, &id).await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":crate::privacy::sanitize_error(&error)})),
        )
            .into_response();
    }
    match st
        .line
        .resolve_review_hold(&id, &hold, action, &user.username)
        .await
    {
        Ok(state) => (
            StatusCode::OK,
            Json(serde_json::json!({"order_id":id,"state":state.to_string()})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":crate::privacy::sanitize_error(&e.to_string())})),
        )
            .into_response(),
    }
}

async fn restore_order(st: &ServeState, id: &str) -> Result<(), String> {
    let rows = st
        .ledger
        .read_all()
        .map_err(|e| format!("review ledger: {e:?}"))?;
    let verified = st
        .ledger
        .verify_chain()
        .map_err(|e| format!("review ledger verification: {e:?}"))?;
    let replayed = foundry_ledger::replay::replay(&rows, |_| verified)
        .map_err(|e| format!("review replay: {e:?}"))?;
    let order = replayed
        .into_iter()
        .filter(|o| o.order_id == id)
        .max_by_key(|o| o.last_seq)
        .ok_or_else(|| "review order not found".to_owned())?
        .order;
    st.line
        .restore_review_hold(order)
        .await
        .map_err(|e| e.to_string())
}
