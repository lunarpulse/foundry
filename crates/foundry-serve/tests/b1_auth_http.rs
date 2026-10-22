use axum::{
    body::Body,
    http::{Request, StatusCode},
};
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

#[cfg(feature = "p2-test-support")]
#[path = "../../foundry-line/tests/support/product_result_fixture.rs"]
mod product_result_fixture;

static NEXT: AtomicU64 = AtomicU64::new(0);
fn temp_db() -> PathBuf {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let scratch = PathBuf::from(
        std::env::var_os("TMPDIR").expect("TMPDIR must be configured private scratch"),
    );
    assert_ne!(scratch, PathBuf::from("/tmp"));
    scratch.join(format!(
        "foundry-b1-auth-{}-{n}-{nonce}.db",
        std::process::id()
    ))
}

#[tokio::test]
async fn registered_product_login_session_and_logout_use_product_store() {
    let path = temp_db();
    let _ = std::fs::remove_file(&path);
    let config = ProductConfig {
        installation_id: "test-install".into(),
        database_id: "test-db".into(),
    };
    let mut store = ProductStore::open(&path, config.clone()).unwrap();
    let phc =
        foundry_ledger::product_identity::ProductPasswordKdf::hash("canary-password").unwrap();
    store
        .bootstrap_owner_scoped("owner-a", &phc, &[], &[])
        .unwrap();
    let state = foundry_serve::auth_http::ProductHttpState::new(store);
    let app = foundry_serve::http::product_router(state);

    let page = app
        .clone()
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    let page_html =
        String::from_utf8_lossy(&page.into_body().collect().await.unwrap().to_bytes()).into_owned();
    assert!(page_html.contains("page's memory"));
    assert!(page_html.contains("Loading active review holds"));
    assert!(!page_html.contains("localStorage"));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/product/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"actor_id":"owner-a","password":"canary-password"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let token = value["access_token"].as_str().unwrap().to_owned();
    assert_eq!(token.len(), 64);
    assert!(!String::from_utf8_lossy(&bytes).contains("canary-password"));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/product/session")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["actor_id"],
        "owner-a"
    );

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/product/accounts/owner-a/authority")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"role":"viewer","enabled":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "last enabled Owner is protected"
    );

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/product/accounts")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"actor_id":"approver-b","role":"approver","password":"approver-canary"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/product/accounts/approver-b/authority")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"role":"viewer","enabled":false}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/product/logout")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/product/session")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[tokio::test]
async fn registered_product_approval_rejects_unbound_result_without_consuming_grant() {
    let path = temp_db();
    let _ = std::fs::remove_file(&path);
    let config = ProductConfig {
        installation_id: "approval-install".into(),
        database_id: "approval-db".into(),
    };
    let mut store = ProductStore::open(&path, config.clone()).unwrap();
    let phc =
        foundry_ledger::product_identity::ProductPasswordKdf::hash("approval-password").unwrap();
    let source = ScopeSourceManifest {
        connector_id: "gh1".into(),
        source_instance_id: "approval-source".into(),
        generation: 1,
        identity: RepositoryIdentity::canonicalized(
            "acme",
            "github",
            "https://api.github.com",
            "repo:42",
        )
        .unwrap(),
        api_owner: Some("acme".into()),
        api_repo: Some("repo".into()),
        legacy_repository_locator: "repo:42".into(),
        scope_query: "open+foundry:make".into(),
        enabled: true,
    };
    store
        .bootstrap_owner_scoped(
            "owner-a",
            &phc,
            std::slice::from_ref(&source),
            &[ScopeGrantManifest {
                actor_id: "owner-a".into(),
                source_instance_id: source.source_instance_id.clone(),
                capabilities: vec![ProductCapability::ApproveOrder],
            }],
        )
        .unwrap();
    store
        .admit_order(
            "order-a",
            "gh1",
            "repo:42",
            "issue:1",
            "dep-bump",
            "snapshot",
            chrono::Utc::now(),
        )
        .unwrap();

    let state = foundry_serve::auth_http::ProductHttpState::new(store);
    let app = foundry_serve::http::product_router(state.clone());
    let login = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/product/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"actor_id":"owner-a","password":"approval-password"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let login_body = login.into_body().collect().await.unwrap().to_bytes();
    let token = serde_json::from_slice::<serde_json::Value>(&login_body).unwrap()["access_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let grant = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/product/approval-grants")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"order_id":"order-a"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(grant.status(), StatusCode::CREATED);
    let grant_body = grant.into_body().collect().await.unwrap().to_bytes();
    let grant: serde_json::Value = serde_json::from_slice(&grant_body).unwrap();
    let grant_id = grant["grant_id"].as_str().unwrap();
    let grant_token = grant["grant_token"].as_str().unwrap();

    let payload = serde_json::json!({
        "grant_id":grant_id,"grant_token":grant_token,"expected_source_revision":1,
        "expected_attempt":1,"run_id":1,"generation":0,"artifact_digest":"d".repeat(40),
        "tuple_digest":"e".repeat(64),"intent":"approve unbound artifact"
    })
    .to_string();
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/api/product/orders/order-a/approve")
            .header("authorization", format!("Bearer {token}"))
            .header("idempotency-key", "approve-a")
            .header("content-type", "application/json")
            .body(Body::from(payload.clone()))
            .unwrap()
    };
    let first = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(first.status(), StatusCode::CONFLICT);
    let consumed: (i64, i64, i64) = rusqlite::Connection::open(&path).unwrap().query_row(
        "SELECT (SELECT count(*) FROM product_token_consumptions_v1),(SELECT count(*) FROM product_outcomes_v1),(SELECT count(*) FROM ledger WHERE to_state='publishing')", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(consumed, (0, 0, 0));
    let grant_state: String = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT state FROM product_grants_v1 WHERE grant_id=?1",
            [grant_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_ne!(grant_state, "consumed");
    drop(app);
    drop(state);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[cfg(feature = "p2-test-support")]
#[tokio::test]
async fn registered_product_approval_route_consumes_genuine_result_and_replays_exact_receipt() {
    use product_result_fixture::{
        FixtureClock, FixtureIdentity, genuine_host_done, genuine_result_finalized,
    };

    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: "approval-install".into(),
            database_id: "approval-db".into(),
        },
        "approval-source",
        "approval-order",
    );
    let mut fixture = genuine_result_finalized(genuine_host_done(identity));
    fixture.assert_genuine_private_handoff();
    fixture.host.take();
    fixture.result_store.take();

    // Reopen the same database and pinned artifact root with the fixture clock
    // before constructing the actual registered ProductAtomicV1 router.
    let mut store = ProductStore::open_with_clock(
        &fixture.db_path,
        fixture.config.clone(),
        std::sync::Arc::new(FixtureClock(fixture.now)),
    )
    .unwrap();
    store
        .configure_product_artifact_root(&fixture.artifact_root)
        .unwrap();
    let state = foundry_serve::auth_http::ProductHttpState::new(store);
    let app = foundry_serve::http::product_router(state.clone());

    let login = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/product/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"actor_id":"reader","password":"result-reader-password"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let login_bytes = login.into_body().collect().await.unwrap().to_bytes();
    let token = serde_json::from_slice::<serde_json::Value>(&login_bytes).unwrap()["access_token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!String::from_utf8_lossy(&login_bytes).contains("result-reader-password"));

    // The grant and opaque grant token come from the HTTP handler. The fixture's
    // preparation proof is intentionally not submitted to this route.
    let grant_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/product/approval-grants")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"order_id": fixture.identity.order_id}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(grant_response.status(), StatusCode::CREATED);
    let grant_bytes = grant_response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let grant_json: serde_json::Value = serde_json::from_slice(&grant_bytes).unwrap();
    let grant_id = grant_json["grant_id"].as_str().unwrap().to_owned();
    let grant_token = grant_json["grant_token"].as_str().unwrap().to_owned();

    let before: (String, String, String, String) = rusqlite::Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT result_id,binding_digest,tuple_digest,receipt_json FROM product_result_bindings_v1 WHERE order_id=?1",
            [&fixture.identity.order_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    let terminal = fixture
        .terminal
        .as_ref()
        .expect("successful HTTP approval fixture must have a real terminal host result");
    let approval_payload = serde_json::json!({
        "grant_id": grant_id,
        "grant_token": grant_token,
        "expected_source_revision": 1,
        "expected_attempt": 1,
        "run_id": fixture.expected.host_key.run as i64,
        "generation": 0,
        "artifact_digest": terminal.result_commit,
        "tuple_digest": before.2,
        "intent": "approve exact genuine local result"
    })
    .to_string();
    let approve_request = || {
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/product/orders/{}/approve",
                fixture.identity.order_id
            ))
            .header("authorization", format!("Bearer {token}"))
            .header("idempotency-key", "genuine-http-approval")
            .header("content-type", "application/json")
            .body(Body::from(approval_payload.clone()))
            .unwrap()
    };

    let first = app.clone().oneshot(approve_request()).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first_bytes = first.into_body().collect().await.unwrap().to_bytes();
    let first_json: serde_json::Value = serde_json::from_slice(&first_bytes).unwrap();
    let receipt: foundry_ledger::ProductApprovalReceipt =
        serde_json::from_value(first_json["receipt"].clone()).unwrap();
    assert_eq!(receipt.order_id, fixture.identity.order_id);
    assert_eq!(
        receipt.sequence,
        first_json["receipt"]["sequence"].as_i64().unwrap()
    );

    let retry = app.clone().oneshot(approve_request()).await.unwrap();
    assert_eq!(retry.status(), StatusCode::OK);
    let retry_bytes = retry.into_body().collect().await.unwrap().to_bytes();
    let retry_json: serde_json::Value = serde_json::from_slice(&retry_bytes).unwrap();
    assert_eq!(first_json, retry_json);
    assert_eq!(
        first_bytes, retry_bytes,
        "full HTTP response body replays exactly"
    );

    let conn = rusqlite::Connection::open(&fixture.db_path).unwrap();
    let participants: (i64, i64, i64, i64, i64) = conn.query_row(
        "SELECT (SELECT count(*) FROM product_token_consumptions_v1),(SELECT count(*) FROM product_outcomes_v1),(SELECT count(*) FROM ledger WHERE to_state='publishing'),(SELECT count(*) FROM product_approval_result_bindings_v1),(SELECT count(*) FROM product_transition_receipts_v1 WHERE operation='human-approval')",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    ).unwrap();
    assert_eq!(participants, (1, 1, 1, 1, 1));
    let side_binding: (String, String, String) = conn.query_row(
        "SELECT result_id,binding_digest,tuple_digest FROM product_approval_result_bindings_v1 WHERE operation_id=?1",
        [&receipt.operation_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!(
        side_binding,
        (before.0.clone(), before.1.clone(), before.2.clone())
    );
    let transition: (String, i64, String) = conn.query_row(
        "SELECT event_hash,lifecycle,operation FROM product_transition_receipts_v1 WHERE seq=?1 AND order_id=?2",
        rusqlite::params![receipt.sequence, fixture.identity.order_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!(
        transition,
        (
            receipt.event_hash.clone(),
            receipt.lifecycle,
            "human-approval".into()
        )
    );
    let grant_state: String = conn
        .query_row(
            "SELECT state FROM product_grants_v1 WHERE grant_id=?1",
            [&grant_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(grant_state, "consumed");
    let consumption_operation: String = conn.query_row(
        "SELECT operation_id FROM product_token_consumptions_v1 WHERE token_digest=(SELECT token_digest FROM product_grants_v1 WHERE grant_id=?1)",
        [&grant_id],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(consumption_operation, receipt.operation_id);
    let after: (String, String, String, String) = conn.query_row(
        "SELECT result_id,binding_digest,tuple_digest,receipt_json FROM product_result_bindings_v1 WHERE order_id=?1",
        [&fixture.identity.order_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ).unwrap();
    assert_eq!(
        before, after,
        "approval leaves genuine result receipt unchanged"
    );
    drop(conn);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 4);
    assert_eq!(fixture.restores.load(Ordering::SeqCst), 0);

    drop(app);
    drop(state);
    let reopened = ProductStore::open_with_clock(
        &fixture.db_path,
        fixture.config.clone(),
        std::sync::Arc::new(FixtureClock(fixture.now)),
    )
    .unwrap();
    let reopened_state = foundry_serve::auth_http::ProductHttpState::new(reopened);
    let reopened_app = foundry_serve::http::product_router(reopened_state.clone());
    let post_reopen = reopened_app
        .clone()
        .oneshot(approve_request())
        .await
        .unwrap();
    assert_eq!(post_reopen.status(), StatusCode::OK);
    let post_reopen_bytes = post_reopen.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(first_bytes, post_reopen_bytes);
    let conn = rusqlite::Connection::open(&fixture.db_path).unwrap();
    let replay_counts: (i64, i64, i64, i64, i64) = conn.query_row(
        "SELECT (SELECT count(*) FROM product_token_consumptions_v1),(SELECT count(*) FROM product_outcomes_v1),(SELECT count(*) FROM ledger WHERE to_state='publishing'),(SELECT count(*) FROM product_approval_result_bindings_v1),(SELECT count(*) FROM product_transition_receipts_v1 WHERE operation='human-approval')",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    ).unwrap();
    assert_eq!(replay_counts, (1, 1, 1, 1, 1));
    let replay_result_receipt: String = conn
        .query_row(
            "SELECT receipt_json FROM product_result_bindings_v1 WHERE order_id=?1",
            [&fixture.identity.order_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(replay_result_receipt, before.3);
    drop(conn);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 4);
    assert_eq!(fixture.restores.load(Ordering::SeqCst), 0);

    drop(reopened_app);
    drop(reopened_state);
    eprintln!(
        "genuine HTTP approval fixture scratch: {}",
        fixture.root.display()
    );
    std::fs::remove_dir_all(&fixture.root).expect("remove this test's private fixture root");
}
