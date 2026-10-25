// Gate3 T4 external compile probes for the cfg-gated support API (documented,
// not run in this batch): with a scratch probe crate whose only source is
// `use foundry_ledger::{AdmissionReceipt, P2ReviewPreparation, ProductServiceContext, ProductStore}; fn probe(s:&mut ProductStore,c:&ProductServiceContext,a:&AdmissionReceipt,d:&str,t:P2ReviewPreparation){ let _=s.prepare_p2_review_fixture(c,a,d,t); } fn main(){}`
// and whose Cargo.toml is
// `[package]\nname="p2-support-probe"\nversion="0.0.0"\nedition="2024"\n[features]\ndefault=[]\nsupport=["foundry-ledger/p2-test-support"]\n[dependencies]\nfoundry-ledger={path="/home/cosmo/foundry/crates/foundry-ledger",default-features=false}`:
// `cargo check --manifest-path /home/cosmo/.hermes/cache/scratch/p2-support-probe/Cargo.toml --locked --offline --no-default-features`
// must fail with both E0432 (unresolved import `P2ReviewPreparation`) and
// E0599 (no method `prepare_p2_review_fixture`); the separate
// `cargo check --manifest-path /home/cosmo/.hermes/cache/scratch/p2-support-probe/Cargo.toml --locked --offline --features support`
// must compile as the enabled-feature positive. Adapter unreachability on
// normal HTTP/CLI/startup is evidenced by
// crates/foundry-serve/src/http.rs product route composition,
// crates/foundry/src/main.rs command dispatch/ProductStore constructors, and
// crates/foundry-ledger/src/product/result.rs feature-gated API declaration:
// those normal paths do not call `prepare_p2_review_fixture`. The probe crate
// uses a path dependency to this workspace's `crates/foundry-ledger`; this
// feature does not add a product HTTP route, CLI command, or startup adapter.
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

#[cfg(feature = "p2-test-support")]
#[path = "../../foundry-line/tests/support/product_result_fixture.rs"]
mod product_result_fixture;

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

#[cfg(feature = "p2-test-support")]
async fn loopback_request(
    address: std::net::SocketAddr,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: &[u8],
) -> (u16, Vec<u8>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    if !body.is_empty() {
        request.push_str("Content-Type: application/json\r\n");
    }
    if let Some(bearer) = bearer {
        request.push_str(&format!("Authorization: Bearer {bearer}\r\n"));
    }
    if path.ends_with("/approve") {
        request.push_str("Idempotency-Key: socket-approval\r\n");
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("HTTP response header terminator");
    let headers = std::str::from_utf8(&response[..split]).unwrap();
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap()
        .parse::<u16>()
        .unwrap();
    let raw_body = &response[split + 4..];
    let chunked = headers.lines().any(|line| {
        line.to_ascii_lowercase()
            .starts_with("transfer-encoding: chunked")
    });
    if !chunked {
        return (status, raw_body.to_vec());
    }
    let mut decoded = Vec::new();
    let mut offset = 0;
    loop {
        let line_end = raw_body[offset..]
            .windows(2)
            .position(|window| window == b"\r\n")
            .expect("chunk size line")
            + offset;
        let length = usize::from_str_radix(
            std::str::from_utf8(&raw_body[offset..line_end]).unwrap(),
            16,
        )
        .unwrap();
        offset = line_end + 2;
        if length == 0 {
            break;
        }
        decoded.extend_from_slice(&raw_body[offset..offset + length]);
        offset += length + 2;
    }
    (status, decoded)
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

#[cfg(feature = "p2-test-support")]
#[tokio::test]
async fn g_final_result_metadata_and_artifact_routes_check_source_and_stored_evidence() {
    use product_result_fixture::{
        FixtureClock, FixtureIdentity, genuine_host_done, genuine_result_finalized,
    };

    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: "result-read-install".into(),
            database_id: "result-read-db".into(),
        },
        "result-read-source",
        "result-read-order",
    );
    let mut fixture = genuine_result_finalized(genuine_host_done(identity));
    fixture.assert_genuine_private_handoff();
    fixture.host.take();
    fixture.result_store.take();

    let mut store = ProductStore::open_with_clock(
        &fixture.db_path,
        fixture.config.clone(),
        std::sync::Arc::new(FixtureClock(fixture.now)),
    )
    .unwrap();
    store
        .configure_product_artifact_root(&fixture.artifact_root)
        .unwrap();
    // Resolve the immutable result identity from the genuine finalizer row;
    // no result or tuple is inserted by this consumer test.
    let result_id: String = rusqlite::Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT result_id FROM product_result_bindings_v1 WHERE order_id=?1",
            [&fixture.identity.order_id],
            |row| row.get(0),
        )
        .unwrap();
    let info = store
        .read_product_result(
            &fixture.human_session_digest,
            &fixture.identity.order_id,
            &result_id,
        )
        .unwrap();
    assert_eq!(info.result_id, result_id);
    assert!(!info.artifact_ids.is_empty());
    let artifact_id = info.final_artifact_id.clone();
    assert!(info.artifact_ids.contains(&artifact_id));
    drop(store);

    let mut http_store = ProductStore::open_with_clock(
        &fixture.db_path,
        fixture.config.clone(),
        std::sync::Arc::new(FixtureClock(fixture.now)),
    )
    .unwrap();
    http_store
        .configure_product_artifact_root(&fixture.artifact_root)
        .unwrap();
    let state = foundry_serve::auth_http::ProductHttpState::new(http_store);
    let app = foundry_serve::http::product_router(state.clone());
    let bearer = std::fs::read_to_string(&fixture.human_session_bearer_path).unwrap();
    let metadata_path = format!(
        "/api/product/orders/{}/results/{}",
        fixture.identity.order_id, result_id
    );
    let artifact_path = format!("{metadata_path}/artifacts/{artifact_id}");

    let metadata = app
        .clone()
        .oneshot(app_request("GET", &metadata_path, Some(&bearer)))
        .await
        .unwrap();
    assert_eq!(metadata.status(), StatusCode::OK);
    assert_eq!(
        metadata.headers()[axum::http::header::CACHE_CONTROL],
        "no-store"
    );
    let metadata_bytes = to_bytes(metadata.into_body(), 1024 * 1024).await.unwrap();
    let metadata_json: serde_json::Value = serde_json::from_slice(&metadata_bytes).unwrap();
    assert_eq!(metadata_json["result_id"], result_id);
    assert_eq!(metadata_json["tuple_digest"], info.tuple_digest);
    assert!(metadata_json["binding_digest"].as_str().unwrap().len() == 64);

    let artifact = app
        .clone()
        .oneshot(app_request("GET", &artifact_path, Some(&bearer)))
        .await
        .unwrap();
    assert_eq!(artifact.status(), StatusCode::OK);
    assert_eq!(
        artifact.headers()[axum::http::header::CACHE_CONTROL],
        "no-store"
    );
    assert_eq!(artifact.headers()["x-foundry-result-id"], result_id);
    assert_eq!(artifact.headers()["x-foundry-artifact-id"], artifact_id);
    assert_eq!(
        artifact.headers()["x-foundry-tuple-digest"],
        info.tuple_digest
    );
    let original_bytes = to_bytes(artifact.into_body(), 1024 * 1024)
        .await
        .unwrap()
        .to_vec();
    assert!(!original_bytes.is_empty());

    let unauthenticated = app
        .clone()
        .oneshot(app_request("GET", &artifact_path, None))
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    for (uri, expected) in [
        (
            format!("/api/product/orders/foreign-order/results/{result_id}"),
            StatusCode::NOT_FOUND,
        ),
        (
            format!(
                "/api/product/orders/{}/results/{}",
                fixture.identity.order_id,
                "f".repeat(64)
            ),
            StatusCode::NOT_FOUND,
        ),
        (
            format!("{metadata_path}/artifacts/{}", "e".repeat(64)),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(app_request("GET", &uri, Some(&bearer)))
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{uri}");
    }

    // Retire/rebind the source after G-final. The old explicit grant remains
    // readable, while a reader granted only on the new source cannot inherit
    // the historical result or artifact.
    let before_publication: (i64, i64, i64) = rusqlite::Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM product_token_consumptions_v1),(SELECT count(*) FROM product_outcomes_v1),(SELECT count(*) FROM ledger WHERE to_state='publishing')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let mut authority = ProductStore::open_with_clock(
        &fixture.db_path,
        fixture.config.clone(),
        std::sync::Arc::new(FixtureClock(fixture.now)),
    )
    .unwrap();
    let connector_revision = authority
        .list_scoped_sources(&fixture.owner_session_digest, fixture.now)
        .unwrap()
        .into_iter()
        .find(|view| view.source.source_instance_id == fixture.identity.source_instance_id)
        .unwrap()
        .connector_revision;
    let rebound = authority
        .configure_scoped_source(
            &fixture.owner_session_digest,
            &fixture.identity.connector_id,
            RepositoryIdentity::canonicalized(
                fixture.identity.repository_owner.clone(),
                "github",
                &fixture.identity.api_origin,
                &fixture.identity.repository_id,
            )
            .unwrap(),
            &fixture.identity.repository_owner,
            "renamed-after-finalization",
            &fixture.identity.repository_id,
            "open+foundry:make",
            true,
            connector_revision,
            "retire-g-final-source",
            fixture.now,
        )
        .unwrap();
    authority
        .create_account(
            &fixture.owner_session_digest,
            "new-source-only-reader",
            "approver",
            &foundry_ledger::product_identity::ProductPasswordKdf::hash("new-source-only-password")
                .unwrap(),
            fixture.now,
        )
        .unwrap();
    authority
        .change_product_scope(
            &fixture.owner_session_digest,
            "new-source-only-reader",
            &rebound.source_instance_id,
            ProductCapability::ReadArtifact,
            ScopeMutation::Grant,
            0,
            "grant-only-new-source",
            fixture.now,
        )
        .unwrap();
    let new_reader = authority
        .login(
            "new-source-only-reader",
            "new-source-only-password",
            fixture.now,
        )
        .unwrap()
        .bearer()
        .to_owned();
    drop(authority);
    let retired_old_read = app
        .clone()
        .oneshot(app_request("GET", &artifact_path, Some(&bearer)))
        .await
        .unwrap();
    assert_eq!(retired_old_read.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(retired_old_read.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .as_ref(),
        original_bytes.as_slice()
    );
    let inherited = app
        .clone()
        .oneshot(app_request("GET", &artifact_path, Some(&new_reader)))
        .await
        .unwrap();
    assert_eq!(inherited.status(), StatusCode::NOT_FOUND);
    let foreign_metadata = app
        .clone()
        .oneshot(app_request(
            "GET",
            "/api/product/orders/foreign-order/results/0000000000000000000000000000000000000000000000000000000000000000",
            Some(&bearer),
        ))
        .await
        .unwrap();
    assert_eq!(foreign_metadata.status(), StatusCode::NOT_FOUND);
    let foreign_metadata_body = to_bytes(foreign_metadata.into_body(), 1024).await.unwrap();
    let new_source_metadata = app
        .clone()
        .oneshot(app_request("GET", &metadata_path, Some(&new_reader)))
        .await
        .unwrap();
    assert_eq!(new_source_metadata.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        to_bytes(new_source_metadata.into_body(), 1024)
            .await
            .unwrap(),
        foreign_metadata_body,
        "new-source-only metadata reader gets the same 404 body as a foreign order"
    );
    let after_publication: (i64, i64, i64) = rusqlite::Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM product_token_consumptions_v1),(SELECT count(*) FROM product_outcomes_v1),(SELECT count(*) FROM ledger WHERE to_state='publishing')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(after_publication, before_publication);

    // A real missing artifact-root capability is service unavailable, not an
    // empty successful response or a path-bearing diagnostic.
    let unavailable_app =
        foundry_serve::http::product_router(foundry_serve::auth_http::ProductHttpState::new(
            ProductStore::open_with_clock(
                &fixture.db_path,
                fixture.config.clone(),
                std::sync::Arc::new(FixtureClock(fixture.now)),
            )
            .unwrap(),
        ));
    let unavailable = unavailable_app
        .oneshot(app_request("GET", &artifact_path, Some(&bearer)))
        .await
        .unwrap();
    assert_eq!(unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);
    let unavailable_body = to_bytes(unavailable.into_body(), 1024).await.unwrap();
    assert!(
        !String::from_utf8_lossy(&unavailable_body)
            .contains(fixture.root.to_string_lossy().as_ref())
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let artifact_root = fixture.artifact_root.clone();
        let displaced_root = fixture.root.join("pinned-artifacts-original");
        let relative: String = rusqlite::Connection::open(&fixture.db_path)
            .unwrap()
            .query_row(
                "SELECT relative_path FROM product_artifacts_v1 WHERE artifact_id=?1",
                [&artifact_id],
                |row| row.get(0),
            )
            .unwrap();
        let outside = fixture.root.join("replacement-artifact-root");
        std::fs::create_dir_all(outside.join(std::path::Path::new(&relative).parent().unwrap()))
            .unwrap();
        std::fs::write(
            outside.join(&relative),
            b"outside-root-secret-must-not-leak",
        )
        .unwrap();
        std::fs::rename(&artifact_root, &displaced_root).unwrap();
        symlink(&outside, &artifact_root).unwrap();
        let replacement = app
            .clone()
            .oneshot(app_request("GET", &artifact_path, Some(&bearer)))
            .await
            .unwrap();
        if replacement.status() == StatusCode::OK {
            let bytes = to_bytes(replacement.into_body(), 1024 * 1024)
                .await
                .unwrap();
            assert_eq!(
                bytes.as_ref(),
                original_bytes.as_slice(),
                "root replacement must keep the pinned root"
            );
        } else {
            assert_eq!(replacement.status(), StatusCode::NOT_FOUND);
            let body = to_bytes(replacement.into_body(), 1024).await.unwrap();
            assert!(!String::from_utf8_lossy(&body).contains("outside-root-secret"));
        }
        std::fs::remove_file(&artifact_root).unwrap();
        std::fs::rename(&displaced_root, &artifact_root).unwrap();
        let restored = app
            .clone()
            .oneshot(app_request("GET", &artifact_path, Some(&bearer)))
            .await
            .unwrap();
        assert_eq!(restored.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(restored.into_body(), 1024 * 1024)
                .await
                .unwrap()
                .as_ref(),
            original_bytes.as_slice()
        );
    }

    // Each DB corruption starts from a successful metadata and byte baseline,
    // is rejected by both result routes, and is restored before the next cell.
    let conn = rusqlite::Connection::open(&fixture.db_path).unwrap();
    conn.execute("DROP TRIGGER product_tuples_no_update", [])
        .unwrap();
    let binding: (String, String, String, i64, i64, i64) = conn
        .query_row(
            "SELECT binding_digest,tuple_digest,host_result_digest,attempt,run_id,generation FROM product_result_bindings_v1 WHERE result_id=?1",
            [&result_id],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
        )
        .unwrap();
    let tuple: (String, String) = conn
        .query_row(
            "SELECT immutable_commit,artifact_location FROM artifact_tuples WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND generation=?4",
            rusqlite::params![fixture.identity.order_id,binding.3,binding.4,binding.5],
            |row| Ok((row.get(0)?,row.get(1)?)),
        )
        .unwrap();
    let host_json: String = conn
        .query_row(
            "SELECT result_json FROM product_review_host_results_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3",
            rusqlite::params![fixture.identity.order_id,binding.3,binding.4],
            |row| row.get(0),
        )
        .unwrap();
    drop(conn);
    let assert_clean = async || {
        let metadata = app
            .clone()
            .oneshot(app_request("GET", &metadata_path, Some(&bearer)))
            .await
            .unwrap();
        assert_eq!(
            metadata.status(),
            StatusCode::OK,
            "restored metadata baseline"
        );
        let artifact = app
            .clone()
            .oneshot(app_request("GET", &artifact_path, Some(&bearer)))
            .await
            .unwrap();
        assert_eq!(
            artifact.status(),
            StatusCode::OK,
            "restored artifact baseline"
        );
        assert_eq!(
            to_bytes(artifact.into_body(), 1024 * 1024)
                .await
                .unwrap()
                .as_ref(),
            original_bytes.as_slice()
        );
    };
    let assert_corrupt = async |label: &str| {
        let metadata = app
            .clone()
            .oneshot(app_request("GET", &metadata_path, Some(&bearer)))
            .await
            .unwrap();
        assert_eq!(
            metadata.status(),
            StatusCode::NOT_FOUND,
            "{label} metadata corruption"
        );
        let metadata_body = to_bytes(metadata.into_body(), 1024).await.unwrap();
        let artifact = app
            .clone()
            .oneshot(app_request("GET", &artifact_path, Some(&bearer)))
            .await
            .unwrap();
        assert_eq!(
            artifact.status(),
            StatusCode::NOT_FOUND,
            "{label} artifact corruption"
        );
        let body = to_bytes(artifact.into_body(), 1024).await.unwrap();
        assert!(
            !original_bytes.starts_with(body.as_ref()),
            "no artifact prefix is returned on {label}"
        );
        let foreign = app
            .clone()
            .oneshot(app_request("GET", &metadata_path, Some(&new_reader)))
            .await
            .unwrap();
        assert_eq!(
            foreign.status(),
            StatusCode::NOT_FOUND,
            "unauthorized plus {label} remains uniform"
        );
        assert_eq!(
            to_bytes(foreign.into_body(), 1024).await.unwrap(),
            metadata_body
        );
    };

    // The genuine fixture has no accepted hold. Keep this census explicit so
    // an empty acceptance/progress set is not presented as mutation coverage.
    let acceptance_counts: (i64, i64) = rusqlite::Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM product_review_hold_acceptances_v1),(SELECT count(*) FROM product_review_hold_progress_v1)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(acceptance_counts, (0, 0));

    for (label, sql, restore_sql) in [
        (
            "tuple",
            "UPDATE artifact_tuples SET immutable_commit='mutated-commit' WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND generation=?4",
            "UPDATE artifact_tuples SET immutable_commit=?1 WHERE order_id=?2 AND attempt=?3 AND run_id=?4 AND generation=?5",
        ),
        (
            "host result",
            "UPDATE product_review_host_results_v1 SET result_json='{}' WHERE order_id=?1 AND attempt=?2 AND run_id=?3",
            "UPDATE product_review_host_results_v1 SET result_json=?1 WHERE order_id=?2 AND attempt=?3 AND run_id=?4",
        ),
    ] {
        assert_clean().await;
        let conn = rusqlite::Connection::open(&fixture.db_path).unwrap();
        if label == "tuple" {
            assert_eq!(
                conn.execute(
                    sql,
                    rusqlite::params![fixture.identity.order_id, binding.3, binding.4, binding.5]
                )
                .unwrap(),
                1
            );
        } else {
            assert_eq!(
                conn.execute(
                    sql,
                    rusqlite::params![fixture.identity.order_id, binding.3, binding.4]
                )
                .unwrap(),
                1
            );
        }
        drop(conn);
        assert_corrupt(label).await;
        let conn = rusqlite::Connection::open(&fixture.db_path).unwrap();
        if label == "tuple" {
            conn.execute(
                restore_sql,
                rusqlite::params![
                    tuple.0,
                    fixture.identity.order_id,
                    binding.3,
                    binding.4,
                    binding.5
                ],
            )
            .unwrap();
        } else {
            conn.execute(
                restore_sql,
                rusqlite::params![host_json, fixture.identity.order_id, binding.3, binding.4],
            )
            .unwrap();
        }
        drop(conn);
    }
    assert_clean().await;

    // Binding corruption uses the real immutable-result row after disabling
    // only its test DB trigger, then restores the exact original digest.
    let conn = rusqlite::Connection::open(&fixture.db_path).unwrap();
    conn.execute("DROP TRIGGER product_result_bindings_no_update", [])
        .unwrap();
    assert_eq!(
        conn.execute(
            "UPDATE product_result_bindings_v1 SET binding_digest=?1 WHERE result_id=?2",
            rusqlite::params!["0".repeat(64), result_id]
        )
        .unwrap(),
        1
    );
    drop(conn);
    assert_corrupt("binding").await;
    let conn = rusqlite::Connection::open(&fixture.db_path).unwrap();
    conn.execute(
        "UPDATE product_result_bindings_v1 SET binding_digest=?1 WHERE result_id=?2",
        rusqlite::params![binding.0, result_id],
    )
    .unwrap();
    drop(conn);
    assert_clean().await;

    let artifact_row: (String, String, String) = rusqlite::Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT relative_path,artifact_sha256,provenance_digest FROM product_artifacts_v1 WHERE artifact_id=?1",
            [&artifact_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let artifact_file = fixture.artifact_root.join(&artifact_row.0);
    assert_clean().await;
    std::fs::write(&artifact_file, b"tampered result artifact bytes").unwrap();
    let bad_bytes = app
        .clone()
        .oneshot(app_request("GET", &artifact_path, Some(&bearer)))
        .await
        .unwrap();
    assert_eq!(
        bad_bytes.status(),
        StatusCode::NOT_FOUND,
        "file-byte mutation"
    );
    let bad_bytes_body = to_bytes(bad_bytes.into_body(), 1024).await.unwrap();
    assert!(
        !original_bytes.starts_with(bad_bytes_body.as_ref()),
        "file-byte rejection contains no artifact prefix"
    );
    let bad_metadata = app
        .clone()
        .oneshot(app_request("GET", &metadata_path, Some(&bearer)))
        .await
        .unwrap();
    assert_eq!(
        bad_metadata.status(),
        StatusCode::NOT_FOUND,
        "file-byte mutation metadata"
    );
    let bad_metadata_body = to_bytes(bad_metadata.into_body(), 1024).await.unwrap();
    assert!(
        !original_bytes.starts_with(bad_metadata_body.as_ref()),
        "file-byte metadata rejection contains no artifact prefix"
    );
    let foreign_bad_artifact = app
        .clone()
        .oneshot(app_request("GET", &artifact_path, Some(&new_reader)))
        .await
        .unwrap();
    assert_eq!(
        foreign_bad_artifact.status(),
        StatusCode::NOT_FOUND,
        "foreign reader file-byte response"
    );
    let foreign_bad_artifact_body = to_bytes(foreign_bad_artifact.into_body(), 1024)
        .await
        .unwrap();
    assert!(
        !original_bytes.starts_with(foreign_bad_artifact_body.as_ref()),
        "foreign response contains no artifact prefix"
    );
    assert_eq!(
        bad_bytes_body, foreign_bad_artifact_body,
        "file-byte corruption and foreign reader artifact responses are uniform"
    );
    let foreign_bad_metadata = app
        .clone()
        .oneshot(app_request("GET", &metadata_path, Some(&new_reader)))
        .await
        .unwrap();
    assert_eq!(
        foreign_bad_metadata.status(),
        StatusCode::NOT_FOUND,
        "foreign reader file-byte metadata response"
    );
    assert_eq!(
        bad_metadata_body,
        to_bytes(foreign_bad_metadata.into_body(), 1024)
            .await
            .unwrap(),
        "file-byte corruption and foreign reader metadata responses are uniform"
    );
    std::fs::write(&artifact_file, &original_bytes).unwrap();
    assert_clean().await;

    let conn = rusqlite::Connection::open(&fixture.db_path).unwrap();
    conn.execute("DROP TRIGGER product_artifact_no_update", [])
        .unwrap();
    drop(conn);
    for (label, sql, restore_sql, changed) in [
        (
            "artifact sha",
            "UPDATE product_artifacts_v1 SET artifact_sha256=?1 WHERE artifact_id=?2",
            "UPDATE product_artifacts_v1 SET artifact_sha256=?1 WHERE artifact_id=?2",
            "0".repeat(64),
        ),
        (
            "artifact provenance",
            "UPDATE product_artifacts_v1 SET provenance_digest=?1 WHERE artifact_id=?2",
            "UPDATE product_artifacts_v1 SET provenance_digest=?1 WHERE artifact_id=?2",
            "0".repeat(64),
        ),
        (
            "artifact path",
            "UPDATE product_artifacts_v1 SET relative_path=?1 WHERE artifact_id=?2",
            "UPDATE product_artifacts_v1 SET relative_path=?1 WHERE artifact_id=?2",
            "../outside-result-secret".to_owned(),
        ),
    ] {
        assert_clean().await;
        let conn = rusqlite::Connection::open(&fixture.db_path).unwrap();
        assert_eq!(
            conn.execute(sql, rusqlite::params![changed, artifact_id])
                .unwrap(),
            1
        );
        drop(conn);
        assert_corrupt(label).await;
        let conn = rusqlite::Connection::open(&fixture.db_path).unwrap();
        let baseline = match label {
            "artifact sha" => &artifact_row.1,
            "artifact provenance" => &artifact_row.2,
            _ => &artifact_row.0,
        };
        conn.execute(restore_sql, rusqlite::params![baseline, artifact_id])
            .unwrap();
        drop(conn);
        assert_clean().await;
    }

    #[cfg(unix)]
    {
        let relative: String = rusqlite::Connection::open(&fixture.db_path)
            .unwrap()
            .query_row(
                "SELECT relative_path FROM product_artifacts_v1 WHERE artifact_id=?1",
                [&artifact_id],
                |row| row.get(0),
            )
            .unwrap();
        let leaf = fixture.artifact_root.join(relative);
        std::fs::remove_file(&leaf).unwrap();
        match std::process::Command::new("mkfifo").arg(&leaf).status() {
            Ok(status) if status.success() => {
                let response = app
                    .clone()
                    .oneshot(app_request("GET", &artifact_path, Some(&bearer)))
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::NOT_FOUND,
                    "FIFO artifact leaf fails closed"
                );
                let body = to_bytes(response.into_body(), 1024).await.unwrap();
                assert!(
                    !original_bytes.starts_with(body.as_ref()),
                    "FIFO response contains no artifact prefix"
                );
                std::fs::remove_file(&leaf).unwrap();
                std::fs::write(&leaf, &original_bytes).unwrap();
                assert_clean().await;
            }
            Ok(status) => {
                assert!(
                    !leaf.exists(),
                    "denied FIFO creation leaves no test artifact ({status})"
                );
                let response = app
                    .clone()
                    .oneshot(app_request("GET", &artifact_path, Some(&bearer)))
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::NOT_FOUND,
                    "denied FIFO artifact leaf fails closed"
                );
                let body = to_bytes(response.into_body(), 1024).await.unwrap();
                assert!(
                    !original_bytes.starts_with(body.as_ref()),
                    "denied FIFO response contains no artifact prefix"
                );
                std::fs::write(&leaf, &original_bytes).unwrap();
                assert_clean().await;
                eprintln!(
                    "sandbox/platform denied FIFO creation ({status}); PT-10 FIFO vector requires parent rerun"
                );
            }
            Err(error) => {
                assert!(!leaf.exists(), "unavailable mkfifo leaves no test artifact");
                let response = app
                    .clone()
                    .oneshot(app_request("GET", &artifact_path, Some(&bearer)))
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::NOT_FOUND,
                    "unavailable FIFO artifact leaf fails closed"
                );
                let body = to_bytes(response.into_body(), 1024).await.unwrap();
                assert!(
                    !original_bytes.starts_with(body.as_ref()),
                    "unavailable FIFO response contains no artifact prefix"
                );
                std::fs::write(&leaf, &original_bytes).unwrap();
                assert_clean().await;
                eprintln!("mkfifo unavailable ({error}); PT-10 FIFO vector requires parent rerun");
            }
        }

        std::fs::remove_file(&leaf).unwrap();
        let device = std::process::Command::new("mknod")
            .args([
                leaf.as_os_str(),
                std::ffi::OsStr::new("c"),
                std::ffi::OsStr::new("1"),
                std::ffi::OsStr::new("3"),
            ])
            .status();
        match device {
            Ok(status) if status.success() => {
                let response = app
                    .clone()
                    .oneshot(app_request("GET", &artifact_path, Some(&bearer)))
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::NOT_FOUND,
                    "device artifact leaf fails closed"
                );
                let body = to_bytes(response.into_body(), 1024).await.unwrap();
                assert!(
                    !original_bytes.starts_with(body.as_ref()),
                    "device response contains no artifact prefix"
                );
                std::fs::remove_file(&leaf).unwrap();
                std::fs::write(&leaf, &original_bytes).unwrap();
                assert_clean().await;
            }
            Ok(status) => {
                assert!(
                    !leaf.exists(),
                    "failed device creation leaves no test artifact"
                );
                let response = app
                    .clone()
                    .oneshot(app_request("GET", &artifact_path, Some(&bearer)))
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::NOT_FOUND,
                    "denied device artifact leaf fails closed"
                );
                let body = to_bytes(response.into_body(), 1024).await.unwrap();
                assert!(
                    !original_bytes.starts_with(body.as_ref()),
                    "denied device response contains no artifact prefix"
                );
                std::fs::write(&leaf, &original_bytes).unwrap();
                assert_clean().await;
                eprintln!(
                    "sandbox/platform denied character-device creation ({status}); PT-10 device vector requires parent rerun"
                );
            }
            Err(error) => {
                assert!(!leaf.exists(), "unavailable mknod leaves no test artifact");
                let response = app
                    .clone()
                    .oneshot(app_request("GET", &artifact_path, Some(&bearer)))
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::NOT_FOUND,
                    "unavailable device artifact leaf fails closed"
                );
                let body = to_bytes(response.into_body(), 1024).await.unwrap();
                assert!(
                    !original_bytes.starts_with(body.as_ref()),
                    "unavailable device response contains no artifact prefix"
                );
                std::fs::write(&leaf, &original_bytes).unwrap();
                assert_clean().await;
                eprintln!("mknod unavailable ({error}); PT-10 device vector requires parent rerun");
            }
        }
    }

    // The result read path is observational: no approval/publish outcome or
    // Publisher exists in this fixture, and result reads add no S1 publish
    // transition. Host invocation counters remain at the producer baseline.
    let publish_rows: i64 = rusqlite::Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM ledger WHERE to_state='publishing'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(publish_rows, 0);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 4);
    assert_eq!(fixture.restores.load(Ordering::SeqCst), 0);

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let original_root = fixture.root.clone();
        let displaced = original_root.with_extension("ancestor-held");
        let outside = original_root.with_extension("ancestor-replacement");
        // The replacement leaf mirrors the resolver path under the swapped
        // ancestor: create the leaf's full parent chain before writing.
        let ancestor_leaf = outside.join("artifacts").join(&artifact_row.0);
        std::fs::create_dir_all(ancestor_leaf.parent().unwrap()).unwrap();
        std::fs::write(&ancestor_leaf, b"ancestor-secret-must-not-leak").unwrap();
        std::fs::rename(&original_root, &displaced).unwrap();
        symlink(&outside, &original_root).unwrap();
        let replaced_ancestor = app
            .clone()
            .oneshot(app_request("GET", &artifact_path, Some(&bearer)))
            .await
            .unwrap();
        if replaced_ancestor.status() == StatusCode::OK {
            assert_eq!(
                to_bytes(replaced_ancestor.into_body(), 1024 * 1024)
                    .await
                    .unwrap()
                    .as_ref(),
                original_bytes.as_slice(),
                "ancestor replacement remains bound to original directory handles"
            );
        } else {
            assert_eq!(replaced_ancestor.status(), StatusCode::NOT_FOUND);
            let body = to_bytes(replaced_ancestor.into_body(), 1024).await.unwrap();
            assert!(!String::from_utf8_lossy(&body).contains("ancestor-secret"));
        }
        std::fs::remove_file(&original_root).unwrap();
        std::fs::rename(&displaced, &original_root).unwrap();
        let restored_ancestor = app
            .clone()
            .oneshot(app_request("GET", &artifact_path, Some(&bearer)))
            .await
            .unwrap();
        assert_eq!(
            restored_ancestor.status(),
            StatusCode::OK,
            "restored original root serves artifact"
        );
        assert_eq!(
            to_bytes(restored_ancestor.into_body(), 1024 * 1024)
                .await
                .unwrap()
                .as_ref(),
            original_bytes.as_slice()
        );
        assert_clean().await;
    }
}

#[cfg(feature = "p2-test-support")]
#[tokio::test]
async fn product_result_read_and_approval_routes_use_a_real_loopback_socket_when_available() {
    use product_result_fixture::{
        FixtureClock, FixtureIdentity, genuine_host_done, genuine_result_finalized,
    };
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: "socket-install".into(),
            database_id: "socket-db".into(),
        },
        "socket-source",
        "socket-order",
    );
    let mut fixture = genuine_result_finalized(genuine_host_done(identity));
    fixture.host.take();
    fixture.result_store.take();
    let mut store = ProductStore::open_with_clock(
        &fixture.db_path,
        fixture.config.clone(),
        std::sync::Arc::new(FixtureClock(fixture.now)),
    )
    .unwrap();
    store
        .configure_product_artifact_root(&fixture.artifact_root)
        .unwrap();
    let result_id: String = rusqlite::Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT result_id FROM product_result_bindings_v1 WHERE order_id=?1",
            [&fixture.identity.order_id],
            |row| row.get(0),
        )
        .unwrap();
    let payload_json: String = rusqlite::Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT payload_json FROM product_result_bindings_v1 WHERE result_id=?1",
            [&result_id],
            |row| row.get(0),
        )
        .unwrap();
    let artifact_id =
        serde_json::from_str::<serde_json::Value>(&payload_json).unwrap()["final_artifact_id"]
            .as_str()
            .unwrap()
            .to_owned();
    let app =
        foundry_serve::http::product_router(foundry_serve::auth_http::ProductHttpState::new(store));
    let listener = match TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await {
        Ok(listener) => listener,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::AddrNotAvailable
            ) =>
        {
            eprintln!(
                "sandbox blocked real loopback bind; C lane is unreachable here and must be rerun by the parent: {error}"
            );
            return;
        }
        Err(error) => panic!("unexpected loopback bind failure: {error}"),
    };
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let (login_status, login_bytes) = loopback_request(
        address,
        "POST",
        "/api/product/login",
        None,
        serde_json::json!({"actor_id":"reader","password":"result-reader-password"})
            .to_string()
            .as_bytes(),
    )
    .await;
    assert_eq!(login_status, 200);
    let login_json: serde_json::Value = serde_json::from_slice(&login_bytes).unwrap();
    let bearer = login_json["access_token"].as_str().unwrap();
    let metadata_path = format!(
        "/api/product/orders/{}/results/{result_id}",
        fixture.identity.order_id
    );
    let (metadata_status, metadata_bytes) =
        loopback_request(address, "GET", &metadata_path, Some(bearer), &[]).await;
    assert_eq!(metadata_status, 200);
    let metadata_json: serde_json::Value = serde_json::from_slice(&metadata_bytes).unwrap();
    assert_eq!(metadata_json["result_id"], result_id);
    let artifact_path = format!("{metadata_path}/artifacts/{artifact_id}");
    let (artifact_status, bytes) =
        loopback_request(address, "GET", &artifact_path, Some(bearer), &[]).await;
    assert_eq!(artifact_status, 200);
    assert!(!bytes.is_empty());
    let (no_auth_status, _) = loopback_request(address, "GET", &metadata_path, None, &[]).await;
    assert_eq!(no_auth_status, 401);
    let (grant_status, grant_bytes) = loopback_request(
        address,
        "POST",
        "/api/product/approval-grants",
        Some(bearer),
        serde_json::json!({"order_id":fixture.identity.order_id})
            .to_string()
            .as_bytes(),
    )
    .await;
    assert_eq!(grant_status, 201);
    let grant_json: serde_json::Value = serde_json::from_slice(&grant_bytes).unwrap();
    let approval = serde_json::json!({
        "grant_id":grant_json["grant_id"], "grant_token":grant_json["grant_token"],
        "expected_source_revision":1, "expected_attempt":1,
        "run_id":fixture.expected.host_key.run as i64, "generation":0,
        "artifact_digest":fixture.terminal.as_ref().unwrap().result_commit,
        "tuple_digest":metadata_json["tuple_digest"], "intent":"socket approval test"
    });
    let approve_path = format!("/api/product/orders/{}/approve", fixture.identity.order_id);
    let approval_body = approval.to_string();
    let (approved_status, approved_bytes) = loopback_request(
        address,
        "POST",
        &approve_path,
        Some(bearer),
        approval_body.as_bytes(),
    )
    .await;
    assert_eq!(approved_status, 200);
    let (retry_status, retry_bytes) = loopback_request(
        address,
        "POST",
        &approve_path,
        Some(bearer),
        approval_body.as_bytes(),
    )
    .await;
    assert_eq!(retry_status, 200);
    assert_eq!(
        retry_bytes, approved_bytes,
        "socket retry replays the exact response"
    );
    let failed_body = serde_json::json!({"grant_id":"missing","grant_token":"missing","expected_source_revision":99,"expected_attempt":1,"run_id":1,"generation":0,"artifact_digest":"0".repeat(40),"tuple_digest":"0".repeat(64),"intent":"must refuse"}).to_string();
    let (failed_status, _) = loopback_request(
        address,
        "POST",
        &approve_path,
        Some(bearer),
        failed_body.as_bytes(),
    )
    .await;
    assert!(matches!(failed_status, 400 | 403 | 404 | 409));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 4);
    assert_eq!(fixture.restores.load(Ordering::SeqCst), 0);
    server.abort();
    let _ = server.await;

    let mut reopened = ProductStore::open_with_clock(
        &fixture.db_path,
        fixture.config.clone(),
        std::sync::Arc::new(FixtureClock(fixture.now)),
    )
    .unwrap();
    reopened
        .configure_product_artifact_root(&fixture.artifact_root)
        .unwrap();
    let reopened_app = foundry_serve::http::product_router(
        foundry_serve::auth_http::ProductHttpState::new(reopened),
    );
    let reopened_listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("loopback bind was available earlier in this C lane");
    let reopened_address = reopened_listener.local_addr().unwrap();
    let reopened_server =
        tokio::spawn(async move { axum::serve(reopened_listener, reopened_app).await });
    let (reopen_status, reopen_bytes) = loopback_request(
        reopened_address,
        "POST",
        &approve_path,
        Some(bearer),
        approval_body.as_bytes(),
    )
    .await;
    assert_eq!(reopen_status, 200);
    assert_eq!(
        reopen_bytes, approved_bytes,
        "reopened socket retries the exact response"
    );
    reopened_server.abort();
    let _ = reopened_server.await;
}
