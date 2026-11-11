#![cfg(feature = "a01-harness-support")]

use chrono::{Duration, Utc};
use foundry_ledger::{
    ProductConfig, ProductServiceAction, ProductServiceGrant, ProductStore, RepositoryIdentity,
    ScopeSourceManifest,
};
use rusqlite::{Connection, types::Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration as StdDuration,
};

struct Fixture {
    root: PathBuf,
    db: PathBuf,
    origin: String,
    listener: TcpListener,
}
impl Fixture {
    fn new() -> Self {
        Self::with_review(true)
    }
    fn with_review(review: bool) -> Self {
        let root = std::env::temp_dir().join(format!(
            "w2a-harness-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let db = root.join("product.sqlite");
        let now = Utc::now();
        let mut store = ProductStore::open(
            &db,
            ProductConfig {
                installation_id: "w2a-install".into(),
                database_id: "w2a-db".into(),
            },
        )
        .unwrap();
        let source = ScopeSourceManifest {
            connector_id: "github-primary".into(),
            source_instance_id: "synthetic-source".into(),
            generation: 1,
            identity: RepositoryIdentity::canonicalized("acme", "github", &origin, "42").unwrap(),
            api_owner: Some("acme".into()),
            api_repo: Some("proj".into()),
            legacy_repository_locator: "acme/proj".into(),
            scope_query: "open+foundry:make".into(),
            enabled: true,
        };
        // bootstrap_owner_scoped installs the synthetic source and its connector; no target rows are injected.
        let password = format!("w2a-owner-{}", now.timestamp_nanos_opt().unwrap());
        let phc = foundry_ledger::product_identity::ProductPasswordKdf::hash(&password).unwrap();
        store
            .bootstrap_owner_scoped("owner", &phc, &[source], &[])
            .unwrap();
        let owner = store.login("owner", &password, now).unwrap();
        let grants: Vec<_> = [
            ProductServiceAction::IntakeRead,
            ProductServiceAction::AdmitDelivery,
            ProductServiceAction::AckDelivery,
            ProductServiceAction::DriveReview,
        ]
        .into_iter()
        .filter(|action| review || !matches!(action, ProductServiceAction::DriveReview))
        .map(|action| ProductServiceGrant {
            source_instance_id: "synthetic-source".into(),
            action,
        })
        .collect();
        store
            .manage_product_service(
                owner.digest(),
                "synthetic-service",
                &grants,
                true,
                now + Duration::hours(3),
                0,
                "manage",
                now,
            )
            .unwrap();
        let credential = store
            .issue_product_service_credential(
                owner.digest(),
                "synthetic-service",
                1,
                now + Duration::hours(2),
                "issue",
                now,
            )
            .unwrap();
        let token = credential.expose_once_to_file_writer();
        store
            .authenticate_product_service(token, "github-primary", &origin, "42", now)
            .unwrap();
        let file = root.join("service");
        std::fs::write(&file, token).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        drop(store);
        Self {
            root,
            db,
            origin,
            listener,
        }
    }
    fn command(&self, flag: bool) -> std::process::Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_foundry"));
        cmd.args([
            "product-watch",
            "--once",
            "--db",
            self.db.to_str().unwrap(),
            "--installation-id",
            "w2a-install",
            "--database-id",
            "w2a-db",
            "--service-file",
            self.root.join("service").to_str().unwrap(),
            "--repo",
            "acme/proj",
            "--repository-id",
            "42",
            "--api-origin",
            &self.origin,
        ])
        .env("HOME", &self.root);
        if flag {
            cmd.arg("--a01-local-provider-test");
        }
        cmd.output().unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn snapshot(db: &Path) -> BTreeMap<String, Vec<Vec<Value>>> {
    let conn = Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut names = conn.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap();
    let tables: Vec<String> = names
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    tables
        .into_iter()
        .map(|name| {
            let escaped = name.replace('"', "\"\"");
            let mut stmt = conn
                .prepare(&format!("SELECT * FROM \"{escaped}\""))
                .unwrap();
            let count = stmt.column_count();
            let mut rows: Vec<Vec<Value>> = stmt
                .query_map([], |r| (0..count).map(|i| r.get(i)).collect())
                .unwrap()
                .map(Result::unwrap)
                .collect();
            rows.sort_by_key(|r| format!("{r:?}"));
            (name, rows)
        })
        .collect()
}

fn assert_snapshots_equal(
    before: &BTreeMap<String, Vec<Vec<Value>>>,
    after: &BTreeMap<String, Vec<Vec<Value>>>,
) {
    assert!(before == after, "logical database snapshot changed");
}

#[test]
fn snapshot_mismatch_diagnostic_does_not_expose_values() {
    let sentinel = "sensitive-sentinel-credential-verifier";
    let before = BTreeMap::from([(
        "credentials".to_string(),
        vec![vec![Value::Text(sentinel.into())]],
    )]);
    let after = BTreeMap::from([(
        "credentials".to_string(),
        vec![vec![Value::Text("different".into())]],
    )]);
    let panic = std::panic::catch_unwind(|| assert_snapshots_equal(&before, &after)).unwrap_err();
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap();
    assert!(!message.contains(sentinel));
    assert_eq!(message, "logical database snapshot changed");
}

#[test]
fn b2_w2a_local_intake_harness_reaches_real_product_entrypoint_and_stops_unsupported() {
    let f = Fixture::new();
    let before = snapshot(&f.db);
    assert_eq!(before["product_orders_v1"].len(), 0);
    assert_eq!(before["product_services_v1"].len(), 1);
    let canary = format!(
        "w2a-canary-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let listener = f.listener.try_clone().unwrap();
    listener.set_nonblocking(true).unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let stop = done.clone();
    let issue = format!(
        r#"[{{"id":711,"number":17,"title":"{canary}","updated_at":"2026-10-15T23:59:00Z","state":"open","labels":[{{"name":"foundry:make"}}],"body":"synthetic body","pull_request":null}}]"#
    );
    let worker = std::thread::spawn(move || {
        let mut requests = Vec::new();
        while !stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(StdDuration::from_secs(5)))
                        .unwrap();
                    let mut buf = [0u8; 8192];
                    let n = stream.read(&mut buf).unwrap();
                    // Capture only the request line, never authorization headers or credentials.
                    let line = String::from_utf8_lossy(&buf[..n])
                        .lines()
                        .next()
                        .unwrap()
                        .to_owned();
                    let body = if line.contains("/issues?") {
                        &issue
                    } else {
                        r#"{"id":42,"full_name":"acme/proj"}"#
                    };
                    requests.push(line);
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(StdDuration::from_millis(5))
                }
                Err(e) => panic!("listener: {e}"),
            }
        }
        requests
    });
    let out = f.command(true);
    done.store(true, Ordering::SeqCst);
    let requests = worker.join().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("test_transport=a01-local-provider; acceptance=A01-NOT-RUN"));
    assert!(stdout.contains("pages=1 fetched=1 admitted=1 acknowledged=1"));
    assert!(stdout.contains("work=empty"));
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0], "GET /repos/acme/proj HTTP/1.1");
    assert_eq!(
        requests[1],
        "GET /repos/acme/proj/issues?labels=foundry%3Amake&state=open&per_page=100&page=1 HTTP/1.1"
    );
    assert_eq!(requests[2], requests[0]);
    let conn =
        Connection::open_with_flags(&f.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let (order, payload, seq, hash): (String, String, i64, String) = conn.query_row("SELECT order_id,payload_snapshot,admission_seq,admission_hash FROM product_orders_v1 WHERE connector_id='github-primary' AND repository_id='42' AND issue_id='711' AND task_kind='github-issue'", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
    let expected_payload =
        format!(r#"{{"body":"synthetic body","issue_number":17,"title":"{canary}"}}"#);
    assert!(payload.contains(&canary));
    assert!(
        payload.as_bytes() == expected_payload.as_bytes(),
        "stored payload differs from provider input"
    );
    let mut digest = Sha256::new();
    digest.update(b"foundry-product-intake-v1\0");
    digest.update(("github-issue".len() as u64).to_be_bytes());
    digest.update(b"github-issue");
    digest.update((payload.len() as u64).to_be_bytes());
    digest.update(payload.as_bytes());
    let expected_digest = hex::encode(digest.finalize());
    let stored_digest: String = conn
        .query_row(
            "SELECT payload_digest FROM product_order_payload_digests_v1 WHERE order_id=?1",
            [&order],
            |r| r.get(0),
        )
        .unwrap();
    assert!(stored_digest == expected_digest, "payload digest mismatch");
    let (delivery, inbox_payload, state): (String, String, String) = conn
        .query_row(
            "SELECT delivery_id,payload,state FROM product_intake_inbox_v1 WHERE issue_id='711'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(payload, inbox_payload);
    assert_eq!(state, "acked");
    let backlog: (String,i64,i64) = conn.query_row("SELECT state,attempts,lifecycle FROM product_work_backlog_v1 WHERE order_id=?1 AND delivery_id=?2", [&order,&delivery], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(backlog, ("waiting_unsupported".into(), 0, 1));
    let receipt: (String,String) = conn.query_row("SELECT operation,event_hash FROM product_transition_receipts_v1 WHERE order_id=?1 AND seq=?2", rusqlite::params![order,seq], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(receipt.0, "github-admission");
    assert_eq!(receipt.1, hash);
    assert!(seq > 0 && !hash.is_empty());
    let ledger_match: i64 = conn.query_row("SELECT count(*) FROM ledger WHERE seq=?1 AND order_id=?2 AND hash=?3 AND from_state='received' AND to_state='queued' AND event='Queued'", rusqlite::params![seq, order, hash], |r| r.get(0)).unwrap();
    assert_eq!(ledger_match, 1);
    let receipt_match: i64 = conn.query_row("SELECT count(*) FROM product_transition_receipts_v1 WHERE seq=?1 AND order_id=?2 AND event_hash=?3 AND lifecycle=1 AND expected_predecessor=0 AND operation='github-admission'", rusqlite::params![seq, order, hash], |r| r.get(0)).unwrap();
    assert_eq!(receipt_match, 1);
    let after = snapshot(&f.db);
    for name in [
        "product_orders_v1",
        "product_intake_inbox_v1",
        "product_work_backlog_v1",
        "product_order_payload_digests_v1",
        "product_transition_receipts_v1",
    ] {
        assert_eq!(after[name].len(), 1, "{name}");
    }
    for name in [
        "artifact_tuples",
        "product_result_bindings_v1",
        "product_approval_result_bindings_v1",
        "product_transition_intents_v1",
    ] {
        assert!(
            before.contains_key(name) && after.contains_key(name),
            "required table missing: {name}"
        );
        assert!(
            before[name].is_empty() && after[name].is_empty(),
            "unexpected rows in {name}"
        );
    }
    // A real authorized claim scan follows intake; empty rows do not prove no call.
    for (name, rows) in &after {
        if name == "product_work_claims_v1"
            || name == "product_service_work_bindings_v1"
            || name == "product_outcomes_v1"
            || name == "product_publish_outcomes_v1"
            || name == "product_operations_v1"
            || (name.starts_with("product_review_") && name != "product_review_meta_v1")
            || (name.starts_with("product_w1_") && before.get(name).is_some_and(|v| v.is_empty()))
        {
            assert!(rows.is_empty(), "unexpected rows in {name}");
        }
    }
}

#[test]
fn feature_binary_without_flag_denies_loopback_without_mutation() {
    let f = Fixture::new();
    f.listener.set_nonblocking(true).unwrap();
    let before = snapshot(&f.db);
    assert_eq!(before["product_services_v1"].len(), 1);
    assert_eq!(before["product_orders_v1"].len(), 0);
    let out = f.command(false);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("product intake tick failed"));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("test_transport="));
    assert_eq!(
        f.listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_snapshots_equal(&before, &snapshot(&f.db));
}

#[test]
fn intake_without_drive_review_persists_admission_but_tick_fails() {
    let f = Fixture::with_review(false);
    let listener = f.listener.try_clone().unwrap();
    listener.set_nonblocking(true).unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let stop = done.clone();
    let worker = std::thread::spawn(move || {
        let mut requests = Vec::new();
        while !stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(StdDuration::from_secs(5)))
                        .unwrap();
                    let mut buf = [0u8; 8192];
                    let n = stream.read(&mut buf).unwrap();
                    let line = String::from_utf8_lossy(&buf[..n])
                        .lines()
                        .next()
                        .unwrap()
                        .to_owned();
                    let body = if line.contains("/issues?") {
                        r#"[{"id":711,"number":17,"title":"negative-canary","updated_at":"2026-10-15T23:59:00Z","state":"open","labels":[{"name":"foundry:make"}],"body":"synthetic body","pull_request":null}]"#
                    } else {
                        r#"{"id":42,"full_name":"acme/proj"}"#
                    };
                    requests.push(line);
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(StdDuration::from_millis(5))
                }
                Err(e) => panic!("listener: {e}"),
            }
        }
        requests
    });
    let out = f.command(true);
    done.store(true, Ordering::SeqCst);
    let requests = worker.join().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("product intake tick failed"));
    assert_eq!(requests.len(), 3);
    let conn =
        Connection::open_with_flags(&f.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let count: i64 = conn.query_row("SELECT count(*) FROM product_orders_v1 o JOIN product_intake_inbox_v1 i ON i.issue_id=o.issue_id JOIN product_work_backlog_v1 b ON b.order_id=o.order_id AND b.delivery_id=i.delivery_id WHERE o.issue_id='711' AND o.payload_snapshot=?1 AND i.state='acked' AND b.state='waiting_unsupported' AND b.attempts=0", [r#"{"body":"synthetic body","issue_number":17,"title":"negative-canary"}"#], |r| r.get(0)).unwrap();
    assert_eq!(count, 1);
    let claims: i64 = conn
        .query_row("SELECT count(*) FROM product_work_claims_v1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(claims, 0);
}


// A01 gates use a fresh marked creator, retained physical identity and real
// scope/task/claim APIs. Every diagnostic below is value-free.
#[allow(dead_code,unused_imports)]
#[path = "../src/main.rs"]
mod a01_application;
use a01_application::a01_coordinator::{self,Producer};
use foundry_ledger::{ProductClock,ProductCapability,ScopeMutation,ProductWorkClaim};
use foundry_ledger::product::{A01TaskStoreIdentity,A01AuthorizeTaskRequest,A01RevokeTaskRequest,A01TaskAdmissionExpectation,A01TaskStoreRef,A01TaskOwnerExpectation,A01TaskServiceExpectation,A01ProducerReservation,A01AcceptedProducerOutput};
use chrono::DateTime;
#[track_caller]
fn must<T,E:std::fmt::Debug>(value: Result<T,E>) -> T { match value {Ok(v)=>v,Err(e)=>panic!("A01 operation rejected: {e:?}")} }
fn a01_now() -> DateTime<Utc> { Utc::now() }
struct A01Dir(PathBuf);
impl A01Dir {
    fn new() -> Self {
        let mut nonce=[0u8;16];
        must(rand_core::OsRng.try_fill_bytes(&mut nonce));
        let root=std::env::temp_dir().join(format!("a01-gates-{}-{}",std::process::id(),hex::encode(nonce)));
        must(std::fs::DirBuilder::new().mode(0o700).create(&root));
        Self(must(root.canonicalize()))
    }
}
impl Drop for A01Dir { fn drop(&mut self) { let _=std::fs::remove_dir_all(&self.0); } }
fn a01_db(path: &Path) -> Connection { must(Connection::open_with_flags(path,rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)) }
fn a01_raw_digest(kind: &str, payload: &str) -> String {
    let mut h=Sha256::new();h.update(b"foundry-product-intake-v1\0");
    for x in [kind.as_bytes(),payload.as_bytes()] {h.update((x.len() as u64).to_be_bytes());h.update(x);}
    hex::encode(h.finalize())
}
use rand_core::RngCore;
struct A01Clock(std::sync::Mutex<DateTime<Utc>>);
impl ProductClock for A01Clock { fn now(&self) -> DateTime<Utc> { *must(self.0.lock()) } }
impl A01Clock { fn set(&self, value: DateTime<Utc>) { *must(self.0.lock())=value; } }

// Transport is simulated. Admission, authentication and every positive write
// use the real ledger APIs. This fixture never seeds ready or claim evidence.
struct A01Fixture {
    dir: A01Dir,
    store: ProductStore,
    identity: A01TaskStoreIdentity,
    clock: Arc<A01Clock>,
    session: String,
    admin: String,
    service: foundry_ledger::ProductServiceContext,
    authorization: A01AuthorizeTaskRequest,
}
impl A01Fixture {
    fn new() -> Self {
        use foundry_ledger::{ProductCapability, ProductServiceAction, ProductServiceGrant, ScopeGrantManifest, ScopeSourceManifest, RepositoryIdentity};
        use foundry_ledger::product_identity::ProductPasswordKdf;
        let dir=A01Dir::new();
        let clock=Arc::new(A01Clock(std::sync::Mutex::new(a01_now())));
        let at=clock.now();
        let (mut store,identity)=must(ProductStore::create_a01_task_claim_test_store(&dir.0,"store.db",config(),clock.clone()));
        let source=ScopeSourceManifest{connector_id:"gh".into(),source_instance_id:"task-source".into(),generation:1,
            identity:must(RepositoryIdentity::canonicalized("acme","GitHub","https://api.github.com","42")),
            api_owner:Some("acme".into()),api_repo:Some("project".into()),legacy_repository_locator:"acme/project".into(),scope_query:"open".into(),enabled:true};
        let grants=[ScopeGrantManifest{actor_id:"owner".into(),source_instance_id:"task-source".into(),capabilities:vec![ProductCapability::ReadOrder,ProductCapability::ReadReview,ProductCapability::ResolveReviewHold,ProductCapability::ReadArtifact]}];
        must(store.bootstrap_owner_scoped("owner",&must(ProductPasswordKdf::hash("task-profile-fixture-password")),&[source],&grants));
        let session=must(store.login("owner","task-profile-fixture-password",at)).digest().to_owned();
        must(store.create_account(&session,"bridge-admin","owner",&must(ProductPasswordKdf::hash("task-admin-fixture-password")),at));
        let admin=must(store.login("bridge-admin","task-admin-fixture-password",at)).digest().to_owned();
        let db=a01_db(store.path());
        let scope_revision:i64=must(db.query_row("SELECT revision FROM product_scope_subject_revisions_v1 WHERE subject_id='owner' AND source_instance_id='task-source'",[],|r|r.get(0)));
        drop(db);
        must(store.change_product_scope(&admin,"owner","task-source",ProductCapability::A01BridgePassed,foundry_ledger::ScopeMutation::Grant,scope_revision,"bridge-grant",at));
        let bridge_permission=must(must(store.read_a01_bridge_permission(&session,"task-source")).ok_or(()));
        let grants:Vec<_>=[ProductServiceAction::IntakeRead,ProductServiceAction::AdmitDelivery,ProductServiceAction::AckDelivery,ProductServiceAction::DriveReview]
            .into_iter().map(|action|ProductServiceGrant{source_instance_id:"task-source".into(),action}).collect();
        must(store.manage_product_service(&session,"task-service",&grants,true,at+Duration::hours(4),0,"service",at));
        let credential=must(store.issue_product_service_credential(&session,"task-service",1,at+Duration::hours(3),"credential",at));
        let service=must(store.authenticate_product_service_for_action(credential.expose_once_to_file_writer(),"gh","https://api.github.com","42",ProductServiceAction::DriveReview,at));
        let payload=r#"{"issue_number":17,"title":"A bounded task","body":"original input"}"#;
        let updated=at.to_rfc3339();
        let version=foundry_ledger::product::delivery_version_digest("issue:17",&updated,"github-issue",payload);
        let delivery=foundry_ledger::ProductDelivery{delivery_id:foundry_ledger::product::github_delivery_id(&config().installation_id,"gh","https://api.github.com","42","issue:17",&version),connector_id:"gh".into(),repository_id:"42".into(),issue_id:"issue:17".into(),observed_updated_at:updated.clone(),version_digest:version,task_kind:"github-issue".into(),payload:payload.into()};
        let cursor=foundry_ledger::ProductPollCursor{connector_id:"gh".into(),query_fingerprint:"task-query".into(),sweep_id:"task-sweep".into(),next_page:1,page_ceiling:1,updated_at:updated};
        must(store.persist_intake_page_for_service(&service,&cursor,&[delivery.clone()],&[],at));
        let receipt=must(store.admit_intake_delivery_for_service(&service,&delivery.delivery_id,"task-order",at));
        must(store.mark_intake_acked_for_service(&service,&delivery.delivery_id,&receipt,at));
        let view=must(store.read_scoped_order(&session,&receipt.order_id,at));
        let db=a01_db(store.path());
        let account_revision=must(db.query_row("SELECT revision FROM product_accounts_v1 WHERE actor_id='owner'",[],|r|r.get(0)));
        let (source_generation,source_instance_revision,connector_revision,provider,api_origin,repository_id,api_owner,api_repo):(i64,i64,i64,String,String,String,String,String)=must(db.query_row("SELECT i.generation,i.revision,s.revision,r.provider,r.api_origin,r.stable_repository_id,l.api_owner,l.api_repo FROM product_scope_source_instances_v1 i JOIN product_scope_repositories_v1 r USING(repository_key) JOIN product_scope_api_locators_v1 l USING(source_instance_id) JOIN product_sources_v1 s ON s.connector_id=i.connector_id WHERE i.source_instance_id='task-source'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))));
        let (service_revision,credential_revision,drive_review_grant_revision):(i64,i64,i64)=must(db.query_row("SELECT s.revision,c.credential_revision,g.grant_revision FROM product_services_v1 s JOIN product_service_credentials_v1 c USING(service_id) JOIN product_service_grants_v1 g USING(service_id) WHERE g.action='drive_review'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))));
        let raw_payload_digest=a01_raw_digest(&delivery.task_kind,&delivery.payload);
        let authorization=A01AuthorizeTaskRequest{bridge_permission,request_key:"authorize".into(),expected_revision:0,
            admission:A01TaskAdmissionExpectation{store:A01TaskStoreRef{installation_id:config().installation_id,database_id:config().database_id},order_id:receipt.order_id,lifecycle:receipt.lifecycle,delivery_id:delivery.delivery_id,admission_seq:receipt.sequence,admission_hash:receipt.event_hash,raw_kind:delivery.task_kind,raw_payload_digest,source_instance_id:view.source_instance_id,source_generation,source_instance_revision,connector_id:"gh".into(),connector_revision,repository_key:view.repository_key,repository_provider:provider,api_origin,stable_repository_id:repository_id,api_owner,api_repo},
            owner:A01TaskOwnerExpectation{account_revision,scope_revision:view.scope_revision,read_order_grant_revision:view.grant_revision},
            service:A01TaskServiceExpectation{service_id:"task-service".into(),service_revision,credential_revision,drive_review_grant_revision,service_lease_until_ns:must((at+Duration::hours(4)).timestamp_nanos_opt().ok_or(())),credential_expires_at_ns:must((at+Duration::hours(3)).timestamp_nanos_opt().ok_or(()))},
            expires_at_ns:must((at+Duration::hours(2)).timestamp_nanos_opt().ok_or(()))};
        drop(db);
        Self{dir,store,identity,clock,session,admin,service,authorization}
    }
    fn authorize(&mut self) -> foundry_ledger::product::A01TaskReceipt {
        must(self.store.authorize_a01_task(&self.session,&self.service,&self.authorization))
    }
    fn claim(&mut self) -> foundry_ledger::ProductWorkClaim {
        match must(self.store.claim_product_work_for_service(&self.service,"task-worker",self.clock.now()-Duration::days(1),Duration::minutes(10))) {
            foundry_ledger::ProductWorkClaimResult::Claimed(c)=>c,_=>panic!("A01 task claim missing"),
        }
    }
    fn revoke_request(&self, revision: i64) -> A01RevokeTaskRequest {
        A01RevokeTaskRequest{request_key:format!("revoke-{revision}"),expected_revision:revision,admission:self.authorization.admission.clone(),owner:self.authorization.owner.clone()}
    }
    fn reopen(self) -> Self {
        let Self{dir,store,identity,clock,session,admin,service,authorization}=self;
        drop(store);
        let store=must(ProductStore::reopen_a01_task_claim_test_store(&dir.0,"store.db",config(),clock.clone(),&identity));
        Self{dir,store,identity,clock,session,admin,service,authorization}
    }
    fn empty(&mut self) {
        assert!(matches!(must(self.store.claim_product_work_for_service(&self.service,"another-worker",self.clock.now(),Duration::minutes(5))),foundry_ledger::ProductWorkClaimResult::Empty),"A01 duplicate acquisition");
    }
}

const A01_PACK_DIGEST: &str = "e925fd014d367c4c7f3d83fe92d03cd4bc4d3b7cbc1ecdf0d4e77320acab1db8";

fn config() -> ProductConfig { ProductConfig{installation_id:"a01-gates-install".into(),database_id:"a01-gates-db".into()} }
fn a01_pack() -> PathBuf { must(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1").canonicalize()) }
fn count_a01(path: &Path, table: &str) -> i64 { must(a01_db(path).query_row(&format!("SELECT count(*) FROM {table}"),[],|r|r.get(0))) }
fn bridge_count(path: &Path) -> i64 { must(a01_db(path).query_row("SELECT count(*) FROM ledger WHERE json_valid(intent) AND json_extract(intent,'$.kind')='a01-passed-bridge-v1'",[],|r|r.get(0))) }
fn scope_snapshot(path: &Path) -> BTreeMap<String,Vec<Vec<Value>>> { snapshot(path).into_iter().filter(|(k,_)|k.starts_with("product_scope_")).collect() }
fn mutate_bridge(store: &mut ProductStore, admin: &str, mutation: ScopeMutation, key: &str, at: DateTime<Utc>) {
    let rev:i64=must(a01_db(store.path()).query_row("SELECT revision FROM product_scope_subject_revisions_v1 WHERE subject_id='owner' AND source_instance_id='task-source'",[],|r|r.get(0)));
    must(store.change_product_scope(admin,"owner","task-source",ProductCapability::A01BridgePassed,mutation,rev,key,at));
}
impl A01Fixture {
    fn checked(&self) -> ProductStore { must(ProductStore::reopen_a01_task_claim_test_store(&self.dir.0,"store.db",config(),self.clock.clone(),&self.identity)) }
    fn backend(&self) -> ProductStore {
        let mut store=self.checked();
        must(store.configure_review_backend(foundry_ledger::ProductReviewConfig{artifact_root:self.dir.0.join("artifacts"),session_root:self.dir.0.join("sessions"),claim_root:self.dir.0.join("claims"),pack_digest:A01_PACK_DIGEST.into()}));store
    }
    fn host(&self, executor: A01Stages) -> foundry_line::PackReviewHost {
        must(must(foundry_line::PackReviewHost::with_product_store(a01_pack(),executor,self.checked(),self.dir.0.join("artifacts"),self.dir.0.join("sessions"),self.dir.0.join("claims"))).with_source_commit("a".repeat(40)))
    }
    fn revoke_bridge(&self) { mutate_bridge(&mut self.checked(),&self.admin,ScopeMutation::Revoke,"bridge-revoke",self.clock.now()); }
    fn accept_output(&mut self) -> (ProductWorkClaim,A01AcceptedProducerOutput,A01Producer) {
        let task=self.authorize();let claim=self.claim();
        let reservation=must(self.store.reserve_a01_producer(&self.service,&claim,task.revision,"producer-start"));
        let mut producer=A01Producer::new();let output=must(producer.produce(&task.task_bytes,&claim,&reservation));
        must(self.store.accept_a01_producer_output(&self.service,&claim,&reservation,&output));
        let accepted=must(must(self.checked().read_a01_producer_output(&self.service,&claim,&reservation)).ok_or(()));
        (claim,accepted,producer)
    }
    fn dispatch(&mut self, stages: A01Stages) -> (ProductWorkClaim,a01_coordinator::AcceptedDispatch,A01Producer) {
        let task=self.authorize();let claim=self.claim();let mut host=self.host(stages);let mut producer=A01Producer::new();
        let accepted=must(a01_coordinator::dispatch(&mut self.store,&self.service,&claim,task.revision,"producer-start",&mut producer,&mut host));
        (claim,accepted,producer)
    }
    fn expected(output: &A01AcceptedProducerOutput, host: &foundry_exec::HostResult) -> foundry_ledger::ProductResultExpectation {
        foundry_ledger::ProductResultExpectation{host_key:foundry_exec::HostKey{order:output.intent.order.0.clone(),attempt:output.intent.attempt.0,run:output.intent.run.0,activation:output.intent.activation},host_result_digest:must(foundry_ledger::product_host_result_digest(host))}
    }
}
struct A01Producer { calls:usize, observed:Option<String>, available:bool, fail:bool }
impl A01Producer { fn new() -> Self {Self{calls:0,observed:None,available:true,fail:false}} }
impl Producer for A01Producer {
    fn ensure_available(&self) -> Result<(),String> {if self.available {Ok(())} else {Err("synthetic unavailable".into())}}
    fn produce(&mut self, task: &[u8], claim: &ProductWorkClaim, reservation: &A01ProducerReservation) -> Result<String,String> {
        assert!(task==reservation.task.task_bytes && claim.order_id()==reservation.claim.order_id && claim.review_binding().fence==reservation.claim.fence,"A01 producer input mismatch");
        self.calls+=1;if self.fail {return Err("synthetic producer failure".into());}
        let mut bytes=[0u8;32];rand_core::OsRng.try_fill_bytes(&mut bytes).map_err(|_|"synthetic nonce failed")?;
        let output=format!("accepted producer output {}",hex::encode(bytes));self.observed=Some(output.clone());Ok(output)
    }
}
type A01CompletionObserver=Box<dyn FnMut(&foundry_exec::ReviewDispatchCompletion)->Result<(),String> + Send>;
struct A01Stages { available:bool, held:bool, calls:Arc<std::sync::atomic::AtomicUsize>, before:Option<A01CompletionObserver> }
impl A01Stages {fn new() -> Self {Self{available:true,held:false,calls:Arc::new(std::sync::atomic::AtomicUsize::new(0)),before:None}}}
impl foundry_line::StageExecutor for A01Stages {
    fn ensure_available(&self) -> Result<(),String> {if self.available {Ok(())} else {Err("synthetic unavailable".into())}}
    fn execute(&mut self, task: &foundry_line::StageTask) -> Result<foundry_line::StageOutput,String> {
        self.calls.fetch_add(1,Ordering::SeqCst);
        let path=task.workdir.join("accepted.log");std::fs::write(&path,b"accepted stage evidence\n").map_err(|_|"stage output failed")?;
        Ok(foundry_line::StageOutput{log_path:path.to_string_lossy().into_owned(),inspected_commit:Some(task.artifact_commit.clone()),findings:self.held && task.role=="verify",produced_commit:(task.role=="fix").then(||task.artifact_commit.clone()),..Default::default()})
    }
    fn restore_session(&mut self,_session: &str,_path: &Path) -> Result<(),String> {Ok(())}
    fn before_terminal_commit(&mut self, completion: &foundry_exec::ReviewDispatchCompletion) -> Result<(),String> {match self.before.as_mut(){Some(f)=>f(completion),None=>Ok(())}}
}
fn assert_no_result(f: &A01Fixture) {
    assert!(count_a01(f.store.path(),"artifact_tuples")==0 && count_a01(f.store.path(),"product_result_bindings_v1")==0,"A01 premature result binding");
}
fn terminal_state(f: &A01Fixture) -> (String,String,String) { must(a01_db(f.store.path()).query_row("SELECT r.status,c.state,b.state FROM product_review_runs_v1 r JOIN product_work_claims_v1 c ON c.order_id=r.order_id JOIN product_work_backlog_v1 b ON b.order_id=c.order_id AND b.lifecycle=c.lifecycle ORDER BY r.rowid DESC LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))) }

// G01–G10 retain the positive chain and fail-closed assembly checks.
#[test]
fn a01_g01_raw_admission_and_fixed_task_are_distinct() {
    let mut f=A01Fixture::new();let before=snapshot(f.store.path());let r=f.authorize();let after=snapshot(f.store.path());
    for table in ["product_orders_v1","product_intake_inbox_v1","product_order_payload_digests_v1"] {assert!(before[table]==after[table],"A01 original admission changed");}
    assert!(r.task_kind!=r.admission.raw_kind && !r.task_bytes.is_empty(),"A01 fixed task missing");
    assert!(count_a01(f.store.path(),"product_a01_task_revisions_v1")==1,"A01 authorization missing");
}
#[test]
fn a01_g02_real_claim_and_one_shot_reservation() {
    let mut f=A01Fixture::new();let task=f.authorize();let claim=f.claim();let r=must(f.store.reserve_a01_producer(&f.service,&claim,task.revision,"start"));
    assert!(r.claim.worker_id==claim.review_binding().worker_id && r.claim.fence==claim.review_binding().fence && r.claim.epoch==claim.review_binding().epoch && r.attempt==1 && r.run==1 && r.activation==1,"A01 coordinates mismatch");
    let before=snapshot(f.store.path());assert!(f.store.reserve_a01_producer(&f.service,&claim,task.revision,"again").is_err(),"A01 duplicate reservation accepted");assert_snapshots_equal(&before,&snapshot(f.store.path()));
}
#[test]
fn a01_g03_output_drives_intent_and_generation_zero() {
    let mut f=A01Fixture::new();let (_,a,p)=f.accept_output();
    assert!(p.calls==1 && p.observed.as_deref()==Some(a.output.as_str()),"A01 output not observed");
    assert!(a.intent.generation.0==0 && a.intent.epoch.0==a.reservation.claim.epoch as u64 && a.intent.attempt.0==a.reservation.attempt && a.intent.external_ref==format!("review:{}:{}:{}..{}",a.intent.order.0,a.intent.attempt.0,a.base_commit,a.result_commit),"A01 intent derivation mismatch");
    assert!(a.base_commit!=a.result_commit && a.base_commit.len()==64 && a.result_commit.len()==64,"A01 producer identifiers invalid");
}
#[test]
fn a01_g04_real_stages_sessions_and_artifacts() {
    let mut f=A01Fixture::new();let stages=A01Stages::new();let calls=stages.calls.clone();let (_,d,_)=f.dispatch(stages);
    assert!(d.host.exit_code==0 && d.host.held.is_none() && d.host.generations[0].commit==d.output.result_commit,"A01 real host did not pass");
    for table in ["product_review_stages_v1","product_review_sessions_v1","product_artifacts_v1"] {assert!(count_a01(f.store.path(),table)>0,"A01 accepted evidence missing");}
    assert!(calls.load(Ordering::SeqCst)>0,"A01 executor unused");assert_no_result(&f);
}
#[test]
fn a01_g05_bridge_is_atomic_and_trigger_receipt_consumes_intent() {
    let mut f=A01Fixture::new();let (_,d,_)=f.dispatch(A01Stages::new());let db=a01_db(f.store.path());
    let exact:bool=must(db.query_row("SELECT EXISTS(SELECT 1 FROM ledger l JOIN product_transition_receipts_v1 r ON r.seq=l.seq AND r.event_hash=l.hash WHERE r.operation='a01-passed-bridge-v1' AND l.event='InspectionPassed' AND l.to_state='waiting' AND r.expected_predecessor=json_extract(l.intent,'$.predecessor_seq'))",[],|r|r.get(0)));
    assert!(exact && bridge_count(f.store.path())==1 && count_a01(f.store.path(),"product_transition_intents_v1")==0,"A01 bridge receipt invalid");
    assert!(terminal_state(&f)==("passed".into(),"completed".into(),"completed".into()),"A01 terminal work not completed");assert_no_result(&f);
    assert!(d.output.intent.order.0=="task-order","A01 order mismatch");
}
#[test]
fn a01_g06_finalizer_replay_preserves_snapshot() {
    let mut f=A01Fixture::new();let (_,d,_)=f.dispatch(A01Stages::new());let expected=A01Fixture::expected(&d.output,&d.host);let mut store=f.backend();
    let first=must(store.finalize_product_review_result(&f.service,&expected));let before=snapshot(f.store.path());let second=must(store.finalize_product_review_result(&f.service,&expected));
    assert!(first==second,"A01 finalizer replay mismatch");assert_snapshots_equal(&before,&snapshot(f.store.path()));
}
#[test]
fn a01_g07_unavailable_producer_never_reserves_or_invokes() {
    let mut f=A01Fixture::new();let task=f.authorize();let claim=f.claim();let mut host=f.host(A01Stages::new());let mut producer=A01Producer::new();producer.available=false;let before=snapshot(f.store.path());
    assert!(a01_coordinator::dispatch(&mut f.store,&f.service,&claim,task.revision,"start",&mut producer,&mut host).is_err(),"A01 unavailable producer accepted");assert!(producer.calls==0,"A01 unavailable producer invoked");assert_snapshots_equal(&before,&snapshot(f.store.path()));
}
#[test]
fn a01_g08_unavailable_executor_never_reserves_or_invokes() {
    let mut f=A01Fixture::new();let task=f.authorize();let claim=f.claim();let mut stages=A01Stages::new();stages.available=false;let mut host=f.host(stages);let mut producer=A01Producer::new();let before=snapshot(f.store.path());
    assert!(a01_coordinator::dispatch(&mut f.store,&f.service,&claim,task.revision,"start",&mut producer,&mut host).is_err(),"A01 unavailable executor accepted");assert!(producer.calls==0,"A01 producer invoked before executor preflight");assert_snapshots_equal(&before,&snapshot(f.store.path()));
}
#[test]
fn a01_g09_producer_failure_and_invalid_outputs_do_not_reach_host() {
    let mut f=A01Fixture::new();let task=f.authorize();let claim=f.claim();let mut producer=A01Producer::new();producer.fail=true;let mut host=f.host(A01Stages::new());
    assert!(a01_coordinator::dispatch(&mut f.store,&f.service,&claim,task.revision,"start",&mut producer,&mut host).is_err(),"A01 producer failure swallowed");
    assert!(producer.calls==1 && count_a01(f.store.path(),"product_review_runs_v1")==0 && count_a01(f.store.path(),"product_review_redrive_v1")==0,"A01 producer failure executed host");
    let mut f=A01Fixture::new();let task=f.authorize();let claim=f.claim();let r=must(f.store.reserve_a01_producer(&f.service,&claim,task.revision,"start"));
    for output in [String::new(),"x".repeat(4097),"x\0y".into()] {let before=snapshot(f.store.path());assert!(f.store.accept_a01_producer_output(&f.service,&claim,&r,&output).is_err(),"A01 invalid output accepted");assert_snapshots_equal(&before,&snapshot(f.store.path()));}
}
#[test]
fn a01_g10_ordinary_store_and_forged_claim_are_rejected() {
    let f=A01Fixture::new();let dir=A01Dir::new();let mut ordinary=must(ProductStore::open(&dir.0.join("ordinary.db"),config()));let before=snapshot(ordinary.path());
    assert!(ordinary.authorize_a01_task(&f.session,&f.service,&f.authorization).is_err(),"A01 ordinary store accepted task");assert_snapshots_equal(&before,&snapshot(ordinary.path()));
    let mut f=f;let task=f.authorize();let claim=f.claim();must(must(Connection::open(f.store.path())).execute("UPDATE product_work_claims_v1 SET fence=fence+1",[]));let before=snapshot(f.store.path());assert!(f.store.reserve_a01_producer(&f.service,&claim,task.revision,"start").is_err(),"A01 forged claim accepted");assert_snapshots_equal(&before,&snapshot(f.store.path()));
}
#[test]
fn a01_g11_bridge_permission_is_separately_issued_before_authorization() {
    let f=A01Fixture::new();let db=a01_db(f.store.path());let p=&f.authorization.bridge_permission;
    let exact:bool=must(db.query_row("SELECT EXISTS(SELECT 1 FROM product_scope_grants_v1 g JOIN product_scope_events_v1 e ON e.subject_id=g.subject_id AND e.source_instance_id=g.source_instance_id AND e.capability=g.capability AND e.grant_revision=g.grant_revision JOIN product_scope_operations_v1 o ON o.actor_id=e.actor_id AND o.request_key=e.request_key WHERE g.capability='a01_bridge_passed' AND g.enabled=1 AND g.grant_id=?1 AND g.grant_revision=?2 AND g.subject_id=?3 AND g.source_instance_id=?4 AND e.operation='grant')",rusqlite::params![p.grant_id,p.grant_revision,p.subject_id,p.source_instance_id],|r|r.get(0)));
    assert!(exact,"A01 separately issued permission missing");
    for table in ["product_a01_task_revisions_v1","product_work_claims_v1","product_review_runs_v1","product_review_host_results_v1","product_review_redrive_v1","artifact_tuples","product_result_bindings_v1"] {assert!(count_a01(f.store.path(),table)==0,"A01 premature execution evidence");}
    let ready:bool=must(db.query_row("SELECT EXISTS(SELECT 1 FROM product_work_backlog_v1 WHERE state='ready')",[],|r|r.get(0)));assert!(!ready,"A01 permission issuance made work ready");
}
#[test]
fn a01_g12_permission_task_claim_output_and_bridge_bind_exactly() {
    let mut f=A01Fixture::new();let (_,d,_)=f.dispatch(A01Stages::new());let task=&d.output.reservation.task;
    assert!(task.bridge_permission==f.authorization.bridge_permission && task.admission==f.authorization.admission && task.service==f.authorization.service,"A01 task binding mismatch");
    let bytes:String=must(a01_db(f.store.path()).query_row("SELECT intent FROM ledger WHERE json_valid(intent) AND json_extract(intent,'$.kind')='a01-passed-bridge-v1'",[],|r|r.get(0)));
    let v:serde_json::Value=must(serde_json::from_str(&bytes));
    assert!(v["task"]==must(serde_json::to_value(task)) && v["claim"]==must(serde_json::to_value(&d.output.reservation.claim)) && v["output_sequence"]==d.output.sequence && v["output_event_hash"]==d.output.event_hash,"A01 bridge provenance mismatch");
}
#[test]
fn a01_g13_authorize_does_not_issue_or_mutate_scope_grants() {
    let mut f=A01Fixture::new();let before=scope_snapshot(f.store.path());f.authorize();assert_snapshots_equal(&before,&scope_snapshot(f.store.path()));
}
#[test]
fn a01_g14_revocation_before_first_finalizer_preserves_evidence() {
    let mut f=A01Fixture::new();let (_,d,_)=f.dispatch(A01Stages::new());assert_no_result(&f);f.revoke_bridge();let before=snapshot(f.store.path());
    assert!(matches!(f.backend().finalize_product_review_result(&f.service,&A01Fixture::expected(&d.output,&d.host)),Err(foundry_ledger::ProductResultError::Conflict)),"A01 revoked finalizer accepted");assert_no_result(&f);assert_snapshots_equal(&before,&snapshot(f.store.path()));
    drop(f.checked()); // Historical reopening must not demand current permission.
}

fn guard_payload(f: &A01Fixture, d: &a01_coordinator::AcceptedDispatch) -> serde_json::Value {
    let a=&d.output;let r=&a.reservation;let t=&r.task;let c=&r.claim;let s=&t.service;let ad=&t.admission;
    let (seq,hash):(i64,String)=must(a01_db(f.store.path()).query_row("SELECT seq,hash FROM ledger WHERE order_id=?1 AND event='InspectionPassed' ORDER BY seq DESC LIMIT 1",[&c.order_id],|v|Ok((v.get(0)?,v.get(1)?))));
    serde_json::json!({"s1_sequence":seq,"s1_event_hash":hash,"host_result":d.host,"work":{
        "delivery_id":c.delivery_id,"worker_id":c.worker_id,"claim_epoch":c.epoch,"claim_fence":c.fence,"source_revision":c.source_revision,"payload_digest":c.payload_digest,
        "authority":{"service_id":s.service_id,"service_revision":s.service_revision,"credential_revision":s.credential_revision,"drive_review_grant_revision":s.drive_review_grant_revision,"source_instance_id":ad.source_instance_id,"source_generation":ad.source_generation,"source_instance_revision":ad.source_instance_revision,"connector_revision":ad.connector_revision}}})
}
// These probes intentionally use the DB boundary, separately from finalizer
// errors. The tuple is inserted first in the same transaction, then ROLLBACK
// must restore every table, including any deliberate evidence substitution.
fn guard_probe(f: &A01Fixture, d: &a01_coordinator::AcceptedDispatch, mut payload: serde_json::Value, variant: &str, expected: &str) {
    let before=snapshot(f.store.path());let c=&d.output.reservation.claim;let i=&d.output.intent;
    let db=must(Connection::open(f.store.path()));must(db.pragma_update(None,"foreign_keys",1));must(db.pragma_update(None,"recursive_triggers",1));must(db.execute_batch("BEGIN IMMEDIATE"));
    let mut order=c.order_id.clone();let mut digest=must(foundry_ledger::product_host_result_digest(&d.host));
    match variant {
        "no-task"=>order="unbound-task".into(),
        "no-provenance"=>payload["s1_sequence"]=0.into(),
        "host"=>digest="different-host-digest".into(),
        "service"=>payload["work"]["authority"]["service_revision"]=serde_json::json!(c.service_revision+1),
        "provenance"=>payload["s1_event_hash"]="different-event".into(),
        "task"=>{
            let mut task=must(serde_json::to_value(&d.output.reservation.task));task["service"]["service_id"]="different-service".into();
            let trigger:String=must(db.query_row("SELECT name FROM sqlite_schema WHERE type='trigger' AND tbl_name='product_a01_task_revisions_v1' AND sql LIKE '%BEFORE UPDATE%'",[],|r|r.get(0)));
            must(db.execute_batch(&format!("DROP TRIGGER \"{}\"",trigger.replace('"',"\"\""))));
            must(db.execute("UPDATE product_a01_task_revisions_v1 SET receipt_bytes=CAST(?1 AS BLOB)",[must(serde_json::to_string(&task))]));
        },
        _=>{},
    }
    must(db.execute("INSERT INTO artifact_tuples(order_id,attempt,run_id,generation,immutable_commit,artifact_location,generations_json) VALUES(?1,?2,?3,0,?4,?5,?6)",rusqlite::params![order,i.attempt.0 as i64,i.run.0 as i64,d.host.result_commit,d.host.log_path,must(serde_json::to_string(&d.host.generations))]));
    let result=db.execute("INSERT INTO product_result_bindings_v1(result_id,order_id,lifecycle,attempt,run_id,activation,generation,host_result_digest,tuple_digest,binding_digest,payload_json,receipt_json) VALUES('guard-probe',?1,?2,?3,?4,?5,0,?6,'probe-tuple','probe-binding',?7,'{}')",rusqlite::params![order,c.lifecycle,i.attempt.0 as i64,i.run.0 as i64,i.activation as i64,digest,must(serde_json::to_string(&payload))]);
    match result {Err(rusqlite::Error::SqliteFailure(_,Some(message)))=>assert!(message==expected,"A01 guard diagnostic mismatch"),_=>panic!("A01 guard failed to reject binding")}
    assert!(db.is_autocommit(),"A01 guard did not roll back transaction");drop(db);assert_snapshots_equal(&before,&snapshot(f.store.path()));assert_no_result(f);
}
#[test]
fn a01_g15_guard_reports_revoked_and_rolls_back_tuple_and_binding() {
    let mut f=A01Fixture::new();let (_,d,_)=f.dispatch(A01Stages::new());let payload=guard_payload(&f,&d);f.revoke_bridge();guard_probe(&f,&d,payload,"revoked","A01 bridge permission revoked");
}
#[test]
fn a01_g16_permission_and_provenance_identity_mismatches_are_atomic() {
    let mut f=A01Fixture::new();
    for which in 0..4 {let mut request=f.authorization.clone();match which {0=>request.bridge_permission.grant_id="other".into(),1=>request.bridge_permission.grant_revision+=1,2=>request.bridge_permission.subject_id="other".into(),_=>request.bridge_permission.source_instance_id="other".into()};let before=snapshot(f.store.path());assert!(f.store.authorize_a01_task(&f.session,&f.service,&request).is_err(),"A01 substituted permission accepted");assert_snapshots_equal(&before,&snapshot(f.store.path()));}
    let (_,d,_)=f.dispatch(A01Stages::new());
    for variant in ["host","task","service","provenance"] {guard_probe(&f,&d,guard_payload(&f,&d),variant,"A01 bridge permission identity mismatch");}
}
#[test]
fn a01_g17_unbound_task_and_missing_bridge_provenance_are_distinct_probes() {
    let mut f=A01Fixture::new();let (_,d,_)=f.dispatch(A01Stages::new());
    guard_probe(&f,&d,guard_payload(&f,&d),"no-task","A01 bridge permission not bound");
    guard_probe(&f,&d,guard_payload(&f,&d),"no-provenance","A01 bridge permission not bound");
}
#[test]
fn a01_g18_revoke_regrant_aba_does_not_revive_old_task_or_bridge() {
    let mut f=A01Fixture::new();let (_,d,_)=f.dispatch(A01Stages::new());let old=f.authorization.bridge_permission.clone();f.revoke_bridge();mutate_bridge(&mut f.checked(),&f.admin,ScopeMutation::Grant,"bridge-regrant",f.clock.now());
    let new=must(must(f.checked().read_a01_bridge_permission(&f.session,"task-source")).ok_or(()));assert!(new.grant_id!=old.grant_id && new.grant_revision>old.grant_revision,"A01 ABA identity unchanged");
    let before=snapshot(f.store.path());assert!(matches!(f.backend().finalize_product_review_result(&f.service,&A01Fixture::expected(&d.output,&d.host)),Err(foundry_ledger::ProductResultError::Conflict)),"A01 ABA finalizer accepted");assert_snapshots_equal(&before,&snapshot(f.store.path()));guard_probe(&f,&d,guard_payload(&f,&d),"revoked","A01 bridge permission revoked");
}
#[test]
fn a01_g19_revoke_after_last_accepted_stage_keeps_terminal_evidence_only() {
    let mut f=A01Fixture::new();let mut revoker=f.checked();let admin=f.admin.clone();let at=f.clock.now();let accepted=Arc::new(std::sync::Mutex::new(None));let observed=accepted.clone();let mut stages=A01Stages::new();
    stages.before=Some(Box::new(move |completion| {
        assert!(completion.status==foundry_exec::ReviewTerminalStatus::Passed,"A01 callback is not Passed");
        let evidence:BTreeMap<_,_>=snapshot(revoker.path()).into_iter().filter(|(k,_)|matches!(k.as_str(),"product_review_stages_v1"|"product_review_sessions_v1"|"product_artifacts_v1")).collect();
        assert!(evidence.values().all(|v|!v.is_empty()),"A01 callback precedes stage acceptance");*must(observed.lock())=Some(evidence);
        mutate_bridge(&mut revoker,&admin,ScopeMutation::Revoke,"after-stages",at);Ok(())
    }));
    let (_,d,_)=f.dispatch(stages);assert!(d.host.exit_code==0 && d.host.held.is_none(),"A01 terminal result lost");
    let after:BTreeMap<_,_>=snapshot(f.store.path()).into_iter().filter(|(k,_)|matches!(k.as_str(),"product_review_stages_v1"|"product_review_sessions_v1"|"product_artifacts_v1")).collect();assert_snapshots_equal(must(accepted.lock()).as_ref().unwrap(),&after);
    assert!(terminal_state(&f)==("passed".into(),"completed".into(),"completed".into()) && bridge_count(f.store.path())==0,"A01 revoked completion created bridge or lost completion");assert_no_result(&f);drop(f.checked());
}
#[test]
fn a01_g20_held_preserves_claim_and_evidence_with_or_without_revocation() {
    for revoke in [false,true] {let mut f=A01Fixture::new();let mut stages=A01Stages::new();stages.held=true;
        if revoke {let mut revoker=f.checked();let admin=f.admin.clone();let at=f.clock.now();stages.before=Some(Box::new(move |_| {mutate_bridge(&mut revoker,&admin,ScopeMutation::Revoke,"held-revoke",at);Ok(())}));}
        let (_,d,_)=f.dispatch(stages);assert!(d.host.held.is_some(),"A01 Held result missing");assert!(terminal_state(&f)==("held".into(),"claimed".into(),"claimed".into()),"A01 Held claim changed");
        for table in ["product_review_holds_v1","product_review_stages_v1","product_review_sessions_v1","product_artifacts_v1"] {assert!(count_a01(f.store.path(),table)>0,"A01 Held evidence lost");}assert!(bridge_count(f.store.path())==0,"A01 Held created bridge");assert_no_result(&f);drop(f.checked());
    }
}
#[test]
fn a01_g21_failed_backend_terminal_keeps_real_accepted_evidence() {
    use foundry_exec::ReviewDispatchStore;
    let mut f=A01Fixture::new();let (claim,output,_)=f.accept_output();let captured=Arc::new(std::sync::Mutex::new(None));let observe=captured.clone();let mut stages=A01Stages::new();
    stages.before=Some(Box::new(move |c|{*must(observe.lock())=Some(c.clone());Err("stop before terminal commit".into())}));
    let mut host=f.host(stages);assert!(host.try_dispatch_product_work(&output.intent,&claim).is_err(),"A01 completion barrier failed");drop(host);
    let before:BTreeMap<_,_>=snapshot(f.store.path()).into_iter().filter(|(k,_)|matches!(k.as_str(),"product_review_stages_v1"|"product_review_sessions_v1"|"product_artifacts_v1")).collect();
    let mut completion=must(must(captured.lock()).take().ok_or(()));completion.status=foundry_exec::ReviewTerminalStatus::Failed;completion.result.exit_code=1;completion.held_snapshot=None;
    must(f.backend().complete_review_dispatch(completion));
    let after:BTreeMap<_,_>=snapshot(f.store.path()).into_iter().filter(|(k,_)|matches!(k.as_str(),"product_review_stages_v1"|"product_review_sessions_v1"|"product_artifacts_v1")).collect();assert_snapshots_equal(&before,&after);
    assert!(terminal_state(&f)==("failed".into(),"completed".into(),"completed".into()) && bridge_count(f.store.path())==0,"A01 Failed behavior changed");assert_no_result(&f);drop(f.checked());
}
#[test]
fn a01_g22_continuation_and_human_pass_bind_resolved_claimed_results() {
    for human_pass in [true,false] {
        let mut f=A01Fixture::new();let mut stages=A01Stages::new();stages.held=true;let (_,d,_)=f.dispatch(stages);let hold=d.host.held.as_ref().unwrap();
        let expected=must(a01_db(f.store.path()).query_row("SELECT lifecycle,attempt,originating_run,activation,host_epoch,hold_revision,source_revision,expected_predecessor,snapshot_digest FROM product_review_holds_v1",[],|r|Ok(foundry_ledger::ProductHoldExpectation{lifecycle:r.get(0)?,attempt:r.get(1)?,originating_run:r.get(2)?,activation:r.get(3)?,host_epoch:r.get(4)?,hold_revision:r.get(5)?,source_revision:r.get(6)?,predecessor_seq:r.get(7)?,snapshot_digest:r.get(8)?})));
        let decision=if human_pass {foundry_ledger::ProductHoldDecision::CommitPass} else {foundry_ledger::ProductHoldDecision::Resume(must(hold.resume_targets.first().cloned().ok_or(())))};
        let receipt=must(f.backend().accept_product_review_hold(&f.session,&d.output.intent.order.0,&hold.hold_instance_id,&expected,"human-accept",decision,f.clock.now()));
        let receipt_before=snapshot(f.store.path())["product_review_hold_acceptances_v1"].clone();
        let mut host=f.host(A01Stages::new());must(host.continue_accepted_product_hold(&f.session,&receipt.operation_id,f.clock.now()));drop(host);
        let db=a01_db(f.store.path());let host_result:foundry_exec::HostResult=must(serde_json::from_str(&must(db.query_row("SELECT result_json FROM product_review_host_results_v1 ORDER BY run_id DESC LIMIT 1",[],|r|r.get::<_,String>(0)))));
        let key=must(db.query_row("SELECT order_id,attempt,run_id,activation FROM product_review_host_results_v1 ORDER BY run_id DESC LIMIT 1",[],|r|Ok(foundry_exec::HostKey{order:r.get(0)?,attempt:r.get(1)?,run:r.get(2)?,activation:r.get(3)?})));
        let expected=foundry_ledger::ProductResultExpectation{host_key:key,host_result_digest:must(foundry_ledger::product_host_result_digest(&host_result))};
        assert!(terminal_state(&f)==("resolved".into(),"claimed".into(),"claimed".into()) && host_result.exit_code==0 && host_result.held.is_none(),"A01 continuation terminal semantics changed");
        assert!(snapshot(f.store.path())["product_review_hold_acceptances_v1"]==receipt_before,"A01 human decision receipt changed");let completed:bool=must(db.query_row("SELECT EXISTS(SELECT 1 FROM product_review_hold_progress_v1 WHERE operation_id=?1 AND phase='completed')",[&receipt.operation_id],|r|r.get(0)));assert!(completed,"A01 human progress lost");drop(db);
        assert_no_result(&f);must(f.backend().finalize_product_review_result(&f.service,&expected));assert!(count_a01(f.store.path(),"artifact_tuples")==1 && count_a01(f.store.path(),"product_result_bindings_v1")==1,"A01 continuation guard rejected valid result");drop(f.checked());
    }
}
#[test]
fn a01_g23_output_original_matches_observed_bytes_and_mirror_never_authorizes() {
    let mut f=A01Fixture::new();let (claim,accepted,producer)=f.accept_output();
    let original:String=must(a01_db(f.store.path()).query_row("SELECT json_extract(intent,'$.output') FROM ledger WHERE seq=?1",[accepted.sequence],|r|r.get(0)));
    assert!(producer.observed.as_deref()==Some(original.as_str()) && accepted.output==original,"A01 output bytes changed");
    // Pin durable review roots before the corruption snapshot; host setup below
    // must repeat this configuration without changing any database evidence.
    must(f.store.configure_review_backend(foundry_ledger::ProductReviewConfig{artifact_root:f.dir.0.join("artifacts"),session_root:f.dir.0.join("sessions"),claim_root:f.dir.0.join("claims"),pack_digest:A01_PACK_DIGEST.into()}));
    let mut mirror=accepted.clone();mirror.output.push_str("tampered");let db=must(Connection::open(f.store.path()));must(db.execute("UPDATE product_review_redrive_v1 SET artifact_json=?1 WHERE payload='a01-producer-output-v1'",[must(serde_json::to_string(&mirror))]));drop(db);
    let before=snapshot(f.store.path());assert!(f.store.read_a01_producer_output(&f.service,&claim,&accepted.reservation).is_err(),"A01 mirror substitution accepted");
    // The host uses a checked connection opened before the corruption, proving
    // its admission validation also refuses a mirror-only execution permit.
    let mut backend=f.store; // No reopen can repair the corrupted mirror.
    must(backend.configure_review_backend(foundry_ledger::ProductReviewConfig{artifact_root:f.dir.0.join("artifacts"),session_root:f.dir.0.join("sessions"),claim_root:f.dir.0.join("claims"),pack_digest:A01_PACK_DIGEST.into()}));
    let mut host=must(must(foundry_line::PackReviewHost::with_product_store(a01_pack(),A01Stages::new(),backend,f.dir.0.join("artifacts"),f.dir.0.join("sessions"),f.dir.0.join("claims"))).with_source_commit("a".repeat(40)));
    assert!(host.try_dispatch_product_work(&accepted.intent,&claim).is_err(),"A01 mirror authorized host");assert_snapshots_equal(&before,&snapshot(&f.dir.0.join("store.db")));
}
#[test]
fn a01_g24_separate_checked_finalizer_and_historical_reopen_preserve_lineage() {
    let mut f=A01Fixture::new();let (_,d,_)=f.dispatch(A01Stages::new());assert_no_result(&f);
    let mut separate=f.backend();must(separate.finalize_product_review_result(&f.service,&A01Fixture::expected(&d.output,&d.host)));drop(separate);
    assert!(count_a01(f.store.path(),"artifact_tuples")==1 && count_a01(f.store.path(),"product_result_bindings_v1")==1,"A01 finalizer did not bind exact pair");
    let before=snapshot(f.store.path());drop(f.checked());assert_snapshots_equal(&before,&snapshot(f.store.path()));
    f.revoke_bridge();let after_revoke=snapshot(f.store.path());drop(f.checked());assert_snapshots_equal(&after_revoke,&snapshot(f.store.path()));
}
