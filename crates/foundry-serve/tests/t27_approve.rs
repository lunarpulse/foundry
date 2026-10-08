//! T27: UI-1 approve action — single token mint, state-machine safety.
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::Router;

use foundry_domain::{Order, OrderEvent, OrderState};
use foundry_ledger::SqliteLedger;
use foundry_serve::ServeState;
use std::sync::Arc;
use tower::ServiceExt;

fn db_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("t27-{}-{}.db", tag, std::process::id()))
}

fn actor() -> foundry_domain::ActorId {
    foundry_domain::ActorId {
        kind: foundry_domain::ActorKind::Agent,
        source: "t".into(),
        id: "t".into(),
    }
}

/// Drive one order to FurnaceHold directly through the journal (the
/// state serve approves FROM).
async fn order_at_hold(st: &ServeState, id: &str) {
    let o = Order::new(
        id.into(),
        foundry_domain::ExternalRef::github("o/r", "1", "k".into()),
        chrono::Utc::now(),
    );
    st.line.submit(o).await;
    // queued -> working -> inspecting -> review -> furnacehold
    let a = actor();
    let mut s = st.line.get(id).await.unwrap();
    let _ = &mut s;
    use foundry_domain::OrderEvent as E;
    let seq = [
        (E::Queued, OrderState::Received, OrderState::Queued),
        (E::DieStarted, OrderState::Queued, OrderState::Working),
        (
            E::DieSucceeded { attempt: 1 },
            OrderState::Working,
            OrderState::Inspecting,
        ),
        (
            E::InspectionPassed,
            OrderState::Inspecting,
            OrderState::FurnaceHold,
        ),
    ];
    for (e, from, to) in seq {
        st.line.commit_for_test(id, &e, from, to, &a).await.unwrap();
    }
    // Die의 result_commit을 모방: 승인은 비어있지 않은 artifact_digest에
    // 결속되어야 함 (order.rs §11). 실제 watch 파이프라인은 DieSucceeded에서
    // digest를 세팅한다 (line.rs:229).
    let mut o = st.line.get(id).await.unwrap();
    o.artifact_digest = Some("commit-abc123".into());
    st.line.submit(o).await;
}

#[tokio::test]
async fn approve_mints_single_token_and_publishes_state() {
    let db = db_path("ok");
    let _ = std::fs::remove_file(&db);
    {
        let led = SqliteLedger::open(&db).unwrap();
        let _ = led;
    }
    let st = Arc::new(ServeState::new(&db).unwrap());
    order_at_hold(&st, "o-27").await;

    let app = Router::new().merge(foundry_serve::http::router(st.clone()));
    let req = Request::builder()
        .method("POST")
        .uri("/api/orders/o-27/approve")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"intent":"bump anyhow, single-line diff verified"}"#,
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["state"], "publishing");
    assert!(v["height"].as_i64().unwrap() >= 5);

    // chain still valid after the write
    let rows = st.ledger.read_all().unwrap();
    assert!(foundry_ledger::replay::replay(&rows, |_| true).is_ok());

    // ---- SECOND POST: same order, now Publishing => 409 (no double mint)
    let app2 = Router::new().merge(foundry_serve::http::router(st.clone()));
    let req2 = Request::builder()
        .method("POST")
        .uri("/api/orders/o-27/approve")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"intent":"again"}"#))
        .unwrap();
    let resp2 = app2.oneshot(req2).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::CONFLICT);
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn empty_intent_is_400() {
    let db = db_path("nointent");
    let _ = std::fs::remove_file(&db);
    {
        let led = SqliteLedger::open(&db).unwrap();
        let _ = led;
    }
    let st = Arc::new(ServeState::new(&db).unwrap());
    order_at_hold(&st, "o-e").await;
    let app = Router::new().merge(foundry_serve::http::router(st));
    let req = Request::builder()
        .method("POST")
        .uri("/api/orders/o-e/approve")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"intent":"  "}"#))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn unknown_order_is_404() {
    let db = db_path("404");
    let _ = std::fs::remove_file(&db);
    {
        let led = SqliteLedger::open(&db).unwrap();
        let _ = led;
    }
    let st = Arc::new(ServeState::new(&db).unwrap());
    let app = Router::new().merge(foundry_serve::http::router(st));
    let req = Request::builder()
        .method("POST")
        .uri("/api/orders/nope/approve")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"intent":"x"}"#))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let _ = std::fs::remove_file(&db);
}
