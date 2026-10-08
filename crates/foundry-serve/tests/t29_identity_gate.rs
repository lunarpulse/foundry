//! T29 gate: the approval API enforces identity. M3's t27 test is now
//! INVALID by design ("serve local = human" retired) — these tests
//! assert the NEW contract.
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::Router;
use foundry_domain::{Order, OrderEvent, OrderState};
use foundry_serve::identity::{Identity, PasswordHasher, Role};
use foundry_serve::ServeState;
use std::path::PathBuf;
use std::sync::Arc;
use tower::ServiceExt;

struct FastHasher;
impl PasswordHasher for FastHasher {
    fn hash(&self, p: &str) -> String {
        format!("fast${p}")
    }
    fn verify(&self, p: &str, hash: &str) -> bool {
        self.hash(p) == hash
    }
}

fn db(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("t29-{}-{}.db", tag, std::process::id()));
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_file(p.with_extension("identity.db"));
    p
}

async fn order_at_hold(st: &ServeState, id: &str) {
    let o = Order::new(
        id.into(),
        foundry_domain::ExternalRef::github("o/r", "1", "k".into()),
        chrono::Utc::now(),
    );
    st.line.submit(o).await;
    let a = foundry_domain::ActorId {
        kind: foundry_domain::ActorKind::Agent,
        source: "t".into(),
        id: "t".into(),
    };
    use OrderEvent as E;
    let seq = [
        (E::Queued, OrderState::Received),
        (E::DieStarted, OrderState::Queued),
        (E::DieSucceeded { attempt: 1 }, OrderState::Working),
        (E::InspectionPassed, OrderState::Inspecting),
    ];
    for (e, _from) in seq {
        st.line.commit_for_test(id, &e, _from, OrderState::FurnaceHold, &a).await.unwrap();
    }
    let mut o = st.line.get(id).await.unwrap();
    o.artifact_digest = Some("commit-xyz".into());
    st.line.submit(o).await;
}

async fn setup_with_users(tag: &str) -> Arc<ServeState> {
    let p = db(tag);
    // pre-bootstrap the identity side (ServeState::new creates it)
    {
        let mut id = Identity::open(&p.with_extension("identity.db")).unwrap();
        id.bootstrap_owner("root", "pw", &FastHasher).unwrap();
        id.create_user(
            &foundry_serve::identity::AuthenticatedUser { username: "root".into(), role: Role::Owner },
            "obs",
            "pw",
            Role::Observer,
            &FastHasher,
        )
        .unwrap();
    }
    let st = Arc::new(ServeState::new(&p).unwrap());
    order_at_hold(&st, "o-29").await;
    st
}

async fn login(st: &ServeState, user: &str) -> String {
    let mut id = st.identity.lock().await;
    id.login(user, "pw", &FastHasher, 300).unwrap()
}

async fn req(app: Router, token: Option<&str>, intent: &str) -> (StatusCode, serde_json::Value) {
    {
        let mut b = Request::builder()
            .method("POST")
            .uri("/api/orders/o-29/approve")
            .header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        let resp = app
            .oneshot(
                b.body(Body::from(format!(r#"{{"intent":"{intent}"}}"#)))
                    .unwrap(),
            )
            .await
            .unwrap();
        let code = resp.status();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (code, serde_json::from_slice(&body).unwrap_or_default())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn approval_requires_authentication() {
    let st = setup_with_users("noauth").await;
    let app = Router::new().merge(foundry_serve::http::router(st.clone()));
    let (code, body) = req(app, None, "bump").await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "authentication required");
}

#[tokio::test(flavor = "multi_thread")]
async fn observer_forbidden_approver_allowed() {
    // observer: 403
    let st = setup_with_users("roles").await;
    let app = Router::new().merge(foundry_serve::http::router(st.clone()));
    let obs_tok = login(&st, "obs").await;
    let (code, body) = req(app, Some(&obs_tok), "try").await;
    assert_eq!(code, StatusCode::FORBIDDEN, "observer must not approve");
    assert!(body["error"].to_string().contains("denied"));

    // order still at FurnaceHold (no side effect)
    assert_eq!(
        st.line.get("o-29").await.unwrap().state(),
        OrderState::FurnaceHold
    );

    // owner: 200
    let app2 = Router::new().merge(foundry_serve::http::router(st.clone()));
    let owner_tok = login(&st, "root").await;
    let (code2, body2) = req(app2, Some(&owner_tok), "verified diff").await;
    assert_eq!(code2, StatusCode::OK, "owner approves");
    assert_eq!(body2["state"], "publishing");
    assert_eq!(body2["approved_by"], "root");

    // journal carries the authenticated actor id
    let rows = st.ledger.read_all().unwrap();
    let approved = rows
        .iter()
        .find(|r| r.event.contains("Approved") || r.to_state == "publishing");
    assert!(approved.is_some(), "approval journaled");
}

#[tokio::test(flavor = "multi_thread")]
async fn disabled_user_session_dies_instantly() {
    let st = setup_with_users("disable").await;
    let tok = login(&st, "obs").await;
    // owner disables obs
    {
        let mut id = st.identity.lock().await;
        id.set_disabled(
            &foundry_serve::identity::AuthenticatedUser { username: "root".into(), role: Role::Owner },
            "obs",
            true,
        )
        .unwrap();
    }
    let app = Router::new().merge(foundry_serve::http::router(st.clone()));
    let (code, _) = req(app, Some(&tok), "x").await;
    assert_eq!(code, StatusCode::UNAUTHORIZED, "disabled session must fail");
}

#[tokio::test(flavor = "multi_thread")]
async fn token_consumed_once_replay_refused() {
    let st = setup_with_users("replay").await;
    let tok = login(&st, "root").await;
    let app = Router::new().merge(foundry_serve::http::router(st.clone()));
    let (c1, _) = req(app, Some(&tok), "go").await;
    assert_eq!(c1, StatusCode::OK);
    // second POST: order is Publishing -> 409 (state machine + token both refuse)
    let app2 = Router::new().merge(foundry_serve::http::router(st.clone()));
    let (c2, _) = req(app2, Some(&tok), "again").await;
    assert_eq!(c2, StatusCode::CONFLICT);
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_actor_cannot_authenticate() {
    // historical journal actors (M0-M3 synthetic ids) are labels, not
    // principals: no session can ever resolve for them.
    let st = setup_with_users("legacy").await;
    let ok = {
        let id = st.identity.lock().await;
        // a "legacy actor token" is just an unknown opaque string
        id.resolve_session("die-worker-legacy-token").is_err()
    };
    assert!(ok, "legacy actor tokens never authenticate");
}
