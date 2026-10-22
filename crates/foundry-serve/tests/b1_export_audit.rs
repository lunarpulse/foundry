use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use foundry_ledger::{
    ProductCapability, ProductConfig, ProductStore, RepositoryIdentity, ScopeGrantManifest,
    ScopeSourceManifest,
};
use foundry_serve::{auth_http::ProductHttpState, http::product_router};
use tower::ServiceExt;

fn request_method(
    app: &axum::Router,
    method: &str,
    path: &str,
    bearer: &str,
    body: &str,
) -> (StatusCode, Vec<u8>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", format!("Bearer {bearer}"));
        if !body.is_empty() {
            builder = builder.header("content-type", "application/json");
        }
        let response = app
            .clone()
            .oneshot(builder.body(Body::from(body.to_owned())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .to_vec();
        (status, bytes)
    })
}

fn request(app: &axum::Router, path: &str, bearer: &str) -> (StatusCode, Vec<u8>) {
    request_method(app, "GET", path, bearer, "")
}

#[tokio::test]
async fn registered_product_export_requires_current_approver_and_redacts_identifiers() {
    let root = std::env::temp_dir().join(format!(
        "foundry-b1-export-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("product.db");
    let mut store = ProductStore::open(
        &db,
        ProductConfig {
            installation_id: "install-canary".into(),
            database_id: "export-db".into(),
        },
    )
    .unwrap();
    let password = "synthetic-export-password";
    let phc = foundry_ledger::product_identity::ProductPasswordKdf::hash(password).unwrap();
    let source = ScopeSourceManifest {
        connector_id: "gh".into(),
        source_instance_id: "export-source".into(),
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
            "canary-owner",
            &phc,
            std::slice::from_ref(&source),
            &[ScopeGrantManifest {
                actor_id: "canary-owner".into(),
                source_instance_id: source.source_instance_id.clone(),
                capabilities: vec![
                    ProductCapability::ReadOrder,
                    ProductCapability::ExportEvidence,
                    ProductCapability::ReadOperationalAudit,
                ],
            }],
        )
        .unwrap();
    store
        .admit_order(
            "ORDER-PII-CANARY-73",
            "gh",
            "repo:42",
            "issue:7",
            "dep-bump",
            "immutable task snapshot",
            chrono::Utc::now(),
        )
        .unwrap();
    let session = store
        .login("canary-owner", password, chrono::Utc::now())
        .unwrap();
    let bearer = session.bearer().to_owned();
    drop(session);

    let app = product_router(ProductHttpState::new(store));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/product/export")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/product/export")
                .header("authorization", format!("Bearer {bearer}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "explicit ExportEvidence grant returns an actual scoped projection"
    );
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    for canary in [
        "ORDER-PII-CANARY-73",
        "canary-owner",
        "install-canary",
        password,
        &bearer,
    ] {
        assert!(!text.contains(canary));
    }
    let export: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(export["schema"], "foundry-scoped-evidence-projection-v1");
    assert_eq!(export["coverage"], "complete");
    assert!(
        export["projected_digest"]
            .as_str()
            .is_some_and(|v| v.len() == 64)
    );
    assert_eq!(export["rows"][0]["ordinal"], 1);
    for forbidden in [
        "seq",
        "sequence",
        "prev_hash",
        "hash",
        "head",
        "prefix_sequence",
        "raw_prefix_head",
    ] {
        assert!(
            export.get(forbidden).is_none(),
            "global coordinate escaped as {forbidden}"
        );
        assert!(
            export["rows"][0].get(forbidden).is_none(),
            "global coordinate escaped as {forbidden}"
        );
    }

    let audit_response = app
        .oneshot(
            Request::builder()
                .uri("/api/product/audit")
                .header("authorization", format!("Bearer {bearer}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        audit_response.status(),
        StatusCode::OK,
        "explicit ReadOperationalAudit grant returns an actual scoped inventory"
    );
    let audit_bytes = axum::body::to_bytes(audit_response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let audit_text = std::str::from_utf8(&audit_bytes).unwrap();
    for canary in [
        "ORDER-PII-CANARY-73",
        "canary-owner",
        "install-canary",
        password,
        &bearer,
    ] {
        assert!(!audit_text.contains(canary));
    }
    let audit: serde_json::Value = serde_json::from_slice(&audit_bytes).unwrap();
    assert_eq!(audit["schema_version"], 3);
    assert_eq!(audit["sources"][0]["source"], "S1");
    assert_eq!(audit["sources"][0]["availability"], "available");
    assert_eq!(audit["sources"][1]["availability"], "unavailable");
    assert!(audit["sources"][1]["missing_reason"].as_str().is_some());
    for forbidden in [
        "prefix_sequence",
        "head_hash",
        "snapshot_digest",
        "identity",
        "raw_prefix_head",
    ] {
        assert!(audit.get(forbidden).is_none());
        assert!(audit["projection"].get(forbidden).is_none());
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn product_export_aliases_short_unicode_and_path_like_identifiers() {
    let key = foundry_serve::privacy::ExportKey::generate().unwrap();
    let order_id = "u/é/7";
    let actor = "s/会/7";
    let path_marker = "/tmp/π/session/7";
    let row = foundry_ledger::LedgerRow {
        seq: 1,
        order_id: order_id.into(),
        from_state: "received".into(),
        to_state: "queued".into(),
        event: "Queued".into(),
        actor: actor.into(),
        intent: path_marker.into(),
        at: "2026-10-16T00:00:00Z".into(),
        prev_hash: "GENESIS".into(),
        hash: "synthetic-head".into(),
        event_json: None,
    };
    let document = foundry_serve::privacy::product_export("i/ø/1", &[row], "ctx", &key);
    let encoded = serde_json::to_string(&document).unwrap();
    for raw in ["i/ø/1", order_id, actor, path_marker] {
        assert!(
            !encoded.contains(raw),
            "raw identifier escaped export projection"
        );
    }
    assert!(document.rows[0].order.starts_with("anon-"));
    assert!(document.rows[0].actor.starts_with("anon-"));
}

#[test]
fn scoped_export_and_audit_require_their_own_capability_and_hide_foreign_rows() {
    let root = std::env::temp_dir().join(format!(
        "foundry-b1-scoped-export-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("product.sqlite");
    let now = chrono::Utc::now();
    let source_a = ScopeSourceManifest {
        connector_id: "gh-a".into(),
        source_instance_id: "source-a".into(),
        generation: 1,
        identity: RepositoryIdentity::canonicalized(
            "acme",
            "github",
            "https://api.github.com",
            "same-repo-id",
        )
        .unwrap(),
        api_owner: Some("acme".into()),
        api_repo: Some("repo-a".into()),
        legacy_repository_locator: "same-repo-id".into(),
        scope_query: "open+foundry:make".into(),
        enabled: true,
    };
    let source_b = ScopeSourceManifest {
        connector_id: "gh-b".into(),
        source_instance_id: "source-b".into(),
        generation: 1,
        // Same stable numeric repository ID at a different origin is a
        // different source and cannot inherit source-a grants.
        identity: RepositoryIdentity::canonicalized(
            "acme",
            "github",
            "https://enterprise.example",
            "same-repo-id",
        )
        .unwrap(),
        api_owner: Some("acme".into()),
        api_repo: Some("repo-b".into()),
        legacy_repository_locator: "same-repo-id".into(),
        scope_query: "open+foundry:make".into(),
        enabled: true,
    };
    let config = ProductConfig {
        installation_id: "private-install-canary".into(),
        database_id: "private-db-canary".into(),
    };
    let owner_phc =
        foundry_ledger::product_identity::ProductPasswordKdf::hash("owner-export-password")
            .unwrap();
    let mut store = ProductStore::open(&db, config).unwrap();
    store
        .bootstrap_owner_scoped(
            "owner",
            &owner_phc,
            &[source_a.clone(), source_b.clone()],
            &[ScopeGrantManifest {
                actor_id: "owner".into(),
                source_instance_id: "source-a".into(),
                capabilities: vec![
                    ProductCapability::ReadOrder,
                    ProductCapability::ExportEvidence,
                    ProductCapability::ReadOperationalAudit,
                ],
            }],
        )
        .unwrap();
    store
        .admit_order(
            "own-order-canary",
            "gh-a",
            "same-repo-id",
            "issue:1",
            "dep-bump",
            "own payload",
            now,
        )
        .unwrap();
    store
        .admit_order(
            "own-order-canary-2",
            "gh-a",
            "same-repo-id",
            "issue:2",
            "dep-bump",
            "own payload 2",
            now,
        )
        .unwrap();
    store
        .admit_order(
            "foreign-order-canary",
            "gh-b",
            "same-repo-id",
            "issue:1",
            "dep-bump",
            "foreign payload",
            now,
        )
        .unwrap();
    for index in 0..120 {
        store
            .admit_order(
                &format!("foreign-order-{index:03}"),
                "gh-b",
                "same-repo-id",
                &format!("issue:{}", index + 3),
                "dep-bump",
                "foreign payload",
                now,
            )
            .unwrap();
    }
    let owner = store.login("owner", "owner-export-password", now).unwrap();
    let owner_bearer = owner.bearer().to_owned();
    let owner_digest = owner.digest().to_owned();
    let viewer_phc =
        foundry_ledger::product_identity::ProductPasswordKdf::hash("viewer-export-password")
            .unwrap();
    store
        .create_account(&owner_digest, "reader", "approver", &viewer_phc, now)
        .unwrap();
    store
        .change_product_scope(
            &owner_digest,
            "reader",
            "source-a",
            ProductCapability::ReadOrder,
            foundry_ledger::product_scope::ScopeMutation::Grant,
            0,
            "reader-order",
            now,
        )
        .unwrap();
    let reader = store
        .login("reader", "viewer-export-password", now)
        .unwrap();
    let reader_bearer = reader.bearer().to_owned();

    let first = store
        .scoped_evidence_snapshot(&owner_digest, ProductCapability::ExportEvidence, 100, now)
        .unwrap();
    let key = foundry_serve::privacy::ExportKey::generate().unwrap();
    let before =
        foundry_serve::privacy::scoped_product_export(&first, "fixed-test-context", &key).unwrap();
    // Add another foreign repository row. Since the projection filters and
    // hashes only authorized rows, it must not disclose chain gaps/head changes.
    store
        .admit_order(
            "foreign-order-2",
            "gh-b",
            "same-repo-id",
            "issue:2",
            "dep-bump",
            "another foreign payload",
            now,
        )
        .unwrap();
    let second = store
        .scoped_evidence_snapshot(&owner_digest, ProductCapability::ExportEvidence, 100, now)
        .unwrap();
    let after =
        foundry_serve::privacy::scoped_product_export(&second, "fixed-test-context", &key).unwrap();
    assert_eq!(
        before, after,
        "foreign rows and global chain changes do not alter visible projection"
    );
    assert_eq!(
        before
            .rows
            .iter()
            .map(|row| row.ordinal)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let first_page = store
        .scoped_evidence_snapshot(&owner_digest, ProductCapability::ExportEvidence, 1, now)
        .unwrap();
    assert!(
        first_page.has_more,
        "has_more reflects only authorized orders"
    );
    assert!(
        first_page
            .rows
            .iter()
            .all(|row| row.order_id != "foreign-order-canary")
    );
    // Retire/rebind the connector. Existing explicit grants can still read the
    // precisely bound historical source, but cannot transfer to the new source.
    store
        .configure_scoped_source(
            &owner_digest,
            "gh-a",
            source_a.identity.clone(),
            "acme",
            "repo-a",
            "same-repo-id",
            "open+foundry:make",
            true,
            1,
            "rebind-gh-a",
            now,
        )
        .unwrap();
    assert_eq!(
        store
            .scoped_evidence_snapshot(&owner_digest, ProductCapability::ExportEvidence, 100, now)
            .unwrap()
            .rows
            .len(),
        2,
        "historical evidence remains tied to its old granted source instance"
    );
    assert!(
        store
            .read_scoped_order(&owner_digest, "foreign-order-canary", now)
            .is_err(),
        "Owner role alone does not grant repository data access"
    );

    let app = product_router(ProductHttpState::new(store));
    for path in ["/api/product/export", "/api/product/audit"] {
        let (status, body) = request(&app, path, &owner_bearer);
        assert_eq!(status, StatusCode::OK);
        let body = String::from_utf8(body).unwrap();
        assert!(body.contains("anon-"));
        for canary in [
            "own-order-canary",
            "foreign-order-canary",
            "private-install-canary",
            "private-db-canary",
        ] {
            assert!(!body.contains(canary));
        }
        for global in [
            "prev_hash",
            "raw_prefix_head",
            "prefix_sequence",
            "head_hash",
        ] {
            assert!(!body.contains(global));
        }
    }
    for path in ["/api/product/export?limit=1", "/api/product/audit?limit=1"] {
        let (status, bytes) = request(&app, path, &owner_bearer);
        assert_eq!(status, StatusCode::OK);
        let projection: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let projection = if path.contains("audit") {
            &projection["projection"]
        } else {
            &projection
        };
        assert_eq!(projection["coverage"], "page");
        assert!(projection["has_more"].as_bool().unwrap());
        assert_eq!(projection["rows"].as_array().unwrap().len(), 1);
        assert_eq!(projection["rows"][0]["ordinal"], 1);
        assert!(!String::from_utf8_lossy(&bytes).contains("foreign-order-canary"));
    }
    assert_eq!(
        request(&app, "/api/product/export", &reader_bearer).0,
        StatusCode::FORBIDDEN,
        "ReadOrder is not ExportEvidence"
    );
    assert_eq!(
        request(&app, "/api/product/audit", &reader_bearer).0,
        StatusCode::FORBIDDEN,
        "ReadOrder is not ReadOperationalAudit"
    );

    let export_grant = {
        let mut store = ProductStore::open(
            &db,
            ProductConfig {
                installation_id: "private-install-canary".into(),
                database_id: "private-db-canary".into(),
            },
        )
        .unwrap();
        store
            .change_product_scope(
                &owner_digest,
                "reader",
                "source-a",
                ProductCapability::ExportEvidence,
                foundry_ledger::product_scope::ScopeMutation::Grant,
                1,
                "reader-export",
                now,
            )
            .unwrap()
    };
    assert_eq!(export_grant.scope_revision, 2);
    // Router holds its own SQLite handle and sees the committed grant.
    assert_eq!(
        request(&app, "/api/product/export", &reader_bearer).0,
        StatusCode::OK
    );
    let mut admin = ProductStore::open(
        &db,
        ProductConfig {
            installation_id: "private-install-canary".into(),
            database_id: "private-db-canary".into(),
        },
    )
    .unwrap();
    admin
        .change_product_scope(
            &owner_digest,
            "reader",
            "source-a",
            ProductCapability::ExportEvidence,
            foundry_ledger::product_scope::ScopeMutation::Revoke,
            2,
            "reader-export-revoke",
            now,
        )
        .unwrap();
    assert_eq!(
        request(&app, "/api/product/export", &reader_bearer).0,
        StatusCode::FORBIDDEN,
        "same session is denied after a committed capability revoke"
    );
    admin
        .change_product_scope(
            &owner_digest,
            "reader",
            "source-a",
            ProductCapability::ReadOperationalAudit,
            foundry_ledger::product_scope::ScopeMutation::Grant,
            3,
            "reader-audit",
            now,
        )
        .unwrap();
    assert_eq!(
        request(&app, "/api/product/audit", &reader_bearer).0,
        StatusCode::OK
    );
    admin
        .change_product_scope(
            &owner_digest,
            "reader",
            "source-a",
            ProductCapability::ReadOperationalAudit,
            foundry_ledger::product_scope::ScopeMutation::Revoke,
            4,
            "reader-audit-revoke",
            now,
        )
        .unwrap();
    assert_eq!(
        request(&app, "/api/product/audit", &reader_bearer).0,
        StatusCode::FORBIDDEN
    );
    let (status, _) = request_method(
        &app,
        "POST",
        "/api/product/accounts/reader/authority",
        &owner_bearer,
        r#"{"role":"approver","enabled":false}"#,
    );
    assert_eq!(status, StatusCode::NO_CONTENT);
    for path in ["/api/product/export", "/api/product/audit"] {
        assert_eq!(
            request(&app, path, &reader_bearer).0,
            StatusCode::FORBIDDEN,
            "disabled account cannot retain export/audit access"
        );
    }
    assert_eq!(
        request_method(&app, "POST", "/api/product/logout", &owner_bearer, "").0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(&app, "/api/product/export", &owner_bearer).0,
        StatusCode::FORBIDDEN,
        "logout revokes export access for the bearer"
    );
    drop(admin);
    drop(owner);
    drop(reader);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
    }
    let _ = std::fs::remove_dir_all(root);
}
