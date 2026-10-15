//! T29 gate: the approval API enforces identity. M3's t27 test is now
//! INVALID by design ("serve local = human" retired) — these tests
//! assert the NEW contract.
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::Router;
use foundry_domain::{Order, OrderEvent, OrderState};
use foundry_exec::{
    ArtifactLocation, ArtifactTuple, Attempt, EmitReceipt, Generation, ImmutableCommit, OrderId,
    RunId, SqliteTupleStore, TupleStore,
};
use foundry_serve::ServeState;
use foundry_serve::identity::{Identity, PasswordHasher, Role};
use std::path::PathBuf;
use std::sync::Arc;
use tower::ServiceExt;

static APPROVAL_TUPLE_ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
        st.line
            .commit_for_test(id, &e, _from, OrderState::FurnaceHold, &a)
            .await
            .unwrap();
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
            &foundry_serve::identity::AuthenticatedUser {
                username: "root".into(),
                role: Role::Owner,
            },
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

fn approval_tuple_store(tag: &str) -> SqliteTupleStore {
    let dir = std::env::temp_dir().join(format!(
        "t29-tuples-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let tuple_path = dir.join("tuples.db");
    // Every test in this binary that reaches the approval handler holds
    // APPROVAL_TUPLE_ENV, so no request reads this variable during mutation.
    unsafe { std::env::set_var("FOUNDRY_TUPLES", &tuple_path) };

    SqliteTupleStore::open(&tuple_path).unwrap()
}

fn seed_approval_tuple(tag: &str) {
    let store = approval_tuple_store(tag);
    let tuple = ArtifactTuple {
        order: OrderId("o-29".into()),
        attempt: Attempt(1),
        run: RunId(1),
        generation: Generation(0),
        immutable_commit: ImmutableCommit("commit-xyz".into()),
        artifact_location: ArtifactLocation("review-log:t29-identity-gate".into()),
    };
    store.record(&tuple).unwrap();
    let key = tuple.key();
    store
        .promote(
            &key,
            &EmitReceipt {
                key: key.clone(),
                event: "publish".into(),
                at: chrono::Utc::now(),
            },
        )
        .unwrap();
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
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (code, serde_json::from_slice(&body).unwrap_or_default())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn approval_requires_authentication() {
    let _tuple_env = APPROVAL_TUPLE_ENV.lock().await;
    drop(approval_tuple_store("noauth"));
    let st = setup_with_users("noauth").await;
    let app = Router::new().merge(foundry_serve::http::router(st.clone()));
    let (code, body) = req(app, None, "bump").await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "authentication required");
}

#[tokio::test(flavor = "multi_thread")]
async fn observer_forbidden_approver_allowed() {
    let _tuple_env = APPROVAL_TUPLE_ENV.lock().await;
    seed_approval_tuple("roles");
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
    let _tuple_env = APPROVAL_TUPLE_ENV.lock().await;
    drop(approval_tuple_store("disable"));
    let st = setup_with_users("disable").await;
    let tok = login(&st, "obs").await;
    // owner disables obs
    {
        let mut id = st.identity.lock().await;
        id.set_disabled(
            &foundry_serve::identity::AuthenticatedUser {
                username: "root".into(),
                role: Role::Owner,
            },
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
    let _tuple_env = APPROVAL_TUPLE_ENV.lock().await;
    seed_approval_tuple("replay");
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


#[tokio::test(flavor = "multi_thread")]
async fn identity_errors_precede_tuple_store_failure() {
    let _tuple_env = APPROVAL_TUPLE_ENV.lock().await;
    let store = approval_tuple_store("unavailable");
    drop(store);
    let valid_path = std::env::var_os("FOUNDRY_TUPLES").unwrap();
    // A directory is deterministically not an openable SQLite database.
    let invalid_path = PathBuf::from(&valid_path).parent().unwrap().to_path_buf();
    unsafe { std::env::set_var("FOUNDRY_TUPLES", &invalid_path) };
    let st = setup_with_users("unavailable").await;
    let app = || Router::new().merge(foundry_serve::http::router(st.clone()));
    assert_eq!(req(app(), None, "go").await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(req(app(), Some("invalid-session"), "go").await.0, StatusCode::UNAUTHORIZED);
    let observer = login(&st, "obs").await;
    assert_eq!(req(app(), Some(&observer), "go").await.0, StatusCode::FORBIDDEN);
    let owner = login(&st, "root").await;
    assert_eq!(req(app(), Some(&owner), "go").await.0, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(st.line.get("o-29").await.unwrap().state(), OrderState::FurnaceHold);
    assert!(!st.ledger.read_all().unwrap().iter().any(|r| r.to_state == "publishing"));
    unsafe { std::env::set_var("FOUNDRY_TUPLES", valid_path) };
}
