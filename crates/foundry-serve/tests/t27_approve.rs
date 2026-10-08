//! RETIRED by M4 T29 (plan v2 review #2/#3): the M3 contract
//! "serve local = human" is gone — the approval API now requires an
//! authenticated session (see t29_identity_gate.rs). This file keeps
//! ONE regression: the unauthenticated path is refused.
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::Router;
use foundry_domain::{Order, OrderEvent, OrderState};
use foundry_serve::ServeState;
use std::sync::Arc;
use tower::ServiceExt;

#[tokio::test(flavor = "multi_thread")]
async fn unauthenticated_approval_is_refused() {
    let db = std::env::temp_dir().join(format!("t27-retired-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_file(db.with_extension("identity.db"));
    let st = Arc::new(ServeState::new(&db).unwrap());
    // order at hold
    let o = Order::new("o-r".into(), foundry_domain::ExternalRef::github("o/r","1","k".into()), chrono::Utc::now());
    st.line.submit(o).await;
    let a = foundry_domain::ActorId { kind: foundry_domain::ActorKind::Agent, source: "t".into(), id: "t".into() };
    for e in [OrderEvent::Queued, OrderEvent::DieStarted] {
        st.line.commit_for_test("o-r", &e, OrderState::Received, OrderState::FurnaceHold, &a).await.unwrap();
    }
    let app = Router::new().merge(foundry_serve::http::router(st));
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/orders/o-r/approve")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"intent":"x"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let _ = std::fs::remove_file(&db);
}
