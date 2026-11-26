//! Host fixture for the same execution handler used by serve --product.
use super::*;
use axum::{extract::Request as HttpRequest, middleware, response::Response};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "parent host only, requires the pinned W31 image and private fixture"]
async fn owned_producer() {
    let root = PathBuf::from(std::env::var_os("W31_FIXTURE").unwrap());
    let out = PathBuf::from(std::env::var_os("W31_WITNESS_DIR").unwrap());
    let setting = |key| std::env::var(key).ok();
    let config = json!({
        "image":setting("FOUNDRY_W3_IMAGE"),
        "timeout_env":setting("FOUNDRY_DIE_TIMEOUT"),
        "timeout_secs":foundry_line::DieStageExecutor::configured_timeout().unwrap(),
        "repo":setting("FOUNDRY_REPO_PATH"), "pack":setting("FOUNDRY_REVIEW_PACK"),
        "artifacts":setting("FOUNDRY_PRODUCT_ARTIFACT_ROOT"), "sessions":setting("FOUNDRY_PRODUCT_SESSION_ROOT"),
        "claims":setting("FOUNDRY_PRODUCT_CLAIM_ROOT")
    });
    fs::write(out.join("controller-config.json"), config.to_string()).unwrap();
    let db = Database {
        db: root.join("product.sqlite"),
        installation_id: "b2-w1-runtime-install".into(),
        database_id: "b2-w1-runtime-database".into(),
    };
    let request: Request =
        serde_json::from_slice(&fs::read(root.join("request.json")).unwrap()).unwrap();
    let mut headers = HeaderMap::new();
    let owner = fs::read_to_string(root.join("owner")).unwrap();
    headers.insert("authorization", format!("Bearer {owner}").parse().unwrap());
    headers.insert(
        "x-foundry-service",
        fs::read_to_string(root.join("service"))
            .unwrap()
            .parse()
            .unwrap(),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let app = foundry_serve::http::product_router(foundry_serve::auth_http::ProductHttpState::new(
        db.open().unwrap(),
    ))
    .layer(middleware::from_fn(
        move |req: HttpRequest, next: middleware::Next| {
            count.fetch_add(1, Ordering::SeqCst);
            async move { next.run(req).await }
        },
    ));
    // The fixture exposes real privileged routes over a local Unix socket only.
    let sock = tokio::net::UnixListener::bind(root.join("serve.sock")).unwrap();
    let server = tokio::spawn(async move { axum::serve(sock, app).await.unwrap() });
    let reply = execute(State(db), headers, Json(request)).await;
    server.abort();
    let n = calls.load(Ordering::SeqCst);
    fs::write(
        out.join("serve-count.json"),
        json!({"requests": n}).to_string(),
    )
    .unwrap();
    let (code, Json(status)) = reply.unwrap();
    fs::write(out.join("controller-result.json"), status.to_string()).unwrap();
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(status["state"], "uncertain");
    assert_eq!(
        n, 1,
        "one host positive control and zero requests from the child"
    );
    assert!(out.join("observed.json").is_file());
}
