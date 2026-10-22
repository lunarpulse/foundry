use chrono::{TimeZone, Utc};
use foundry_ledger::product_identity::ProductPasswordKdf;
use foundry_ledger::{
    ProductCapability, ProductConfig, ProductServiceAction, ProductServiceGrant, ProductStore,
    RepositoryIdentity, ScopeGrantManifest, ScopeSourceManifest,
};
use foundry_watch::intake::{
    ProductClock, ProductGithubConfig, ProductPollError,
    poll_product_github_page_batch_for_service,
    poll_product_github_page_batch_for_service_with_clock,
};
use rusqlite::{Connection, OpenFlags, types::Value};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;

struct Secrets;
const SYNTHETIC_TOKEN: &str = "synthetic-private-token";

struct AdvancingClock {
    base: chrono::DateTime<Utc>,
    samples: std::sync::atomic::AtomicI64,
}
impl ProductClock for AdvancingClock {
    fn now(&self) -> chrono::DateTime<Utc> {
        let offset = self
            .samples
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.base + chrono::Duration::seconds(offset)
    }
}

struct ExpireAfterRequestClock {
    before: chrono::DateTime<Utc>,
    expires_at: chrono::DateTime<Utc>,
    requests_seen: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl ProductClock for ExpireAfterRequestClock {
    fn now(&self) -> chrono::DateTime<Utc> {
        if self.requests_seen.load(std::sync::atomic::Ordering::SeqCst) >= 3 {
            self.expires_at
        } else {
            self.before
        }
    }
}

impl foundry_domain::ports::SecretProvider for Secrets {
    fn resolve(&self, name: &str) -> foundry_domain::ports::PortResult<String> {
        assert_eq!(name, "test/github");
        Ok(SYNTHETIC_TOKEN.into())
    }
}

fn issue(id: u64, number: u64, title: &str, pull_request: bool) -> String {
    format!(
        r#"{{"id":{id},"number":{number},"title":"{title}","updated_at":"2026-10-15T23:59:00Z","state":"open","labels":[{{"name":"foundry:make"}}],"body":"body","pull_request":{}}}"#,
        if pull_request { "{}" } else { "null" }
    )
}

fn serve(
    listener: TcpListener,
    responses: Vec<(u16, String, Option<String>, Option<String>)>,
) -> std::thread::JoinHandle<Vec<String>> {
    std::thread::spawn(move || {
        let mut requests = Vec::new();
        for (status, body, link, location) in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 8192];
            let n = stream.read(&mut request).unwrap();
            requests.push(String::from_utf8_lossy(&request[..n]).to_string());
            let link_header = link.map(|v| format!("Link: {v}\r\n")).unwrap_or_default();
            let location_header = location
                .map(|v| format!("Location: {v}\r\n"))
                .unwrap_or_default();
            write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\n{link_header}{location_header}Content-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            stream.flush().unwrap();
        }
        requests
    })
}

fn serve_signaled(
    listener: TcpListener,
    responses: Vec<(u16, String, Option<String>, Option<String>)>,
    requests_seen: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> std::thread::JoinHandle<Vec<String>> {
    std::thread::spawn(move || {
        let mut requests = Vec::new();
        for (status, body, link, location) in responses {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = [0u8; 8192];
            let n = stream.read(&mut request).unwrap();
            requests_seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            requests.push(String::from_utf8_lossy(&request[..n]).to_string());
            let link_header = link.map(|v| format!("Link: {v}\r\n")).unwrap_or_default();
            let location_header = location
                .map(|v| format!("Location: {v}\r\n"))
                .unwrap_or_default();
            write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\n{link_header}{location_header}Content-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            stream.flush().unwrap();
        }
        requests
    })
}

fn scratch() -> std::path::PathBuf {
    let path = std::env::var_os("TMPDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!(
            "foundry-b1-github-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[derive(Debug, PartialEq, Eq)]
struct CapturedRequest {
    method: String,
    path: String,
    query: BTreeMap<String, String>,
}

fn captured_request(request: &str) -> CapturedRequest {
    let line = request.lines().next().expect("captured HTTP request line");
    let mut fields = line.split_whitespace();
    let method = fields.next().expect("request method").to_owned();
    let target = fields.next().expect("request target");
    let version = fields.next().expect("HTTP version");
    assert!(
        fields.next().is_none(),
        "request line has exactly three fields"
    );
    assert!(version.starts_with("HTTP/"), "request uses an HTTP version");

    let url = reqwest::Url::parse(&format!("http://request.invalid{target}"))
        .expect("captured request target is a valid origin-form URL");
    let mut query = BTreeMap::new();
    for (key, value) in url.query_pairs() {
        assert!(
            query.insert(key.into_owned(), value.into_owned()).is_none(),
            "request query contains no duplicate keys"
        );
    }
    CapturedRequest {
        method,
        path: url.path().to_owned(),
        query,
    }
}

fn durable_intake_state(db: &std::path::Path) -> Vec<(String, Vec<Vec<Value>>)> {
    // Read the durable rows a rejected page could affect, without write access.
    const TABLES: &[&str] = &[
        "ledger",
        "product_transition_receipts_v1",
        "product_transition_intents_v1",
        "product_orders_v1",
        "product_order_payload_digests_v1",
        "product_poll_state_v1",
        "product_poll_source_bindings_v1",
        "product_poll_backoff_v1",
        "product_intake_inbox_v1",
        "product_intake_delivery_meta_v1",
        "product_intake_source_bindings_v1",
        "product_intake_rejections_v1",
        "product_work_backlog_v1",
        "product_work_claims_v1",
        "product_service_work_bindings_v1",
    ];
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open test database read-only");
    TABLES
        .iter()
        .map(|table| {
            let mut stmt = conn
                .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                .expect("prepare fixed test snapshot query");
            let mut rows = stmt.query([]).expect("query durable state");
            let mut values = Vec::new();
            while let Some(row) = rows.next().expect("read durable state row") {
                let mut record = Vec::with_capacity(row.as_ref().column_count());
                for column in 0..row.as_ref().column_count() {
                    record.push(
                        row.get::<_, Value>(column)
                            .expect("read durable state value"),
                    );
                }
                values.push(record);
            }
            ((*table).to_owned(), values)
        })
        .collect()
}

fn assert_get_metadata(request: &CapturedRequest) {
    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/repos/acme/proj");
    assert!(
        request.query.is_empty(),
        "metadata request has an empty query"
    );
}

fn assert_get_issue_page(request: &CapturedRequest, page: &str) {
    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/repos/acme/proj/issues");
    let expected = BTreeMap::from([
        ("labels".to_owned(), "foundry:make".to_owned()),
        ("state".to_owned(), "open".to_owned()),
        ("per_page".to_owned(), "100".to_owned()),
        ("page".to_owned(), page.to_owned()),
    ]);
    assert_eq!(
        request.query, expected,
        "issue request has the exact polling query"
    );
}

fn scoped_service(
    db: &std::path::Path,
    origin: &str,
    now: chrono::DateTime<Utc>,
) -> (ProductStore, foundry_ledger::ProductServiceContext, String) {
    let mut store = ProductStore::open(
        db,
        ProductConfig {
            installation_id: "test-install".into(),
            database_id: "test-db".into(),
        },
    )
    .unwrap();
    let source = ScopeSourceManifest {
        connector_id: "github-main".into(),
        source_instance_id: "source-github-main-v1".into(),
        generation: 1,
        identity: RepositoryIdentity::canonicalized("acme", "github", origin, "42").unwrap(),
        api_owner: Some("acme".into()),
        api_repo: Some("proj".into()),
        legacy_repository_locator: "acme/proj".into(),
        scope_query: "open+foundry:make".into(),
        enabled: true,
    };
    let phc = ProductPasswordKdf::hash("synthetic-owner-password").unwrap();
    store
        .bootstrap_owner_scoped(
            "owner",
            &phc,
            std::slice::from_ref(&source),
            &[ScopeGrantManifest {
                actor_id: "owner".into(),
                source_instance_id: source.source_instance_id.clone(),
                capabilities: vec![ProductCapability::ReadOrder],
            }],
        )
        .unwrap();
    let owner = store
        .login("owner", "synthetic-owner-password", now)
        .unwrap();
    let actions = [
        ProductServiceAction::IntakeRead,
        ProductServiceAction::AdmitDelivery,
        ProductServiceAction::AckDelivery,
        ProductServiceAction::DriveReview,
    ]
    .into_iter()
    .map(|action| ProductServiceGrant {
        source_instance_id: source.source_instance_id.clone(),
        action,
    })
    .collect::<Vec<_>>();
    store
        .manage_product_service(
            owner.digest(),
            "github-watch",
            &actions,
            true,
            now + chrono::Duration::hours(3),
            0,
            "service-config-1",
            now,
        )
        .unwrap();
    let credential = store
        .issue_product_service_credential(
            owner.digest(),
            "github-watch",
            1,
            now + chrono::Duration::hours(2),
            "service-issue-1",
            now,
        )
        .unwrap();
    let token = credential.expose_once_to_file_writer().to_owned();
    let context = store
        .authenticate_product_service(&token, "github-main", origin, "42", now)
        .unwrap();
    (store, context, token)
}

#[test]
fn paginated_private_poll_resumes_after_restart_and_skips_pull_requests() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let origin = format!("http://{addr}");
    let next = format!(
        "<{origin}/repos/acme/proj/issues?labels=foundry%3Amake&state=open&per_page=100&page=2>; rel=\"next\""
    );
    let page1 = (1..=100)
        .map(|n| issue(7000 + n, n, "item", false))
        .collect::<Vec<_>>()
        .join(",");
    let page2 = issue(7101, 101, "last", false);
    let worker = serve(
        listener,
        vec![
            (
                200,
                r#"{"id":42,"full_name":"acme/proj"}"#.into(),
                None,
                None,
            ),
            (200, format!("[{page1}]"), Some(next), None),
            (
                200,
                r#"{"id":42,"full_name":"acme/proj"}"#.into(),
                None,
                None,
            ),
            (
                200,
                r#"{"id":42,"full_name":"acme/proj"}"#.into(),
                None,
                None,
            ),
            (200, format!("[{page2}]"), None, None),
            (
                200,
                r#"{"id":42,"full_name":"acme/proj"}"#.into(),
                None,
                None,
            ),
        ],
    );
    let dir = scratch();
    let db = dir.join("product.sqlite");
    let product = ProductConfig {
        installation_id: "test-install".into(),
        database_id: "test-db".into(),
    };
    let now = Utc.with_ymd_and_hms(2026, 10, 16, 0, 0, 0).unwrap();
    let cfg = ProductGithubConfig {
        connector_id: "github-main".into(),
        api_origin: origin.clone(),
        owner: "acme".into(),
        repo: "proj".into(),
        repository_id: "42".into(),
        private: true,
        credential_ref: Some("test/github".into()),
        allow_loopback_http: true,
        max_pages_per_tick: 1,
        now,
    };
    let (mut store, service, token) = scoped_service(&db, &origin, now);
    let first =
        poll_product_github_page_batch_for_service(&mut store, &cfg, &Secrets, &service).unwrap();
    assert_eq!((first.pages, first.fetched, first.next_page), (1, 100, 2));
    drop(store);

    let mut resumed = ProductStore::open(&db, product).unwrap();
    let service = resumed
        .authenticate_product_service(&token, "github-main", &origin, "42", now)
        .unwrap();
    let second =
        poll_product_github_page_batch_for_service(&mut resumed, &cfg, &Secrets, &service).unwrap();
    assert_eq!((second.pages, second.fetched, second.next_page), (1, 1, 1));
    let deliveries = resumed
        .due_intake_deliveries_for_service(&service, now, 128)
        .unwrap();
    assert_eq!(deliveries.len(), 101);
    assert!(
        deliveries
            .iter()
            .all(|d| !d.delivery.payload.contains("pull"))
    );
    let requests = worker.join().unwrap();
    assert_eq!(requests.len(), 6);
    let parsed = requests
        .iter()
        .map(|request| captured_request(request))
        .collect::<Vec<_>>();
    assert_get_metadata(&parsed[0]);
    assert_get_issue_page(&parsed[1], "1");
    assert_get_metadata(&parsed[2]);
    assert_get_metadata(&parsed[3]);
    assert_get_issue_page(&parsed[4], "2");
    assert_get_metadata(&parsed[5]);
    let expected_auth = format!("authorization: bearer {SYNTHETIC_TOKEN}");
    assert!(
        requests
            .iter()
            .all(|r| r.to_ascii_lowercase().contains(&expected_auth))
    );
    drop(resumed);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn poller_resamples_clock_after_each_request_before_durable_page_commit() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let origin = format!("http://{addr}");
    let worker = serve(
        listener,
        vec![
            (
                200,
                r#"{"id":42,"full_name":"acme/proj"}"#.into(),
                None,
                None,
            ),
            (200, "[]".into(), None, None),
            (
                200,
                r#"{"id":42,"full_name":"acme/proj"}"#.into(),
                None,
                None,
            ),
        ],
    );
    let dir = scratch();
    let db = dir.join("product.sqlite");
    let base = Utc.with_ymd_and_hms(2026, 10, 16, 0, 0, 0).unwrap();
    let (mut store, service, _token) = scoped_service(&db, &origin, base);
    let cfg = ProductGithubConfig {
        connector_id: "github-main".into(),
        api_origin: origin,
        owner: "acme".into(),
        repo: "proj".into(),
        repository_id: "42".into(),
        private: false,
        credential_ref: None,
        allow_loopback_http: true,
        max_pages_per_tick: 1,
        now: base,
    };
    let clock = AdvancingClock {
        base,
        samples: std::sync::atomic::AtomicI64::new(0),
    };
    let report = poll_product_github_page_batch_for_service_with_clock(
        &mut store, &cfg, &Secrets, &service, &clock,
    )
    .unwrap();
    assert_eq!(report.pages, 1);
    assert!(
        clock.samples.load(std::sync::atomic::Ordering::SeqCst) >= 6,
        "source checks, requests, and page commit each sampled the injected clock"
    );
    use sha2::Digest as _;
    let mut hash = sha2::Sha256::new();
    hash.update(b"foundry-github-query-v1\0");
    for part in [&cfg.api_origin, &cfg.owner, &cfg.repo, "open+foundry:make"] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    let fingerprint = hex::encode(hash.finalize());
    let saved = store
        .intake_cursor_for_service(&service, &fingerprint, clock.now())
        .unwrap()
        .unwrap();
    let saved_at = chrono::DateTime::parse_from_rfc3339(&saved.updated_at)
        .unwrap()
        .with_timezone(&Utc);
    assert!(
        saved_at > base,
        "page cursor records the fresh post-HTTP instant, not ProductGithubConfig.now"
    );
    let requests = worker.join().unwrap();
    assert_eq!(requests.len(), 3);
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn service_expiry_at_page_save_after_valid_metadata_and_issue_gets_is_no_write() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let origin = format!("http://{addr}");
    let requests_seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let worker = serve_signaled(
        listener,
        vec![
            (
                200,
                r#"{"id":42,"full_name":"acme/proj"}"#.into(),
                None,
                None,
            ),
            (
                200,
                format!("[{}]", issue(9201, 1, "must not persist", false)),
                None,
                None,
            ),
            (
                200,
                r#"{"id":42,"full_name":"acme/proj"}"#.into(),
                None,
                None,
            ),
        ],
        requests_seen.clone(),
    );
    let dir = scratch();
    let db = dir.join("product.sqlite");
    let before_time = Utc.with_ymd_and_hms(2026, 10, 18, 0, 0, 0).unwrap();
    let expires_at = before_time + chrono::Duration::hours(2);
    let (mut store, service, _token) = scoped_service(&db, &origin, before_time);
    let cfg = ProductGithubConfig {
        connector_id: "github-main".into(),
        api_origin: origin,
        owner: "acme".into(),
        repo: "proj".into(),
        repository_id: "42".into(),
        private: false,
        credential_ref: None,
        allow_loopback_http: true,
        max_pages_per_tick: 1,
        now: before_time,
    };
    let before = durable_intake_state(&db);
    let clock = ExpireAfterRequestClock {
        before: before_time,
        expires_at,
        requests_seen,
    };
    let result = poll_product_github_page_batch_for_service_with_clock(
        &mut store, &cfg, &Secrets, &service, &clock,
    );
    assert!(
        matches!(
            result,
            Err(ProductPollError::Store(
                foundry_ledger::ProductError::Invariant(
                    "service authority is absent, expired, or revoked"
                )
            ))
        ),
        "the durable page transaction must reject an expired service lease at equality"
    );
    let requests = worker
        .join()
        .expect("bounded mock responder completed all expected requests");
    assert_eq!(
        requests.len(),
        3,
        "metadata, issue page, and post-fetch metadata each run once"
    );
    let parsed = requests
        .iter()
        .map(|request| captured_request(request))
        .collect::<Vec<_>>();
    assert_get_metadata(&parsed[0]);
    assert_get_issue_page(&parsed[1], "1");
    assert_get_metadata(&parsed[2]);
    let after = durable_intake_state(&db);
    assert_eq!(
        before, after,
        "expiry at the page-save boundary leaves inbox, cursor, rejection, admission, S1, backlog, and claims unchanged"
    );
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn rejects_cross_origin_pagination_and_redirects_without_following() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let origin = format!("http://{addr}");
    let evil = "<http://127.0.0.1:1/repos/acme/proj/issues?labels=foundry%3Amake&state=open&per_page=100&page=2>; rel=\"next\"";
    let worker = serve(
        listener,
        vec![
            (
                200,
                r#"{"id":42,"full_name":"acme/proj"}"#.into(),
                None,
                None,
            ),
            (200, "[]".into(), Some(evil.into()), None),
        ],
    );
    let dir = scratch();
    let db = dir.join("product.sqlite");
    let (mut store, service, _token) = scoped_service(&db, &origin, Utc::now());
    let cfg = ProductGithubConfig {
        connector_id: "github-main".into(),
        api_origin: origin.clone(),
        owner: "acme".into(),
        repo: "proj".into(),
        repository_id: "42".into(),
        private: false,
        credential_ref: None,
        allow_loopback_http: true,
        max_pages_per_tick: 1,
        now: Utc::now(),
    };
    assert!(matches!(
        poll_product_github_page_batch_for_service(&mut store, &cfg, &Secrets, &service),
        Err(ProductPollError::Permanent)
    ));
    let requests = worker.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request.starts_with("GET ")));
    assert!(
        store
            .intake_cursor_for_service(&service, "not-the-query", cfg.now)
            .unwrap()
            .is_none()
    );
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn github_redirect_is_not_followed_and_every_request_is_get() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let origin = format!("http://{addr}");
    let worker = serve(
        listener,
        vec![
            (
                200,
                r#"{"id":42,"full_name":"acme/proj"}"#.into(),
                None,
                None,
            ),
            (
                302,
                String::new(),
                None,
                Some("http://127.0.0.1:1/stolen".into()),
            ),
        ],
    );
    let dir = scratch();
    let db = dir.join("product.sqlite");
    let (mut store, service, _token) = scoped_service(&db, &origin, Utc::now());
    let cfg = ProductGithubConfig {
        connector_id: "github-main".into(),
        api_origin: origin,
        owner: "acme".into(),
        repo: "proj".into(),
        repository_id: "42".into(),
        private: false,
        credential_ref: None,
        allow_loopback_http: true,
        max_pages_per_tick: 1,
        now: Utc::now(),
    };
    assert!(matches!(
        poll_product_github_page_batch_for_service(&mut store, &cfg, &Secrets, &service),
        Err(ProductPollError::Permanent)
    ));
    let requests = worker.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request.starts_with("GET ")));
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn malformed_item_isolated_and_only_digest_is_durably_recorded() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let origin = format!("http://{addr}");
    let worker = serve(
        listener,
        vec![
            (
                200,
                r#"{"id":42,"full_name":"acme/proj"}"#.into(),
                None,
                None,
            ),
            (
                200,
                format!(
                    r#"[{{"id":1,"number":"bad","title":"private canary","updated_at":"x","state":"open","labels":[]}},{}]"#,
                    issue(2, 2, "valid", false)
                ),
                None,
                None,
            ),
            (
                200,
                r#"{"id":42,"full_name":"acme/proj"}"#.into(),
                None,
                None,
            ),
        ],
    );
    let dir = scratch();
    let db = dir.join("product.sqlite");
    let (mut store, service, _token) = scoped_service(&db, &origin, Utc::now());
    let cfg = ProductGithubConfig {
        connector_id: "github-main".into(),
        api_origin: origin,
        owner: "acme".into(),
        repo: "proj".into(),
        repository_id: "42".into(),
        private: false,
        credential_ref: None,
        allow_loopback_http: true,
        max_pages_per_tick: 1,
        now: Utc::now(),
    };
    let report =
        poll_product_github_page_batch_for_service(&mut store, &cfg, &Secrets, &service).unwrap();
    assert_eq!(report.fetched, 1);
    assert_eq!(
        store
            .intake_rejection_count_for_service(&service, cfg.now)
            .unwrap(),
        1
    );
    let due = store
        .due_intake_deliveries_for_service(&service, cfg.now, 10)
        .unwrap();
    assert_eq!(due.len(), 1);
    assert!(!format!("{due:?}").contains("private canary"));
    assert_eq!(worker.join().unwrap().len(), 3);
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn repository_id_and_full_name_are_both_required_before_issue_data_is_accepted() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let origin = format!("http://{addr}");
    let worker = serve(
        listener,
        vec![(
            200,
            r#"{"id":99,"full_name":"acme/proj"}"#.into(),
            None,
            None,
        )],
    );
    let dir = scratch();
    let db = dir.join("product.sqlite");
    let now = Utc::now();
    let (mut store, service, _token) = scoped_service(&db, &origin, now);
    let cfg = ProductGithubConfig {
        connector_id: "github-main".into(),
        api_origin: origin,
        owner: "acme".into(),
        repo: "proj".into(),
        repository_id: "42".into(),
        private: false,
        credential_ref: None,
        allow_loopback_http: true,
        max_pages_per_tick: 1,
        now,
    };
    let before = durable_intake_state(&db);
    assert!(matches!(
        poll_product_github_page_batch_for_service(&mut store, &cfg, &Secrets, &service),
        Err(ProductPollError::Permanent)
    ));
    let requests = worker.join().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "a mismatched stable ID stops before the issue page request"
    );
    assert_get_metadata(&captured_request(&requests[0]));
    let after = durable_intake_state(&db);
    assert!(
        before == after,
        "stable-ID mismatch leaves polling and admission state unchanged"
    );
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn rename_or_name_reuse_between_metadata_and_issue_page_is_rejected_before_page_commit() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let origin = format!("http://{addr}");
    let worker = serve(
        listener,
        vec![
            (
                200,
                r#"{"id":42,"full_name":"acme/proj"}"#.into(),
                None,
                None,
            ),
            (
                200,
                format!("[{}]", issue(7001, 1, "must not be admitted", false)),
                None,
                None,
            ),
            // The locator now resolves to a different repository (or the
            // configured repository was renamed during the fetch).
            (
                200,
                r#"{"id":42,"full_name":"other/project"}"#.into(),
                None,
                None,
            ),
        ],
    );
    let dir = scratch();
    let db = dir.join("product.sqlite");
    let now = Utc::now();
    let (mut store, service, _token) = scoped_service(&db, &origin, now);
    let cfg = ProductGithubConfig {
        connector_id: "github-main".into(),
        api_origin: origin,
        owner: "acme".into(),
        repo: "proj".into(),
        repository_id: "42".into(),
        private: false,
        credential_ref: None,
        allow_loopback_http: true,
        max_pages_per_tick: 1,
        now,
    };
    let before = durable_intake_state(&db);
    assert!(matches!(
        poll_product_github_page_batch_for_service(&mut store, &cfg, &Secrets, &service),
        Err(ProductPollError::Permanent)
    ));
    let requests = worker.join().unwrap();
    assert_eq!(requests.len(), 3);
    let parsed = requests
        .iter()
        .map(|request| captured_request(request))
        .collect::<Vec<_>>();
    assert_get_metadata(&parsed[0]);
    assert_get_issue_page(&parsed[1], "1");
    assert_get_metadata(&parsed[2]);
    let after = durable_intake_state(&db);
    assert!(
        before == after,
        "post-page locator rejection leaves inbox, cursor, rejection, admission, S1, backlog, and claims unchanged"
    );
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}
