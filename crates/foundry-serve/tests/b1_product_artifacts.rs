use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use foundry_ledger::{
    ProductCapability, ProductConfig, ProductDelivery, ProductPollCursor, ProductServiceAction,
    ProductServiceContext, ProductServiceGrant, ProductStore, RepositoryIdentity,
    ScopeGrantManifest, ScopeMutation, ScopeSourceManifest,
};
use foundry_line::{PackReviewHost, StageExecutor, StageOutput, StageTask};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use tower::ServiceExt;

static NEXT: AtomicU64 = AtomicU64::new(0);

fn root() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "foundry-b1-artifact-http-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

struct GapExecutor;
impl StageExecutor for GapExecutor {
    fn execute(&mut self, task: &StageTask) -> Result<StageOutput, String> {
        let path = task.workdir.join("product-artifact.log");
        std::fs::write(&path, b"genuine product host stage artifact\n")
            .map_err(|e| e.to_string())?;
        Ok(StageOutput {
            log_path: path.to_string_lossy().into_owned(),
            inspected_commit: (task.role == "verify").then(|| task.artifact_commit.clone()),
            verdict_category: (task.role == "lens").then(|| "intent_gap".into()),
            findings: task.role == "lens",
            ..Default::default()
        })
    }
}

fn config() -> ProductConfig {
    ProductConfig {
        installation_id: "artifact-install".into(),
        database_id: "artifact-db".into(),
    }
}

fn source() -> ScopeSourceManifest {
    ScopeSourceManifest {
        connector_id: "github-a".into(),
        source_instance_id: "artifact-source-v1".into(),
        generation: 1,
        identity: RepositoryIdentity::canonicalized(
            "acme",
            "github",
            "https://api.github.com",
            "42",
        )
        .unwrap(),
        api_owner: Some("acme".into()),
        api_repo: Some("repo".into()),
        legacy_repository_locator: "42".into(),
        scope_query: "open+foundry:make".into(),
        enabled: true,
    }
}

fn issue(now: chrono::DateTime<chrono::Utc>) -> ProductDelivery {
    let updated = now.to_rfc3339();
    let task_kind = "dep-bump";
    let payload = "synthetic test adapter payload";
    let version =
        foundry_ledger::product::delivery_version_digest("issue:7", &updated, task_kind, payload);
    ProductDelivery {
        delivery_id: foundry_ledger::product::github_delivery_id(
            "artifact-install",
            "github-a",
            "https://api.github.com",
            "42",
            "issue:7",
            &version,
        ),
        connector_id: "github-a".into(),
        repository_id: "42".into(),
        issue_id: "issue:7".into(),
        observed_updated_at: updated,
        version_digest: version,
        task_kind: task_kind.into(),
        payload: payload.into(),
    }
}

fn service_grants(source_id: &str) -> Vec<ProductServiceGrant> {
    [
        ProductServiceAction::IntakeRead,
        ProductServiceAction::AdmitDelivery,
        ProductServiceAction::AckDelivery,
        ProductServiceAction::DriveReview,
    ]
    .into_iter()
    .map(|action| ProductServiceGrant {
        source_instance_id: source_id.into(),
        action,
    })
    .collect()
}

fn app_request(method: &str, uri: &str, bearer: Option<&str>) -> Request<Body> {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(bearer) = bearer {
        request = request.header("authorization", format!("Bearer {bearer}"));
    }
    request.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn genuine_product_host_artifact_is_read_only_by_scoped_opaque_id_and_verified_bytes() {
    use foundry_exec::{Attempt, Epoch, ExecutionIntent, Generation, OrderId, RunId};
    let root = root();
    let db = root.join("product.sqlite");
    let now = chrono::Utc::now();
    let mut store = ProductStore::open(&db, config()).unwrap();
    let owner_phc =
        foundry_ledger::product_identity::ProductPasswordKdf::hash("artifact-owner-password")
            .unwrap();
    let source = source();
    store
        .bootstrap_owner_scoped(
            "owner",
            &owner_phc,
            std::slice::from_ref(&source),
            &[ScopeGrantManifest {
                actor_id: "owner".into(),
                source_instance_id: source.source_instance_id.clone(),
                capabilities: vec![ProductCapability::ReadArtifact],
            }],
        )
        .unwrap();
    let owner_session = store
        .login("owner", "artifact-owner-password", now)
        .unwrap();
    let owner_bearer = owner_session.bearer().to_owned();
    let owner_digest = owner_session.digest().to_owned();
    let reader_phc =
        foundry_ledger::product_identity::ProductPasswordKdf::hash("artifact-reader-password")
            .unwrap();
    store
        .create_account(
            &owner_digest,
            "owner-without-data",
            "owner",
            &reader_phc,
            now,
        )
        .unwrap();

    let service_receipt = store
        .manage_product_service(
            &owner_digest,
            "artifact-reviewer",
            &service_grants(&source.source_instance_id),
            true,
            now + chrono::Duration::hours(1),
            0,
            "artifact-service-config",
            now,
        )
        .unwrap();
    let credential = store
        .issue_product_service_credential(
            &owner_digest,
            "artifact-reviewer",
            service_receipt.revision,
            now + chrono::Duration::minutes(30),
            "artifact-service-credential",
            now,
        )
        .unwrap();
    let service: ProductServiceContext = store
        .authenticate_product_service(
            credential.expose_once_to_file_writer(),
            &source.connector_id,
            &source.identity.api_origin,
            &source.identity.stable_repository_id,
            now,
        )
        .unwrap();
    let delivery = issue(now);
    store
        .persist_intake_page_for_service(
            &service,
            &ProductPollCursor {
                connector_id: "github-a".into(),
                query_fingerprint: "artifact-test-query".into(),
                sweep_id: "artifact-test-sweep".into(),
                next_page: 1,
                page_ceiling: 1,
                updated_at: now.to_rfc3339(),
            },
            std::slice::from_ref(&delivery),
            &[],
            now,
        )
        .unwrap();
    store
        .admit_intake_delivery_for_service(&service, &delivery.delivery_id, "artifact-order", now)
        .unwrap();
    // This narrowly scoped test adapter selects synthetic dep-bump work. The
    // ProductStore still issues and validates the service-bound durable claim;
    // it never writes artifact metadata or bypasses stage registration.
    rusqlite::Connection::open(&db).unwrap().execute(
        "UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id='artifact-order' AND lifecycle=1 AND state='waiting_unsupported'", [],
    ).unwrap();
    let claim = match store
        .claim_product_work_for_service(
            &service,
            "artifact-worker",
            now,
            chrono::Duration::minutes(5),
        )
        .unwrap()
    {
        foundry_ledger::ProductWorkClaimResult::Claimed(claim) => claim,
        other => panic!("expected service-authorized synthetic claim, got {other:?}"),
    };
    let pack = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
    let mut host = PackReviewHost::with_product_store(
        pack,
        GapExecutor,
        store,
        root.join("artifacts"),
        root.join("sessions"),
        root.join("claims"),
    )
    .unwrap()
    .with_source_commit("67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0")
    .unwrap();
    let intent = ExecutionIntent {
        order: OrderId("artifact-order".into()),
        attempt: Attempt(1),
        run: RunId(1),
        activation: 1,
        generation: Generation(0),
        epoch: Epoch(1),
        external_ref: "review:artifact-order:1:base..result".into(),
        recorded_at: now,
    };
    let result = host.try_dispatch_product_work(&intent, &claim).unwrap();
    assert_eq!(
        result.exit_code, 0,
        "the genuine product host accepted the synthetic test output"
    );
    drop(host);

    let mut reopened = ProductStore::open(&db, config()).unwrap();
    reopened
        .configure_product_artifact_root(root.join("artifacts"))
        .unwrap();
    let mut conflicting_root = ProductStore::open(&db, config()).unwrap();
    assert!(
        conflicting_root
            .configure_product_artifact_root(root.join("different-artifacts"))
            .is_err(),
        "a second handle cannot silently select another artifact root"
    );
    drop(conflicting_root);
    let listed = reopened
        .list_product_artifacts(&owner_digest, "artifact-order", chrono::Utc::now())
        .unwrap();
    assert!(
        !listed.is_empty(),
        "host stage completion registered the output durably"
    );
    let artifact = listed[0].clone();
    assert_eq!(artifact.order_id, "artifact-order");
    assert_eq!(artifact.lifecycle, 1);
    let app = foundry_serve::http::product_router(foundry_serve::auth_http::ProductHttpState::new(
        reopened,
    ));

    let listing = app
        .clone()
        .oneshot(app_request(
            "GET",
            "/api/product/orders/artifact-order/artifacts",
            Some(&owner_bearer),
        ))
        .await
        .unwrap();
    assert_eq!(listing.status(), StatusCode::OK);
    let body = to_bytes(listing.into_body(), 1024 * 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["artifacts"][0]["artifact_id"], artifact.artifact_id);

    let path = format!(
        "/api/product/orders/artifact-order/artifacts/{}",
        artifact.artifact_id
    );
    let response = app
        .clone()
        .oneshot(app_request("GET", &path, Some(&owner_bearer)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-foundry-artifact-sha256"],
        artifact.sha256
    );
    assert_eq!(
        to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .as_ref(),
        b"genuine product host stage artifact\n"
    );

    // IDs are not bearer credentials. A valid Owner with no explicit
    // ReadArtifact grant and a caller without a session learn no artifact.
    let mut auth = ProductStore::open(&db, config()).unwrap();
    let no_grant = auth
        .login(
            "owner-without-data",
            "artifact-reader-password",
            chrono::Utc::now(),
        )
        .unwrap();
    let no_grant_bearer = no_grant.bearer().to_owned();
    let admin_digest = no_grant.digest().to_owned();
    let denied = app
        .clone()
        .oneshot(app_request("GET", &path, Some(&no_grant_bearer)))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::NOT_FOUND);
    let foreign_order = app
        .clone()
        .oneshot(app_request(
            "GET",
            &format!(
                "/api/product/orders/foreign-order/artifacts/{}",
                artifact.artifact_id
            ),
            Some(&owner_bearer),
        ))
        .await
        .unwrap();
    assert_eq!(foreign_order.status(), StatusCode::NOT_FOUND);
    let foreign_id = app.clone().oneshot(app_request("GET", "/api/product/orders/artifact-order/artifacts/ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", Some(&owner_bearer))).await.unwrap();
    assert_eq!(foreign_id.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        app.clone()
            .oneshot(app_request("GET", &path, None))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let traversal = app
        .clone()
        .oneshot(app_request(
            "GET",
            "/api/product/orders/artifact-order/artifacts/../../etc/passwd",
            Some(&owner_bearer),
        ))
        .await
        .unwrap();
    assert_eq!(traversal.status(), StatusCode::NOT_FOUND);

    // Revocation is checked on every read. Regranting the same source grants
    // fresh current historical-read authority; it does not alter the artifact.
    let grant = auth
        .list_product_scope_grants(
            &owner_digest,
            "owner",
            &source.source_instance_id,
            chrono::Utc::now(),
        )
        .unwrap()
        .into_iter()
        .find(|g| g.capability == ProductCapability::ReadArtifact)
        .unwrap();
    auth.change_product_scope(
        &owner_digest,
        "owner",
        &source.source_instance_id,
        ProductCapability::ReadArtifact,
        ScopeMutation::Revoke,
        grant.scope_revision,
        "artifact-revoke",
        chrono::Utc::now(),
    )
    .unwrap();
    drop(auth);
    let denied = app
        .clone()
        .oneshot(app_request("GET", &path, Some(&owner_bearer)))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::NOT_FOUND);
    let mut auth = ProductStore::open(&db, config()).unwrap();
    let grants = auth
        .list_product_scope_grants(
            &owner_digest,
            "owner",
            &source.source_instance_id,
            chrono::Utc::now(),
        )
        .unwrap();
    let revision = grants.first().unwrap().scope_revision;
    auth.change_product_scope(
        &admin_digest,
        "owner",
        &source.source_instance_id,
        ProductCapability::ReadArtifact,
        ScopeMutation::Grant,
        revision,
        "artifact-regrant",
        chrono::Utc::now(),
    )
    .unwrap();
    drop(auth);
    let restored = app
        .clone()
        .oneshot(app_request("GET", &path, Some(&owner_bearer)))
        .await
        .unwrap();
    assert_eq!(restored.status(), StatusCode::OK);
    let mut auth = ProductStore::open(&db, config()).unwrap();

    // A source rebind creates a different source instance. The original
    // explicit historical grant remains usable for the retired source, while
    // a grant on the new source does not confer access to its old artifact.
    let source_revision = auth
        .list_scoped_sources(&owner_digest, chrono::Utc::now())
        .unwrap()
        .into_iter()
        .find(|view| view.source.source_instance_id == source.source_instance_id)
        .unwrap()
        .connector_revision;
    let rebound = auth
        .configure_scoped_source(
            &owner_digest,
            "github-a",
            RepositoryIdentity::canonicalized("acme", "github", "https://api.github.example", "42")
                .unwrap(),
            "acme",
            "renamed",
            "42",
            "open+foundry:make",
            true,
            source_revision,
            "artifact-source-rebind",
            chrono::Utc::now(),
        )
        .unwrap();
    auth.change_product_scope(
        &owner_digest,
        "owner-without-data",
        &rebound.source_instance_id,
        ProductCapability::ReadArtifact,
        ScopeMutation::Grant,
        0,
        "grant-new-source-artifacts",
        chrono::Utc::now(),
    )
    .unwrap();
    drop(auth);
    assert_eq!(
        app.clone()
            .oneshot(app_request("GET", &path, Some(&owner_bearer)))
            .await
            .unwrap()
            .status(),
        StatusCode::OK,
        "historical read follows the original explicit grant even after source retirement"
    );
    assert_eq!(
        app.clone()
            .oneshot(app_request("GET", &path, Some(&no_grant_bearer)))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND,
        "a grant on the rebound source does not inherit historical artifacts"
    );

    // The route verifies bytes before responding and never returns filesystem
    // paths when content is missing or tampered.
    let conn = rusqlite::Connection::open(&db).unwrap();
    let relative: String = conn
        .query_row(
            "SELECT relative_path FROM product_artifacts_v1 WHERE artifact_id=?1",
            [&artifact.artifact_id],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn);
    let artifact_path = root.join("artifacts").join(relative);
    let original = std::fs::read(&artifact_path).unwrap();
    let clean = app
        .clone()
        .oneshot(app_request("GET", &path, Some(&owner_bearer)))
        .await
        .unwrap();
    assert_eq!(
        clean.status(),
        StatusCode::OK,
        "verified file baseline succeeds before byte corruption"
    );
    assert_eq!(
        to_bytes(clean.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .as_ref(),
        original.as_slice()
    );
    std::fs::write(&artifact_path, b"tampered bytes").unwrap();
    let tampered = app
        .clone()
        .oneshot(app_request("GET", &path, Some(&owner_bearer)))
        .await
        .unwrap();
    assert_eq!(tampered.status(), StatusCode::NOT_FOUND);
    let text = to_bytes(tampered.into_body(), 1024).await.unwrap();
    assert!(!String::from_utf8_lossy(&text).contains(root.to_string_lossy().as_ref()));
    std::fs::write(&artifact_path, original).unwrap();
    let clean = app
        .clone()
        .oneshot(app_request("GET", &path, Some(&owner_bearer)))
        .await
        .unwrap();
    assert_eq!(
        clean.status(),
        StatusCode::OK,
        "byte corruption is restored before the next mutation"
    );
    assert_eq!(
        to_bytes(clean.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .as_ref(),
        b"genuine product host stage artifact\n"
    );
    let clean = app
        .clone()
        .oneshot(app_request("GET", &path, Some(&owner_bearer)))
        .await
        .unwrap();
    assert_eq!(
        clean.status(),
        StatusCode::OK,
        "verified file baseline succeeds before missing-file corruption"
    );
    assert_eq!(
        to_bytes(clean.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .as_ref(),
        b"genuine product host stage artifact\n"
    );
    std::fs::remove_file(&artifact_path).unwrap();
    assert_eq!(
        app.clone()
            .oneshot(app_request("GET", &path, Some(&owner_bearer)))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND,
        "missing files are not returned as empty or partial success"
    );
    std::fs::write(&artifact_path, b"genuine product host stage artifact\n").unwrap();
    let clean = app
        .clone()
        .oneshot(app_request("GET", &path, Some(&owner_bearer)))
        .await
        .unwrap();
    assert_eq!(
        clean.status(),
        StatusCode::OK,
        "missing-file corruption is restored to a verified baseline"
    );
    assert_eq!(
        to_bytes(clean.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .as_ref(),
        b"genuine product host stage artifact\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        std::fs::remove_file(&artifact_path).unwrap();
        let outside = root.join("outside-secret");
        std::fs::write(&outside, b"outside root secret").unwrap();
        symlink(&outside, &artifact_path).unwrap();
        assert_eq!(
            app.clone()
                .oneshot(app_request("GET", &path, Some(&owner_bearer)))
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND,
            "a symlink escape is refused"
        );
        std::fs::remove_file(&artifact_path).unwrap();
        std::fs::write(&artifact_path, b"genuine product host stage artifact\n").unwrap();
        let clean = app
            .clone()
            .oneshot(app_request("GET", &path, Some(&owner_bearer)))
            .await
            .unwrap();
        assert_eq!(
            clean.status(),
            StatusCode::OK,
            "symlink replacement is removed before subsequent metadata mutations"
        );
        assert_eq!(
            to_bytes(clean.into_body(), 1024 * 1024)
                .await
                .unwrap()
                .as_ref(),
            b"genuine product host stage artifact\n"
        );
    }
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute("DROP TRIGGER product_artifact_no_update", [])
        .unwrap();
    let baseline: (String, String, String) = conn.query_row(
        "SELECT relative_path,artifact_sha256,provenance_digest FROM product_artifacts_v1 WHERE artifact_id=?1",
        [&artifact.artifact_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).unwrap();
    drop(conn);
    let assert_baseline = async || {
        let response = app
            .clone()
            .oneshot(app_request("GET", &path, Some(&owner_bearer)))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "clean baseline must read before each independent mutation"
        );
        assert_eq!(
            to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap()
                .as_ref(),
            b"genuine product host stage artifact\n"
        );
    };
    let assert_denied_without_path = async |label: &str| {
        let response = app
            .clone()
            .oneshot(app_request("GET", &path, Some(&owner_bearer)))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "{label} corruption must be rejected"
        );
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert!(
            !String::from_utf8_lossy(&body).contains(root.to_string_lossy().as_ref()),
            "{label} error must not disclose a filesystem path"
        );
    };

    // Relative path corruption starts from and returns to a verified baseline.
    assert_baseline().await;
    let conn = rusqlite::Connection::open(&db).unwrap();
    assert_eq!(conn.execute("UPDATE product_artifacts_v1 SET relative_path='../outside-secret' WHERE artifact_id=?1", [&artifact.artifact_id]).unwrap(), 1);
    drop(conn);
    assert_denied_without_path("relative path").await;
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "UPDATE product_artifacts_v1 SET relative_path=?1 WHERE artifact_id=?2",
        rusqlite::params![baseline.0, artifact.artifact_id],
    )
    .unwrap();
    drop(conn);
    assert_baseline().await;

    // Durable file digest is mutated alone, after restoring the path baseline.
    let conn = rusqlite::Connection::open(&db).unwrap();
    assert_eq!(
        conn.execute(
            "UPDATE product_artifacts_v1 SET artifact_sha256=?1 WHERE artifact_id=?2",
            rusqlite::params!["0".repeat(64), artifact.artifact_id]
        )
        .unwrap(),
        1
    );
    drop(conn);
    assert_denied_without_path("durable digest").await;
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "UPDATE product_artifacts_v1 SET artifact_sha256=?1 WHERE artifact_id=?2",
        rusqlite::params![baseline.1, artifact.artifact_id],
    )
    .unwrap();
    drop(conn);
    assert_baseline().await;

    // Immutable provenance is independently checked, not masked by the
    // preceding path or digest corruption.
    let conn = rusqlite::Connection::open(&db).unwrap();
    assert_eq!(
        conn.execute(
            "UPDATE product_artifacts_v1 SET provenance_digest=?1 WHERE artifact_id=?2",
            rusqlite::params!["0".repeat(64), artifact.artifact_id]
        )
        .unwrap(),
        1
    );
    drop(conn);
    assert_denied_without_path("provenance").await;
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "UPDATE product_artifacts_v1 SET provenance_digest=?1 WHERE artifact_id=?2",
        rusqlite::params![baseline.2, artifact.artifact_id],
    )
    .unwrap();
    drop(conn);
    assert_baseline().await;

    // Reopening the ProductStore cannot silently adopt a replacement root at
    // the same configured path; the installation retained the pinned identity.
    let artifact_root = root.join("artifacts");
    std::fs::rename(&artifact_root, root.join("artifacts-original")).unwrap();
    std::fs::create_dir(&artifact_root).unwrap();
    let mut root_replaced = ProductStore::open(&db, config()).unwrap();
    assert!(
        root_replaced
            .configure_product_artifact_root(&artifact_root)
            .is_err()
    );
    std::fs::remove_dir_all(root).unwrap();
}
