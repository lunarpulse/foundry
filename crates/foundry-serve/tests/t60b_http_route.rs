//! T60b — the /api/policy/apply HTTP route is REGISTERED and behaves
//! per contract (astra B1: "a registered HTTP handler, not a library
//! function"). Uses axum's oneshot test adapter: real router, real
//! request, no network.
use axum::body::Body;
use axum::http::{Request, StatusCode};
use foundry_serve::ServeState;
use std::sync::Arc;
use http_body_util::BodyExt;
use tower::ServiceExt; // for .oneshot

fn state() -> Arc<ServeState> {
    // isolated env: temp ledger + temp governance journal
    let dir = std::env::temp_dir().join(format!("t60b-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // tests in this file are #[tokio::test] single-threaded per test
    // but run in parallel with each other; env is process-wide, so we
    // take the test lock via a global mutex to serialize state setup.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::set_var("FOUNDRY_LEDGER", dir.join("ledger.db"));
        std::env::set_var("FOUNDRY_GOVERNANCE_DB", dir.join("gov.db"));
    }
    // seed a ledger row so the corpus digest reader has an anchor
    {
        let c = rusqlite::Connection::open(dir.join("ledger.db")).unwrap();
        c.execute(
            "CREATE TABLE IF NOT EXISTS ledger (seq INTEGER PRIMARY KEY AUTOINCREMENT, order_id TEXT, from_state TEXT, to_state TEXT, event TEXT, actor TEXT, intent TEXT, at TEXT, prev_hash TEXT, hash TEXT, event_json TEXT)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO ledger (order_id, event, prev_hash, hash, event_json) VALUES ('o1','DieSucceeded { attempt: 1 }','GENESIS','h1','{\"attempt\":1}')",
            [],
        )
        .unwrap();
    }
    let st = Arc::new(ServeState::new(dir.join("serve-ledger.db")).expect("state"));
    st
}

#[tokio::test]
async fn route_is_registered_and_rejects_without_session() {
    let st = state();
    let router = foundry_serve::http::router(st);
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/policy/apply")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"op_id":"x","approver":"who","role":"Owner","proposal_digest":"prop-x","prediction":{"metric":"m","direction":"down","threshold":"t","window":"w"},"base_policy_version":0,"issued_at":1,"expires_at":2}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "no bearer -> 401");
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let txt = String::from_utf8_lossy(&body);
    assert!(txt.contains("bearer"), "auth failure is explicit: {txt}");
}

#[tokio::test]
async fn route_rejects_garbage_session() {
    let st = state();
    let router = foundry_serve::http::router(st);
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/policy/apply")
                .header("content-type", "application/json")
                .header("authorization", "Bearer not-a-session")
                .body(Body::from(
                    r#"{"op_id":"x","approver":"who","role":"Owner","proposal_digest":"prop-x","prediction":{"metric":"m","direction":"down","threshold":"t","window":"w"},"base_policy_version":0,"issued_at":1,"expires_at":2}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "unknown session -> 401");
}

#[tokio::test]
async fn route_rejects_malformed_body() {
    let st = state();
    let router = foundry_serve::http::router(st);
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/policy/apply")
                .header("content-type", "application/json")
                .header("authorization", "Bearer x")
                .body(Body::from("not json"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(resp.status() != StatusCode::OK, "malformed body must not succeed");
}
