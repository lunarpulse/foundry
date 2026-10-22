use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use foundry_ledger::product_identity::ProductPasswordKdf;
use foundry_ledger::{
    ProductCapability, ProductConfig, ProductStore, RepositoryIdentity, ScopeGrantManifest,
    ScopeSourceManifest,
};
use http_body_util::BodyExt;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use tower::ServiceExt;

static NEXT: AtomicU64 = AtomicU64::new(0);

fn scratch() -> (PathBuf, PathBuf) {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "foundry-b1-scope-http-{}-{n}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("product.sqlite");
    (root, path)
}

fn source(connector: &str, id: &str, origin: &str) -> ScopeSourceManifest {
    ScopeSourceManifest {
        connector_id: connector.into(),
        source_instance_id: id.into(),
        generation: 1,
        identity: RepositoryIdentity::canonicalized("acme", "github", origin, "42").unwrap(),
        api_owner: Some("acme".into()),
        api_repo: Some("repo".into()),
        legacy_repository_locator: "42".into(),
        scope_query: "open+foundry:make".into(),
        enabled: true,
    }
}

async fn json_request(
    app: &axum::Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: &str,
) -> (StatusCode, Vec<u8>) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, bytes)
}

async fn get(app: &axum::Router, uri: &str, token: &str) -> (StatusCode, Vec<u8>) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, bytes)
}

#[tokio::test]
async fn product_http_scope_admin_and_order_reads_are_isolated_and_revocable() {
    let (root, path) = scratch();
    let config = ProductConfig {
        installation_id: "scope-http-install".into(),
        database_id: "scope-http-db".into(),
    };
    let source_a = source("github-a", "source-a", "https://api.github.com");
    let source_b = source("github-b", "source-b", "https://github.enterprise.example");
    let phc = ProductPasswordKdf::hash("owner-password-strong").unwrap();
    let mut store = ProductStore::open(&path, config.clone()).unwrap();
    store
        .bootstrap_owner_scoped(
            "owner",
            &phc,
            &[source_a.clone(), source_b.clone()],
            &[ScopeGrantManifest {
                actor_id: "owner".into(),
                source_instance_id: source_a.source_instance_id.clone(),
                capabilities: vec![ProductCapability::ReadOrder],
            }],
        )
        .unwrap();
    // The account is created through the registered Owner HTTP operation.
    let owner = store
        .login("owner", "owner-password-strong", chrono::Utc::now())
        .unwrap();
    let owner_token = owner.bearer().to_owned();
    let viewer_login_before = store.login("viewer", "viewer-password-strong", chrono::Utc::now());
    assert!(viewer_login_before.is_err());
    let state = foundry_serve::auth_http::ProductHttpState::new(store);
    let app = foundry_serve::http::product_router(state);

    let (status, _) = json_request(
        &app,
        "POST",
        "/api/product/accounts",
        Some(&owner_token),
        r#"{"actor_id":"viewer","role":"viewer","password":"viewer-password-strong"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = json_request(
        &app,
        "POST",
        "/api/product/login",
        None,
        r#"{"actor_id":"owner","password":"owner-password-strong"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let owner_token = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["access_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, body) = json_request(
        &app,
        "POST",
        "/api/product/login",
        None,
        r#"{"actor_id":"viewer","password":"viewer-password-strong"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let viewer_token = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["access_token"]
        .as_str()
        .unwrap()
        .to_owned();

    let (status, body) = get(&app, "/api/product/sources", &owner_token).await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("source-a"));
    assert_eq!(
        get(&app, "/api/product/sources", &viewer_token).await.0,
        StatusCode::FORBIDDEN
    );

    // The registered source-configure route validates canonical origin and
    // returns the new source instance plus the revision required for the next
    // conditional update.
    let (status, _body) = json_request(&app, "POST", "/api/product/sources", Some(&owner_token),
        r#"{"connector_id":"github-c","organization":"acme","provider":"github","api_origin":"https://bad.example/path","stable_repository_id":"99","api_owner":"acme","api_repo":"repo","legacy_repository_locator":"99","scope_query":"open","enabled":true,"expected_revision":0,"request_key":"bad-origin"}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, body) = json_request(&app, "POST", "/api/product/sources", Some(&owner_token),
        r#"{"connector_id":"github-c","organization":"acme","provider":"github","api_origin":"https://api.github.com","stable_repository_id":"99","api_owner":"acme","api_repo":"repo","legacy_repository_locator":"99","scope_query":"open+foundry:make","enabled":true,"expected_revision":0,"request_key":"source-c"}"#).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let configured: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(configured["source_revision"], 1);
    assert!(
        configured["source"]["source_instance_id"]
            .as_str()
            .unwrap()
            .len()
            == 64
    );

    // Add one order to each repository through the ProductStore admission
    // API, then verify the registered list/detail consumers return only
    // explicit grants.
    let mut writer = ProductStore::open(&path, config.clone()).unwrap();
    writer
        .admit_order(
            "order-a",
            "github-a",
            "42",
            "issue:1",
            "dep-bump",
            "payload-a",
            chrono::Utc::now(),
        )
        .unwrap();
    writer
        .admit_order(
            "order-b",
            "github-b",
            "42",
            "issue:1",
            "dep-bump",
            "payload-b",
            chrono::Utc::now(),
        )
        .unwrap();
    drop(writer);

    let (status, body) = get(&app, "/api/product/orders", &owner_token).await;
    assert_eq!(status, StatusCode::OK);
    let owner_orders = String::from_utf8_lossy(&body).into_owned();
    assert!(owner_orders.contains("order-a"));
    assert!(!owner_orders.contains("order-b"));
    assert!(!owner_orders.contains("payload-b"));
    assert_eq!(
        get(&app, "/api/product/orders/order-a", &owner_token)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        get(&app, "/api/product/orders/order-b", &owner_token)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let foreign_detail = get(&app, "/api/product/orders/order-a", &viewer_token).await;
    let unknown_detail = get(&app, "/api/product/orders/does-not-exist", &viewer_token).await;
    assert_eq!(foreign_detail.0, StatusCode::NOT_FOUND);
    assert_eq!(
        foreign_detail, unknown_detail,
        "foreign and unknown orders have identical public shape"
    );

    let grant_body = format!(
        r#"{{"subject_id":"viewer","source_instance_id":"{}","capability":"read_order","expected_scope_revision":0,"request_key":"viewer-a-grant"}}"#,
        source_a.source_instance_id
    );
    let (status, body) = json_request(
        &app,
        "POST",
        "/api/product/scope/grants",
        Some(&owner_token),
        &grant_body,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let receipt = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["receipt"].clone();
    assert_eq!(receipt["scope_revision"], 1);
    let (status, _) = json_request(
        &app,
        "POST",
        "/api/product/scope/grants",
        Some(&owner_token),
        &grant_body,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "exact grant retry returns receipt");
    let changed = grant_body.replace("read_order", "read_review");
    let (status, _) = json_request(
        &app,
        "POST",
        "/api/product/scope/grants",
        Some(&owner_token),
        &changed,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "same request key with changed body conflicts"
    );

    let (status, body) = get(&app, "/api/product/orders", &viewer_token).await;
    assert_eq!(status, StatusCode::OK);
    let viewer_orders = String::from_utf8_lossy(&body).into_owned();
    assert!(viewer_orders.contains("order-a"));
    assert!(!viewer_orders.contains("order-b"));
    assert_eq!(
        get(&app, "/api/product/orders/order-b", &viewer_token)
            .await
            .0,
        StatusCode::NOT_FOUND
    );

    let revoke_body = format!(
        r#"{{"subject_id":"viewer","source_instance_id":"{}","capability":"read_order","expected_scope_revision":1,"request_key":"viewer-a-revoke"}}"#,
        source_a.source_instance_id
    );
    let (status, _) = json_request(
        &app,
        "POST",
        "/api/product/scope/grants/revoke",
        Some(&owner_token),
        &revoke_body,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        String::from_utf8_lossy(&get(&app, "/api/product/orders", &viewer_token).await.1)
            .contains("\"orders\":[]")
    );
    assert_eq!(
        get(&app, "/api/product/orders/order-a", &viewer_token)
            .await
            .0,
        StatusCode::NOT_FOUND
    );

    let regrant_body = format!(
        r#"{{"subject_id":"viewer","source_instance_id":"{}","capability":"read_order","expected_scope_revision":2,"request_key":"viewer-a-regrant"}}"#,
        source_a.source_instance_id
    );
    assert_eq!(
        json_request(
            &app,
            "POST",
            "/api/product/scope/grants",
            Some(&owner_token),
            &regrant_body
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        get(&app, "/api/product/orders/order-a", &viewer_token)
            .await
            .0,
        StatusCode::OK
    );

    drop(app);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    let _ = std::fs::remove_dir_all(root);
}
