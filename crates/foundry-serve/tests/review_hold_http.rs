use axum::{body::Body, http::{Request, StatusCode}, Router};
use foundry_serve::ServeState;
use std::{sync::Arc, time::{SystemTime, UNIX_EPOCH}};
use tower::ServiceExt;

#[tokio::test]
async fn all_review_hold_routes_require_an_authenticated_session() {
    let unique=SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let db=std::env::temp_dir().join(format!("review-hold-{unique}.db"));
    let app=Router::new().merge(foundry_serve::http::router(Arc::new(ServeState::new(&db).unwrap())));
    for (suffix,body) in [
        ("resume",r#"{"target":"lenses"}"#),
        ("commit-pass","{}"),
        ("commit-fail","{}"),
        ("abort","{}"),
    ] {
        let response=app.clone().oneshot(Request::builder().method("POST")
            .uri(format!("/api/orders/o-1/review/hold/h-1/{suffix}"))
            .header("content-type","application/json").body(Body::from(body)).unwrap()).await.unwrap();
        assert_eq!(response.status(),StatusCode::UNAUTHORIZED);
    }
    let _=std::fs::remove_file(&db);
}
