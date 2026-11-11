#![cfg(all(feature = "w1-isolated-test-support", unix))]

use foundry_ledger::{ProductConfig, ProductError, ProductStore, SystemProductClock, ScopeMigrationManifest, ScopeSourceManifest, RepositoryIdentity};
use rusqlite::Connection;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::sync::Arc;

fn snapshot(dir: &std::path::Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(dir).unwrap().map(|entry| {
        let entry = entry.unwrap();
        let bytes = if entry.file_type().unwrap().is_dir() { Vec::new() } else { std::fs::read(entry.path()).unwrap() };
        (entry.file_name().into_string().unwrap(), bytes)
    }).collect()
}

// Include every directory entry and file bytes; do not follow symlinks.
fn recursive_snapshot(dir: &std::path::Path) -> std::collections::BTreeMap<PathBuf, (String, Vec<u8>)> {
    fn visit(root: &std::path::Path, current: &std::path::Path, out: &mut std::collections::BTreeMap<PathBuf, (String, Vec<u8>)>) {
        for entry in std::fs::read_dir(current).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let kind = entry.file_type().unwrap();
            let (label, bytes) = if kind.is_dir() { ("directory", Vec::new()) } else if kind.is_file() { ("file", std::fs::read(&path).unwrap()) } else if kind.is_symlink() { ("symlink", std::os::unix::ffi::OsStrExt::as_bytes(std::fs::read_link(&path).unwrap().as_os_str()).to_vec()) } else { panic!("unexpected filesystem entry: {}", path.display()) };
            out.insert(path.strip_prefix(root).unwrap().to_path_buf(), (label.into(), bytes));
            if kind.is_dir() { visit(root, &path, out); }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    visit(dir, dir, &mut out);
    out
}

struct PrivateDir(PathBuf);

impl PrivateDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "foundry-w1-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
}

impl Drop for PrivateDir {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

fn config() -> ProductConfig {
    ProductConfig { installation_id: "w1-install".into(), database_id: "w1-db".into() }
}

#[test]
fn isolated_create_rejects_each_preexisting_sidecar_without_mutation() {
    for (suffix, sentinel) in [
        ("-journal", b"journal sentinel".as_slice()),
        ("-wal", b"wal sentinel".as_slice()),
        ("-shm", b"shm sentinel".as_slice()),
    ] {
        let dir = PrivateDir::new();
        let sidecar = dir.0.join(format!("isolated.db{suffix}"));
        std::fs::write(&sidecar, sentinel).unwrap();
        assert!(sidecar.is_file());
        assert_eq!(std::fs::read(&sidecar).unwrap(), sentinel);
        assert!(!dir.0.join("isolated.db").exists());
        let before = snapshot(&dir.0);
        assert_eq!(before.len(), 1);
        assert!(matches!(
            ProductStore::create_isolated_test_store(&dir.0, "isolated.db", config()),
            Err(ProductError::InvalidConfig)
        ));
        assert_eq!(snapshot(&dir.0), before, "sidecar {suffix}");
    }
}

#[test]
fn isolated_reopen_rejects_each_preexisting_sidecar_without_mutation() {
    for (suffix, sentinel) in [
        ("-journal", b"reopen journal sentinel".as_slice()),
        ("-wal", b"reopen wal sentinel".as_slice()),
        ("-shm", b"reopen shm sentinel".as_slice()),
    ] {
        let path = PathBuf::from("/home/cosmo/.hermes/cache/scratch").join(format!(
            "foundry-w1-reopen-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        let dir = PrivateDir(path);
        let database = dir.0.join("isolated.db");
        drop(ProductStore::create_isolated_test_store(&dir.0, "isolated.db", config()).unwrap());
        assert!(database.is_file());
        for other in ["-journal", "-wal", "-shm"] {
            assert!(!dir.0.join(format!("isolated.db{other}")).exists());
        }
        let sidecar = dir.0.join(format!("isolated.db{suffix}"));
        std::fs::write(&sidecar, sentinel).unwrap();
        assert!(sidecar.is_file());
        assert_eq!(std::fs::read(&sidecar).unwrap(), sentinel);
        let before = snapshot(&dir.0);
        assert_eq!(before.len(), 2);
        let result = ProductStore::reopen_isolated_test_store(&dir.0, "isolated.db", config());
        assert_eq!(snapshot(&dir.0), before, "sidecar {suffix}");
        assert!(matches!(result, Err(ProductError::InvalidConfig)), "sidecar {suffix}");
    }
}

#[test]
fn isolated_create_reopen_and_normal_open_rejection() {
    let dir = PrivateDir::new();
    let path = dir.0.join("isolated.db");
    drop(ProductStore::create_isolated_test_store(&dir.0, "isolated.db", config()).unwrap());
    let conn = Connection::open(&path).unwrap();
    let marker: i64 = conn.query_row("SELECT version FROM product_w1_isolated_marker_v1 WHERE singleton=1", [], |row| row.get(0)).unwrap();
    assert_eq!(marker, 2);
    drop(conn);
    drop(ProductStore::reopen_isolated_test_store(&dir.0, "isolated.db", config()).unwrap());
    let before = snapshot(&dir.0);
    assert!(matches!(ProductStore::open(&path, config()), Err(ProductError::ContractMismatch)));
    assert_eq!(snapshot(&dir.0), before);
    assert!(matches!(ProductStore::open_with_clock(&path, config(), Arc::new(SystemProductClock)), Err(ProductError::ContractMismatch)));
    assert_eq!(snapshot(&dir.0), before);
    let ambiguous = dir.0.join("missing").join("..").join("isolated.db");
    assert!(matches!(ProductStore::open(&ambiguous, config()), Err(ProductError::InvalidConfig)));
    assert_eq!(snapshot(&dir.0), before);
    assert!(matches!(ProductStore::open_with_clock(&ambiguous, config(), Arc::new(SystemProductClock)), Err(ProductError::InvalidConfig)));
    assert_eq!(snapshot(&dir.0), before);
    std::fs::create_dir(dir.0.join("existing")).unwrap();
    let before = snapshot(&dir.0);
    let resolved = dir.0.join("existing").join("..").join("isolated.db");
    assert!(matches!(ProductStore::open(&resolved, config()), Err(ProductError::ContractMismatch)));
    assert_eq!(snapshot(&dir.0), before);
    assert!(matches!(ProductStore::open_with_clock(&resolved, config(), Arc::new(SystemProductClock)), Err(ProductError::ContractMismatch)));
    assert_eq!(snapshot(&dir.0), before);
}

#[test]
fn ordinary_nested_path_is_created() {
    let dir = PrivateDir::new();
    let path = dir.0.join("nested").join("deeper").join("ordinary.db");
    drop(ProductStore::open(&path, config()).unwrap());
    assert!(path.is_file());
}

#[test]
fn existing_file_and_unmarked_database_are_unchanged() {
    let dir = PrivateDir::new();
    let path = dir.0.join("ordinary.db");
    drop(ProductStore::open(&path, config()).unwrap());
    let before = snapshot(&dir.0);
    assert!(ProductStore::create_isolated_test_store(&dir.0, "ordinary.db", config()).is_err());
    assert_eq!(snapshot(&dir.0), before);
    assert!(matches!(ProductStore::reopen_isolated_test_store(&dir.0, "ordinary.db", config()), Err(ProductError::ContractMismatch)));
    assert_eq!(snapshot(&dir.0), before);
    drop(ProductStore::open(&path, config()).unwrap());
    let conn = Connection::open(&path).unwrap();
    let count: i64 = conn.query_row("SELECT count(*) FROM sqlite_master WHERE name LIKE 'product_w1_%'", [], |row| row.get(0)).unwrap();
    assert_eq!(count, 0);
}

#[test]
fn isolated_paths_reject_symlinks_and_existing_files() {
    let dir = PrivateDir::new();
    let target = dir.0.join("target");
    std::fs::write(&target, b"untouched").unwrap();
    std::os::unix::fs::symlink(&target, dir.0.join("link")).unwrap();
    assert!(ProductStore::create_isolated_test_store(&dir.0, "link", config()).is_err());
    assert!(ProductStore::reopen_isolated_test_store(&dir.0, "link", config()).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
    assert!(ProductStore::create_isolated_test_store(&dir.0, "../target", config()).is_err());
}

#[test]
fn private_directory_and_hardlink_guards() {
    use std::os::unix::fs::PermissionsExt;
    let dir = PrivateDir::new();
    let public = dir.0.join("public");
    std::fs::create_dir(&public).unwrap();
    std::fs::set_permissions(&public, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(ProductStore::create_isolated_test_store(&public, "db", config()), Err(ProductError::InvalidConfig)));
    let link = dir.0.join("directory-link");
    std::os::unix::fs::symlink(&dir.0, &link).unwrap();
    assert!(matches!(ProductStore::create_isolated_test_store(&link, "db", config()), Err(ProductError::InvalidConfig)));
    drop(ProductStore::create_isolated_test_store(&dir.0, "db", config()).unwrap());
    std::fs::hard_link(dir.0.join("db"), dir.0.join("alias")).unwrap();
    assert!(matches!(ProductStore::reopen_isolated_test_store(&dir.0, "db", config()), Err(ProductError::InvalidConfig)));
}

#[test]
fn migration_rejects_marked_v1_without_changes() {
    let dir = PrivateDir::new();
    let path = dir.0.join("v1.db");
    drop(ProductStore::create_isolated_test_store(&dir.0, "v1.db", config()).unwrap());
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("DROP TRIGGER product_result_meta_no_update;
             DROP TRIGGER product_result_meta_no_delete;
             DROP TRIGGER product_result_meta_no_replace;
             DROP TRIGGER product_result_legacy_hosts_no_insert;
             DROP TRIGGER product_result_legacy_hosts_no_update;
             DROP TRIGGER product_result_legacy_hosts_no_delete;
             DROP TRIGGER product_tuples_no_update;
             DROP TRIGGER product_tuples_no_delete;
             DROP TRIGGER product_tuples_no_replace;
             DROP TRIGGER product_result_legacy_tuples_no_insert;
             DROP TRIGGER product_result_legacy_tuples_no_update;
             DROP TRIGGER product_result_legacy_tuples_no_delete;
             DROP TRIGGER product_review_authority_no_update;
             DROP TRIGGER product_review_authority_no_delete;
             DROP TRIGGER product_review_authority_no_replace;
             DROP TRIGGER product_review_authority_no_legacy;
             DROP TRIGGER product_result_bindings_no_update;
             DROP TRIGGER product_result_bindings_no_delete;
             DROP TRIGGER product_result_bindings_no_replace;
             DROP TRIGGER product_result_bindings_no_legacy_tuple;
             DROP TRIGGER product_approval_result_bindings_no_update;
             DROP TRIGGER product_approval_result_bindings_no_delete;
             DROP TRIGGER product_approval_result_bindings_no_replace;
             DROP TABLE product_approval_result_bindings_v1;
             DROP TABLE product_result_bindings_v1;
             DROP TABLE product_review_authority_v1;
             DROP TABLE product_result_legacy_hosts_v1;
             DROP TABLE product_result_legacy_tuples_v1;
             DROP TABLE product_result_meta_v1;
             DROP TRIGGER IF EXISTS product_scope_provenance_immutable_update;
             DROP TRIGGER IF EXISTS product_scope_provenance_immutable_delete;
             DROP TRIGGER IF EXISTS product_scope_repository_no_update;
             DROP TRIGGER IF EXISTS product_scope_repository_no_delete;
             DROP TRIGGER IF EXISTS product_scope_source_no_delete;
             DROP TRIGGER IF EXISTS product_scope_api_locator_no_update;
             DROP TRIGGER IF EXISTS product_scope_api_locator_no_delete;
             DROP TABLE product_scope_api_locators_v1;
             DROP TABLE product_scope_operations_v1;
             DROP TABLE product_scope_source_events_v1;
             DROP TABLE product_scope_events_v1;
             DROP TABLE product_scope_grants_v1;
             DROP TABLE product_scope_subject_revisions_v1;
             DROP TABLE product_scope_quarantine_v1;
             DROP TABLE product_scope_order_provenance_v1;
             DROP TABLE product_scope_current_sources_v1;
             DROP TABLE product_scope_source_instances_v1;
             DROP TABLE product_scope_repositories_v1;
             DROP TABLE product_scope_migrations_v1;
             DROP TABLE product_scope_policy_state_v1;
             UPDATE product_meta_v1 SET schema_version=1 WHERE singleton=1;").unwrap();
    conn.execute("INSERT INTO product_sources_v1(connector_id,provider,origin,repository_id,scope,revision,enabled) VALUES('gh','GitHub','https://api.github.com','acme/proj','open',1,1)", []).unwrap();
    drop(conn);
    let manifest = ScopeMigrationManifest {
        manifest_id: "reviewed".into(), reviewed_by: "tester".into(),
        sources: vec![ScopeSourceManifest {
            connector_id: "gh".into(), source_instance_id: "source-instance-1".into(), generation: 1,
            identity: RepositoryIdentity::canonicalized("acme", "GitHub", "https://api.github.com", "42").unwrap(),
            api_owner: Some("acme".into()), api_repo: Some("proj".into()),
            legacy_repository_locator: "acme/proj".into(), scope_query: "open".into(), enabled: true,
        }],
        verified_orders: vec![], grants: vec![],
    };
    let before = snapshot(&dir.0);
    assert!(matches!(ProductStore::migrate_scope_v1(&path, &config(), &manifest, chrono::Utc::now()), Err(ProductError::ContractMismatch)));
    assert_eq!(snapshot(&dir.0), before);
    // The same disposable v1 fixture without the marker remains migratable.
    let conn = Connection::open(&path).unwrap();
    conn.execute("DROP TABLE product_w1_isolated_marker_v1", []).unwrap();
    drop(conn);
    ProductStore::migrate_scope_v1(&path, &config(), &manifest, chrono::Utc::now()).unwrap();
    let conn = Connection::open(&path).unwrap();
    let version: i64 = conn.query_row("SELECT schema_version FROM product_meta_v1 WHERE singleton=1", [], |r| r.get(0)).unwrap();
    assert_eq!(version, 2);
}

// Snapshot every durable table and every value, including rows unrelated to review,
// through an independent connection so an attempted admission cannot hide writes.
fn durable_rows(path: &std::path::Path) -> std::collections::BTreeMap<String, Vec<String>> {
    let conn = Connection::open(path).unwrap();
    let mut tables = conn.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name").unwrap();
    let names: Vec<String> = tables.query_map([], |row| row.get(0)).unwrap().map(Result::unwrap).collect();
    names.into_iter().map(|name| {
        let sql = format!("SELECT * FROM \"{}\"", name.replace('"', "\"\""));
        let mut stmt = conn.prepare(&sql).unwrap();
        let columns = stmt.column_count();
        let mut rows: Vec<String> = stmt.query_map([], |row| {
            let values: rusqlite::Result<Vec<rusqlite::types::Value>> = (0..columns).map(|i| row.get(i)).collect();
            Ok(format!("{:?}", values?))
        }).unwrap().map(Result::unwrap).collect();
        rows.sort();
        (name, rows)
    }).collect()
}

fn prepare_review(store: &mut ProductStore, dir: &std::path::Path, path: &std::path::Path) -> foundry_exec::ReviewAdmissionRequest {
    prepare_review_with_claim(store, dir, path).0
}

fn prepare_review_with_claim(store: &mut ProductStore, dir: &std::path::Path, path: &std::path::Path) -> (foundry_exec::ReviewAdmissionRequest, foundry_ledger::ProductWorkClaim) {
    use foundry_exec::{Attempt, Epoch, ExecutionIntent, Generation, OrderId, ReviewAdmissionRequest, RunId};
    store.bind_source("gh", "github", "https://api.github.com", "repo:42", "repo:42").unwrap();
    let now = chrono::Utc::now();
    let updated = now.to_rfc3339();
    let version = foundry_ledger::product::delivery_version_digest("issue:8", &updated, "dep-bump", "immutable task");
    let delivery = foundry_ledger::ProductDelivery {
        delivery_id: foundry_ledger::product::github_delivery_id("w1-install", "gh", "https://api.github.com", "repo:42", "issue:8", &version),
        connector_id: "gh".into(), repository_id: "repo:42".into(), issue_id: "issue:8".into(),
        observed_updated_at: updated.clone(), version_digest: version, task_kind: "dep-bump".into(), payload: "immutable task".into(),
    };
    store.persist_intake_page(&foundry_ledger::ProductPollCursor {
        connector_id: "gh".into(), query_fingerprint: "w1-test".into(), sweep_id: "sweep".into(),
        next_page: 1, page_ceiling: 1, updated_at: updated,
    }, &[delivery.clone()]).unwrap();
    store.admit_intake_delivery(&delivery.delivery_id, "review-order", now).unwrap();
    // As in b1_product_atomic, select the test adapter by SQL; the actual work
    // claim is issued by ProductStore and no review acceptance or result is seeded.
    Connection::open(path).unwrap().execute(
        "UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id='review-order'", [],
    ).unwrap();
    let claim = match store.claim_product_work("w1-worker", now, chrono::Duration::minutes(5)).unwrap() {
        foundry_ledger::ProductWorkClaimResult::Claimed(claim) => claim,
        other => panic!("expected work claim, got {other:?}"),
    };
    let pack = "a".repeat(64);
    store.configure_review_backend(foundry_ledger::ProductReviewConfig {
        artifact_root: dir.join("artifacts"), session_root: dir.join("sessions"),
        claim_root: dir.join("claims"), pack_digest: pack.clone(),
    }).unwrap();
    (ReviewAdmissionRequest {
        intent: ExecutionIntent {
            order: OrderId("review-order".into()), attempt: Attempt(1), run: RunId(4),
            activation: 1, generation: Generation(0), epoch: Epoch(1),
            external_ref: "review:review-order:1:base..target".into(), recorded_at: now,
        },
        base_commit: "base".into(), result_commit: "target".into(), pack_digest: pack,
        product_work: Some(claim.review_binding()),
    }, claim)
}

#[test]
fn isolated_review_admission_is_rejected_and_normal_store_admits() {
    use foundry_exec::{HostError, ReviewAdmissionStore};
    let isolated_dir = PrivateDir::new();
    let isolated_path = isolated_dir.0.join("isolated.db");
    let mut isolated = ProductStore::create_isolated_test_store(&isolated_dir.0, "isolated.db", config()).unwrap();
    let request = prepare_review(&mut isolated, &isolated_dir.0, &isolated_path);
    let before = durable_rows(&isolated_path);
    let error = isolated.admit_review_activation(request.clone()).unwrap_err();
    assert!(matches!(error, HostError::Acceptance(ref message) if message == "isolated product review admission requires an atomic pin"));
    assert_eq!(durable_rows(&isolated_path), before);
    drop(isolated);
    let mut isolated = ProductStore::reopen_isolated_test_store(&isolated_dir.0, "isolated.db", config()).unwrap();
    isolated.configure_review_backend(foundry_ledger::ProductReviewConfig {
        artifact_root: isolated_dir.0.join("artifacts"), session_root: isolated_dir.0.join("sessions"),
        claim_root: isolated_dir.0.join("claims"), pack_digest: "a".repeat(64),
    }).unwrap();
    let before = durable_rows(&isolated_path);
    let error = isolated.admit_review_activation(request.clone()).unwrap_err();
    assert!(matches!(error, HostError::Acceptance(ref message) if message == "isolated product review admission requires an atomic pin"));
    assert_eq!(durable_rows(&isolated_path), before);
    Connection::open(&isolated_path).unwrap().execute("DELETE FROM product_w1_isolated_marker_v1", []).unwrap();
    let before = durable_rows(&isolated_path);
    let error = isolated.admit_review_activation(request).unwrap_err();
    assert!(matches!(error, HostError::Acceptance(ref message) if message == "isolated product review admission requires an atomic pin"));
    assert_eq!(durable_rows(&isolated_path), before);

    let normal_dir = PrivateDir::new();
    let normal_path = normal_dir.0.join("normal.db");
    let mut normal = ProductStore::open(&normal_path, config()).unwrap();
    let request = prepare_review(&mut normal, &normal_dir.0, &normal_path);
    let admitted = normal.admit_review_activation(request).unwrap();
    assert_eq!(admitted.journal_run, "review-order:1:4:1");
    let conn = Connection::open(&normal_path).unwrap();
    for table in ["product_review_epochs_v1", "product_review_runs_v1", "product_review_work_bindings_v1", "product_review_host_accept_v1", "product_review_bindings_v1", "product_review_active_v1"] {
        let count: i64 = conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row.get(0)).unwrap();
        assert_eq!(count, 1, "{table}");
    }
    let accepted: (String, i64, i64, i64) = conn.query_row(
        "SELECT order_id,attempt,run_id,activation FROM product_review_host_accept_v1", [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ).unwrap();
    assert_eq!(accepted, ("review-order".into(), 1, 4, 1));
    let run: (String, String, String, String) = conn.query_row(
        "SELECT order_id,run_id,status,pack_digest FROM product_review_runs_v1", [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ).unwrap();
    assert_eq!(run, ("review-order".into(), "review-order:1:4:1".into(), "running".into(), "a".repeat(64)));
}

const MARKER_DDL: &str = "CREATE TABLE product_w1_isolated_marker_v1(singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=2))";

fn marker_state(conn: &Connection) -> (String, Vec<(i64, i64)>) {
    let ddl = conn.query_row("SELECT sql FROM sqlite_master WHERE type='table' AND name='product_w1_isolated_marker_v1'", [], |row| row.get(0)).unwrap();
    let mut stmt = conn.prepare("SELECT singleton, version FROM product_w1_isolated_marker_v1 ORDER BY singleton").unwrap();
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap().map(Result::unwrap).collect();
    (ddl, rows)
}

fn fresh_control(dir: &PrivateDir) -> PathBuf {
    drop(ProductStore::create_isolated_test_store(&dir.0, "isolated.db", config()).unwrap());
    drop(ProductStore::reopen_isolated_test_store(&dir.0, "isolated.db", config()).unwrap());
    dir.0.join("isolated.db")
}

#[test]
fn isolated_marker_v2_schema_and_reopen() {
    let dir = PrivateDir::new();
    let path = fresh_control(&dir);
    let conn = Connection::open(&path).unwrap();
    assert_eq!(marker_state(&conn), (MARKER_DDL.into(), vec![(1, 2)]));
    let mut stmt = conn.prepare("SELECT type,name FROM sqlite_master WHERE type IN ('table','trigger') AND name LIKE 'product_w1_%' ORDER BY name").unwrap();
    let objects: Vec<(String, String)> = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap().map(Result::unwrap).collect();
    assert_eq!(objects, vec![
        ("table".into(), "product_w1_isolated_marker_v1".into()),
        ("trigger".into(), "product_w1_operations_no_delete".into()),
        ("trigger".into(), "product_w1_operations_no_replace".into()),
        ("trigger".into(), "product_w1_operations_no_update".into()),
        ("table".into(), "product_w1_operations_v1".into()),
    ]);
    drop(stmt);
    drop(conn);
    drop(ProductStore::reopen_isolated_test_store(&dir.0, "isolated.db", config()).unwrap());
    let before = snapshot(&dir.0);
    assert!(matches!(ProductStore::open(&path, config()), Err(ProductError::ContractMismatch)));
    assert_eq!(snapshot(&dir.0), before);
}

#[test]
fn isolated_marker_v1_reopen_rejected_without_upgrade() {
    let dir = PrivateDir::new();
    let path = fresh_control(&dir);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("DROP TABLE product_w1_isolated_marker_v1; CREATE TABLE product_w1_isolated_marker_v1(singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=1)); INSERT INTO product_w1_isolated_marker_v1 VALUES(1,1);").unwrap();
    assert_eq!(marker_state(&conn), (MARKER_DDL.replace("version=2", "version=1"), vec![(1, 1)]));
    drop(conn);
    let schema_before = selection_schema_observation(&path);
        let before = snapshot(&dir.0);
    assert!(matches!(ProductStore::reopen_isolated_test_store(&dir.0, "isolated.db", config()), Err(ProductError::ContractMismatch)));
    assert_eq!(snapshot(&dir.0), before);
    assert_eq!(selection_schema_observation(&path), schema_before);
}

#[test]
fn isolated_marker_missing_malformed_and_partial_rejected() {
    for fixture in 0..7 {
        let dir = PrivateDir::new();
        let path = fresh_control(&dir);
        let conn = Connection::open(&path).unwrap();
        match fixture {
            0 => conn.execute_batch("DROP TABLE product_w1_isolated_marker_v1").unwrap(),
            1 => conn.execute_batch("DELETE FROM product_w1_isolated_marker_v1").unwrap(),
            2 => conn.execute_batch("DROP TABLE product_w1_isolated_marker_v1; CREATE TABLE product_w1_isolated_marker_v1(singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version IN (1,2))); INSERT INTO product_w1_isolated_marker_v1 VALUES(1,2)").unwrap(),
            3 => conn.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE product_w1_isolated_marker_v1 SET version=1; PRAGMA ignore_check_constraints=OFF").unwrap(),
            4 => conn.execute_batch("PRAGMA ignore_check_constraints=ON; INSERT INTO product_w1_isolated_marker_v1 VALUES(2,2); PRAGMA ignore_check_constraints=OFF").unwrap(),
            5 => conn.execute_batch("DROP TABLE foundry_contract").unwrap(),
            6 => {
                conn.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
                let foreign_keys: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0)).unwrap();
                assert_eq!(foreign_keys, 0);
                conn.execute_batch("DROP TABLE product_meta_v1").unwrap();
                conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
                let foreign_keys: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0)).unwrap();
                assert_eq!(foreign_keys, 1);
            },
            _ => unreachable!(),
        }
        if fixture == 0 {
            let count: i64 = conn.query_row("SELECT count(*) FROM sqlite_master WHERE name='product_w1_isolated_marker_v1'", [], |row| row.get(0)).unwrap();
            assert_eq!(count, 0);
        } else {
            let ddl = if fixture == 2 { MARKER_DDL.replace("version=2", "version IN (1,2)") } else { MARKER_DDL.into() };
            let rows = match fixture { 1 => vec![], 3 => vec![(1,1)], 4 => vec![(1,2),(2,2)], _ => vec![(1,2)] };
            assert_eq!(marker_state(&conn), (ddl, rows), "fixture {fixture}");
        }
        for (index, table) in [(5, "foundry_contract"), (6, "product_meta_v1")] {
            let count: i64 = conn.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1", [table], |row| row.get(0)).unwrap();
            assert_eq!(count, if fixture == index { 0 } else { 1 }, "fixture {fixture}: {table}");
        }
        drop(conn);
        let before = snapshot(&dir.0);
        assert!(matches!(ProductStore::reopen_isolated_test_store(&dir.0, "isolated.db", config()), Err(ProductError::ContractMismatch)), "fixture {fixture}");
        assert_eq!(snapshot(&dir.0), before, "fixture {fixture}");
    }
}

#[test]
fn isolated_dangling_sidecars_are_rejected_without_mutation() {
    for reopen in [false, true] {
        let dir = PrivateDir::new();
        if reopen { fresh_control(&dir); }
        let sidecar = dir.0.join("isolated.db-wal");
        let target = PathBuf::from("missing-target");
        std::os::unix::fs::symlink(&target, &sidecar).unwrap();
        assert!(std::fs::symlink_metadata(&sidecar).unwrap().file_type().is_symlink());
        assert!(!sidecar.exists());
        let before_link = std::fs::read_link(&sidecar).unwrap();
        let before_db = if reopen { Some(std::fs::read(dir.0.join("isolated.db")).unwrap()) } else { None };
        let mut names: Vec<_> = std::fs::read_dir(&dir.0).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        names.sort();
        let result = if reopen { ProductStore::reopen_isolated_test_store(&dir.0, "isolated.db", config()) } else { ProductStore::create_isolated_test_store(&dir.0, "isolated.db", config()) };
        assert!(matches!(result, Err(ProductError::InvalidConfig)));
        assert!(std::fs::symlink_metadata(&sidecar).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_link(&sidecar).unwrap(), before_link);
        let mut after_names: Vec<_> = std::fs::read_dir(&dir.0).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        after_names.sort();
        assert_eq!(after_names, names);
        if let Some(bytes) = before_db { assert_eq!(std::fs::read(dir.0.join("isolated.db")).unwrap(), bytes); }
    }
}

const TEST_W1_TABLE: &str = r#"CREATE TABLE product_w1_operations_v1(
 actor_id TEXT COLLATE BINARY NOT NULL CHECK(typeof(actor_id)='text' AND length(CAST(actor_id AS BLOB)) BETWEEN 1 AND 256 AND instr(actor_id,char(0))=0),
 request_key TEXT COLLATE BINARY NOT NULL CHECK(typeof(request_key)='text' AND length(CAST(request_key AS BLOB)) BETWEEN 1 AND 256 AND instr(request_key,char(0))=0),
 operation TEXT COLLATE BINARY NOT NULL CHECK(operation IN ('approve','revoke','select')),
 authority_revision INTEGER NOT NULL CHECK(typeof(authority_revision)='integer' AND authority_revision>=0),
 source_instance_id TEXT COLLATE BINARY NOT NULL CHECK(typeof(source_instance_id)='text' AND length(CAST(source_instance_id AS BLOB)) BETWEEN 1 AND 256 AND instr(source_instance_id,char(0))=0),
 digest TEXT COLLATE BINARY NOT NULL CHECK(typeof(digest)='text' AND length(CAST(digest AS BLOB))=64 AND digest NOT GLOB '*[^0-9a-f]*'),
 expected_revision INTEGER NOT NULL CHECK(typeof(expected_revision)='integer' AND expected_revision>=0 AND expected_revision<9223372036854775807),
 approval_result_revision INTEGER,
 order_id TEXT COLLATE BINARY,
 lifecycle INTEGER,
 selection_result_revision INTEGER,
 referenced_approval_revision INTEGER,
 referenced_operation TEXT COLLATE BINARY,
 request BLOB NOT NULL CHECK(typeof(request)='blob' AND length(request)<=8192),
 receipt BLOB NOT NULL CHECK(typeof(receipt)='blob' AND length(receipt)<=8192),
 PRIMARY KEY(actor_id,request_key),
 UNIQUE(source_instance_id,digest,approval_result_revision),
 UNIQUE(order_id,lifecycle,selection_result_revision),
 UNIQUE(source_instance_id,digest,approval_result_revision,operation),
 FOREIGN KEY(source_instance_id,digest,referenced_approval_revision,referenced_operation)
   REFERENCES product_w1_operations_v1(source_instance_id,digest,approval_result_revision,operation),
 CHECK((operation IN ('approve','revoke') AND typeof(approval_result_revision)='integer' AND approval_result_revision=expected_revision+1 AND order_id IS NULL AND lifecycle IS NULL AND selection_result_revision IS NULL AND referenced_approval_revision IS NULL AND referenced_operation IS NULL)
    OR (operation='select' AND approval_result_revision IS NULL AND typeof(order_id)='text' AND length(CAST(order_id AS BLOB)) BETWEEN 1 AND 256 AND instr(order_id,char(0))=0 AND typeof(lifecycle)='integer' AND lifecycle>0 AND typeof(selection_result_revision)='integer' AND selection_result_revision=expected_revision+1 AND typeof(referenced_approval_revision)='integer' AND referenced_approval_revision>0 AND referenced_operation IS NOT NULL AND referenced_operation='approve'))
)"#;
const TEST_W1_UPDATE: &str = r#"CREATE TRIGGER product_w1_operations_no_update BEFORE UPDATE ON product_w1_operations_v1 BEGIN SELECT RAISE(ABORT,'w1 operation immutable'); END"#;
const TEST_W1_DELETE: &str = r#"CREATE TRIGGER product_w1_operations_no_delete BEFORE DELETE ON product_w1_operations_v1 BEGIN SELECT RAISE(ABORT,'w1 operation immutable'); END"#;
const TEST_W1_REPLACE: &str = r#"CREATE TRIGGER product_w1_operations_no_replace BEFORE INSERT ON product_w1_operations_v1 WHEN EXISTS(SELECT 1 FROM product_w1_operations_v1 o WHERE (o.actor_id=NEW.actor_id AND o.request_key=NEW.request_key) OR (NEW.approval_result_revision IS NOT NULL AND o.source_instance_id=NEW.source_instance_id AND o.digest=NEW.digest AND o.approval_result_revision=NEW.approval_result_revision) OR (NEW.selection_result_revision IS NOT NULL AND o.order_id=NEW.order_id AND o.lifecycle=NEW.lifecycle AND o.selection_result_revision=NEW.selection_result_revision)) BEGIN SELECT RAISE(ABORT,'w1 operation duplicate'); END"#;

fn operation_objects(conn: &Connection) -> Vec<(String, String)> {
    let mut stmt = conn.prepare("SELECT name,sql FROM sqlite_schema WHERE name IN ('product_w1_operations_v1','product_w1_operations_no_update','product_w1_operations_no_delete','product_w1_operations_no_replace') ORDER BY name").unwrap();
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect();
    rows
}

fn expected_operation_objects() -> Vec<(String, String)> {
    [("product_w1_operations_no_delete", TEST_W1_DELETE), ("product_w1_operations_no_replace", TEST_W1_REPLACE), ("product_w1_operations_no_update", TEST_W1_UPDATE), ("product_w1_operations_v1", TEST_W1_TABLE)].into_iter().map(|(n, s)| (n.into(), s.into())).collect()
}

#[test]
fn isolated_operation_schema_is_canonical_on_create_and_reopen() {
    let dir = PrivateDir::new();
    let path = dir.0.join("isolated.db");
    drop(ProductStore::create_isolated_test_store(&dir.0, "isolated.db", config()).unwrap());
    for _ in 0..2 {
        let conn = Connection::open(&path).unwrap();
        assert_eq!(operation_objects(&conn), expected_operation_objects());
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA recursive_triggers=ON").unwrap();
        for pragma in ["PRAGMA foreign_keys", "PRAGMA recursive_triggers"] {
            assert_eq!(conn.query_row(pragma, [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        }
        assert!(conn.prepare("PRAGMA foreign_key_check").unwrap().query([]).unwrap().next().unwrap().is_none());
        drop(conn);
        drop(ProductStore::reopen_isolated_test_store(&dir.0, "isolated.db", config()).unwrap());
    }
}

#[test]
fn isolated_operation_corruption_rejected_without_repair() {
    for fixture in 0..10 {
        let dir = PrivateDir::new();
        let path = fresh_control(&dir);
        let conn = Connection::open(&path).unwrap();
        assert_eq!(operation_objects(&conn), expected_operation_objects());
        match fixture {
            0 => conn.execute_batch("DROP TABLE product_w1_operations_v1").unwrap(),
            1 => { conn.execute_batch("DROP TABLE product_w1_operations_v1").unwrap(); conn.execute_batch(&TEST_W1_TABLE.replace("authority_revision>=0", "authority_revision>0")).unwrap(); for ddl in [TEST_W1_UPDATE, TEST_W1_DELETE, TEST_W1_REPLACE] { conn.execute_batch(ddl).unwrap(); } },
            2..=7 => {
                let (name, ddl) = [("product_w1_operations_no_update", TEST_W1_UPDATE), ("product_w1_operations_no_delete", TEST_W1_DELETE), ("product_w1_operations_no_replace", TEST_W1_REPLACE)][(fixture - 2) / 2];
                conn.execute_batch(&format!("DROP TRIGGER {name}")).unwrap();
                if fixture % 2 == 1 { conn.execute_batch(&ddl.replace("RAISE(ABORT", "RAISE(FAIL")).unwrap(); }
            },
            8 => conn.execute_batch("CREATE TRIGGER unexpected_w1 AFTER INSERT ON product_w1_operations_v1 BEGIN SELECT 1; END").unwrap(),
            9 => conn.execute_batch("CREATE INDEX unexpected_w1 ON product_w1_operations_v1(actor_id)").unwrap(),
            _ => unreachable!(),
        }
        let objects = operation_objects(&conn);
        if fixture == 0 { assert!(objects.is_empty()); }
        else if fixture == 1 { assert_eq!(objects[3].1, TEST_W1_TABLE.replace("authority_revision>=0", "authority_revision>0")); }
        else if fixture <= 7 {
            let (name, ddl) = [("product_w1_operations_no_update", TEST_W1_UPDATE), ("product_w1_operations_no_delete", TEST_W1_DELETE), ("product_w1_operations_no_replace", TEST_W1_REPLACE)][(fixture - 2) / 2];
            let target = objects.iter().find(|(n, _)| n == name);
            if fixture % 2 == 0 { assert!(target.is_none()); } else { assert_eq!(target.unwrap().1, ddl.replace("RAISE(ABORT", "RAISE(FAIL")); }
            for (n, sql) in expected_operation_objects() { if n != name { assert!(objects.contains(&(n, sql))); } }
        } else {
            assert_eq!(objects, expected_operation_objects());
            let kind = if fixture == 8 { "trigger" } else { "index" };
            assert_eq!(conn.query_row("SELECT count(*) FROM sqlite_schema WHERE name='unexpected_w1' AND type=?1", [kind], |r| r.get::<_, i64>(0)).unwrap(), 1);
        }
        drop(conn);
        let schema_before = selection_schema_observation(&path);
        let before = snapshot(&dir.0);
        assert!(matches!(ProductStore::reopen_isolated_test_store(&dir.0, "isolated.db", config()), Err(ProductError::ContractMismatch)), "fixture {fixture}");
        assert_eq!(snapshot(&dir.0), before, "fixture {fixture}");
        assert_eq!(selection_schema_observation(&path), schema_before);
    }
}

#[test]
fn synthetic_operation_schema_enforces_fk_and_immutability() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA recursive_triggers=ON").unwrap();
    for ddl in [TEST_W1_TABLE, TEST_W1_UPDATE, TEST_W1_DELETE, TEST_W1_REPLACE] { conn.execute_batch(ddl).unwrap(); }
    assert_eq!(operation_objects(&conn), expected_operation_objects());
    let digest = "a".repeat(64);
    let approve = "INSERT INTO product_w1_operations_v1(actor_id,request_key,operation,authority_revision,source_instance_id,digest,expected_revision,approval_result_revision,request,receipt) VALUES(?1,?2,'approve',0,?3,?4,0,1,x'01',x'02')";
    conn.execute(approve, rusqlite::params!["actor", "approve", "source", digest]).unwrap();
    let select = "INSERT INTO product_w1_operations_v1(actor_id,request_key,operation,authority_revision,source_instance_id,digest,expected_revision,order_id,lifecycle,selection_result_revision,referenced_approval_revision,referenced_operation,request,receipt) VALUES(?1,?2,'select',0,?3,?4,0,?5,1,1,1,'approve',x'01',x'02')";
    conn.execute(select, rusqlite::params!["actor", "select", "source", digest, "order"]).unwrap();
    let rows = || -> Vec<String> { let mut stmt = conn.prepare("SELECT quote(actor_id)||quote(request_key)||quote(operation)||quote(source_instance_id)||quote(digest)||quote(approval_result_revision)||quote(order_id)||quote(selection_result_revision)||quote(request)||quote(receipt) FROM product_w1_operations_v1 ORDER BY actor_id,request_key").unwrap(); let result = stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect(); result };
    // Keep the original snapshot and additionally capture every column, including authority_revision.
    let all_rows = || -> Vec<Vec<rusqlite::types::Value>> {
        let mut stmt = conn.prepare("SELECT * FROM product_w1_operations_v1 ORDER BY actor_id,request_key").unwrap();
        let count = stmt.column_count();
        let result = stmt.query_map([], |row| (0..count).map(|i| row.get(i)).collect()).unwrap().map(Result::unwrap).collect();
        result
    };
    let before = rows();
    let all_before = all_rows();
    let error = conn.execute(select, rusqlite::params!["other", "missing", "different", digest, "other-order"]).unwrap_err();
    assert_eq!(error.sqlite_error().unwrap().extended_code, 787);
    assert_eq!(rows(), before);
    assert_eq!(all_rows(), all_before);
    for sql in ["UPDATE product_w1_operations_v1 SET authority_revision=1", "DELETE FROM product_w1_operations_v1"] {
        assert_eq!(conn.execute(sql, []).unwrap_err().sqlite_error().unwrap().extended_code, 1811);
        assert_eq!(rows(), before);
        assert_eq!(all_rows(), all_before);
    }
    let replace_approve = approve.replacen("INSERT INTO", "INSERT OR REPLACE INTO", 1);
    for args in [("actor", "approve", "other-source"), ("other", "other-key", "source")] {
        assert_eq!(conn.execute(&replace_approve, rusqlite::params![args.0, args.1, args.2, digest]).unwrap_err().sqlite_error().unwrap().extended_code, 1811);
        assert_eq!(rows(), before);
        assert_eq!(all_rows(), all_before);
    }
    let replace_select = select.replacen("INSERT INTO", "INSERT OR REPLACE INTO", 1);
    assert_eq!(conn.execute(&replace_select, rusqlite::params!["other", "other-key", "source", digest, "order"]).unwrap_err().sqlite_error().unwrap().extended_code, 1811);
    assert_eq!(rows(), before);
    assert_eq!(all_rows(), all_before);
    conn.execute(approve, rusqlite::params!["other", "new-approval", "other-source", digest]).unwrap();
    conn.execute(select, rusqlite::params!["other", "new-selection", "other-source", digest, "other-order"]).unwrap();
    assert!(conn.prepare("PRAGMA foreign_key_check").unwrap().query([]).unwrap().next().unwrap().is_none());
}


#[test]
fn isolated_operation_key_collation_and_fk_corruption_rejected_without_repair() {
    // Exact DDL validation precedes metadata checks, so these fixtures can be
    // rejected before the key, collation, or FK metadata branches are reached.
    for (old, new) in [
        ("PRIMARY KEY(actor_id,request_key)", "PRIMARY KEY(request_key,actor_id)"),
        ("actor_id TEXT COLLATE BINARY", "actor_id TEXT COLLATE NOCASE"),
        ("REFERENCES product_w1_operations_v1(source_instance_id,digest,approval_result_revision,operation)", "REFERENCES product_w1_operations_v1(source_instance_id,digest,approval_result_revision,operation) ON DELETE CASCADE"),
    ] {
        let dir = PrivateDir::new();
        let path = fresh_control(&dir);
        let conn = Connection::open(&path).unwrap();
        assert_eq!(operation_objects(&conn), expected_operation_objects());
        assert_eq!(TEST_W1_TABLE.matches(old).count(), 1);
        let changed = TEST_W1_TABLE.replacen(old, new, 1);
        conn.execute_batch("DROP TABLE product_w1_operations_v1").unwrap();
        conn.execute_batch(&changed).unwrap();
        for ddl in [TEST_W1_UPDATE, TEST_W1_DELETE, TEST_W1_REPLACE] { conn.execute_batch(ddl).unwrap(); }
        let objects = operation_objects(&conn);
        assert_eq!(objects[3].1, changed);
        for (name, ddl) in expected_operation_objects().into_iter().filter(|(name, _)| name != "product_w1_operations_v1") { assert!(objects.contains(&(name, ddl))); }
        drop(conn);
        let schema_before = selection_schema_observation(&path);
        let before = snapshot(&dir.0);
        assert!(matches!(ProductStore::reopen_isolated_test_store(&dir.0, "isolated.db", config()), Err(ProductError::ContractMismatch)), "{old}");
        assert_eq!(snapshot(&dir.0), before, "{old}");
        assert_eq!(selection_schema_observation(&path), schema_before);
    }
}

#[test]
fn isolated_preexisting_operation_fk_violation_rejected_without_repair() {
    let dir = PrivateDir::new();
    let path = fresh_control(&dir);
    // This independent setup connection inserts only a schema-level synthetic row.
    let conn = Connection::open(&path).unwrap();
    assert_eq!(operation_objects(&conn), expected_operation_objects());
    conn.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    assert_eq!(conn.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    assert!(conn.is_autocommit());
    conn.execute("INSERT INTO product_w1_operations_v1(actor_id,request_key,operation,authority_revision,source_instance_id,digest,expected_revision,order_id,lifecycle,selection_result_revision,referenced_approval_revision,referenced_operation,request,receipt) VALUES('actor','missing','select',0,'source',?1,0,'order',1,1,1,'approve',x'01',x'02')", ["a".repeat(64)]).unwrap();
    assert_eq!(operation_objects(&conn), expected_operation_objects());
    let violations: i64 = conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get(0)).unwrap();
    assert_eq!(violations, 1);
    drop(conn);
    let schema_before = selection_schema_observation(&path);
        let before = snapshot(&dir.0);
    assert!(matches!(ProductStore::reopen_isolated_test_store(&dir.0, "isolated.db", config()), Err(ProductError::ContractMismatch)));
    assert_eq!(snapshot(&dir.0), before);
    assert_eq!(selection_schema_observation(&path), schema_before);
}

fn approval_fixture() -> (PrivateDir, ProductStore, String, chrono::DateTime<chrono::Utc>) {
    use foundry_ledger::{ProductCapability, ScopeGrantManifest};
    use foundry_ledger::product_identity::ProductPasswordKdf;
    let dir = PrivateDir::new();
    let mut store = ProductStore::create_isolated_test_store(&dir.0, "isolated.db", config()).unwrap();
    let source = ScopeSourceManifest {
        connector_id: "gh".into(), source_instance_id: "source-instance-1".into(), generation: 1,
        identity: RepositoryIdentity::canonicalized("acme", "GitHub", "https://api.github.com", "42").unwrap(),
        api_owner: Some("acme".into()), api_repo: Some("proj".into()),
        legacy_repository_locator: "acme/proj".into(), scope_query: "open".into(), enabled: true,
    };
    let grant = ScopeGrantManifest { actor_id: "owner".into(), source_instance_id: source.source_instance_id.clone(), capabilities: vec![ProductCapability::ReadOrder] };
    let phc = ProductPasswordKdf::hash("test-password").unwrap();
    store.bootstrap_owner_scoped("owner", &phc, &[source], &[grant]).unwrap();
    let now = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z").unwrap().with_timezone(&chrono::Utc);
    let session = store.login("owner", "test-password", now).unwrap().digest().to_owned();
    let sources = store.list_scoped_sources(&session, now).unwrap();
    assert_eq!(sources.len(), 1);
    assert!(sources[0].current);
    let grants = store.list_product_scope_grants(&session, "owner", "source-instance-1", now).unwrap();
    assert_eq!(grants.len(), 1);
    assert!(grants[0].enabled);
    (dir, store, session, now)
}

#[test]
fn w1_approval_aba_and_historical_replay() {
    use foundry_ledger::product::W1Operation;
    let (dir, mut store, session, now) = approval_fixture();
    let path = dir.0.join("isolated.db");
    let digest = "a".repeat(64);
    let mut before = durable_rows(&path);
    let mut receipts = Vec::new();
    let conn = Connection::open(&path).unwrap();
    let authority: i64 = conn.query_row("SELECT authority_revision FROM product_sessions_v1 WHERE session_digest=?1", [&session], |r| r.get(0)).unwrap();
    drop(conn);
    for (expected, approve, key) in [(0, true, "a1"), (1, false, "r1"), (2, true, "a2")] {
        let receipt = store.w1_change_pack_approval(&session, "source-instance-1", &digest, expected, approve, key, now).unwrap();
        assert_eq!(receipt.operation, if approve { W1Operation::Approve } else { W1Operation::Revoke });
        assert_eq!(receipt.authority_revision, authority);
        assert_canonical_receipt(&receipt);
        assert_eq!((&*receipt.installation_id, &*receipt.database_id, &*receipt.actor_id, &*receipt.source_instance_id, &*receipt.digest, &*receipt.request_key), ("w1-install", "w1-db", "owner", "source-instance-1", digest.as_str(), key));
        assert_eq!((receipt.expected_revision, receipt.result_revision), (expected, expected + 1));
        assert_eq!((receipt.order_id.as_ref(), receipt.lifecycle, receipt.approval_revision), (None, None, None));
        assert!(!receipt.canonical.is_empty());
        let after = durable_rows(&path);
        for (table, rows) in &before { if table != "product_w1_operations_v1" { assert_eq!(after.get(table), Some(rows), "{table}"); } }
        assert_eq!(after["product_w1_operations_v1"].len(), (expected + 1) as usize);
        before = after;
        receipts.push(receipt);
    }
    let conn = Connection::open(&path).unwrap();
    let mut stmt = conn.prepare("SELECT actor_id,request_key,operation,authority_revision,source_instance_id,digest,expected_revision,approval_result_revision,order_id,lifecycle,selection_result_revision,referenced_approval_revision,referenced_operation,request,receipt FROM product_w1_operations_v1 ORDER BY approval_result_revision").unwrap();
    let rows: Vec<_> = stmt.query_map([], |r| (0..15).map(|i| r.get::<_, rusqlite::types::Value>(i)).collect::<rusqlite::Result<Vec<_>>>()).unwrap().map(Result::unwrap).collect();
    assert_eq!(rows.len(), 3);
    for (i, row) in rows.iter().enumerate() {
        use rusqlite::types::Value::{Blob, Integer, Null, Text};
        let receipt = &receipts[i];
        assert_eq!(row[0], Text("owner".into()));
        assert_eq!(row[1], Text(receipt.request_key.clone()));
        assert_eq!(row[2], Text(if i == 1 { "revoke" } else { "approve" }.into()));
        assert_eq!(row[3], Integer(receipt.authority_revision));
        assert_eq!(row[4], Text("source-instance-1".into()));
        assert_eq!(row[5], Text(digest.clone()));
        assert_eq!(row[6], Integer(i as i64));
        assert_eq!(row[7], Integer(i as i64 + 1));
        assert_eq!(&row[8..13], &[Null, Null, Null, Null, Null]);
        assert_eq!(row[13], Blob(fixture_frame(b"foundry-w1-owner-operation-v2\0", &[Some("w1-install"), Some("w1-db"), Some(if i == 1 { "revoke" } else { "approve" }), Some("owner"), Some("source-instance-1"), Some(&digest), Some(&i.to_string()), Some(&receipt.request_key), None, None])));
        assert_eq!(row[14], Blob(receipt.canonical.clone()));
    }
    drop(stmt); drop(conn);
    for (i, (expected, approve, key)) in [(0, true, "a1"), (1, false, "r1"), (2, true, "a2")].into_iter().enumerate() {
        assert_eq!(store.w1_change_pack_approval(&session, "source-instance-1", &digest, expected, approve, key, now).unwrap(), receipts[i]);
        assert_eq!(durable_rows(&path), before);
    }
    for (expected, approve, message) in [(3, true, "w1 approval state invalid"), (0, false, "w1 stale revision")] {
        assert!(matches!(store.w1_change_pack_approval(&session, "source-instance-1", &digest, expected, approve, "other", now), Err(ProductError::Invariant(m)) if m == message));
        assert_eq!(durable_rows(&path), before);
    }
}

fn second_owner(store: &mut ProductStore, session: &str, now: chrono::DateTime<chrono::Utc>) -> String {
    use foundry_ledger::{ProductCapability, ScopeMutation};
    let phc = foundry_ledger::product_identity::ProductPasswordKdf::hash("test-password").unwrap();
    store.create_account(session, "owner2", "owner", &phc, now).unwrap();
    assert!(store.list_product_scope_grants(session, "owner2", "source-instance-1", now).unwrap().is_empty());
    store.change_product_scope(session, "owner2", "source-instance-1", ProductCapability::ReadOrder, ScopeMutation::Grant, 0, "grant-owner2", now).unwrap();
    let b = store.login("owner2", "test-password", now).unwrap().digest().to_owned();
    assert_eq!(store.resolve_session(&b, now).unwrap().actor_id, "owner2");
    let grants = store.list_product_scope_grants(&b, "owner2", "source-instance-1", now).unwrap();
    assert_eq!(grants.len(), 1);
    assert!(grants[0].enabled);
    assert_eq!(grants[0].scope_revision, 1);
    assert!(store.list_scoped_sources(&b, now).unwrap()[0].current);
    b
}

// Check the entire durable database against the pre-call snapshot plus exactly
// the independently framed operation row, using a separate observer connection.
fn assert_approval_insert(path: &std::path::Path, before: &std::collections::BTreeMap<String, Vec<String>>, receipt: &foundry_ledger::product::W1OperationReceipt, session: &str, actor: &str, source: &str, digest: &str, expected: i64, approve: bool, key: &str) {
    use foundry_ledger::product::W1Operation;
    let conn = Connection::open(path).unwrap();
    let authority: i64 = conn.query_row("SELECT authority_revision FROM product_sessions_v1 WHERE session_digest=?1", [session], |r| r.get(0)).unwrap();
    assert_eq!((&*receipt.installation_id, &*receipt.database_id, &*receipt.actor_id, &*receipt.source_instance_id, &*receipt.digest, &*receipt.request_key), ("w1-install", "w1-db", actor, source, digest, key));
    assert_eq!(receipt.operation, if approve { W1Operation::Approve } else { W1Operation::Revoke });
    assert_eq!((receipt.authority_revision, receipt.expected_revision, receipt.result_revision), (authority, expected, expected + 1));
    assert_eq!((receipt.order_id.as_ref(), receipt.lifecycle, receipt.approval_revision), (None, None, None));
    assert_canonical_receipt(receipt);
    let row = expected_approval_row(receipt);
    assert!(operation_value_rows(&conn).contains(&row));
    drop(conn);
    let mut expected_rows = before.clone();
    let operations = expected_rows.get_mut("product_w1_operations_v1").unwrap();
    operations.push(format!("{:?}", row));
    operations.sort();
    assert_eq!(durable_rows(path), expected_rows);
}

#[test]
fn w1_actor_key_isolation_and_conflict() {
    let (dir, mut store, a, now) = approval_fixture();
    let b = second_owner(&mut store, &a, now);
    let path = dir.0.join("isolated.db");
    let x = "a".repeat(64); let y = "b".repeat(64);
    let initial = durable_rows(&path);
    let ra = store.w1_change_pack_approval(&a, "source-instance-1", &x, 0, true, "a1", now).unwrap();
    assert_approval_insert(&path, &initial, &ra, &a, "owner", "source-instance-1", &x, 0, true, "a1");
    let before = durable_rows(&path);
    for (digest, expected, approve) in [(&x, 1, false), (&y, 0, true)] {
        assert!(matches!(store.w1_change_pack_approval(&a, "source-instance-1", digest, expected, approve, "a1", now), Err(ProductError::Invariant("w1 request key conflict"))));
        assert_eq!(durable_rows(&path), before);
    }
    let rb = store.w1_change_pack_approval(&b, "source-instance-1", &y, 0, true, "a1", now).unwrap();
    for (receipt, actor, digest, session) in [(&ra, "owner", &x, &a), (&rb, "owner2", &y, &b)] {
        assert_eq!((&*receipt.installation_id, &*receipt.database_id, &*receipt.actor_id, &*receipt.source_instance_id, &*receipt.digest, &*receipt.request_key), ("w1-install", "w1-db", actor, "source-instance-1", digest.as_str(), "a1"));
        assert_eq!(receipt.operation, foundry_ledger::product::W1Operation::Approve);
        assert_eq!((receipt.expected_revision, receipt.result_revision, receipt.order_id.as_ref(), receipt.lifecycle, receipt.approval_revision), (0, 1, None, None, None));
        let conn = Connection::open(&path).unwrap();
        let authority: i64 = conn.query_row("SELECT authority_revision FROM product_sessions_v1 WHERE session_digest=?1", [session], |r| r.get(0)).unwrap();
        assert_eq!(receipt.authority_revision, authority);
        assert_canonical_receipt(receipt);
    }
    assert_eq!(rb.actor_id, "owner2"); assert_ne!(rb, ra);
    let after = durable_rows(&path);
    assert_eq!(after["product_w1_operations_v1"].len(), 2);
    let conn = Connection::open(&path).unwrap();
    let rows = operation_value_rows(&conn);
    assert_eq!(rows, vec![expected_approval_row(&ra), expected_approval_row(&rb)]);
    drop(conn);
    for (table, rows) in &before { if table != "product_w1_operations_v1" { assert_eq!(after.get(table), Some(rows), "{table}"); } }
    assert!(matches!(store.w1_change_pack_approval(&b, "source-instance-1", &x, 0, true, "other", now), Err(ProductError::Invariant("w1 stale revision"))));
    assert_eq!(store.w1_change_pack_approval(&a, "source-instance-1", &x, 0, true, "a1", now).unwrap(), ra);
    assert_eq!(store.w1_change_pack_approval(&b, "source-instance-1", &y, 0, true, "a1", now).unwrap(), rb);
    assert_eq!(durable_rows(&path), after);
}

#[test]
fn w1_stale_before_state() {
    let (dir, mut store, session, now) = approval_fixture();
    let path = dir.0.join("isolated.db"); let x = "a".repeat(64);
    let before = durable_rows(&path);
    for (expected, message) in [(1, "w1 stale revision"), (0, "w1 approval state invalid")] {
        assert!(matches!(store.w1_change_pack_approval(&session, "source-instance-1", &x, expected, false, "other", now), Err(ProductError::Invariant(m)) if m == message));
        assert_eq!(durable_rows(&path), before);
    }
    let receipt = store.w1_change_pack_approval(&session, "source-instance-1", &x, 0, true, "a1", now).unwrap();
    assert_approval_insert(&path, &before, &receipt, &session, "owner", "source-instance-1", &x, 0, true, "a1");
    let after = durable_rows(&path);
    assert_eq!(after["product_w1_operations_v1"].len(), 1);
    for (table, rows) in &before { if table != "product_w1_operations_v1" { assert_eq!(after.get(table), Some(rows), "{table}"); } }
    for (expected, message) in [(0, "w1 stale revision"), (1, "w1 approval state invalid")] {
        assert!(matches!(store.w1_change_pack_approval(&session, "source-instance-1", &x, expected, true, "other", now), Err(ProductError::Invariant(m)) if m == message));
        assert_eq!(durable_rows(&path), after);
    }
}

#[test]
fn w1_precommit_rollback() {
    let (dir, mut store, session, now) = approval_fixture();
    let path = dir.0.join("isolated.db"); let x = "a".repeat(64);
    let before = durable_rows(&path);
    for (expected, message) in [(1, "w1 stale revision"), (0, "w1 approval state invalid")] {
        assert!(matches!(store.w1_change_pack_approval(&session, "source-instance-1", &x, expected, false, "other", now), Err(ProductError::Invariant(m)) if m == message));
        assert_eq!(durable_rows(&path), before);
    }
    let receipt = store.w1_change_pack_approval(&session, "source-instance-1", &x, 0, true, "a1", now).unwrap();
    assert_eq!(receipt.result_revision, 1);
    assert_approval_insert(&path, &before, &receipt, &session, "owner", "source-instance-1", &x, 0, true, "a1");
    let after = durable_rows(&path);
    assert_eq!(after["product_w1_operations_v1"].len(), 1);
    for (table, rows) in &before { if table != "product_w1_operations_v1" { assert_eq!(after.get(table), Some(rows), "{table}"); } }
}

#[test]
fn w1_session_authority_precedence() {
    let x = "a".repeat(64);
    for case in 0..7 {
        let (dir, mut store, session, now) = approval_fixture();
        let path = dir.0.join("isolated.db");
        // A genuine successful control also provides the conflicting key.
        let before_control = durable_rows(&path);
        let control = store.w1_change_pack_approval(&session, "source-instance-1", &x, 0, true, "a1", now).unwrap();
        assert_approval_insert(&path, &before_control, &control, &session, "owner", "source-instance-1", &x, 0, true, "a1");
        let mut call_session = session.clone();
        let mut call_now = now;
        match case {
            0 => call_session = "bad".into(),
            1 => call_session = "0".repeat(64),
            2 => {
                let conn = Connection::open(&path).unwrap();
                let expiry: String = conn.query_row("SELECT expires_at FROM product_sessions_v1 WHERE session_digest=?1", [&session], |r| r.get(0)).unwrap();
                call_now = chrono::DateTime::parse_from_rfc3339(&expiry).unwrap().with_timezone(&chrono::Utc);
            },
            3 => store.revoke_session(&session, now).unwrap(),
            // These three independently isolated SQL mutations preserve the grant and
            // source so the session/account condition is the only changed gate.
            4..=6 => {
                drop(store);
                let conn = Connection::open(&path).unwrap();
                let sql = match case {
                    4 => "UPDATE product_accounts_v1 SET revision=revision+1 WHERE actor_id='owner'",
                    5 => "UPDATE product_accounts_v1 SET enabled=0 WHERE actor_id='owner'",
                    _ => "UPDATE product_accounts_v1 SET role='viewer' WHERE actor_id='owner'",
                };
                assert_eq!(conn.execute(sql, []).unwrap(), 1);
                drop(conn);
                store = ProductStore::reopen_isolated_test_store(&dir.0, "isolated.db", config()).unwrap();
            },
            _ => unreachable!(),
        }
        let before = durable_rows(&path);
        assert!(matches!(store.w1_change_pack_approval(&call_session, "source-instance-1", &x, 1, false, "a1", call_now), Err(ProductError::Invariant("w1 authorization or binding unavailable"))), "case {case}");
        assert_eq!(durable_rows(&path), before, "case {case}");
        assert!(matches!(store.w1_change_pack_approval(&call_session, "source-instance-1", &"A".repeat(64), 0, true, "other", call_now), Err(ProductError::InvalidConfig)));
        assert_eq!(durable_rows(&path), before);
    }
}

// Independent framing for adversarial canonical field substitutions.
fn fixture_frame(domain: &[u8], fields: &[Option<&str>]) -> Vec<u8> {
    let mut out = domain.to_vec();
    for field in fields {
        match field {
            None => out.push(0),
            Some(value) => { out.push(1); out.extend_from_slice(&(value.len() as u64).to_be_bytes()); out.extend_from_slice(value.as_bytes()); }
        }
    }
    out
}

#[test]
fn w1_stored_integrity() {
    use rusqlite::types::Value;
    let x = "a".repeat(64);
    for case in 0..6 {
        let (dir, mut store, session, now) = approval_fixture();
        let path = dir.0.join("isolated.db");
        let first = store.w1_change_pack_approval(&session, "source-instance-1", &x, 0, true, "a1", now).unwrap();
        let mut expected_baseline = vec![expected_approval_row(&first)];
        assert_canonical_receipt(&first);
        if case == 5 {
            let second = store.w1_change_pack_approval(&session, "source-instance-1", &x, 1, false, "r1", now).unwrap();
            let third = store.w1_change_pack_approval(&session, "source-instance-1", &x, 2, true, "a2", now).unwrap();
            assert_canonical_receipt(&second); assert_canonical_receipt(&third);
            expected_baseline.push(expected_approval_row(&third));
            expected_baseline.push(expected_approval_row(&second));
        }
        let observer = Connection::open(&path).unwrap();
        assert_eq!(operation_value_rows(&observer), expected_baseline);
        drop(observer);
        assert_eq!(store.w1_change_pack_approval(&session, "source-instance-1", &x, 0, true, "a1", now).unwrap(), first);
        let baseline = durable_rows(&path);
        drop(store);
        let mut conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA recursive_triggers=ON;").unwrap();
        for pragma in ["PRAGMA foreign_keys", "PRAGMA recursive_triggers"] { assert_eq!(conn.query_row(pragma, [], |r| r.get::<_, i64>(0)).unwrap(), 1); }
        assert_eq!(conn.query_row("PRAGMA ignore_check_constraints", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(operation_objects(&conn), expected_operation_objects());
        let trigger = if case == 5 { "product_w1_operations_no_delete" } else { "product_w1_operations_no_update" };
        let ddl: String = conn.query_row("SELECT sql FROM sqlite_schema WHERE type='trigger' AND name=?1", [trigger], |r| r.get(0)).unwrap();
        assert_eq!(ddl, if case == 5 { TEST_W1_DELETE } else { TEST_W1_UPDATE });
        let (request, receipt): (Vec<u8>, Vec<u8>) = conn.query_row("SELECT request,receipt FROM product_w1_operations_v1 WHERE actor_id='owner' AND request_key='a1'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(receipt, first.canonical);
        let original = operation_value_rows(&conn);
        let mut expected_rows = original.clone();
        let index = expected_rows.iter().position(|row| row[1] == Value::Text("a1".into())).unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute_batch(&format!("DROP TRIGGER {trigger}")).unwrap();
        match case {
            0 => { let mut v = request; v[0] ^= 1; expected_rows[index][13] = Value::Blob(v.clone()); assert_eq!(tx.execute("UPDATE product_w1_operations_v1 SET request=?1 WHERE request_key='a1'", [Value::Blob(v)]).unwrap(), 1); },
            1 => { let mut v = receipt; v.push(0); assert!(v.len() <= 8192); expected_rows[index][14] = Value::Blob(v.clone()); assert_eq!(tx.execute("UPDATE product_w1_operations_v1 SET receipt=?1 WHERE request_key='a1'", [Value::Blob(v)]).unwrap(), 1); },
            2 => { let v = fixture_frame(b"foundry-w1-owner-operation-v2\0", &[Some("w1-install"), Some("w1-db"), Some("approve"), Some("other-actor"), Some("source-instance-1"), Some(&x), Some("0"), Some("a1"), None, None]); expected_rows[index][13] = Value::Blob(v.clone()); assert_eq!(tx.execute("UPDATE product_w1_operations_v1 SET request=?1 WHERE request_key='a1'", [Value::Blob(v)]).unwrap(), 1); },
            3 => { let authority = if first.authority_revision == 0 { "1" } else { "0" }; let v = fixture_frame(b"foundry-w1-owner-receipt-v2\0", &[Some("w1-install"), Some("w1-db"), Some("approve"), Some("owner"), Some(authority), Some("source-instance-1"), Some(&x), Some("0"), Some("1"), Some("a1"), None, None, None]); expected_rows[index][14] = Value::Blob(v.clone()); assert_eq!(tx.execute("UPDATE product_w1_operations_v1 SET receipt=?1 WHERE request_key='a1'", [Value::Blob(v)]).unwrap(), 1); },
            4 => { expected_rows[index][5] = Value::Text("b".repeat(64)); assert_eq!(tx.execute("UPDATE product_w1_operations_v1 SET digest=?1 WHERE request_key='a1'", ["b".repeat(64)]).unwrap(), 1); },
            5 => { expected_rows.retain(|row| row[1] != Value::Text("r1".into())); assert_eq!(tx.execute("DELETE FROM product_w1_operations_v1 WHERE request_key='r1'", []).unwrap(), 1); },
            _ => unreachable!(),
        }
        tx.execute_batch(&ddl).unwrap(); tx.commit().unwrap();
        assert_eq!(operation_value_rows(&conn), expected_rows);
        assert_eq!(operation_objects(&conn), expected_operation_objects());
        assert_eq!(conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        drop(conn);
        let before = durable_rows(&path);
        for (table, rows) in &baseline { if table != "product_w1_operations_v1" { assert_eq!(before.get(table), Some(rows)); } }
        assert_ne!(before["product_w1_operations_v1"], baseline["product_w1_operations_v1"]);
        let mut store = ProductStore::reopen_isolated_test_store(&dir.0, "isolated.db", config()).unwrap();
        // Schema and database-wide FK checks use an observer; connection-local
        // PRAGMAs on the ProductStore connection are not exposed by this API.
        let observer = Connection::open(&path).unwrap();
        assert_eq!(operation_objects(&observer), expected_operation_objects());
        assert_eq!(observer.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(observer.query_row("PRAGMA ignore_check_constraints", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        drop(observer);
        assert_eq!(store.resolve_session(&session, now).unwrap().actor_id, "owner");
        assert!(store.list_scoped_sources(&session, now).unwrap()[0].current);
        assert!(store.list_product_scope_grants(&session, "owner", "source-instance-1", now).unwrap()[0].enabled);
        assert!(matches!(store.w1_change_pack_approval(&session, "source-instance-1", &x, 0, true, "a1", now), Err(ProductError::ContractMismatch)), "case {case}");
        assert_eq!(durable_rows(&path), before, "case {case}");
    }
}

#[test]
fn w1_grant_loss_and_retirement() {
    use foundry_ledger::{ProductCapability, ScopeMutation};
    let (dir, mut store, a, now) = approval_fixture();
    let b = second_owner(&mut store, &a, now);
    let path = dir.0.join("isolated.db"); let x = "a".repeat(64);
    let before_first = durable_rows(&path);
    let first = store.w1_change_pack_approval(&a, "source-instance-1", &x, 0, true, "a1", now).unwrap();
    assert_approval_insert(&path, &before_first, &first, &a, "owner", "source-instance-1", &x, 0, true, "a1");
    let revision = store.list_product_scope_grants(&b, "owner", "source-instance-1", now).unwrap()[0].scope_revision;
    store.change_product_scope(&b, "owner", "source-instance-1", ProductCapability::ReadOrder, ScopeMutation::Revoke, revision, "remove-a", now).unwrap();
    assert!(!store.list_product_scope_grants(&b, "owner", "source-instance-1", now).unwrap()[0].enabled);
    assert!(store.list_product_scope_grants(&b, "owner2", "source-instance-1", now).unwrap()[0].enabled);
    let before = durable_rows(&path);
    for (expected, approve, key) in [(0, true, "a1"), (1, false, "r1")] {
        assert!(matches!(store.w1_change_pack_approval(&a, "source-instance-1", &x, expected, approve, key, now), Err(ProductError::Invariant("w1 authorization or binding unavailable"))));
        assert_eq!(durable_rows(&path), before);
    }
    let revision = store.list_product_scope_grants(&b, "owner", "source-instance-1", now).unwrap()[0].scope_revision;
    store.change_product_scope(&b, "owner", "source-instance-1", ProductCapability::ReadOrder, ScopeMutation::Grant, revision, "restore-a", now).unwrap();
    for actor in ["owner", "owner2"] { assert!(store.list_product_scope_grants(&b, actor, "source-instance-1", now).unwrap()[0].enabled); }
    let before = durable_rows(&path);
    assert_eq!(store.w1_change_pack_approval(&a, "source-instance-1", &x, 0, true, "a1", now).unwrap(), first);
    assert_eq!(durable_rows(&path), before);
    let revoked = store.w1_change_pack_approval(&a, "source-instance-1", &x, 1, false, "r1", now).unwrap();
    assert_eq!(revoked.result_revision, 2);
    assert_approval_insert(&path, &before, &revoked, &a, "owner", "source-instance-1", &x, 1, false, "r1");
    let before_active = durable_rows(&path);
    let active = store.w1_change_pack_approval(&a, "source-instance-1", &x, 2, true, "a2", now).unwrap();
    assert_eq!(active.result_revision, 3);
    assert_approval_insert(&path, &before_active, &active, &a, "owner", "source-instance-1", &x, 2, true, "a2");
    let sources = store.list_scoped_sources(&b, now).unwrap();
    let revision = sources.iter().find(|s| s.source.source_instance_id == "source-instance-1").unwrap().source_revision;
    let replacement = store.configure_scoped_source(&b, "gh", RepositoryIdentity::canonicalized("acme", "GitHub", "https://api.github.com", "43").unwrap(), "acme", "other", "acme/other", "open", true, revision, "rebind", now).unwrap();
    assert_eq!(replacement.generation, 2); assert_ne!(replacement.source_instance_id, "source-instance-1");
    let conn = Connection::open(&path).unwrap();
    let old: (i64, Option<String>) = conn.query_row("SELECT enabled,retired_at FROM product_scope_source_instances_v1 WHERE source_instance_id='source-instance-1'", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(old.0, 0); assert!(old.1.is_some());
    let current: String = conn.query_row("SELECT source_instance_id FROM product_scope_current_sources_v1 WHERE connector_id='gh'", [], |r| r.get(0)).unwrap();
    assert_eq!(current, replacement.source_instance_id); drop(conn);
    let before = durable_rows(&path);
    for (expected, approve, key) in [(0, true, "a1"), (3, false, "other")] {
        assert!(matches!(store.w1_change_pack_approval(&a, "source-instance-1", &x, expected, approve, key, now), Err(ProductError::Invariant("w1 authorization or binding unavailable"))));
        assert_eq!(durable_rows(&path), before);
    }
}

#[test]
fn w1_cross_source_key_authority() {
    use foundry_ledger::{ProductCapability, ScopeMutation};
    let (dir, mut store, a, now) = approval_fixture();
    let b = second_owner(&mut store, &a, now);
    let path = dir.0.join("isolated.db"); let x = "a".repeat(64);
    let s2 = store.configure_scoped_source(&a, "gh2", RepositoryIdentity::canonicalized("acme", "GitHub", "https://api.github.com", "43").unwrap(), "acme", "other", "acme/other", "open", true, 0, "source-2", now).unwrap();
    for (admin, actor, key) in [(&b, "owner", "grant-a-s2"), (&a, "owner2", "grant-b-s2")] {
        store.change_product_scope(admin, actor, &s2.source_instance_id, ProductCapability::ReadOrder, ScopeMutation::Grant, 0, key, now).unwrap();
    }
    for actor in ["owner", "owner2"] { for source in ["source-instance-1", s2.source_instance_id.as_str()] {
        assert!(store.list_product_scope_grants(&b, actor, source, now).unwrap()[0].enabled);
    }}
    let sources = store.list_scoped_sources(&b, now).unwrap();
    assert!(sources.iter().any(|s| s.current && s.source.source_instance_id == "source-instance-1"));
    assert!(sources.iter().any(|s| s.current && s.source.source_instance_id == s2.source_instance_id));
    let before_first = durable_rows(&path);
    let first = store.w1_change_pack_approval(&a, "source-instance-1", &x, 0, true, "a1", now).unwrap();
    assert_approval_insert(&path, &before_first, &first, &a, "owner", "source-instance-1", &x, 0, true, "a1");
    let revision = store.list_product_scope_grants(&b, "owner", "source-instance-1", now).unwrap()[0].scope_revision;
    store.change_product_scope(&b, "owner", "source-instance-1", ProductCapability::ReadOrder, ScopeMutation::Revoke, revision, "remove-s1", now).unwrap();
    assert!(!store.list_product_scope_grants(&b, "owner", "source-instance-1", now).unwrap()[0].enabled);
    assert!(store.list_product_scope_grants(&b, "owner", &s2.source_instance_id, now).unwrap()[0].enabled);
    assert!(store.list_product_scope_grants(&b, "owner2", "source-instance-1", now).unwrap()[0].enabled);
    let before = durable_rows(&path);
    assert!(matches!(store.w1_change_pack_approval(&a, &s2.source_instance_id, &x, 0, true, "a1", now), Err(ProductError::Invariant("w1 authorization or binding unavailable"))));
    assert_eq!(durable_rows(&path), before);
    let revision = store.list_product_scope_grants(&b, "owner", "source-instance-1", now).unwrap()[0].scope_revision;
    store.change_product_scope(&b, "owner", "source-instance-1", ProductCapability::ReadOrder, ScopeMutation::Grant, revision, "restore-s1", now).unwrap();
    assert!(store.list_product_scope_grants(&b, "owner", "source-instance-1", now).unwrap()[0].enabled);
    for (session, actor) in [(&a, "owner"), (&b, "owner2")] {
        assert_eq!(store.resolve_session(session, now).unwrap().actor_id, actor);
        for source in ["source-instance-1", s2.source_instance_id.as_str()] {
            assert!(store.list_product_scope_grants(session, actor, source, now).unwrap()[0].enabled);
        }
        let sources = store.list_scoped_sources(session, now).unwrap();
        for source in ["source-instance-1", s2.source_instance_id.as_str()] {
            assert!(sources.iter().any(|s| s.current && s.source.source_instance_id == source));
        }
    }
    let before = durable_rows(&path);
    assert!(matches!(store.w1_change_pack_approval(&a, &s2.source_instance_id, &x, 0, true, "a1", now), Err(ProductError::Invariant("w1 request key conflict"))));
    assert_eq!(durable_rows(&path), before);
    assert_eq!(store.w1_change_pack_approval(&a, "source-instance-1", &x, 0, true, "a1", now).unwrap(), first);
    assert_eq!(durable_rows(&path), before);
    drop(store);
    // Isolated SQL corruption of the already publicly created S1 operation.
    let mut conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA recursive_triggers=ON").unwrap();
    assert_eq!(operation_objects(&conn), expected_operation_objects());
    let ddl: String = conn.query_row("SELECT sql FROM sqlite_schema WHERE name='product_w1_operations_no_update'", [], |r| r.get(0)).unwrap();
    assert_eq!(ddl, TEST_W1_UPDATE);
    let mut bytes: Vec<u8> = conn.query_row("SELECT request FROM product_w1_operations_v1 WHERE actor_id='owner' AND request_key='a1'", [], |r| r.get(0)).unwrap(); bytes[0] ^= 1;
    let tx = conn.transaction().unwrap(); tx.execute_batch("DROP TRIGGER product_w1_operations_no_update").unwrap();
    assert_eq!(tx.execute("UPDATE product_w1_operations_v1 SET request=?1 WHERE actor_id='owner' AND request_key='a1'", [bytes]).unwrap(), 1);
    tx.execute_batch(&ddl).unwrap(); tx.commit().unwrap();
    assert_eq!(operation_objects(&conn), expected_operation_objects());
    assert_eq!(conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get::<_, i64>(0)).unwrap(), 0); drop(conn);
    let mut store = ProductStore::reopen_isolated_test_store(&dir.0, "isolated.db", config()).unwrap();
    assert!(store.list_product_scope_grants(&a, "owner", "source-instance-1", now).unwrap()[0].enabled);
    let before = durable_rows(&path);
    assert!(matches!(store.w1_change_pack_approval(&a, &s2.source_instance_id, &x, 0, true, "a1", now), Err(ProductError::ContractMismatch)));
    assert_eq!(durable_rows(&path), before);
}

#[test]
fn w1_missing_source_authority_controls() {
    let x = "a".repeat(64); let y = "b".repeat(64);
    for case in 0..5 {
        let (dir, mut store, session, now) = approval_fixture();
        let path = dir.0.join("isolated.db");
        let before_control = durable_rows(&path);
        let control = store.w1_change_pack_approval(&session, "source-instance-1", &x, 0, true, "control", now).unwrap();
        assert_approval_insert(&path, &before_control, &control, &session, "owner", "source-instance-1", &x, 0, true, "control");
        drop(store);
        if case != 0 {
            // Isolated SQL changes one binding prerequisite in each fixture.
            let conn = Connection::open(&path).unwrap();
            let sql = match case {
                1 => "UPDATE product_scope_source_instances_v1 SET enabled=0,revision=revision+1 WHERE source_instance_id='source-instance-1'",
                2 => "DELETE FROM product_scope_current_sources_v1 WHERE source_instance_id='source-instance-1'",
                3 => "DELETE FROM product_scope_grants_v1 WHERE subject_id='owner' AND source_instance_id='source-instance-1' AND capability='read_order'",
                4 => "DELETE FROM product_scope_subject_revisions_v1 WHERE subject_id='owner' AND source_instance_id='source-instance-1'",
                _ => unreachable!(),
            };
            assert_eq!(conn.execute(sql, []).unwrap(), 1, "case {case}"); drop(conn);
        }
        let mut store = ProductStore::reopen_isolated_test_store(&dir.0, "isolated.db", config()).unwrap();
        assert_eq!(store.resolve_session(&session, now).unwrap().actor_id, "owner");
        let before = durable_rows(&path);
        let source = if case == 0 { "absent-source" } else { "source-instance-1" };
        assert!(matches!(store.w1_change_pack_approval(&session, source, &y, 0, true, "other", now), Err(ProductError::Invariant("w1 authorization or binding unavailable"))), "case {case}");
        assert_eq!(durable_rows(&path), before, "case {case}");
    }
}

fn operation_value_rows(conn: &Connection) -> Vec<Vec<rusqlite::types::Value>> {
    let mut stmt = conn.prepare("SELECT * FROM product_w1_operations_v1 ORDER BY actor_id,request_key").unwrap();
    let count = stmt.column_count();
    let rows = stmt.query_map([], |row| (0..count).map(|i| row.get(i)).collect()).unwrap().map(Result::unwrap).collect();
    rows
}

fn expected_approval_row(receipt: &foundry_ledger::product::W1OperationReceipt) -> Vec<rusqlite::types::Value> {
    use rusqlite::types::Value::{Blob, Integer, Null, Text};
    let op = match receipt.operation { foundry_ledger::product::W1Operation::Approve => "approve", foundry_ledger::product::W1Operation::Revoke => "revoke", foundry_ledger::product::W1Operation::Select => panic!("expected approval") };
    let expected = receipt.expected_revision.to_string();
    let request = fixture_frame(b"foundry-w1-owner-operation-v2\0", &[Some(&receipt.installation_id), Some(&receipt.database_id), Some(op), Some(&receipt.actor_id), Some(&receipt.source_instance_id), Some(&receipt.digest), Some(&expected), Some(&receipt.request_key), None, None]);
    vec![Text(receipt.actor_id.clone()), Text(receipt.request_key.clone()), Text(op.into()), Integer(receipt.authority_revision), Text(receipt.source_instance_id.clone()), Text(receipt.digest.clone()), Integer(receipt.expected_revision), Integer(receipt.result_revision), Null, Null, Null, Null, Null, Blob(request), Blob(receipt.canonical.clone())]
}

fn assert_canonical_receipt(r: &foundry_ledger::product::W1OperationReceipt) {
    let op = match r.operation { foundry_ledger::product::W1Operation::Approve => "approve", foundry_ledger::product::W1Operation::Revoke => "revoke", foundry_ledger::product::W1Operation::Select => panic!("expected approval") };
    let authority = r.authority_revision.to_string();
    let expected = r.expected_revision.to_string();
    let result = r.result_revision.to_string();
    assert_eq!(r.canonical, fixture_frame(b"foundry-w1-owner-receipt-v2\0", &[Some(&r.installation_id), Some(&r.database_id), Some(op), Some(&r.actor_id), Some(&authority), Some(&r.source_instance_id), Some(&r.digest), Some(&expected), Some(&result), Some(&r.request_key), None, None, None]));
}

#[test]
fn w1_feature_off_and_existing_guards() {
    let dir = PrivateDir::new();
    let ordinary = dir.0.join("ordinary.db");
    drop(ProductStore::open(&ordinary, config()).unwrap());
    let observer = Connection::open(&ordinary).unwrap();
    let mut stmt = observer.prepare("SELECT type,name FROM sqlite_schema WHERE name GLOB 'product_w1_*' ORDER BY type,name").unwrap();
    let objects: Vec<(String, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect();
    assert!(objects.is_empty(), "ordinary store has W1 objects: {objects:?}");
    drop(stmt); drop(observer);
    let typed_before = selection_typed_snapshot(&ordinary);
    let before = durable_rows(&ordinary);
    assert!(matches!(ProductStore::reopen_isolated_test_store(&dir.0, "ordinary.db", config()), Err(ProductError::ContractMismatch)));
    assert_eq!(durable_rows(&ordinary), before);
    assert_eq!(selection_typed_snapshot(&ordinary), typed_before);
    let isolated = dir.0.join("isolated.db");
    drop(ProductStore::create_isolated_test_store(&dir.0, "isolated.db", config()).unwrap());
    let typed_before = selection_typed_snapshot(&isolated);
    let before = durable_rows(&isolated);
    assert!(matches!(ProductStore::open(&isolated, config()), Err(ProductError::ContractMismatch)));
    assert_eq!(durable_rows(&isolated), before);
    assert_eq!(selection_typed_snapshot(&isolated), typed_before);
    drop(ProductStore::reopen_isolated_test_store(&dir.0, "isolated.db", config()).unwrap());
    assert_eq!(durable_rows(&isolated), before);
    assert_eq!(selection_typed_snapshot(&isolated), typed_before);
}

fn selection_fixture() -> (PrivateDir, ProductStore, String, chrono::DateTime<chrono::Utc>) {
    let (dir, mut store, session, now) = approval_fixture();
    assert_eq!(store.resolve_session(&session, now).unwrap().actor_id, "owner");
    for (order, issue) in [("order-1", "issue:1"), ("order-2", "issue:2")] {
        let receipt = store.admit_order(order, "gh", "42", issue, "dep-bump", "immutable task", now).unwrap();
        assert_eq!((receipt.order_id.as_str(), receipt.lifecycle), (order, 1));
        assert!(!receipt.event_hash.is_empty());
        let view = store.read_scoped_order(&session, order, now).unwrap();
        assert_eq!((view.order_id.as_str(), view.lifecycle, view.source_instance_id.as_str(), view.admission_sequence), (order, 1, "source-instance-1", receipt.sequence));
    }
    (dir, store, session, now)
}

#[test]
fn w1_select_shared_source_success() {
    let (dir, mut store, session, now) = selection_fixture();
    let path = dir.0.join("isolated.db");
    let x = "a".repeat(64); let y = "b".repeat(64);
    for (order, digest, expected, key, approval) in [(None, &x, 0, "a1", true), (Some("order-1"), &x, 0, "s1", false), (Some("order-2"), &x, 0, "s2", false), (None, &y, 0, "a2", true), (Some("order-1"), &y, 1, "s3", false)] {
        let before = durable_rows(&path);
        let mut typed_before = selection_typed_snapshot(&path);
        if approval {
            let r = store.w1_change_pack_approval(&session, "source-instance-1", digest, expected, true, key, now).unwrap();
            assert_approval_insert(&path, &before, &r, &session, "owner", "source-instance-1", digest, expected, true, key);
            typed_before.get_mut("product_w1_operations_v1").unwrap().push(expected_approval_row(&r));
        } else {
            let order = order.unwrap();
            let r = store.w1_select_pack(&session, order, 1, "source-instance-1", digest, expected, key, now).unwrap();
            assert_eq!(r.operation, foundry_ledger::product::W1Operation::Select);
            assert_eq!((r.installation_id.as_str(), r.database_id.as_str(), r.actor_id.as_str(), r.source_instance_id.as_str(), r.digest.as_str(), r.request_key.as_str()), ("w1-install", "w1-db", "owner", "source-instance-1", digest.as_str(), key));
            assert_eq!((r.expected_revision, r.result_revision, r.order_id.as_deref(), r.lifecycle, r.approval_revision), (expected, expected + 1, Some(order), Some(1), Some(1)));
            let authority = r.authority_revision.to_string(); let e = expected.to_string(); let result = (expected + 1).to_string();
            let request = fixture_frame(b"foundry-w1-owner-operation-v2\0", &[Some("w1-install"), Some("w1-db"), Some("select"), Some("owner"), Some("source-instance-1"), Some(digest), Some(&e), Some(key), Some(order), Some("1")]);
            assert_eq!(r.canonical, fixture_frame(b"foundry-w1-owner-receipt-v2\0", &[Some("w1-install"), Some("w1-db"), Some("select"), Some("owner"), Some(&authority), Some("source-instance-1"), Some(digest), Some(&e), Some(&result), Some(key), Some(order), Some("1"), Some("1")]));
            use rusqlite::types::Value::{Blob, Integer, Null, Text};
            let row = vec![Text("owner".into()), Text(key.into()), Text("select".into()), Integer(r.authority_revision), Text("source-instance-1".into()), Text(digest.clone()), Integer(expected), Null, Text(order.into()), Integer(1), Integer(expected + 1), Integer(1), Text("approve".into()), Blob(request), Blob(r.canonical.clone())];
            let conn = Connection::open(&path).unwrap(); assert!(operation_value_rows(&conn).contains(&row)); drop(conn);
            typed_before.get_mut("product_w1_operations_v1").unwrap().push(row.clone());
            let mut expected_rows = before; expected_rows.get_mut("product_w1_operations_v1").unwrap().push(format!("{:?}", row)); expected_rows.get_mut("product_w1_operations_v1").unwrap().sort();
            assert_eq!(durable_rows(&path), expected_rows);
        }
        typed_before.get_mut("product_w1_operations_v1").unwrap().sort_by_cached_key(|row| format!("{row:?}"));
        assert_eq!(selection_typed_snapshot(&path), typed_before);
    }
}

// Preserve SQLite types in the observation itself; sorting only establishes a
// stable multiset order, and equality compares the original typed values.
fn selection_schema_observation(path: &std::path::Path) -> (Vec<(String, String, String, Option<String>)>, Vec<i64>, std::collections::BTreeMap<String, Vec<Vec<rusqlite::types::Value>>>) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA recursive_triggers=ON").unwrap();
    let mut stmt = conn.prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name").unwrap();
    let schema = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap().map(Result::unwrap).collect();
    let pragmas = ["PRAGMA foreign_keys", "PRAGMA recursive_triggers"].iter().map(|q| conn.query_row(q, [], |r| r.get(0)).unwrap()).collect();
    (schema, pragmas, selection_typed_snapshot(path))
}

fn selection_typed_snapshot(path: &std::path::Path) -> std::collections::BTreeMap<String, Vec<Vec<rusqlite::types::Value>>> {
    let conn = Connection::open(path).unwrap();
    let mut tables = conn.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name").unwrap();
    let names: Vec<String> = tables.query_map([], |row| row.get(0)).unwrap().map(Result::unwrap).collect();
    names.into_iter().map(|name| {
        let mut stmt = conn.prepare(&format!("SELECT * FROM \"{}\"", name.replace('"', "\"\""))).unwrap();
        let count = stmt.column_count();
        let mut rows: Vec<Vec<rusqlite::types::Value>> = stmt.query_map([], |row| (0..count).map(|i| row.get(i)).collect()).unwrap().map(Result::unwrap).collect();
        rows.sort_by_cached_key(|row| format!("{row:?}"));
        (name, rows)
    }).collect()
}

#[test]
fn selection_non_key_hit_approval_receipt_corruption() {
    let (dir, mut store, session, now) = selection_fixture();
    let path = dir.0.join("isolated.db"); let x = "a".repeat(64);
    let first = store.w1_change_pack_approval(&session, "source-instance-1", &x, 0, true, "a1", now).unwrap();
    store.w1_select_pack(&session, "order-1", 1, "source-instance-1", &x, 0, "s1", now).unwrap();
    store.w1_change_pack_approval(&session, "source-instance-1", &x, 1, false, "r1", now).unwrap();
    let third = store.w1_change_pack_approval(&session, "source-instance-1", &x, 2, true, "a2", now).unwrap();
    drop(store);
    let mut conn = Connection::open(&path).unwrap();
    let authority = (third.authority_revision + 1).to_string();
    let corrupted = fixture_frame(b"foundry-w1-owner-receipt-v2\0", &[Some("w1-install"), Some("w1-db"), Some("approve"), Some("owner"), Some(&authority), Some("source-instance-1"), Some(&x), Some("2"), Some("3"), Some("a2"), None, None, None]);
    let tx = conn.transaction().unwrap();
    tx.execute_batch("DROP TRIGGER product_w1_operations_no_update").unwrap();
    assert_eq!(tx.execute("UPDATE product_w1_operations_v1 SET receipt=?1 WHERE actor_id='owner' AND request_key='a2'", [corrupted]).unwrap(), 1);
    tx.execute_batch(TEST_W1_UPDATE).unwrap(); tx.commit().unwrap();
    assert_eq!(operation_objects(&conn), expected_operation_objects());
    assert_eq!(conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    drop(conn);
    let mut store = ProductStore::reopen_isolated_test_store(&dir.0, "isolated.db", config()).unwrap();
    assert_eq!(store.read_scoped_order(&session, "order-1", now).unwrap().lifecycle, 1);
    let before = selection_typed_snapshot(&path);
    assert!(matches!(store.w1_change_pack_approval(&session, "source-instance-1", &x, 0, true, "a1", now), Err(ProductError::ContractMismatch)));
    assert_eq!(selection_typed_snapshot(&path), before);
    assert_eq!(first.result_revision, 1);
}

fn selection_assert_insert(path: &std::path::Path, mut before: std::collections::BTreeMap<String, Vec<Vec<rusqlite::types::Value>>>, r: &foundry_ledger::product::W1OperationReceipt, session: &str, operation: &str, order: Option<&str>, digest: &str, expected: i64, key: &str, approval: Option<i64>) {
    use rusqlite::types::Value::{Blob, Integer, Null, Text};
    let conn = Connection::open(path).unwrap();
    let authority: i64 = conn.query_row("SELECT authority_revision FROM product_sessions_v1 WHERE session_digest=?1", [session], |row| row.get(0)).unwrap();
    assert_eq!((r.installation_id.as_str(), r.database_id.as_str(), r.actor_id.as_str(), r.source_instance_id.as_str(), r.digest.as_str(), r.request_key.as_str()), ("w1-install", "w1-db", "owner", "source-instance-1", digest, key));
    assert_eq!((r.authority_revision, r.expected_revision, r.result_revision, r.order_id.as_deref(), r.lifecycle, r.approval_revision), (authority, expected, expected + 1, order, order.map(|_| 1), approval));
    assert_eq!(r.operation, match operation { "approve" => foundry_ledger::product::W1Operation::Approve, "revoke" => foundry_ledger::product::W1Operation::Revoke, "select" => foundry_ledger::product::W1Operation::Select, _ => unreachable!() });
    let e = expected.to_string(); let result = (expected + 1).to_string(); let auth = authority.to_string(); let reference = approval.map(|n| n.to_string());
    let request = fixture_frame(b"foundry-w1-owner-operation-v2\0", &[Some("w1-install"), Some("w1-db"), Some(operation), Some("owner"), Some("source-instance-1"), Some(digest), Some(&e), Some(key), order, order.map(|_| "1")]);
    assert_eq!(r.canonical, fixture_frame(b"foundry-w1-owner-receipt-v2\0", &[Some("w1-install"), Some("w1-db"), Some(operation), Some("owner"), Some(&auth), Some("source-instance-1"), Some(digest), Some(&e), Some(&result), Some(key), order, order.map(|_| "1"), reference.as_deref()]));
    let row = vec![Text("owner".into()), Text(key.into()), Text(operation.into()), Integer(authority), Text("source-instance-1".into()), Text(digest.into()), Integer(expected), if order.is_some() { Null } else { Integer(expected + 1) }, order.map_or(Null, |s| Text(s.into())), order.map_or(Null, |_| Integer(1)), order.map_or(Null, |_| Integer(expected + 1)), approval.map_or(Null, Integer), order.map_or(Null, |_| Text("approve".into())), Blob(request), Blob(r.canonical.clone())];
    assert!(operation_value_rows(&conn).contains(&row)); drop(conn);
    let rows = before.get_mut("product_w1_operations_v1").unwrap(); rows.push(row); rows.sort_by_cached_key(|row| format!("{row:?}"));
    assert_eq!(selection_typed_snapshot(path), before);
}

#[test]
fn w1_select_cas_active_aba() {
    let (dir, mut store, session, now) = selection_fixture(); let path = dir.0.join("isolated.db"); let x = "a".repeat(64);
    macro_rules! denied { ($call:expr, $message:literal) => {{ let before = selection_typed_snapshot(&path); assert!(matches!($call, Err(ProductError::Invariant($message)))); assert_eq!(selection_typed_snapshot(&path), before); }} }
    macro_rules! approve { ($expected:expr, $active:expr, $key:expr) => {{ let before = selection_typed_snapshot(&path); let r = store.w1_change_pack_approval(&session, "source-instance-1", &x, $expected, $active, $key, now).unwrap(); selection_assert_insert(&path, before, &r, &session, if $active { "approve" } else { "revoke" }, None, &x, $expected, $key, None); r }} }
    macro_rules! select { ($expected:expr, $key:expr, $reference:expr) => {{ let before = selection_typed_snapshot(&path); let r = store.w1_select_pack(&session, "order-1", 1, "source-instance-1", &x, $expected, $key, now).unwrap(); selection_assert_insert(&path, before, &r, &session, "select", Some("order-1"), &x, $expected, $key, Some($reference)); r }} }
    denied!(store.w1_select_pack(&session, "order-1", 1, "source-instance-1", &x, 0, "no-approval", now), "w1 approval inactive");
    approve!(0, true, "a1"); let first = select!(0, "s1", 1);
    denied!(store.w1_select_pack(&session, "order-1", 1, "source-instance-1", &x, 0, "stale1", now), "w1 stale revision");
    select!(1, "s2", 1);
    denied!(store.w1_change_pack_approval(&session, "source-instance-1", &x, 1, true, "active", now), "w1 approval state invalid");
    approve!(1, false, "r1");
    denied!(store.w1_change_pack_approval(&session, "source-instance-1", &x, 2, false, "inactive", now), "w1 approval state invalid");
    denied!(store.w1_select_pack(&session, "order-1", 1, "source-instance-1", &x, 2, "inactive-select", now), "w1 approval inactive");
    denied!(store.w1_select_pack(&session, "order-1", 1, "source-instance-1", &x, 0, "stale2", now), "w1 stale revision");
    let before = selection_typed_snapshot(&path); assert_eq!(store.w1_select_pack(&session, "order-1", 1, "source-instance-1", &x, 0, "s1", now).unwrap(), first); assert_eq!(selection_typed_snapshot(&path), before);
    approve!(2, true, "a2"); select!(2, "s3", 3);
    let before = selection_typed_snapshot(&path); assert_eq!(store.w1_select_pack(&session, "order-1", 1, "source-instance-1", &x, 0, "s1", now).unwrap(), first); assert_eq!(selection_typed_snapshot(&path), before);
}

#[test]
fn selection_corrupt_tag_requires_persisted_order_authority() {
    use foundry_ledger::{ProductCapability, ScopeMutation};
    let (dir, mut store, a, now) = selection_fixture(); let b = second_owner(&mut store, &a, now);
    let path = dir.0.join("isolated.db"); let x = "a".repeat(64);
    let s2 = store.configure_scoped_source(&a, "gh2", RepositoryIdentity::canonicalized("acme", "GitHub", "https://api.github.com", "43").unwrap(), "acme", "other", "acme/other", "open", true, 0, "source-2", now).unwrap();
    store.change_product_scope(&b, "owner", &s2.source_instance_id, ProductCapability::ReadOrder, ScopeMutation::Grant, 0, "grant-s2", now).unwrap();
    let before = selection_typed_snapshot(&path); let approval = store.w1_change_pack_approval(&a, "source-instance-1", &x, 0, true, "a1", now).unwrap(); selection_assert_insert(&path, before, &approval, &a, "approve", None, &x, 0, "a1", None);
    let before = selection_typed_snapshot(&path); let selection = store.w1_select_pack(&a, "order-1", 1, "source-instance-1", &x, 0, "s1", now).unwrap(); selection_assert_insert(&path, before, &selection, &a, "select", Some("order-1"), &x, 0, "s1", Some(1));
    let rev = store.list_product_scope_grants(&b, "owner", "source-instance-1", now).unwrap()[0].scope_revision;
    store.change_product_scope(&b, "owner", "source-instance-1", ProductCapability::ReadOrder, ScopeMutation::Revoke, rev, "revoke-s1", now).unwrap();
    drop(store);
    let mut conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA ignore_check_constraints=ON").unwrap();
    let tx = conn.transaction().unwrap(); tx.execute_batch("DROP TRIGGER product_w1_operations_no_update").unwrap();
    assert_eq!(tx.execute("UPDATE product_w1_operations_v1 SET operation='approve' WHERE actor_id='owner' AND request_key='s1'", []).unwrap(), 1);
    tx.execute_batch(TEST_W1_UPDATE).unwrap(); tx.commit().unwrap();
    conn.execute_batch("PRAGMA ignore_check_constraints=OFF").unwrap();
    assert_eq!(conn.query_row("PRAGMA ignore_check_constraints", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(operation_objects(&conn), expected_operation_objects());
    assert_eq!(conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get::<_, i64>(0)).unwrap(), 0); drop(conn);
    let mut store = ProductStore::reopen_isolated_test_store(&dir.0, "isolated.db", config()).unwrap();
    assert!(store.list_product_scope_grants(&b, "owner", &s2.source_instance_id, now).unwrap()[0].enabled);
    let before = selection_typed_snapshot(&path);
    assert!(matches!(store.w1_change_pack_approval(&a, &s2.source_instance_id, &x, 0, true, "s1", now), Err(ProductError::Invariant("w1 authorization or binding unavailable"))));
    assert_eq!(selection_typed_snapshot(&path), before);
    let rev = store.list_product_scope_grants(&b, "owner", "source-instance-1", now).unwrap()[0].scope_revision;
    store.change_product_scope(&b, "owner", "source-instance-1", ProductCapability::ReadOrder, ScopeMutation::Grant, rev, "restore-s1", now).unwrap();
    assert_eq!(store.read_scoped_order(&a, "order-1", now).unwrap().lifecycle, 1);
    let before = selection_typed_snapshot(&path);
    assert!(matches!(store.w1_change_pack_approval(&a, &s2.source_instance_id, &x, 0, true, "s1", now), Err(ProductError::ContractMismatch)));
    assert_eq!(selection_typed_snapshot(&path), before);
}

#[test]
fn w1_cross_operation_replay_and_conflict() {
    use foundry_ledger::{ProductCapability, ScopeMutation};
    let (dir, mut store, session, now) = selection_fixture(); let path = dir.0.join("isolated.db"); let x = "a".repeat(64); let y = "b".repeat(64);
    let admin = second_owner(&mut store, &session, now);
    let s2 = store.configure_scoped_source(&session, "gh2", RepositoryIdentity::canonicalized("acme", "GitHub", "https://api.github.com", "43").unwrap(), "acme", "other", "acme/other", "open", true, 0, "source-2", now).unwrap();
    store.change_product_scope(&admin, "owner", &s2.source_instance_id, ProductCapability::ReadOrder, ScopeMutation::Grant, 0, "grant-s2", now).unwrap();
    let receipt = store.admit_order("order-3", "gh2", "43", "issue:3", "dep-bump", "immutable task", now).unwrap();
    let view = store.read_scoped_order(&session, "order-3", now).unwrap(); assert_eq!((view.lifecycle, view.source_instance_id.as_str(), view.admission_sequence), (1, s2.source_instance_id.as_str(), receipt.sequence));
    macro_rules! approval { ($n:expr, $active:expr, $key:expr) => {{ let before = selection_typed_snapshot(&path); let r = store.w1_change_pack_approval(&session, "source-instance-1", &x, $n, $active, $key, now).unwrap(); selection_assert_insert(&path, before, &r, &session, if $active { "approve" } else { "revoke" }, None, &x, $n, $key, None); r }} }
    macro_rules! selection { ($n:expr, $key:expr, $reference:expr) => {{ let before = selection_typed_snapshot(&path); let r = store.w1_select_pack(&session, "order-1", 1, "source-instance-1", &x, $n, $key, now).unwrap(); selection_assert_insert(&path, before, &r, &session, "select", Some("order-1"), &x, $n, $key, Some($reference)); r }} }
    let first = approval!(0, true, "a1"); let selected = selection!(0, "s1", 1); approval!(1, false, "r1"); approval!(2, true, "a2"); selection!(1, "s2", 3);
    let before = selection_typed_snapshot(&path);
    assert_eq!(store.w1_change_pack_approval(&session, "source-instance-1", &x, 0, true, "a1", now).unwrap(), first);
    assert_eq!(selection_typed_snapshot(&path), before);
    let before = selection_typed_snapshot(&path);
    assert_eq!(store.w1_select_pack(&session, "order-1", 1, "source-instance-1", &x, 0, "s1", now).unwrap(), selected);
    assert_eq!(selection_typed_snapshot(&path), before);
    for (order, source, digest, expected, key) in [("order-1", "source-instance-1", x.as_str(), 0, "a1"), ("order-1", "source-instance-1", y.as_str(), 0, "s1"), ("order-1", "source-instance-1", x.as_str(), 1, "s1"), ("order-2", "source-instance-1", x.as_str(), 0, "s1"), ("order-3", s2.source_instance_id.as_str(), x.as_str(), 0, "s1")] {
        let before = selection_typed_snapshot(&path);
        assert!(matches!(store.w1_select_pack(&session, order, 1, source, digest, expected, key, now), Err(ProductError::Invariant("w1 request key conflict"))));
        assert_eq!(selection_typed_snapshot(&path), before);
    }
    for (expected, active, key) in [(0, true, "s1"), (0, false, "a1")] {
        let before = selection_typed_snapshot(&path);
        assert!(matches!(store.w1_change_pack_approval(&session, "source-instance-1", &x, expected, active, key, now), Err(ProductError::Invariant("w1 request key conflict"))));
        assert_eq!(selection_typed_snapshot(&path), before);
    }
}

#[test]
fn w1_selection_lifecycle_and_rebind() {
    use foundry_ledger::{ProductCapability, ScopeMutation};
    let (dir, mut store, a, now) = selection_fixture(); let b = second_owner(&mut store, &a, now);
    let path = dir.0.join("isolated.db"); let x = "a".repeat(64);
    let before = selection_typed_snapshot(&path); let approval = store.w1_change_pack_approval(&a, "source-instance-1", &x, 0, true, "a1", now).unwrap(); selection_assert_insert(&path, before, &approval, &a, "approve", None, &x, 0, "a1", None);
    let before = selection_typed_snapshot(&path); let selected = store.w1_select_pack(&a, "order-1", 1, "source-instance-1", &x, 0, "s1", now).unwrap(); selection_assert_insert(&path, before, &selected, &a, "select", Some("order-1"), &x, 0, "s1", Some(1));
    let before = selection_typed_snapshot(&path);
    assert!(matches!(store.w1_select_pack(&a, "order-1", 2, "source-instance-1", &x, 0, "s1", now), Err(ProductError::Invariant("w1 authorization or binding unavailable")))); assert_eq!(selection_typed_snapshot(&path), before);
    let rev = store.list_scoped_sources(&b, now).unwrap().iter().find(|s| s.source.source_instance_id == "source-instance-1").unwrap().source_revision;
    let replacement = store.configure_scoped_source(&b, "gh", RepositoryIdentity::canonicalized("acme", "GitHub", "https://api.github.com", "43").unwrap(), "acme", "other", "acme/other", "open", true, rev, "rebind", now).unwrap();
    assert_eq!(replacement.generation, 2); assert_ne!(replacement.source_instance_id, "source-instance-1");
    let after = selection_typed_snapshot(&path);
    for table in ["product_scope_order_provenance_v1", "product_w1_operations_v1"] { assert_eq!(before[table], after[table]); }
    let conn = Connection::open(&path).unwrap();
    let old: (i64, Option<String>) = conn.query_row("SELECT enabled,retired_at FROM product_scope_source_instances_v1 WHERE source_instance_id='source-instance-1'", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap(); assert_eq!(old.0, 0); assert!(old.1.is_some()); drop(conn);
    let before = selection_typed_snapshot(&path);
    assert!(matches!(store.w1_select_pack(&a, "order-1", 1, "source-instance-1", &x, 0, "s1", now), Err(ProductError::Invariant("w1 authorization or binding unavailable"))));
    assert_eq!(selection_typed_snapshot(&path), before);
    let before = selection_typed_snapshot(&path);
    assert!(matches!(store.w1_change_pack_approval(&a, "source-instance-1", &x, 1, false, "r1", now), Err(ProductError::Invariant("w1 authorization or binding unavailable"))));
    assert_eq!(selection_typed_snapshot(&path), before);
    let before = selection_typed_snapshot(&path);
    assert!(matches!(store.w1_select_pack(&a, "order-1", 1, "source-instance-1", &x, 1, "new", now), Err(ProductError::Invariant("w1 authorization or binding unavailable"))));
    assert_eq!(selection_typed_snapshot(&path), before);
    let before = selection_typed_snapshot(&path);
    assert!(matches!(store.w1_change_pack_approval(&a, &replacement.source_instance_id, &x, 0, true, "new-a", now), Err(ProductError::Invariant("w1 authorization or binding unavailable"))));
    assert_eq!(selection_typed_snapshot(&path), before);
    store.change_product_scope(&b, "owner", &replacement.source_instance_id, ProductCapability::ReadOrder, ScopeMutation::Grant, 0, "grant-new", now).unwrap();
    let before = selection_typed_snapshot(&path);
    assert!(matches!(store.w1_select_pack(&a, "order-1", 1, &replacement.source_instance_id, &x, 0, "new-s", now), Err(ProductError::Invariant("w1 authorization or binding unavailable"))));
    assert_eq!(selection_typed_snapshot(&path), before);
    let old_rows = durable_rows(&path);
    let new_approval = store.w1_change_pack_approval(&a, &replacement.source_instance_id, &x, 0, true, "new-a", now).unwrap();
    assert_approval_insert(&path, &old_rows, &new_approval, &a, "owner", &replacement.source_instance_id, &x, 0, true, "new-a");
    let mut expected = before; let rows = expected.get_mut("product_w1_operations_v1").unwrap(); rows.push(expected_approval_row(&new_approval)); rows.sort_by_cached_key(|row| format!("{row:?}")); assert_eq!(selection_typed_snapshot(&path), expected);
    // BLOCKED: no public verified lifecycle transition is available for a reset/replay test.
}

#[test]
fn w1_selection_immutability_and_rollback() {
    let (dir, mut store, session, now) = selection_fixture(); let path = dir.0.join("isolated.db"); let x = "a".repeat(64);
    let before = selection_typed_snapshot(&path); let approval = store.w1_change_pack_approval(&session, "source-instance-1", &x, 0, true, "a1", now).unwrap(); selection_assert_insert(&path, before, &approval, &session, "approve", None, &x, 0, "a1", None);
    let before = selection_typed_snapshot(&path); let selection = store.w1_select_pack(&session, "order-1", 1, "source-instance-1", &x, 0, "s1", now).unwrap(); selection_assert_insert(&path, before, &selection, &session, "select", Some("order-1"), &x, 0, "s1", Some(1));
    let conn = Connection::open(&path).unwrap(); conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA recursive_triggers=ON").unwrap();
    for sql in [
        "UPDATE product_w1_operations_v1 SET digest=digest WHERE request_key='s1'",
        "DELETE FROM product_w1_operations_v1 WHERE request_key='s1'",
        "INSERT OR REPLACE INTO product_w1_operations_v1 SELECT * FROM product_w1_operations_v1 WHERE request_key='s1'",
        "INSERT OR REPLACE INTO product_w1_operations_v1 SELECT actor_id,'collision-a',operation,authority_revision,source_instance_id,digest,expected_revision,approval_result_revision,order_id,lifecycle,selection_result_revision,referenced_approval_revision,referenced_operation,request,receipt FROM product_w1_operations_v1 WHERE request_key='a1'",
        "INSERT OR REPLACE INTO product_w1_operations_v1 SELECT actor_id,'collision-s',operation,authority_revision,source_instance_id,digest,expected_revision,approval_result_revision,order_id,lifecycle,selection_result_revision,referenced_approval_revision,referenced_operation,request,receipt FROM product_w1_operations_v1 WHERE request_key='s1'",
        "INSERT INTO product_w1_operations_v1 SELECT actor_id,'bad-fk',operation,authority_revision,source_instance_id,digest,1,NULL,'order-2',1,2,99,'approve',request,receipt FROM product_w1_operations_v1 WHERE request_key='s1'",
    ] {
        let before = selection_typed_snapshot(&path); assert!(conn.execute(sql, []).is_err(), "{sql}"); assert_eq!(selection_typed_snapshot(&path), before, "{sql}");
        let before = selection_typed_snapshot(&path);
        let error = conn.execute(sql, []).unwrap_err();
        // A missing approval reaches the FK; the other statements hit immutable triggers.
        assert_eq!(error.sqlite_error().unwrap().extended_code, if sql.contains("bad-fk") { 787 } else { 1811 }, "{sql}: {error}");
        assert_eq!(selection_typed_snapshot(&path), before, "{sql}");
    }
    drop(conn);
    let before = selection_typed_snapshot(&path);
    assert!(matches!(store.w1_select_pack(&session, "order-1", 1, "source-instance-1", &x, 0, "stale", now), Err(ProductError::Invariant("w1 stale revision")))); assert_eq!(selection_typed_snapshot(&path), before);
    let before = selection_typed_snapshot(&path); let next = store.w1_select_pack(&session, "order-1", 1, "source-instance-1", &x, 1, "s2", now).unwrap(); selection_assert_insert(&path, before, &next, &session, "select", Some("order-1"), &x, 1, "s2", Some(1));
    // BLOCKED: no authorized deterministic after-insert or commit fault seam.
}

#[test]
fn w1_selection_review_guard_storage() {
    // These rows simulate storage only; none represents a real review or dispatch.
    for case in 0..6 {
        let (dir, mut store, session, now) = selection_fixture(); let path = dir.0.join("isolated.db"); let x = "a".repeat(64);
        let before = selection_typed_snapshot(&path); let a = store.w1_change_pack_approval(&session, "source-instance-1", &x, 0, true, "a1", now).unwrap(); selection_assert_insert(&path, before, &a, &session, "approve", None, &x, 0, "a1", None);
        let before = selection_typed_snapshot(&path); let first = store.w1_select_pack(&session, "order-1", 1, "source-instance-1", &x, 0, "s1", now).unwrap(); selection_assert_insert(&path, before, &first, &session, "select", Some("order-1"), &x, 0, "s1", Some(1));
        let conn = Connection::open(&path).unwrap();
        match case {
            0 => (),
            1 | 4 => { let lifecycle = if case == 1 { 1 } else { 2 }; assert_eq!(conn.execute("INSERT INTO product_review_runs_v1(order_id,attempt,run_id,base_commit,result_commit,pack_digest,status,counters_json,product_lifecycle,created_at) VALUES('order-1',1,'run','base','result','digest','completed','{}',?1,0)", [lifecycle]).unwrap(), 1); },
            2 | 5 => { let lifecycle = if case == 2 { 1 } else { 2 }; assert_eq!(conn.execute("INSERT INTO product_review_holds_v1 VALUES('order-1',?1,1,1,1,1,'hold',1,0,'digest','{}','[]','deadline',1,1,'hash','active')", [lifecycle]).unwrap(), 1); },
            3 => { assert_eq!(conn.execute("INSERT INTO product_review_active_v1 VALUES('order-1','{}',1,'run')", []).unwrap(), 1); },
            _ => unreachable!(),
        }
        drop(conn);
        let before = selection_typed_snapshot(&path);
        assert_eq!(store.w1_select_pack(&session, "order-1", 1, "source-instance-1", &x, 0, "s1", now).unwrap(), first); assert_eq!(selection_typed_snapshot(&path), before);
        if (1..=3).contains(&case) {
            let before = selection_typed_snapshot(&path);
            assert!(matches!(store.w1_select_pack(&session, "order-1", 1, "source-instance-1", &x, 0, "blocked", now), Err(ProductError::Invariant("w1 selection blocked by review")))); assert_eq!(selection_typed_snapshot(&path), before);
            let revoke = store.w1_change_pack_approval(&session, "source-instance-1", &x, 1, false, "r1", now).unwrap(); selection_assert_insert(&path, before, &revoke, &session, "revoke", None, &x, 1, "r1", None);
            let before = selection_typed_snapshot(&path);
            assert!(matches!(store.w1_select_pack(&session, "order-1", 1, "source-instance-1", &x, 1, "blocked-inactive", now), Err(ProductError::Invariant("w1 selection blocked by review")))); assert_eq!(selection_typed_snapshot(&path), before);
        } else {
            let next = store.w1_select_pack(&session, "order-1", 1, "source-instance-1", &x, 1, "s2", now).unwrap(); selection_assert_insert(&path, before, &next, &session, "select", Some("order-1"), &x, 1, "s2", Some(1));
        }
    }
}

#[test]
fn w1_actor_and_foreign_binding_nondisclosure() {
    use foundry_ledger::{ProductCapability, ScopeMutation};
    let (dir, mut store, a, now) = selection_fixture(); let b = second_owner(&mut store, &a, now); let path = dir.0.join("isolated.db"); let x = "a".repeat(64); let y = "b".repeat(64);
    let s2 = store.configure_scoped_source(&a, "gh2", RepositoryIdentity::canonicalized("acme", "GitHub", "https://api.github.com", "43").unwrap(), "acme", "other", "acme/other", "open", true, 0, "source-2", now).unwrap();
    store.change_product_scope(&b, "owner", &s2.source_instance_id, ProductCapability::ReadOrder, ScopeMutation::Grant, 0, "grant-s2", now).unwrap();
    let admitted = store.admit_order("order-3", "gh2", "43", "issue:3", "dep-bump", "immutable task", now).unwrap(); let v = store.read_scoped_order(&a, "order-3", now).unwrap(); assert_eq!((v.source_instance_id.as_str(), v.lifecycle, v.admission_sequence), (s2.source_instance_id.as_str(), 1, admitted.sequence));
    let before = selection_typed_snapshot(&path); let first = store.w1_change_pack_approval(&a, "source-instance-1", &x, 0, true, "a1", now).unwrap(); selection_assert_insert(&path, before, &first, &a, "approve", None, &x, 0, "a1", None);
    let before_typed = selection_typed_snapshot(&path); let before = durable_rows(&path);
    let other = store.w1_change_pack_approval(&b, "source-instance-1", &y, 0, true, "a1", now).unwrap(); assert_approval_insert(&path, &before, &other, &b, "owner2", "source-instance-1", &y, 0, true, "a1");
    let mut expected = before_typed; let rows = expected.get_mut("product_w1_operations_v1").unwrap(); rows.push(expected_approval_row(&other)); rows.sort_by_cached_key(|row| format!("{row:?}")); assert_eq!(selection_typed_snapshot(&path), expected); assert_ne!(first.actor_id, other.actor_id);
    let before = selection_typed_snapshot(&path);
    assert!(matches!(store.w1_change_pack_approval(&b, "source-instance-1", &x, 0, true, "other", now), Err(ProductError::Invariant("w1 stale revision")))); assert_eq!(selection_typed_snapshot(&path), before);
    let before = selection_typed_snapshot(&path); let selected = store.w1_select_pack(&a, "order-1", 1, "source-instance-1", &x, 0, "s1", now).unwrap(); selection_assert_insert(&path, before, &selected, &a, "select", Some("order-1"), &x, 0, "s1", Some(1));
    let before = selection_typed_snapshot(&path);
    assert_eq!(selection_typed_snapshot(&path), before);
    for (order, lifecycle, source) in [("order-1", 2, "source-instance-1"), ("missing-order", 1, "source-instance-1"), ("order-1", 1, s2.source_instance_id.as_str())] {
        let before = selection_typed_snapshot(&path);
        assert!(matches!(store.w1_select_pack(&a, order, lifecycle, source, &x, 0, "s1", now), Err(ProductError::Invariant("w1 authorization or binding unavailable")))); assert_eq!(selection_typed_snapshot(&path), before);
    }
    // Publicly admitted order on a second source becomes foreign when A's grant is revoked.
    let rev2 = store.list_product_scope_grants(&b, "owner", &s2.source_instance_id, now).unwrap()[0].scope_revision;
    store.change_product_scope(&b, "owner", &s2.source_instance_id, ProductCapability::ReadOrder, ScopeMutation::Revoke, rev2, "revoke-requested-source", now).unwrap();
    let before = selection_typed_snapshot(&path);
    assert!(matches!(store.w1_select_pack(&a, "order-3", 1, &s2.source_instance_id, &x, 0, "s1", now), Err(ProductError::Invariant("w1 authorization or binding unavailable"))));
    assert_eq!(selection_typed_snapshot(&path), before);
    let before = selection_typed_snapshot(&path);
    assert!(matches!(store.w1_change_pack_approval(&a, &s2.source_instance_id, &x, 0, true, "s1", now), Err(ProductError::Invariant("w1 authorization or binding unavailable"))));
    assert_eq!(selection_typed_snapshot(&path), before);
    let rev2 = store.list_product_scope_grants(&b, "owner", &s2.source_instance_id, now).unwrap()[0].scope_revision;
    store.change_product_scope(&b, "owner", &s2.source_instance_id, ProductCapability::ReadOrder, ScopeMutation::Grant, rev2, "restore-requested-source", now).unwrap();
    assert_eq!(store.read_scoped_order(&a, "order-3", now).unwrap().source_instance_id, s2.source_instance_id);
    let rev = store.list_product_scope_grants(&b, "owner", "source-instance-1", now).unwrap()[0].scope_revision;
    store.change_product_scope(&b, "owner", "source-instance-1", ProductCapability::ReadOrder, ScopeMutation::Revoke, rev, "revoke-s1", now).unwrap();
    let before = selection_typed_snapshot(&path);
    assert!(matches!(store.w1_change_pack_approval(&a, &s2.source_instance_id, &x, 0, true, "s1", now), Err(ProductError::Invariant("w1 authorization or binding unavailable"))));
    assert_eq!(selection_typed_snapshot(&path), before);
    let before = selection_typed_snapshot(&path);
    assert!(matches!(store.w1_select_pack(&a, "order-3", 1, &s2.source_instance_id, &x, 0, "s1", now), Err(ProductError::Invariant("w1 authorization or binding unavailable")))); assert_eq!(selection_typed_snapshot(&path), before);
    let rev = store.list_product_scope_grants(&b, "owner", "source-instance-1", now).unwrap()[0].scope_revision;
    store.change_product_scope(&b, "owner", "source-instance-1", ProductCapability::ReadOrder, ScopeMutation::Grant, rev, "restore-s1", now).unwrap();
    let before = selection_typed_snapshot(&path);
    assert_eq!(selection_typed_snapshot(&path), before);
    for (order, digest) in [("order-2", x.as_str()), ("order-1", y.as_str())] { let before = selection_typed_snapshot(&path); assert!(matches!(store.w1_select_pack(&a, order, 1, "source-instance-1", digest, 0, "s1", now), Err(ProductError::Invariant("w1 request key conflict")))); assert_eq!(selection_typed_snapshot(&path), before); }
    let before = selection_typed_snapshot(&path);
    assert_eq!(store.w1_select_pack(&a, "order-1", 1, "source-instance-1", &x, 0, "s1", now).unwrap(), selected); assert_eq!(selection_typed_snapshot(&path), before);
    // SQL-only unavailable binding simulation, in an independent disposable store.
    let (other_dir, mut other_store, other_session, other_now) = selection_fixture(); let other_path = other_dir.0.join("isolated.db");
    let before = selection_typed_snapshot(&other_path); let a1 = other_store.w1_change_pack_approval(&other_session, "source-instance-1", &x, 0, true, "a1", other_now).unwrap(); selection_assert_insert(&other_path, before, &a1, &other_session, "approve", None, &x, 0, "a1", None);
    let before = selection_typed_snapshot(&other_path); let s1 = other_store.w1_select_pack(&other_session, "order-1", 1, "source-instance-1", &x, 0, "s1", other_now).unwrap(); selection_assert_insert(&other_path, before, &s1, &other_session, "select", Some("order-1"), &x, 0, "s1", Some(1)); drop(other_store);
    let mut conn = Connection::open(&other_path).unwrap(); let tx = conn.transaction().unwrap(); tx.execute_batch("DROP TRIGGER product_w1_operations_no_update").unwrap(); assert_eq!(tx.execute("UPDATE product_w1_operations_v1 SET lifecycle=2 WHERE request_key='s1'", []).unwrap(), 1); tx.execute_batch(TEST_W1_UPDATE).unwrap(); tx.commit().unwrap(); assert_eq!(operation_objects(&conn), expected_operation_objects()); assert_eq!(conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get::<_, i64>(0)).unwrap(), 0); drop(conn);
    let mut other_store = ProductStore::reopen_isolated_test_store(&other_dir.0, "isolated.db", config()).unwrap(); assert_eq!(other_store.read_scoped_order(&other_session, "order-1", other_now).unwrap().lifecycle, 1);
    let before = selection_typed_snapshot(&other_path); assert!(matches!(other_store.w1_change_pack_approval(&other_session, "source-instance-1", &x, 0, true, "s1", other_now), Err(ProductError::Invariant("w1 authorization or binding unavailable")))); assert_eq!(selection_typed_snapshot(&other_path), before);
}

#[test]
fn w1_select_authority_precedence() {
    let x = "a".repeat(64);
    for case in 0..13 {
        let (dir, mut store, session, now) = selection_fixture(); let path = dir.0.join("isolated.db");
        let before = selection_typed_snapshot(&path); let a = store.w1_change_pack_approval(&session, "source-instance-1", &x, 0, true, "a1", now).unwrap(); selection_assert_insert(&path, before, &a, &session, "approve", None, &x, 0, "a1", None);
        let before = selection_typed_snapshot(&path); let s = store.w1_select_pack(&session, "order-1", 1, "source-instance-1", &x, 0, "s1", now).unwrap(); selection_assert_insert(&path, before, &s, &session, "select", Some("order-1"), &x, 0, "s1", Some(1));
        let mut call_session = session.clone(); let mut call_now = now;
        match case {
            0 => call_session = "bad".into(),
            1 => call_session = "0".repeat(64),
            2 => { let conn = Connection::open(&path).unwrap(); let expiry: String = conn.query_row("SELECT expires_at FROM product_sessions_v1 WHERE session_digest=?1", [&session], |r| r.get(0)).unwrap(); call_now = chrono::DateTime::parse_from_rfc3339(&expiry).unwrap().with_timezone(&chrono::Utc); },
            3 => store.revoke_session(&session, now).unwrap(),
            8 => {
                let revision = store.list_scoped_sources(&session, now).unwrap()[0].source_revision;
                store.configure_scoped_source(&session, "gh", RepositoryIdentity::canonicalized("acme", "GitHub", "https://api.github.com", "43").unwrap(), "acme", "other", "acme/other", "open", true, revision, "retire-source", now).unwrap();
            },
            4..=7 | 9 | 11 | 12 => {
                drop(store); let conn = Connection::open(&path).unwrap();
                let sql = match case {
                    4 => "UPDATE product_accounts_v1 SET revision=revision+1 WHERE actor_id='owner'",
                    5 => "UPDATE product_accounts_v1 SET role='viewer' WHERE actor_id='owner'",
                    6 => "DELETE FROM product_scope_grants_v1 WHERE subject_id='owner' AND source_instance_id='source-instance-1' AND capability='read_order'",
                    7 => "UPDATE product_scope_grants_v1 SET enabled=0 WHERE subject_id='owner' AND source_instance_id='source-instance-1' AND capability='read_order'",
                    9 => "DELETE FROM product_scope_current_sources_v1 WHERE source_instance_id='source-instance-1'",
                    11 => "UPDATE product_scope_source_instances_v1 SET enabled=0,revision=revision+1 WHERE source_instance_id='source-instance-1'",
                    12 => "DELETE FROM product_scope_source_instances_v1 WHERE source_instance_id='source-instance-1'",
                    _ => unreachable!(),
                };
                // Missing-source simulation temporarily removes and restores the immutable delete trigger.
                let trigger: Option<String> = if case == 12 {
                    let ddl = conn.query_row("SELECT sql FROM sqlite_schema WHERE type='trigger' AND name='product_scope_source_no_delete'", [], |r| r.get(0)).unwrap();
                    conn.execute_batch("DROP TRIGGER product_scope_source_no_delete").unwrap();
                    Some(ddl)
                } else { None };
                assert_eq!(conn.execute(sql, []).unwrap(), 1);
                if let Some(ddl) = trigger {
                    conn.execute_batch(&ddl).unwrap();
                    assert_eq!(conn.query_row("SELECT sql FROM sqlite_schema WHERE name='product_scope_source_no_delete'", [], |r| r.get::<_, String>(0)).unwrap(), ddl);
                }
                drop(conn); store = ProductStore::reopen_isolated_test_store(&dir.0, "isolated.db", config()).unwrap();
            },
            _ => (),
        }
        let before = selection_typed_snapshot(&path);
        assert!(matches!(store.w1_select_pack(&call_session, "order-1", if case == 10 { 2 } else { 1 }, "source-instance-1", &x, 1, "s1", call_now), Err(ProductError::Invariant("w1 authorization or binding unavailable"))), "case {case}");
        assert_eq!(selection_typed_snapshot(&path), before, "case {case}");
        let before = selection_typed_snapshot(&path);
        assert!(matches!(store.w1_select_pack(&call_session, "order-1", 1, "source-instance-1", &"A".repeat(64), 0, "s1", call_now), Err(ProductError::InvalidConfig))); assert_eq!(selection_typed_snapshot(&path), before);
    }
    let (dir, mut store, session, now) = selection_fixture(); let path = dir.0.join("isolated.db");
    let conn = Connection::open(&path).unwrap(); conn.execute_batch("DROP TRIGGER product_w1_operations_no_update").unwrap();
    let schema = operation_objects(&conn); let schema_before = selection_schema_observation(&path); let before = selection_typed_snapshot(&path);
    assert!(matches!(store.w1_select_pack("bad", "order-1", 1, "source-instance-1", &x, 0, "other", now), Err(ProductError::ContractMismatch)));
    assert_eq!(selection_typed_snapshot(&path), before); assert_eq!(operation_objects(&conn), schema); assert_eq!(selection_schema_observation(&path), schema_before);
    conn.execute_batch(TEST_W1_UPDATE).unwrap(); assert_eq!(operation_objects(&conn), expected_operation_objects());
    conn.execute_batch("BEGIN IMMEDIATE").unwrap(); let before = selection_typed_snapshot(&path);
    assert!(matches!(store.w1_select_pack(&session, "order-1", 1, "source-instance-1", &x, 0, "other", now), Err(ProductError::Database(_))));
    assert_eq!(selection_typed_snapshot(&path), before); conn.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn w1_selection_history_corruption() {
    // Each case corrupts one publicly created history in a private disposable store.
    for case in 0..11 {
        let (dir, mut store, session, now) = selection_fixture(); let path = dir.0.join("isolated.db"); let x = "a".repeat(64);
        let before = selection_typed_snapshot(&path); let a = store.w1_change_pack_approval(&session, "source-instance-1", &x, 0, true, "a1", now).unwrap(); selection_assert_insert(&path, before, &a, &session, "approve", None, &x, 0, "a1", None);
        let before = selection_typed_snapshot(&path); let s = store.w1_select_pack(&session, "order-1", 1, "source-instance-1", &x, 0, "s1", now).unwrap(); selection_assert_insert(&path, before, &s, &session, "select", Some("order-1"), &x, 0, "s1", Some(1));
        let before = selection_typed_snapshot(&path); let r = store.w1_change_pack_approval(&session, "source-instance-1", &x, 1, false, "r1", now).unwrap(); selection_assert_insert(&path, before, &r, &session, "revoke", None, &x, 1, "r1", None);
        let before = selection_typed_snapshot(&path); assert_eq!(store.w1_select_pack(&session, "order-1", 1, "source-instance-1", &x, 0, "s1", now).unwrap(), s); assert_eq!(selection_typed_snapshot(&path), before); drop(store);
        let mut conn = Connection::open(&path).unwrap(); conn.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA ignore_check_constraints=ON").unwrap();
        let tx = conn.transaction().unwrap(); tx.execute_batch("DROP TRIGGER product_w1_operations_no_update; DROP TRIGGER product_w1_operations_no_delete").unwrap();
        let sql = match case {
            0 => "UPDATE product_w1_operations_v1 SET request=x'00' WHERE request_key='s1'",
            1 => "UPDATE product_w1_operations_v1 SET receipt=x'00' WHERE request_key='s1'",
            2 => "UPDATE product_w1_operations_v1 SET actor_id='other' WHERE request_key='s1'",
            3 => "UPDATE product_w1_operations_v1 SET request_key='other' WHERE request_key='s1'",
            4 => "UPDATE product_w1_operations_v1 SET expected_revision=1,selection_result_revision=2 WHERE request_key='s1'",
            5 => "UPDATE product_w1_operations_v1 SET order_id='order-2' WHERE request_key='s1'",
            6 => "UPDATE product_w1_operations_v1 SET referenced_approval_revision=99 WHERE request_key='s1'",
            7 => "UPDATE product_w1_operations_v1 SET operation='approve' WHERE request_key='r1'",
            8 => "UPDATE product_w1_operations_v1 SET source_instance_id='foreign-source' WHERE request_key='s1'",
            9 => "DELETE FROM product_w1_operations_v1 WHERE request_key='a1'",
            10 => "UPDATE product_w1_operations_v1 SET referenced_approval_revision=2,referenced_operation='revoke' WHERE request_key='s1'",
            _ => unreachable!(),
        };
        assert_eq!(tx.execute(sql, []).unwrap(), 1, "case {case}");
        // Reframe both blobs so the sequence errors are not hidden by stale bytes.
        if case == 4 || case == 7 || case == 10 {
            let (op, key, expected, result, order, reference, authority) = if case == 4 {
                ("select", "s1", "1", "2", Some("order-1"), Some("1"), s.authority_revision)
            } else if case == 7 { ("approve", "r1", "1", "2", None, None, r.authority_revision) } else { ("select", "s1", "0", "1", Some("order-1"), Some("2"), s.authority_revision) };
            let auth = authority.to_string();
            let request = fixture_frame(b"foundry-w1-owner-operation-v2\0", &[Some("w1-install"), Some("w1-db"), Some(op), Some("owner"), Some("source-instance-1"), Some(&x), Some(expected), Some(key), order, order.map(|_| "1")]);
            let receipt = fixture_frame(b"foundry-w1-owner-receipt-v2\0", &[Some("w1-install"), Some("w1-db"), Some(op), Some("owner"), Some(&auth), Some("source-instance-1"), Some(&x), Some(expected), Some(result), Some(key), order, order.map(|_| "1"), reference]);
            assert_eq!(tx.execute("UPDATE product_w1_operations_v1 SET request=?1,receipt=?2 WHERE request_key=?3", rusqlite::params![request, receipt, key]).unwrap(), 1);
        }
        tx.execute_batch(TEST_W1_UPDATE).unwrap(); tx.execute_batch(TEST_W1_DELETE).unwrap(); tx.commit().unwrap();
        conn.execute_batch("PRAGMA ignore_check_constraints=OFF; PRAGMA foreign_keys=ON").unwrap(); assert_eq!(operation_objects(&conn), expected_operation_objects());
        let violations: i64 = conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get(0)).unwrap(); assert_eq!(violations, i64::from(matches!(case, 6 | 8 | 9))); drop(conn);
        let before = selection_typed_snapshot(&path);
        if matches!(case, 6 | 8 | 9) { assert!(matches!(ProductStore::reopen_isolated_test_store(&dir.0, "isolated.db", config()), Err(ProductError::ContractMismatch))); }
        else {
            let mut store = ProductStore::reopen_isolated_test_store(&dir.0, "isolated.db", config()).unwrap(); assert_eq!(store.read_scoped_order(&session, "order-1", now).unwrap().lifecycle, 1);
            let immediate_before = selection_typed_snapshot(&path);
            assert!(matches!(store.w1_select_pack(&session, "order-1", 1, "source-instance-1", &x, 1, "fresh", now), Err(ProductError::ContractMismatch)), "case {case}");
            assert_eq!(selection_typed_snapshot(&path), immediate_before, "case {case}");
        }
        assert_eq!(selection_typed_snapshot(&path), before, "case {case}");
    }
    // Independent non-key receipt and tag corruption controls exercise distinct paths.
    selection_non_key_hit_approval_receipt_corruption();
    selection_corrupt_tag_requires_persisted_order_authority();
}

#[test]
fn w1_selection_schema_and_feature_regression() {
    selection_schema_snapshot_survives_reopen();
    isolated_create_reopen_and_normal_open_rejection();
    isolated_marker_v2_schema_and_reopen();
    isolated_marker_v1_reopen_rejected_without_upgrade();
    isolated_create_rejects_each_preexisting_sidecar_without_mutation();
    isolated_reopen_rejects_each_preexisting_sidecar_without_mutation();
    isolated_operation_schema_is_canonical_on_create_and_reopen();
    isolated_operation_corruption_rejected_without_repair();
    isolated_operation_key_collation_and_fk_corruption_rejected_without_repair();
    isolated_preexisting_operation_fk_violation_rejected_without_repair();
    w1_feature_off_and_existing_guards();
}

#[test]
fn selection_corruption_hidden_until_persisted_source_restored() {
    use foundry_ledger::{ProductCapability, ScopeMutation};
    let (dir, mut store, a, now) = selection_fixture();
    let b = second_owner(&mut store, &a, now);
    let path = dir.0.join("isolated.db");
    let x = "a".repeat(64);
    let s2 = store.configure_scoped_source(&b, "gh2", RepositoryIdentity::canonicalized("acme", "GitHub", "https://api.github.com", "43").unwrap(), "acme", "other", "acme/other", "open", true, 0, "source2", now).unwrap();
    store.change_product_scope(&b, "owner", &s2.source_instance_id, ProductCapability::ReadOrder, ScopeMutation::Grant, 0, "grant2", now).unwrap();
    let before = selection_typed_snapshot(&path);
    let approval = store.w1_change_pack_approval(&a, "source-instance-1", &x, 0, true, "a1", now).unwrap();
    selection_assert_insert(&path, before, &approval, &a, "approve", None, &x, 0, "a1", None);
    let before = selection_typed_snapshot(&path);
    let selection = store.w1_select_pack(&a, "order-1", 1, "source-instance-1", &x, 0, "s1", now).unwrap();
    selection_assert_insert(&path, before, &selection, &a, "select", Some("order-1"), &x, 0, "s1", Some(1));
    let rev = store.list_product_scope_grants(&b, "owner", "source-instance-1", now).unwrap()[0].scope_revision;
    store.change_product_scope(&b, "owner", "source-instance-1", ProductCapability::ReadOrder, ScopeMutation::Revoke, rev, "revoke", now).unwrap();
    drop(store);
    let mut conn = Connection::open(&path).unwrap();
    let tx = conn.transaction().unwrap();
    tx.execute_batch("DROP TRIGGER product_w1_operations_no_update").unwrap();
    assert_eq!(tx.execute("UPDATE product_w1_operations_v1 SET receipt=x'00' WHERE request_key='s1'", []).unwrap(), 1);
    tx.execute_batch(TEST_W1_UPDATE).unwrap(); tx.commit().unwrap();
    assert_eq!(operation_objects(&conn), expected_operation_objects());
    assert_eq!(conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    drop(conn);
    let mut store = ProductStore::reopen_isolated_test_store(&dir.0, "isolated.db", config()).unwrap();
    assert!(store.list_product_scope_grants(&b, "owner", &s2.source_instance_id, now).unwrap()[0].enabled);
    let before = selection_typed_snapshot(&path);
    assert!(matches!(store.w1_change_pack_approval(&a, &s2.source_instance_id, &x, 0, true, "s1", now), Err(ProductError::Invariant("w1 authorization or binding unavailable"))));
    assert_eq!(selection_typed_snapshot(&path), before);
    let rev = store.list_product_scope_grants(&b, "owner", "source-instance-1", now).unwrap()[0].scope_revision;
    store.change_product_scope(&b, "owner", "source-instance-1", ProductCapability::ReadOrder, ScopeMutation::Grant, rev, "restore", now).unwrap();
    assert_eq!(store.read_scoped_order(&a, "order-1", now).unwrap().lifecycle, 1);
    let before = selection_typed_snapshot(&path);
    assert!(matches!(store.w1_change_pack_approval(&a, &s2.source_instance_id, &x, 0, true, "s1", now), Err(ProductError::ContractMismatch)));
    assert_eq!(selection_typed_snapshot(&path), before);
}

#[test]
fn selection_schema_snapshot_survives_reopen() {
    let dir = PrivateDir::new();
    let path = dir.0.join("isolated.db");
    drop(ProductStore::create_isolated_test_store(&dir.0, "isolated.db", config()).unwrap());
    let observe = || {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA recursive_triggers=ON").unwrap();
        let mut stmt = conn.prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name").unwrap();
        let schema: Vec<(String,String,String,Option<String>)> = stmt.query_map([], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap().map(Result::unwrap).collect();
        let pragmas: Vec<i64> = ["PRAGMA foreign_keys", "PRAGMA recursive_triggers"].iter().map(|sql| conn.query_row(sql, [], |r| r.get(0)).unwrap()).collect();
        assert_eq!(pragmas, vec![1, 1]);
        assert_eq!(operation_objects(&conn), expected_operation_objects());
        let marker = marker_state(&conn);
        assert_eq!(marker, (MARKER_DDL.into(), vec![(1,2)]));
        (schema, marker, pragmas, selection_typed_snapshot(&path))
    };
    let before = observe();
    drop(ProductStore::reopen_isolated_test_store(&dir.0, "isolated.db", config()).unwrap());
    assert_eq!(observe(), before);
}

#[test]
fn selection_corrupt_tag_order_provenance_unavailable_with_source_authority() {
    let (dir, mut store, session, now) = selection_fixture();
    let path = dir.0.join("isolated.db");
    let x = "a".repeat(64); let y = "b".repeat(64);
    let before = selection_typed_snapshot(&path);
    let a = store.w1_change_pack_approval(&session, "source-instance-1", &x, 0, true, "a1", now).unwrap();
    selection_assert_insert(&path, before, &a, &session, "approve", None, &x, 0, "a1", None);
    let before = selection_typed_snapshot(&path);
    let s = store.w1_select_pack(&session, "order-1", 1, "source-instance-1", &x, 0, "s1", now).unwrap();
    selection_assert_insert(&path, before, &s, &session, "select", Some("order-1"), &x, 0, "s1", Some(1));
    let before = selection_typed_snapshot(&path);
    let control = store.w1_change_pack_approval(&session, "source-instance-1", &y, 0, true, "control", now).unwrap();
    selection_assert_insert(&path, before, &control, &session, "approve", None, &y, 0, "control", None);
    drop(store);
    let mut conn = Connection::open(&path).unwrap();
    let trigger: String = conn.query_row("SELECT sql FROM sqlite_schema WHERE type='trigger' AND name='product_scope_provenance_immutable_update' AND tbl_name='product_scope_order_provenance_v1'", [], |r| r.get(0)).unwrap();
    assert!(trigger.contains("BEFORE UPDATE ON product_scope_order_provenance_v1"));
    assert_eq!(conn.query_row("SELECT verified FROM product_scope_order_provenance_v1 WHERE order_id='order-1' AND lifecycle=1", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    let tx = conn.transaction().unwrap();
    tx.execute_batch("PRAGMA ignore_check_constraints=ON; DROP TRIGGER product_w1_operations_no_update; DROP TRIGGER product_scope_provenance_immutable_update").unwrap();
    assert_eq!(tx.execute("UPDATE product_w1_operations_v1 SET operation='approve' WHERE actor_id='owner' AND request_key='s1'", []).unwrap(), 1);
    assert_eq!(tx.execute("UPDATE product_scope_order_provenance_v1 SET verified=0 WHERE order_id='order-1' AND lifecycle=1", []).unwrap(), 1);
    tx.execute_batch(TEST_W1_UPDATE).unwrap(); tx.execute_batch(&trigger).unwrap(); tx.commit().unwrap();
    conn.execute_batch("PRAGMA ignore_check_constraints=OFF").unwrap();
    assert_eq!(conn.query_row("SELECT sql FROM sqlite_schema WHERE type='trigger' AND name='product_scope_provenance_immutable_update'", [], |r| r.get::<_, String>(0)).unwrap(), trigger);
    assert_eq!(operation_objects(&conn), expected_operation_objects());
    assert_eq!(conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    drop(conn);
    let mut store = ProductStore::reopen_isolated_test_store(&dir.0, "isolated.db", config()).unwrap();
    let before = selection_typed_snapshot(&path);
    assert_eq!(store.w1_change_pack_approval(&session, "source-instance-1", &y, 0, true, "control", now).unwrap(), control);
    assert_eq!(selection_typed_snapshot(&path), before);
    let before = selection_typed_snapshot(&path);
    assert!(matches!(store.read_scoped_order(&session, "order-1", now), Err(ProductError::Invariant("order scope provenance unavailable"))));
    assert_eq!(selection_typed_snapshot(&path), before);
    let before = selection_typed_snapshot(&path);
    assert!(matches!(store.w1_change_pack_approval(&session, "source-instance-1", &x, 0, true, "s1", now), Err(ProductError::Invariant("w1 authorization or binding unavailable"))));
    assert_eq!(selection_typed_snapshot(&path), before);
    drop(store);
    let mut conn = Connection::open(&path).unwrap();
    let tx = conn.transaction().unwrap();
    tx.execute_batch("DROP TRIGGER product_scope_provenance_immutable_update").unwrap();
    assert_eq!(tx.execute("UPDATE product_scope_order_provenance_v1 SET verified=1 WHERE order_id='order-1' AND lifecycle=1", []).unwrap(), 1);
    tx.execute_batch(&trigger).unwrap(); tx.commit().unwrap();
    assert_eq!(conn.query_row("SELECT sql FROM sqlite_schema WHERE type='trigger' AND name='product_scope_provenance_immutable_update'", [], |r| r.get::<_, String>(0)).unwrap(), trigger);
    assert_eq!(conn.query_row("SELECT operation FROM product_w1_operations_v1 WHERE request_key='s1'", [], |r| r.get::<_, String>(0)).unwrap(), "approve");
    drop(conn);
    let mut store = ProductStore::reopen_isolated_test_store(&dir.0, "isolated.db", config()).unwrap();
    assert_eq!(store.read_scoped_order(&session, "order-1", now).unwrap().lifecycle, 1);
    let before = selection_typed_snapshot(&path);
    assert!(matches!(store.w1_change_pack_approval(&session, "source-instance-1", &x, 0, true, "s1", now), Err(ProductError::ContractMismatch)));
    assert_eq!(selection_typed_snapshot(&path), before);
}

fn service_admission_fixture() -> (PrivateDir, ProductStore, String, chrono::DateTime<chrono::Utc>) {
    use foundry_ledger::{ProductCapability, ProductClock, ScopeGrantManifest};
    use foundry_ledger::product_identity::ProductPasswordKdf;
    let dir = PrivateDir::new();
    let mut store = ProductStore::create_isolated_test_store(&dir.0, "isolated.db", config()).unwrap();
    let source = ScopeSourceManifest {
        connector_id: "gh".into(), source_instance_id: "source-instance-1".into(), generation: 1,
        identity: RepositoryIdentity::canonicalized("acme", "GitHub", "https://api.github.com", "42").unwrap(),
        api_owner: Some("acme".into()), api_repo: Some("proj".into()),
        legacy_repository_locator: "acme/proj".into(), scope_query: "open".into(), enabled: true,
    };
    let grant = ScopeGrantManifest { actor_id: "owner".into(), source_instance_id: source.source_instance_id.clone(), capabilities: vec![ProductCapability::ReadOrder] };
    let phc = ProductPasswordKdf::hash("test-password").unwrap();
    store.bootstrap_owner_scoped("owner", &phc, &[source], &[grant]).unwrap();
    let now = SystemProductClock.now();
    let session = store.login("owner", "test-password", now).unwrap().digest().to_owned();
    let sources = store.list_scoped_sources(&session, now).unwrap();
    assert_eq!(sources.len(), 1);
    assert!(sources[0].current);
    let grants = store.list_product_scope_grants(&session, "owner", "source-instance-1", now).unwrap();
    assert_eq!(grants.len(), 1);
    assert!(grants[0].enabled);
    (dir, store, session, now)
}

#[test]
fn w1_service_scoped_selection_preserves_isolated_admission_stop() {
    use foundry_exec::{Attempt, Epoch, ExecutionIntent, Generation, HostError, OrderId, ReviewAdmissionRequest, ReviewAdmissionStore, ReviewServiceBinding, RunId};
    use foundry_ledger::{ProductPollCursor, ProductServiceAction, ProductServiceGrant, ProductWorkClaimResult};
    use sha2::{Digest, Sha256};

    let (dir, mut store, session, now) = service_admission_fixture();
    let path = dir.0.join("isolated.db");
    let source = "source-instance-1";
    let service_id = "w1-service";
    let grants: Vec<_> = [ProductServiceAction::IntakeRead, ProductServiceAction::AdmitDelivery, ProductServiceAction::AckDelivery, ProductServiceAction::DriveReview].into_iter().map(|action| ProductServiceGrant { source_instance_id: source.into(), action }).collect();
    let service_receipt = store.manage_product_service(&session, service_id, &grants, true, now + chrono::Duration::hours(3), 0, "service-config", now).expect("manage_product_service");
    assert_eq!((service_receipt.service_id.as_str(), service_receipt.revision, service_receipt.enabled), (service_id, 1, true));
    let token = store.issue_product_service_credential(&session, service_id, 1, now + chrono::Duration::hours(2), "service-credential", now).expect("issue_product_service_credential").expose_once_to_file_writer().to_owned();
    let service = store.authenticate_product_service_for_action(&token, "gh", "https://api.github.com", "42", ProductServiceAction::DriveReview, now).expect("authenticate_product_service_for_action DriveReview");
    assert_eq!((service.service_id(), service.source_instance_id()), (service_id, source));
    let issue = "issue:service-w1";
    let payload = "immutable local adapter task";
    let updated = now.to_rfc3339();
    let version = foundry_ledger::product::delivery_version_digest(issue, &updated, "dep-bump", payload);
    let delivery = foundry_ledger::ProductDelivery { delivery_id: foundry_ledger::product::github_delivery_id("w1-install", "gh", "https://api.github.com", "42", issue, &version), connector_id: "gh".into(), repository_id: "42".into(), issue_id: issue.into(), observed_updated_at: updated.clone(), version_digest: version, task_kind: "dep-bump".into(), payload: payload.into() };
    let cursor = ProductPollCursor { connector_id: "gh".into(), query_fingerprint: "w1-service-query".into(), sweep_id: "w1-service-sweep".into(), next_page: 1, page_ceiling: 1, updated_at: updated };
    store.persist_intake_page_for_service(&service, &cursor, &[delivery.clone()], &[], now).expect("persist_intake_page_for_service");
    let receipt = store.admit_intake_delivery_for_service(&service, &delivery.delivery_id, "service-order", now).expect("admit_intake_delivery_for_service");
    assert_eq!((receipt.order_id.as_str(), receipt.lifecycle), ("service-order", 1));
    assert!(!receipt.event_hash.is_empty());
    store.mark_intake_acked_for_service(&service, &delivery.delivery_id, &receipt, now).expect("mark_intake_acked_for_service");
    let view = store.read_scoped_order(&session, &receipt.order_id, now).expect("read_scoped_order");
    assert_eq!((view.order_id.as_str(), view.lifecycle, view.source_instance_id.as_str(), view.admission_sequence), (receipt.order_id.as_str(), receipt.lifecycle, source, receipt.sequence));

    // Synthetic fixture adapter selection: only promote this admitted backlog row.
    let mut expected = selection_typed_snapshot(&path);
    let rows = expected.get_mut("product_work_backlog_v1").unwrap();
    let row = rows.iter_mut().find(|row| row[0] == rusqlite::types::Value::Text(receipt.order_id.clone()) && row[1] == rusqlite::types::Value::Integer(receipt.lifecycle) && row[2] == rusqlite::types::Value::Text(delivery.delivery_id.clone())).expect("admitted backlog row");
    assert_eq!(row[3], rusqlite::types::Value::Text("waiting_unsupported".into()));
    row[3] = rusqlite::types::Value::Text("ready".into());
    rows.sort_by_cached_key(|row| format!("{row:?}"));
    assert_eq!(Connection::open(&path).unwrap().execute("UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id=?1 AND lifecycle=?2 AND delivery_id=?3 AND state='waiting_unsupported'", rusqlite::params![receipt.order_id, receipt.lifecycle, delivery.delivery_id]).unwrap(), 1);
    assert_eq!(selection_typed_snapshot(&path), expected);
    let claim = match store.claim_product_work_for_service(&service, "w1-service-worker", now, chrono::Duration::minutes(5)).expect("claim_product_work_for_service") { ProductWorkClaimResult::Claimed(claim) => claim, other => panic!("expected service claim: {other:?}") };
    let binding = claim.review_binding();
    let mut h = Sha256::new();
    h.update(b"foundry-product-intake-v1\0"); h.update(("dep-bump".len() as u64).to_be_bytes()); h.update(b"dep-bump"); h.update((payload.len() as u64).to_be_bytes()); h.update(payload.as_bytes());
    let payload_digest = hex::encode(h.finalize());
    assert_eq!((binding.order_id.as_str(), binding.lifecycle, binding.delivery_id.as_str(), binding.worker_id.as_str(), binding.epoch, binding.fence, binding.source_revision, binding.lease_until, binding.task_kind.as_str(), binding.payload_snapshot.as_str(), binding.payload_digest.as_str()), (receipt.order_id.as_str(), receipt.lifecycle, delivery.delivery_id.as_str(), "w1-service-worker", 1, 1, 1, now + chrono::Duration::minutes(5), "dep-bump", payload, payload_digest.as_str()));
    assert_eq!(binding.service_binding, Some(ReviewServiceBinding { service_id: service_id.into(), service_revision: 1, credential_revision: 1, source_instance_id: source.into() }));
    let conn = Connection::open(&path).unwrap();
    let linked: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM product_intake_inbox_v1 i JOIN product_intake_source_bindings_v1 ib USING(delivery_id) JOIN product_work_backlog_v1 b USING(delivery_id) JOIN product_orders_v1 o ON o.order_id=b.order_id AND o.lifecycle=b.lifecycle JOIN product_scope_order_provenance_v1 p ON p.order_id=o.order_id AND p.lifecycle=o.lifecycle JOIN product_order_payload_digests_v1 d ON d.order_id=o.order_id WHERE i.delivery_id=?1 AND i.connector_id=?2 AND i.repository_id=?3 AND i.issue_id=?4 AND i.version_digest=?5 AND i.payload=?6 AND i.state='acked' AND ib.source_instance_id=?7 AND p.source_instance_id=?7 AND p.verified=1 AND o.connector_id=i.connector_id AND o.repository_id=i.repository_id AND o.issue_id=i.issue_id AND o.order_id=?8 AND o.lifecycle=?9 AND o.admission_seq=?10 AND o.admission_hash=?11 AND p.admission_seq=?10 AND p.admission_hash=?11 AND d.payload_digest=?12)",
        rusqlite::params![delivery.delivery_id, delivery.connector_id, delivery.repository_id, delivery.issue_id, delivery.version_digest, payload, source, receipt.order_id, receipt.lifecycle, receipt.sequence, receipt.event_hash, payload_digest], |row| row.get(0),
    ).unwrap();
    assert!(linked, "intake, receipt, source, order and payload digest must refer to the same admission");
    drop(conn);
    store.validate_product_work_claim(&claim, now).expect("validate_product_work_claim");
    let pack = "a".repeat(64);
    let before = selection_typed_snapshot(&path);
    let approval = store.w1_change_pack_approval(&session, source, &pack, 0, true, "service-approve", now).expect("w1_change_pack_approval");
    selection_assert_insert(&path, before, &approval, &session, "approve", None, &pack, 0, "service-approve", None);
    let before = selection_typed_snapshot(&path);
    let selection = store.w1_select_pack(&session, &receipt.order_id, receipt.lifecycle, source, &pack, 0, "service-select", now).expect("w1_select_pack");
    selection_assert_insert(&path, before, &selection, &session, "select", Some(&receipt.order_id), &pack, 0, "service-select", Some(approval.result_revision));
    store.configure_review_backend(foundry_ledger::ProductReviewConfig { artifact_root: dir.0.join("artifacts"), session_root: dir.0.join("sessions"), claim_root: dir.0.join("claims"), pack_digest: pack.clone() }).expect("configure_review_backend");
    let request = ReviewAdmissionRequest { intent: ExecutionIntent { order: OrderId(receipt.order_id.clone()), attempt: Attempt(1), run: RunId(4), activation: 1, generation: Generation(0), epoch: Epoch(1), external_ref: format!("review:{}:1:base..target", receipt.order_id), recorded_at: now }, base_commit: "base".into(), result_commit: "target".into(), pack_digest: pack, product_work: Some(binding) };
    let before = selection_typed_snapshot(&path);
    for table in ["product_review_runs_v1", "product_review_active_v1", "product_review_work_bindings_v1", "product_review_host_accept_v1", "product_review_authority_v1"] { assert!(before.get(table).expect(table).is_empty(), "{table}"); }
    let admission_binding = request.product_work.as_ref().unwrap();
    let lease_until = admission_binding.lease_until;
    let validation_time = chrono::Utc::now();
    ReviewAdmissionStore::validate_product_work_claim(&store, admission_binding, &request.intent, validation_time).expect("validate admission claim, intent and service binding");
    let error = store.admit_review_activation(request).unwrap_err();
    let after_admission = chrono::Utc::now();
    assert!(after_admission >= validation_time && lease_until > after_admission, "claim must remain unexpired throughout admission");
    assert!(matches!(error, HostError::Acceptance(ref message) if message == "isolated product review admission requires an atomic pin"));
    assert_eq!(selection_typed_snapshot(&path), before);
}

#[test]
fn w1_v3_container_create_reopen_and_tamper() {
    let dir = PrivateDir::new();
    let path = dir.0.join("v3.db");
    let clock = Arc::new(SystemProductClock);
    drop(ProductStore::create_w1_capture_test_store(&dir.0, "v3.db", config(), clock.clone()).unwrap());
    let conn = Connection::open(&path).unwrap();
    let marker: (String, i64) = conn.query_row("SELECT sql,(SELECT version FROM product_w1_isolated_marker_v1) FROM sqlite_schema WHERE name='product_w1_isolated_marker_v1'", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(marker, ("CREATE TABLE product_w1_isolated_marker_v1(singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=3))".into(), 3));
    let mut stmt = conn.prepare("SELECT type,name FROM sqlite_schema WHERE tbl_name='product_w1_claim_authority_v1' ORDER BY type,name").unwrap();
    let objects: Vec<(String,String)> = stmt.query_map([], |r| Ok((r.get(0)?,r.get(1)?))).unwrap().map(Result::unwrap).collect();
    assert_eq!(objects, vec![("index".into(),"sqlite_autoindex_product_w1_claim_authority_v1_1".into()), ("index".into(),"sqlite_autoindex_product_w1_claim_authority_v1_2".into()), ("table".into(),"product_w1_claim_authority_v1".into()), ("trigger".into(),"product_w1_claim_authority_no_delete".into()), ("trigger".into(),"product_w1_claim_authority_no_replace".into()), ("trigger".into(),"product_w1_claim_authority_no_update".into())]);
    drop(stmt);

    // These are literal contract definitions, independent of the implementation.
    let expected_sql = [
        r#"CREATE TABLE product_w1_claim_authority_v1(order_id TEXT NOT NULL, lifecycle INTEGER NOT NULL CHECK(lifecycle>0), epoch INTEGER NOT NULL CHECK(epoch>0), fence INTEGER NOT NULL CHECK(fence>0), installation_id TEXT NOT NULL, database_id TEXT NOT NULL, delivery_id TEXT NOT NULL, worker_id TEXT NOT NULL, claimed_at TEXT NOT NULL, claim_lease_until TEXT NOT NULL, payload_digest TEXT NOT NULL, admission_seq INTEGER NOT NULL, admission_hash TEXT NOT NULL, service_id TEXT NOT NULL, service_revision INTEGER NOT NULL, service_lease_until TEXT NOT NULL, credential_revision INTEGER NOT NULL, credential_expires_at TEXT NOT NULL, grant_action TEXT NOT NULL CHECK(grant_action='drive_review'), grant_revision INTEGER NOT NULL, source_instance_id TEXT NOT NULL, source_generation INTEGER NOT NULL, source_instance_revision INTEGER NOT NULL, connector_id TEXT NOT NULL, connector_revision INTEGER NOT NULL, api_origin TEXT NOT NULL, repository_id TEXT NOT NULL, api_owner TEXT NOT NULL, api_repo TEXT NOT NULL, PRIMARY KEY(order_id,lifecycle,epoch), UNIQUE(fence))"#,
        r#"CREATE TRIGGER product_w1_claim_authority_no_update BEFORE UPDATE ON product_w1_claim_authority_v1 BEGIN SELECT RAISE(ABORT,'immutable claim authority'); END"#,
        r#"CREATE TRIGGER product_w1_claim_authority_no_delete BEFORE DELETE ON product_w1_claim_authority_v1 BEGIN SELECT RAISE(ABORT,'immutable claim authority'); END"#,
        r#"CREATE TRIGGER product_w1_claim_authority_no_replace BEFORE INSERT ON product_w1_claim_authority_v1 WHEN EXISTS(SELECT 1 FROM product_w1_claim_authority_v1 WHERE (order_id=NEW.order_id AND lifecycle=NEW.lifecycle AND epoch=NEW.epoch) OR fence=NEW.fence) BEGIN SELECT RAISE(ABORT,'immutable claim authority'); END"#,
    ];
    let actual_sql: Vec<String> = ["product_w1_claim_authority_v1", "product_w1_claim_authority_no_update", "product_w1_claim_authority_no_delete", "product_w1_claim_authority_no_replace"].iter().map(|name| conn.query_row("SELECT sql FROM sqlite_schema WHERE name=?1", [name], |r| r.get(0)).unwrap()).collect();
    assert_eq!(actual_sql, expected_sql);
    let columns = [("order_id","TEXT",1), ("lifecycle","INTEGER",2), ("epoch","INTEGER",3), ("fence","INTEGER",0), ("installation_id","TEXT",0), ("database_id","TEXT",0), ("delivery_id","TEXT",0), ("worker_id","TEXT",0), ("claimed_at","TEXT",0), ("claim_lease_until","TEXT",0), ("payload_digest","TEXT",0), ("admission_seq","INTEGER",0), ("admission_hash","TEXT",0), ("service_id","TEXT",0), ("service_revision","INTEGER",0), ("service_lease_until","TEXT",0), ("credential_revision","INTEGER",0), ("credential_expires_at","TEXT",0), ("grant_action","TEXT",0), ("grant_revision","INTEGER",0), ("source_instance_id","TEXT",0), ("source_generation","INTEGER",0), ("source_instance_revision","INTEGER",0), ("connector_id","TEXT",0), ("connector_revision","INTEGER",0), ("api_origin","TEXT",0), ("repository_id","TEXT",0), ("api_owner","TEXT",0), ("api_repo","TEXT",0)];
    let mut st = conn.prepare("PRAGMA table_info(product_w1_claim_authority_v1)").unwrap();
    let actual_columns: Vec<(String,String,i64,i64)> = st.query_map([], |r| Ok((r.get(1)?,r.get(2)?,r.get(3)?,r.get(5)?))).unwrap().map(Result::unwrap).collect();
    assert_eq!(actual_columns, columns.iter().map(|(name, ty, pk)| (name.to_string(), ty.to_string(), 1, *pk)).collect::<Vec<_>>());
    drop(st);
    let mut st = conn.prepare("PRAGMA index_list(product_w1_claim_authority_v1)").unwrap();
    let mut indexes: Vec<(String,i64,String,i64)> = st.query_map([], |r| Ok((r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap().map(Result::unwrap).collect();
    drop(st);
    indexes.sort();
    assert_eq!(indexes, vec![("sqlite_autoindex_product_w1_claim_authority_v1_1".into(),1,"pk".into(),0), ("sqlite_autoindex_product_w1_claim_authority_v1_2".into(),1,"u".into(),0)]);
    for (index, expected) in [("sqlite_autoindex_product_w1_claim_authority_v1_1", vec!["order_id", "lifecycle", "epoch"]), ("sqlite_autoindex_product_w1_claim_authority_v1_2", vec!["fence"])] {
        let mut st = conn.prepare(&format!("PRAGMA index_info({index})")).unwrap();
        let names: Vec<String> = st.query_map([], |r| r.get(2)).unwrap().map(Result::unwrap).collect();
        assert_eq!(names, expected);
    }
    assert_eq!(conn.prepare("PRAGMA foreign_key_list(product_w1_claim_authority_v1)").unwrap().query([]).unwrap().next().unwrap().is_none(), true);
    assert_eq!(conn.prepare("PRAGMA foreign_key_check").unwrap().query([]).unwrap().next().unwrap().is_none(), true);
    // These values belong only to this observer, not the product connection.
    let observer_pragmas: (i64,i64) = (conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap(), conn.query_row("PRAGMA recursive_triggers", [], |r| r.get(0)).unwrap());
    assert_eq!(observer_pragmas, (1,0));
    drop(conn);
    let fresh_schema = selection_schema_observation(&path);
    let before = snapshot(&dir.0);
    assert!(matches!(ProductStore::reopen_isolated_test_store(&dir.0, "v3.db", config()), Err(ProductError::ContractMismatch)));
    assert_eq!(snapshot(&dir.0), before);
    assert!(matches!(ProductStore::open(&path, config()), Err(ProductError::ContractMismatch)));
    assert_eq!(snapshot(&dir.0), before);
    drop(ProductStore::reopen_w1_capture_test_store(&dir.0, "v3.db", config(), clock.clone()).unwrap());
    assert_eq!(selection_schema_observation(&path), fresh_schema);
    let v2 = dir.0.join("v2.db");
    drop(ProductStore::create_isolated_test_store(&dir.0, "v2.db", config()).unwrap());
    let before = snapshot(&dir.0);
    assert!(matches!(ProductStore::reopen_w1_capture_test_store(&dir.0, "v2.db", config(), clock.clone()), Err(ProductError::ContractMismatch)));
    assert_eq!(snapshot(&dir.0), before);
    drop(ProductStore::reopen_isolated_test_store(&dir.0, "v2.db", config()).unwrap());
    assert!(v2.exists());
    Connection::open(&path).unwrap().execute_batch("DROP TRIGGER product_w1_claim_authority_no_update").unwrap();
    let before = selection_schema_observation(&path);
    let files = recursive_snapshot(&dir.0);
    assert!(matches!(ProductStore::reopen_w1_capture_test_store(&dir.0, "v3.db", config(), clock), Err(ProductError::ContractMismatch)));
    assert_eq!(selection_schema_observation(&path), before);
    assert_eq!(recursive_snapshot(&dir.0), files);
}

#[test]
fn w1_v3_container_service_claim_stops_without_mutation() {
    v3_first_claim_fixture(false, false);
}

#[test]
fn w1_capture_first_claim_exact() {
    v3_first_claim_fixture(false, false);
}

#[test]
fn w1_capture_atomic_rollback() {
    v3_first_claim_fixture(false, true);
}

#[test]
fn w1_v3_missing_fence_singleton_rejected() {
    v3_first_claim_fixture(true, false);
}

fn v3_first_claim_fixture(missing_fence: bool, rollback: bool) {
    use foundry_ledger::{ProductCapability, ProductClock, ProductServiceAction, ProductServiceGrant, ScopeGrantManifest, ProductPollCursor};
    use foundry_ledger::product_identity::ProductPasswordKdf;
    let dir = PrivateDir::new();
    struct FixedClock(chrono::DateTime<chrono::Utc>);
    impl ProductClock for FixedClock { fn now(&self) -> chrono::DateTime<chrono::Utc> { self.0 } }
    let now = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00+00:00").unwrap().with_timezone(&chrono::Utc);
    let clock = Arc::new(FixedClock(now));
    let mut store = ProductStore::create_w1_capture_test_store(&dir.0, "v3.db", config(), clock.clone()).unwrap();
    let source = ScopeSourceManifest { connector_id: "gh".into(), source_instance_id: "source-instance-1".into(), generation: 1, identity: RepositoryIdentity::canonicalized("acme", "GitHub", "https://api.github.com", "42").unwrap(), api_owner: Some("acme".into()), api_repo: Some("proj".into()), legacy_repository_locator: "acme/proj".into(), scope_query: "open".into(), enabled: true };
    let grant = ScopeGrantManifest { actor_id: "owner".into(), source_instance_id: source.source_instance_id.clone(), capabilities: vec![ProductCapability::ReadOrder] };
    store.bootstrap_owner_scoped("owner", &ProductPasswordKdf::hash("test-password").unwrap(), &[source], &[grant]).unwrap();
    let session = store.login("owner", "test-password", now).unwrap().digest().to_owned();
    let grants: Vec<_> = [ProductServiceAction::IntakeRead, ProductServiceAction::AdmitDelivery, ProductServiceAction::AckDelivery, ProductServiceAction::DriveReview].into_iter().map(|action| ProductServiceGrant { source_instance_id: "source-instance-1".into(), action }).collect();
    let service_receipt = store.manage_product_service(&session, "service", &grants, true, now + chrono::Duration::hours(3), 0, "configure", now).unwrap();
    assert_eq!((service_receipt.service_id.as_str(), service_receipt.revision, service_receipt.enabled), ("service", 1, true));
    let token = store.issue_product_service_credential(&session, "service", 1, now + chrono::Duration::hours(2), "credential", now).unwrap().expose_once_to_file_writer().to_owned();
    let service = store.authenticate_product_service_for_action(&token, "gh", "https://api.github.com", "42", ProductServiceAction::DriveReview, now).unwrap();
    assert_eq!((service.service_id(), service.source_instance_id()), ("service", "source-instance-1"));
    let path = dir.0.join("v3.db");
    let issue = "issue:service-w1";
    let payload = "immutable local adapter task";
    let updated = now.to_rfc3339();
    let version = foundry_ledger::product::delivery_version_digest(issue, &updated, "dep-bump", payload);
    let delivery = foundry_ledger::ProductDelivery { delivery_id: foundry_ledger::product::github_delivery_id("w1-install", "gh", "https://api.github.com", "42", issue, &version), connector_id: "gh".into(), repository_id: "42".into(), issue_id: issue.into(), observed_updated_at: updated.clone(), version_digest: version, task_kind: "dep-bump".into(), payload: payload.into() };
    let cursor = ProductPollCursor { connector_id: "gh".into(), query_fingerprint: "v3-query".into(), sweep_id: "v3-sweep".into(), next_page: 1, page_ceiling: 1, updated_at: updated };
    store.persist_intake_page_for_service(&service, &cursor, &[delivery.clone()], &[], now).unwrap();
    let receipt = store.admit_intake_delivery_for_service(&service, &delivery.delivery_id, "service-order", now).unwrap();
    assert_eq!((receipt.order_id.as_str(), receipt.lifecycle), ("service-order", 1));
    assert!(!receipt.event_hash.is_empty());
    store.mark_intake_acked_for_service(&service, &delivery.delivery_id, &receipt, now).unwrap();
    let view = store.read_scoped_order(&session, &receipt.order_id, now).unwrap();
    assert_eq!((view.order_id.as_str(), view.lifecycle, view.source_instance_id.as_str(), view.admission_sequence), (receipt.order_id.as_str(), receipt.lifecycle, "source-instance-1", receipt.sequence));
    // Synthetic adapter selection: exactly this admitted row changes from waiting_unsupported to ready.
    let mut expected = selection_typed_snapshot(&path);
    let rows = expected.get_mut("product_work_backlog_v1").unwrap();
    let row = rows.iter_mut().find(|row| row[0] == rusqlite::types::Value::Text(receipt.order_id.clone()) && row[1] == rusqlite::types::Value::Integer(receipt.lifecycle) && row[2] == rusqlite::types::Value::Text(delivery.delivery_id.clone())).unwrap();
    assert_eq!(row[3], rusqlite::types::Value::Text("waiting_unsupported".into()));
    row[3] = rusqlite::types::Value::Text("ready".into());
    rows.sort_by_cached_key(|row| format!("{row:?}"));
    assert_eq!(Connection::open(&path).unwrap().execute("UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id=?1 AND lifecycle=?2 AND delivery_id=?3 AND state='waiting_unsupported'", rusqlite::params![receipt.order_id, receipt.lifecycle, delivery.delivery_id]).unwrap(), 1);
    assert_eq!(selection_typed_snapshot(&path), expected);
    if missing_fence {
        use rusqlite::types::Value::{Integer, Text};
        let conn = Connection::open(&path).unwrap();
        let singleton: (i64, i64) = conn.query_row("SELECT singleton,next FROM product_fence_counter_v1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(singleton, (1, 0));
        let backlog = &expected["product_work_backlog_v1"];
        assert_eq!(backlog.len(), 1);
        assert_eq!((&backlog[0][0], &backlog[0][1], &backlog[0][2], &backlog[0][3], &backlog[0][5]), (&Text(receipt.order_id.clone()), &Integer(receipt.lifecycle), &Text(delivery.delivery_id.clone()), &Text("ready".into()), &Integer(0)));
        assert_eq!(conn.execute("DELETE FROM product_fence_counter_v1 WHERE singleton=1", []).unwrap(), 1);
        drop(conn);
        let before = selection_schema_observation(&path);
        let files = recursive_snapshot(&dir.0);
        assert!(matches!(store.claim_product_work_for_service(&service, "worker", now, chrono::Duration::minutes(5)), Err(ProductError::ContractMismatch)));
        assert_eq!(selection_schema_observation(&path), before);
        assert_eq!(recursive_snapshot(&dir.0), files);
        return;
    }
    let current = now;
    let invalid_grants = vec![ProductServiceGrant { source_instance_id: "source-instance-1".into(), action: ProductServiceAction::IntakeRead }];
    store.manage_product_service(&session, "invalid-service", &invalid_grants, true, now + chrono::Duration::hours(3), 0, "invalid-configure", now).unwrap();
    let invalid_token = store.issue_product_service_credential(&session, "invalid-service", 1, now + chrono::Duration::hours(2), "invalid-credential", now).unwrap().expose_once_to_file_writer().to_owned();
    let invalid = store.authenticate_product_service_for_action(&invalid_token, "gh", "https://api.github.com", "42", ProductServiceAction::IntakeRead, now).unwrap();
    let before = selection_typed_snapshot(&path);
    let files = recursive_snapshot(&dir.0);
    assert!(matches!(store.claim_product_work_for_service(&invalid, "worker", now, chrono::Duration::minutes(5)), Err(ProductError::Invariant("service authority is absent, expired, or revoked"))));
    assert_eq!(selection_typed_snapshot(&path), before);
    assert_eq!(recursive_snapshot(&dir.0), files);

    use rusqlite::types::Value as V;
    use sha2::{Digest, Sha256};
    use foundry_ledger::ProductWorkClaimResult;
    use foundry_exec::ReviewServiceBinding;
    let mut h = Sha256::new();
    h.update(b"foundry-product-intake-v1\0"); h.update(("dep-bump".len() as u64).to_be_bytes()); h.update(b"dep-bump"); h.update((payload.len() as u64).to_be_bytes()); h.update(payload.as_bytes());
    let digest = hex::encode(h.finalize());
    let lease = now + chrono::Duration::minutes(5);
    let conn = Connection::open(&path).unwrap();
    // Every authority value is observed before the public claim, from its original rows.
    let sql = "SELECT o.order_id,o.lifecycle,1,?1+1,m.installation_id,m.database_id,b.delivery_id,'worker',?2,?3,?4,o.admission_seq,o.admission_hash,s.service_id,s.revision,s.lease_until,c.credential_revision,c.expires_at,g.action,g.grant_revision,i.source_instance_id,i.generation,i.revision,i.connector_id,ps.revision,r.api_origin,r.stable_repository_id,l.api_owner,l.api_repo FROM product_orders_v1 o JOIN product_work_backlog_v1 b ON b.order_id=o.order_id AND b.lifecycle=o.lifecycle JOIN product_scope_order_provenance_v1 p ON p.order_id=o.order_id AND p.lifecycle=o.lifecycle AND p.verified=1 AND p.admission_seq=o.admission_seq AND p.admission_hash=o.admission_hash JOIN product_scope_source_instances_v1 i ON i.source_instance_id=p.source_instance_id JOIN product_scope_repositories_v1 r ON r.repository_key=i.repository_key JOIN product_scope_api_locators_v1 l ON l.source_instance_id=i.source_instance_id JOIN product_sources_v1 ps ON ps.connector_id=i.connector_id JOIN product_service_grants_v1 g ON g.source_instance_id=i.source_instance_id AND g.action='drive_review' AND g.enabled=1 JOIN product_services_v1 s ON s.service_id=g.service_id JOIN product_service_credentials_v1 c ON c.service_id=s.service_id CROSS JOIN product_meta_v1 m WHERE o.order_id=?5 AND s.service_id='service' AND m.singleton=1";
    let old_fence = match &before["product_fence_counter_v1"][0][1] { V::Integer(n) => *n, _ => panic!("fence type") };
    let capture: Vec<V> = conn.query_row(sql, rusqlite::params![old_fence, now.to_rfc3339(), lease.to_rfc3339(), digest, receipt.order_id], |r| (0..29).map(|i| r.get(i)).collect()).unwrap();
    let schema_before = selection_schema_observation(&path);
    let marker_before = marker_state(&conn);
    assert_eq!(marker_before, (MARKER_DDL.replace("version=2", "version=3"), vec![(1, 3)]));
    let observer_pragmas: (i64,i64) = (conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap(), conn.query_row("PRAGMA recursive_triggers", [], |r| r.get(0)).unwrap());
    assert_eq!(observer_pragmas, (1, 0));
    drop(conn);
    assert_ne!(capture[9], capture[15]); assert_ne!(capture[9], capture[17]); assert_ne!(capture[15], capture[17]);
    assert_eq!(capture[0], V::Text(receipt.order_id.clone())); assert_eq!(capture[1], V::Integer(receipt.lifecycle));
    assert_eq!(capture[6], V::Text(delivery.delivery_id.clone()));
    let mut expected = before.clone();
    expected.get_mut("product_fence_counter_v1").unwrap()[0][1] = V::Integer(old_fence + 1);
    let backlog = &mut expected.get_mut("product_work_backlog_v1").unwrap()[0];
    assert_eq!(backlog[3], V::Text("ready".into())); assert_eq!(backlog[5], V::Integer(0));
    backlog[3] = V::Text("claimed".into()); backlog[5] = V::Integer(1);
    expected.get_mut("product_work_claims_v1").unwrap().push(vec![capture[0].clone(),capture[1].clone(),V::Text("worker".into()),V::Integer(1),capture[3].clone(),V::Text("claimed".into()),capture[9].clone(),capture[24].clone(),V::Text(digest.clone())]);
    expected.get_mut("product_service_work_bindings_v1").unwrap().push(vec![capture[0].clone(),capture[1].clone(),capture[13].clone(),capture[14].clone(),capture[16].clone(),capture[20].clone()]);
    expected.get_mut("product_w1_claim_authority_v1").unwrap().push(capture.clone());
    if rollback {
        store.w1_capture_fail_after_legacy_writes_once().unwrap();
        let rollback_before = selection_schema_observation(&path);
        let rollback_marker = marker_state(&Connection::open(&path).unwrap());
        let mut rollback_files = recursive_snapshot(&dir.0);
        assert_eq!(rollback_before.1, vec![1, 1]); // Observer connection only.
        assert_eq!(rollback_before.2, before);
        assert_eq!(rollback_marker, marker_before);
        assert!(!rollback_before.0.is_empty());
        assert_eq!(before["product_fence_counter_v1"], vec![vec![V::Integer(1), V::Integer(0)]]);
        assert_eq!(before["product_work_backlog_v1"].len(), 1);
        for table in ["product_work_claims_v1", "product_service_work_bindings_v1", "product_w1_claim_authority_v1"] {
            assert!(before[table].is_empty(), "{table}");
        }
        assert!(rollback_files.contains_key(&PathBuf::from("v3.db")));
        assert!(matches!(store.claim_product_work_for_service(&service, "worker", now + chrono::Duration::days(1), chrono::Duration::minutes(5)), Err(ProductError::Invariant("w1 capture injected failure"))));
        assert_eq!(selection_schema_observation(&path), rollback_before);
        assert_eq!(marker_state(&Connection::open(&path).unwrap()), rollback_marker);
        let mut after_failure_files = recursive_snapshot(&dir.0);
        rollback_files.get_mut(&PathBuf::from("v3.db")).unwrap().1.clear();
        after_failure_files.get_mut(&PathBuf::from("v3.db")).unwrap().1.clear();
        assert_eq!(after_failure_files, rollback_files);
        // Reopen independently without losing the consumed one-shot flag on the original handle.
        drop(ProductStore::reopen_w1_capture_test_store(&dir.0, "v3.db", config(), clock.clone()).unwrap());
        assert_eq!(selection_schema_observation(&path), rollback_before);
        assert_eq!(marker_state(&Connection::open(&path).unwrap()), rollback_marker);
        let mut reopened_files = recursive_snapshot(&dir.0);
        reopened_files.get_mut(&PathBuf::from("v3.db")).unwrap().1.clear();
        assert_eq!(reopened_files, rollback_files);
    }
    let claim = match store.claim_product_work_for_service(&service, "worker", now + chrono::Duration::days(1), chrono::Duration::minutes(5)).unwrap() { ProductWorkClaimResult::Claimed(c) => c, other => panic!("expected claim: {other:?}") };
    let binding = claim.review_binding();
    assert_eq!((binding.order_id.as_str(),binding.lifecycle,binding.delivery_id.as_str(),binding.worker_id.as_str(),binding.epoch,binding.fence,binding.source_revision,binding.lease_until,binding.task_kind.as_str(),binding.payload_snapshot.as_str(),binding.payload_digest.as_str()), (receipt.order_id.as_str(),receipt.lifecycle,delivery.delivery_id.as_str(),"worker",1,old_fence+1,1,lease,"dep-bump",payload,digest.as_str()));
    assert_eq!(binding.service_binding, Some(ReviewServiceBinding { service_id: "service".into(), service_revision: match capture[14] { V::Integer(n) => n, _ => panic!() }, credential_revision: match capture[16] { V::Integer(n) => n, _ => panic!() }, source_instance_id: "source-instance-1".into() }));
    assert_eq!(selection_typed_snapshot(&path), expected);
    let schema_after = selection_schema_observation(&path);
    assert_eq!(schema_after.0, schema_before.0); assert_eq!(schema_after.1, schema_before.1);
    assert_eq!(marker_state(&Connection::open(&path).unwrap()), marker_before);
    let mut after_files = recursive_snapshot(&dir.0); let mut before_files = files;
    after_files.get_mut(&PathBuf::from("v3.db")).unwrap().1.clear(); before_files.get_mut(&PathBuf::from("v3.db")).unwrap().1.clear();
    assert_eq!(after_files, before_files);
    drop(store);
    let mut store = ProductStore::reopen_w1_capture_test_store(&dir.0, "v3.db", config(), clock).unwrap();
    assert_eq!(marker_state(&Connection::open(&path).unwrap()), marker_before);
    if rollback {
        assert_eq!(selection_typed_snapshot(&path), expected);
        let reopened_schema = selection_schema_observation(&path);
        assert_eq!(reopened_schema.0, schema_before.0);
        assert_eq!(reopened_schema.1, schema_before.1);
        let mut reopened_files = recursive_snapshot(&dir.0);
        reopened_files.get_mut(&PathBuf::from("v3.db")).unwrap().1.clear();
        assert_eq!(reopened_files, before_files);
    }
    assert_eq!(selection_typed_snapshot(&path)["product_w1_claim_authority_v1"], vec![capture]);
    let digest = "a".repeat(64);
    let before = selection_typed_snapshot(&path);
    let approval = store.w1_change_pack_approval(&session, "source-instance-1", &digest, 0, true, "approve", current).unwrap();
    selection_assert_insert(&path, before, &approval, &session, "approve", None, &digest, 0, "approve", None);
    let before = selection_typed_snapshot(&path);
    let selected = store.w1_select_pack(&session, &receipt.order_id, receipt.lifecycle, "source-instance-1", &digest, 0, "select", current).unwrap();
    selection_assert_insert(&path, before, &selected, &session, "select", Some(&receipt.order_id), &digest, 0, "select", Some(approval.result_revision));
}

#[test]
fn w1_v3_container_review_admission_denied() {
    use foundry_exec::{HostError, ReviewAdmissionStore};
    let dir = PrivateDir::new();
    let path = dir.0.join("v3.db");
    let clock = Arc::new(SystemProductClock);
    let mut store = ProductStore::create_w1_capture_test_store(&dir.0, "v3.db", config(), clock.clone()).unwrap();
    let request = prepare_review(&mut store, &dir.0, &path);
    let before = selection_typed_snapshot(&path);
    let files = recursive_snapshot(&dir.0);
    let validation_time = chrono::Utc::now();
    ReviewAdmissionStore::validate_product_work_claim(&store, request.product_work.as_ref().unwrap(), &request.intent, validation_time).unwrap();
    assert!(matches!(store.admit_review_activation(request.clone()), Err(HostError::Acceptance(ref m)) if m == "isolated product review admission requires an atomic pin"));
    assert!(chrono::Utc::now() < request.product_work.as_ref().unwrap().lease_until);
    assert_eq!(selection_typed_snapshot(&path), before);
    assert_eq!(recursive_snapshot(&dir.0), files);
    drop(store);
    let mut store = ProductStore::reopen_w1_capture_test_store(&dir.0, "v3.db", config(), clock).unwrap();
    store.configure_review_backend(foundry_ledger::ProductReviewConfig { artifact_root: dir.0.join("artifacts"), session_root: dir.0.join("sessions"), claim_root: dir.0.join("claims"), pack_digest: "a".repeat(64) }).unwrap();
    let before = selection_typed_snapshot(&path);
    let files = recursive_snapshot(&dir.0);
    let validation_time = chrono::Utc::now();
    ReviewAdmissionStore::validate_product_work_claim(&store, request.product_work.as_ref().unwrap(), &request.intent, validation_time).unwrap();
    let lease = request.product_work.as_ref().unwrap().lease_until;
    assert!(matches!(store.admit_review_activation(request.clone()), Err(HostError::Acceptance(ref m)) if m == "isolated product review admission requires an atomic pin"));
    assert!(chrono::Utc::now() < lease);
    assert_eq!(selection_typed_snapshot(&path), before);
    assert_eq!(recursive_snapshot(&dir.0), files);
}

#[test]
fn w1_v3_container_capture_triggers_deny_mutation() {
    let dir = PrivateDir::new();
    let path = dir.0.join("v3.db");
    drop(ProductStore::create_w1_capture_test_store(&dir.0, "v3.db", config(), Arc::new(SystemProductClock)).unwrap());
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA recursive_triggers=ON; PRAGMA foreign_keys=ON;").unwrap();
    let values = "('order',1,1,1,'installation','database','delivery','worker','claimed','claim lease','digest',1,'hash','service',1,'service lease',1,'expiry','drive_review',1,'source',1,1,'connector',1,'origin','repository','owner','repo')";
    conn.execute(&format!("INSERT INTO product_w1_claim_authority_v1 VALUES{values}"), []).unwrap();
    for sql in ["UPDATE product_w1_claim_authority_v1 SET worker_id='other'".to_owned(), "DELETE FROM product_w1_claim_authority_v1".to_owned(), format!("INSERT OR REPLACE INTO product_w1_claim_authority_v1 VALUES{values}"), format!("INSERT OR REPLACE INTO product_w1_claim_authority_v1 VALUES{}", values.replacen("'order',1,1,1", "'other',1,1,1", 1))] {
        let before = selection_typed_snapshot(&path);
        assert!(conn.execute(&sql, []).is_err(), "{sql}");
        assert_eq!(selection_typed_snapshot(&path), before);
    }
}

#[test]
fn w1_capture_reclaim_append() {
    use foundry_ledger::{ProductCapability, ProductClock, ProductServiceAction, ProductServiceGrant, ScopeGrantManifest, ProductPollCursor};
    use foundry_ledger::product_identity::ProductPasswordKdf;
    let dir = PrivateDir::new();
    struct FixedClock {
        time: std::sync::Mutex<chrono::DateTime<chrono::Utc>>,
        samples: std::sync::atomic::AtomicUsize,
    }
    impl ProductClock for FixedClock {
        fn now(&self) -> chrono::DateTime<chrono::Utc> {
            self.samples.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *self.time.lock().unwrap()
        }
    }
    let now = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00+00:00").unwrap().with_timezone(&chrono::Utc);
    let clock = Arc::new(FixedClock { time: std::sync::Mutex::new(now), samples: std::sync::atomic::AtomicUsize::new(0) });
    let mut store = ProductStore::create_w1_capture_test_store(&dir.0, "v3.db", config(), clock.clone()).unwrap();
    let source = ScopeSourceManifest { connector_id: "gh".into(), source_instance_id: "source-instance-1".into(), generation: 1, identity: RepositoryIdentity::canonicalized("acme", "GitHub", "https://api.github.com", "42").unwrap(), api_owner: Some("acme".into()), api_repo: Some("proj".into()), legacy_repository_locator: "acme/proj".into(), scope_query: "open".into(), enabled: true };
    let grant = ScopeGrantManifest { actor_id: "owner".into(), source_instance_id: source.source_instance_id.clone(), capabilities: vec![ProductCapability::ReadOrder] };
    store.bootstrap_owner_scoped("owner", &ProductPasswordKdf::hash("test-password").unwrap(), &[source], &[grant]).unwrap();
    let session = store.login("owner", "test-password", now).unwrap().digest().to_owned();
    let grants: Vec<_> = [ProductServiceAction::IntakeRead, ProductServiceAction::AdmitDelivery, ProductServiceAction::AckDelivery, ProductServiceAction::DriveReview].into_iter().map(|action| ProductServiceGrant { source_instance_id: "source-instance-1".into(), action }).collect();
    let service_receipt = store.manage_product_service(&session, "service", &grants, true, now + chrono::Duration::hours(3), 0, "configure", now).unwrap();
    assert_eq!((service_receipt.service_id.as_str(), service_receipt.revision, service_receipt.enabled), ("service", 1, true));
    let token = store.issue_product_service_credential(&session, "service", 1, now + chrono::Duration::hours(2), "credential", now).unwrap().expose_once_to_file_writer().to_owned();
    let service = store.authenticate_product_service_for_action(&token, "gh", "https://api.github.com", "42", ProductServiceAction::DriveReview, now).unwrap();
    assert_eq!((service.service_id(), service.source_instance_id()), ("service", "source-instance-1"));
    let path = dir.0.join("v3.db");
    let issue = "issue:service-w1";
    let payload = "immutable local adapter task";
    let updated = now.to_rfc3339();
    let version = foundry_ledger::product::delivery_version_digest(issue, &updated, "dep-bump", payload);
    let delivery = foundry_ledger::ProductDelivery { delivery_id: foundry_ledger::product::github_delivery_id("w1-install", "gh", "https://api.github.com", "42", issue, &version), connector_id: "gh".into(), repository_id: "42".into(), issue_id: issue.into(), observed_updated_at: updated.clone(), version_digest: version, task_kind: "dep-bump".into(), payload: payload.into() };
    let cursor = ProductPollCursor { connector_id: "gh".into(), query_fingerprint: "v3-query".into(), sweep_id: "v3-sweep".into(), next_page: 1, page_ceiling: 1, updated_at: updated };
    store.persist_intake_page_for_service(&service, &cursor, &[delivery.clone()], &[], now).unwrap();
    let receipt = store.admit_intake_delivery_for_service(&service, &delivery.delivery_id, "service-order", now).unwrap();
    assert_eq!((receipt.order_id.as_str(), receipt.lifecycle), ("service-order", 1));
    assert!(!receipt.event_hash.is_empty());
    store.mark_intake_acked_for_service(&service, &delivery.delivery_id, &receipt, now).unwrap();
    let view = store.read_scoped_order(&session, &receipt.order_id, now).unwrap();
    assert_eq!((view.order_id.as_str(), view.lifecycle, view.source_instance_id.as_str(), view.admission_sequence), (receipt.order_id.as_str(), receipt.lifecycle, "source-instance-1", receipt.sequence));
    // Synthetic adapter selection: exactly this admitted row changes from waiting_unsupported to ready.
    let mut expected = selection_typed_snapshot(&path);
    let rows = expected.get_mut("product_work_backlog_v1").unwrap();
    let row = rows.iter_mut().find(|row| row[0] == rusqlite::types::Value::Text(receipt.order_id.clone()) && row[1] == rusqlite::types::Value::Integer(receipt.lifecycle) && row[2] == rusqlite::types::Value::Text(delivery.delivery_id.clone())).unwrap();
    assert_eq!(row[3], rusqlite::types::Value::Text("waiting_unsupported".into()));
    row[3] = rusqlite::types::Value::Text("ready".into());
    rows.sort_by_cached_key(|row| format!("{row:?}"));
    assert_eq!(Connection::open(&path).unwrap().execute("UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id=?1 AND lifecycle=?2 AND delivery_id=?3 AND state='waiting_unsupported'", rusqlite::params![receipt.order_id, receipt.lifecycle, delivery.delivery_id]).unwrap(), 1);
    assert_eq!(selection_typed_snapshot(&path), expected);
    let before = selection_typed_snapshot(&path);
    let files = recursive_snapshot(&dir.0);
    use rusqlite::types::Value as V;
    use sha2::{Digest, Sha256};
    use foundry_ledger::ProductWorkClaimResult;
    use foundry_exec::ReviewServiceBinding;
    let mut h = Sha256::new();
    h.update(b"foundry-product-intake-v1\0"); h.update(("dep-bump".len() as u64).to_be_bytes()); h.update(b"dep-bump"); h.update((payload.len() as u64).to_be_bytes()); h.update(payload.as_bytes());
    let digest = hex::encode(h.finalize());
    let lease = now + chrono::Duration::minutes(5);
    let conn = Connection::open(&path).unwrap();
    // Every authority value is observed before the public claim, from its original rows.
    let sql = "SELECT o.order_id,o.lifecycle,1,?1+1,m.installation_id,m.database_id,b.delivery_id,'worker',?2,?3,?4,o.admission_seq,o.admission_hash,s.service_id,s.revision,s.lease_until,c.credential_revision,c.expires_at,g.action,g.grant_revision,i.source_instance_id,i.generation,i.revision,i.connector_id,ps.revision,r.api_origin,r.stable_repository_id,l.api_owner,l.api_repo FROM product_orders_v1 o JOIN product_work_backlog_v1 b ON b.order_id=o.order_id AND b.lifecycle=o.lifecycle JOIN product_scope_order_provenance_v1 p ON p.order_id=o.order_id AND p.lifecycle=o.lifecycle AND p.verified=1 AND p.admission_seq=o.admission_seq AND p.admission_hash=o.admission_hash JOIN product_scope_source_instances_v1 i ON i.source_instance_id=p.source_instance_id JOIN product_scope_repositories_v1 r ON r.repository_key=i.repository_key JOIN product_scope_api_locators_v1 l ON l.source_instance_id=i.source_instance_id JOIN product_sources_v1 ps ON ps.connector_id=i.connector_id JOIN product_service_grants_v1 g ON g.source_instance_id=i.source_instance_id AND g.action='drive_review' AND g.enabled=1 JOIN product_services_v1 s ON s.service_id=g.service_id JOIN product_service_credentials_v1 c ON c.service_id=s.service_id CROSS JOIN product_meta_v1 m WHERE o.order_id=?5 AND s.service_id='service' AND m.singleton=1";
    let old_fence = match &before["product_fence_counter_v1"][0][1] { V::Integer(n) => *n, _ => panic!("fence type") };
    let capture: Vec<V> = conn.query_row(sql, rusqlite::params![old_fence, now.to_rfc3339(), lease.to_rfc3339(), digest, receipt.order_id], |r| (0..29).map(|i| r.get(i)).collect()).unwrap();
    let schema_before = selection_schema_observation(&path);
    let marker_before = marker_state(&conn);
    assert_eq!(marker_before, (MARKER_DDL.replace("version=2", "version=3"), vec![(1, 3)]));
    let observer_pragmas: (i64,i64) = (conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap(), conn.query_row("PRAGMA recursive_triggers", [], |r| r.get(0)).unwrap());
    assert_eq!(observer_pragmas, (1, 0));
    drop(conn);
    assert_ne!(capture[9], capture[15]); assert_ne!(capture[9], capture[17]); assert_ne!(capture[15], capture[17]);
    assert_eq!(capture[0], V::Text(receipt.order_id.clone())); assert_eq!(capture[1], V::Integer(receipt.lifecycle));
    assert_eq!(capture[6], V::Text(delivery.delivery_id.clone()));
    let mut expected = before.clone();
    expected.get_mut("product_fence_counter_v1").unwrap()[0][1] = V::Integer(old_fence + 1);
    let backlog = &mut expected.get_mut("product_work_backlog_v1").unwrap()[0];
    assert_eq!(backlog[3], V::Text("ready".into())); assert_eq!(backlog[5], V::Integer(0));
    backlog[3] = V::Text("claimed".into()); backlog[5] = V::Integer(1);
    expected.get_mut("product_work_claims_v1").unwrap().push(vec![capture[0].clone(),capture[1].clone(),V::Text("worker".into()),V::Integer(1),capture[3].clone(),V::Text("claimed".into()),capture[9].clone(),capture[24].clone(),V::Text(digest.clone())]);
    expected.get_mut("product_service_work_bindings_v1").unwrap().push(vec![capture[0].clone(),capture[1].clone(),capture[13].clone(),capture[14].clone(),capture[16].clone(),capture[20].clone()]);
    expected.get_mut("product_w1_claim_authority_v1").unwrap().push(capture.clone());

    use std::sync::atomic::Ordering;
    let samples_before = clock.samples.load(Ordering::SeqCst);
    let claim = match store.claim_product_work_for_service(&service, "worker", now + chrono::Duration::days(1), chrono::Duration::minutes(5)).unwrap() {
        ProductWorkClaimResult::Claimed(claim) => claim,
        other => panic!("expected first claim: {other:?}"),
    };
    assert_eq!(clock.samples.load(Ordering::SeqCst), samples_before + 1);
    let binding = claim.review_binding();
    let expected_service = Some(ReviewServiceBinding { service_id: "service".into(), service_revision: match capture[14] { V::Integer(n) => n, _ => panic!() }, credential_revision: match capture[16] { V::Integer(n) => n, _ => panic!() }, source_instance_id: "source-instance-1".into() });
    assert_eq!((binding.order_id.as_str(), binding.lifecycle, binding.delivery_id.as_str(), binding.worker_id.as_str(), binding.epoch, binding.fence, binding.source_revision, binding.lease_until, binding.task_kind.as_str(), binding.payload_snapshot.as_str(), binding.payload_digest.as_str()), (receipt.order_id.as_str(), receipt.lifecycle, delivery.delivery_id.as_str(), "worker", 1, old_fence + 1, 1, lease, "dep-bump", payload, digest.as_str()));
    assert_eq!(binding.service_binding, expected_service);
    let mut expected_observation = schema_before.clone();
    expected_observation.2 = expected.clone();
    assert_eq!(selection_schema_observation(&path), expected_observation);
    assert_eq!(marker_state(&Connection::open(&path).unwrap()), marker_before);
    let mut expected_files = files;
    expected_files.get_mut(&PathBuf::from("v3.db")).unwrap().1.clear();
    let files_without_db_bytes = || {
        let mut files = recursive_snapshot(&dir.0);
        files.get_mut(&PathBuf::from("v3.db")).unwrap().1.clear();
        files
    };
    assert_eq!(files_without_db_bytes(), expected_files);

    *clock.time.lock().unwrap() = now + chrono::Duration::minutes(4);
    assert!(matches!(store.claim_product_work_for_service(&service, "other-worker", now + chrono::Duration::days(1), chrono::Duration::minutes(5)).unwrap(), ProductWorkClaimResult::Empty));
    assert_eq!(clock.samples.load(Ordering::SeqCst), samples_before + 2);
    assert_eq!(selection_schema_observation(&path), expected_observation);
    assert_eq!(marker_state(&Connection::open(&path).unwrap()), marker_before);
    assert_eq!(files_without_db_bytes(), expected_files);
    assert_eq!(expected["product_work_backlog_v1"][0][5], V::Integer(1));
    assert_eq!(expected["product_w1_claim_authority_v1"], vec![capture.clone()]);

    let reclaim_time = now + chrono::Duration::minutes(6);
    let reclaim_lease = reclaim_time + chrono::Duration::minutes(5);
    *clock.time.lock().unwrap() = reclaim_time;
    let mut next_capture = capture.clone();
    next_capture[2] = V::Integer(2);
    next_capture[3] = V::Integer(old_fence + 2);
    next_capture[7] = V::Text("other-worker".into());
    next_capture[8] = V::Text(reclaim_time.to_rfc3339());
    next_capture[9] = V::Text(reclaim_lease.to_rfc3339());
    expected.get_mut("product_fence_counter_v1").unwrap()[0][1] = V::Integer(old_fence + 2);
    expected.get_mut("product_work_backlog_v1").unwrap()[0][5] = V::Integer(2);
    let current_claim = &mut expected.get_mut("product_work_claims_v1").unwrap()[0];
    current_claim[2] = V::Text("other-worker".into());
    current_claim[3] = V::Integer(2);
    current_claim[4] = V::Integer(old_fence + 2);
    current_claim[6] = V::Text(reclaim_lease.to_rfc3339());
    expected.get_mut("product_w1_claim_authority_v1").unwrap().push(next_capture.clone());
    expected.get_mut("product_w1_claim_authority_v1").unwrap().sort_by_cached_key(|row| format!("{row:?}"));
    let reclaim = match store.claim_product_work_for_service(&service, "other-worker", now - chrono::Duration::days(1), chrono::Duration::minutes(5)).unwrap() {
        ProductWorkClaimResult::Claimed(claim) => claim,
        other => panic!("expected reclaim: {other:?}"),
    };
    assert_eq!(clock.samples.load(Ordering::SeqCst), samples_before + 3);
    let binding = reclaim.review_binding();
    assert_eq!((binding.order_id.as_str(), binding.lifecycle, binding.delivery_id.as_str(), binding.worker_id.as_str(), binding.epoch, binding.fence, binding.source_revision, binding.lease_until, binding.task_kind.as_str(), binding.payload_snapshot.as_str(), binding.payload_digest.as_str()), (receipt.order_id.as_str(), receipt.lifecycle, delivery.delivery_id.as_str(), "other-worker", 2, old_fence + 2, 1, reclaim_lease, "dep-bump", payload, digest.as_str()));
    assert_eq!(binding.service_binding, expected_service);
    assert_ne!(capture[3], next_capture[3]);
    expected_observation.2 = expected.clone();
    assert_eq!(selection_schema_observation(&path), expected_observation);
    assert_eq!(marker_state(&Connection::open(&path).unwrap()), marker_before);
    assert_eq!(files_without_db_bytes(), expected_files);
    drop(store);
    let reopened = ProductStore::reopen_w1_capture_test_store(&dir.0, "v3.db", config(), clock.clone()).unwrap();
    assert_eq!(selection_schema_observation(&path), expected_observation);
    assert_eq!(marker_state(&Connection::open(&path).unwrap()), marker_before);
    assert_eq!(files_without_db_bytes(), expected_files);
    drop(reopened);
}

#[test]
fn w1_capture_clock_after_lock() {
    {
    use foundry_ledger::{ProductCapability, ProductClock, ProductServiceAction, ProductServiceGrant, ScopeGrantManifest, ProductPollCursor};
    use foundry_ledger::product_identity::ProductPasswordKdf;
    let dir = PrivateDir::new();
    struct FixedClock {
        time: std::sync::Mutex<chrono::DateTime<chrono::Utc>>,
        samples: std::sync::atomic::AtomicUsize,
        signal: std::sync::mpsc::SyncSender<()>,
    }
    impl ProductClock for FixedClock {
        fn now(&self) -> chrono::DateTime<chrono::Utc> {
            self.samples.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.signal.try_send(()).unwrap();
            *self.time.lock().unwrap()
        }
    }
    let now = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00+00:00").unwrap().with_timezone(&chrono::Utc);
    let (clock_tx, clock_rx) = std::sync::mpsc::sync_channel(1);
    let clock = Arc::new(FixedClock { time: std::sync::Mutex::new(now), samples: std::sync::atomic::AtomicUsize::new(0), signal: clock_tx });
    let mut store = ProductStore::create_w1_capture_test_store(&dir.0, "v3.db", config(), clock.clone()).unwrap();
    let source = ScopeSourceManifest { connector_id: "gh".into(), source_instance_id: "source-instance-1".into(), generation: 1, identity: RepositoryIdentity::canonicalized("acme", "GitHub", "https://api.github.com", "42").unwrap(), api_owner: Some("acme".into()), api_repo: Some("proj".into()), legacy_repository_locator: "acme/proj".into(), scope_query: "open".into(), enabled: true };
    let grant = ScopeGrantManifest { actor_id: "owner".into(), source_instance_id: source.source_instance_id.clone(), capabilities: vec![ProductCapability::ReadOrder] };
    store.bootstrap_owner_scoped("owner", &ProductPasswordKdf::hash("test-password").unwrap(), &[source], &[grant]).unwrap();
    let session = store.login("owner", "test-password", now).unwrap().digest().to_owned();
    let grants: Vec<_> = [ProductServiceAction::IntakeRead, ProductServiceAction::AdmitDelivery, ProductServiceAction::AckDelivery, ProductServiceAction::DriveReview].into_iter().map(|action| ProductServiceGrant { source_instance_id: "source-instance-1".into(), action }).collect();
    let service_receipt = store.manage_product_service(&session, "service", &grants, true, now + chrono::Duration::hours(3), 0, "configure", now).unwrap();
    assert_eq!((service_receipt.service_id.as_str(), service_receipt.revision, service_receipt.enabled), ("service", 1, true));
    let token = store.issue_product_service_credential(&session, "service", 1, now + chrono::Duration::hours(2), "credential", now).unwrap().expose_once_to_file_writer().to_owned();
    let service = store.authenticate_product_service_for_action(&token, "gh", "https://api.github.com", "42", ProductServiceAction::DriveReview, now).unwrap();
    assert_eq!((service.service_id(), service.source_instance_id()), ("service", "source-instance-1"));
    let path = dir.0.join("v3.db");
    let issue = "issue:service-w1";
    let payload = "immutable local adapter task";
    let updated = now.to_rfc3339();
    let version = foundry_ledger::product::delivery_version_digest(issue, &updated, "dep-bump", payload);
    let delivery = foundry_ledger::ProductDelivery { delivery_id: foundry_ledger::product::github_delivery_id("w1-install", "gh", "https://api.github.com", "42", issue, &version), connector_id: "gh".into(), repository_id: "42".into(), issue_id: issue.into(), observed_updated_at: updated.clone(), version_digest: version, task_kind: "dep-bump".into(), payload: payload.into() };
    let cursor = ProductPollCursor { connector_id: "gh".into(), query_fingerprint: "v3-query".into(), sweep_id: "v3-sweep".into(), next_page: 1, page_ceiling: 1, updated_at: updated };
    store.persist_intake_page_for_service(&service, &cursor, &[delivery.clone()], &[], now).unwrap();
    let receipt = store.admit_intake_delivery_for_service(&service, &delivery.delivery_id, "service-order", now).unwrap();
    assert_eq!((receipt.order_id.as_str(), receipt.lifecycle), ("service-order", 1));
    assert!(!receipt.event_hash.is_empty());
    store.mark_intake_acked_for_service(&service, &delivery.delivery_id, &receipt, now).unwrap();
    let view = store.read_scoped_order(&session, &receipt.order_id, now).unwrap();
    assert_eq!((view.order_id.as_str(), view.lifecycle, view.source_instance_id.as_str(), view.admission_sequence), (receipt.order_id.as_str(), receipt.lifecycle, "source-instance-1", receipt.sequence));
    // Synthetic adapter selection: exactly this admitted row changes from waiting_unsupported to ready.
    let mut expected = selection_typed_snapshot(&path);
    let rows = expected.get_mut("product_work_backlog_v1").unwrap();
    let row = rows.iter_mut().find(|row| row[0] == rusqlite::types::Value::Text(receipt.order_id.clone()) && row[1] == rusqlite::types::Value::Integer(receipt.lifecycle) && row[2] == rusqlite::types::Value::Text(delivery.delivery_id.clone())).unwrap();
    assert_eq!(row[3], rusqlite::types::Value::Text("waiting_unsupported".into()));
    row[3] = rusqlite::types::Value::Text("ready".into());
    rows.sort_by_cached_key(|row| format!("{row:?}"));
    assert_eq!(Connection::open(&path).unwrap().execute("UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id=?1 AND lifecycle=?2 AND delivery_id=?3 AND state='waiting_unsupported'", rusqlite::params![receipt.order_id, receipt.lifecycle, delivery.delivery_id]).unwrap(), 1);
    assert_eq!(selection_typed_snapshot(&path), expected);

    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};
    assert_eq!(clock.samples.load(Ordering::SeqCst), 0);
    let baseline = selection_schema_observation(&path);
    let marker = marker_state(&Connection::open(&path).unwrap());
    let files = recursive_snapshot(&dir.0);
    assert_eq!(baseline.2["product_work_backlog_v1"].len(), 1);
    assert_eq!(baseline.2["product_work_backlog_v1"][0][4], rusqlite::types::Value::Text(now.to_rfc3339()));
    let lock = Connection::open(&path).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
    let (busy_tx, busy_rx) = std::sync::mpsc::sync_channel(1);
    store.w1_capture_observe_busy_once(busy_tx).unwrap();
    let worker = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let result = store.claim_product_work_for_service(&service, "worker", now, chrono::Duration::minutes(5));
        let timeout_ms = store.w1_capture_busy_timeout_ms().unwrap();
        done_tx.send((result, timeout_ms)).unwrap();
    });
    started_rx.recv_timeout(Duration::from_millis(500)).expect("worker did not start");
    busy_rx.recv_timeout(Duration::from_millis(500)).expect("claim did not encounter SQLite busy");
    assert_eq!(clock.samples.load(Ordering::SeqCst), 0);
    assert!(matches!(clock_rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)), "clock sampled before busy rendezvous");
    assert!(matches!(done_rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)), "claim completed before busy rendezvous");
    assert!(matches!(clock_rx.recv_timeout(Duration::from_millis(200)), Err(std::sync::mpsc::RecvTimeoutError::Timeout)), "clock sampled under competing lock");
    assert_eq!(clock.samples.load(Ordering::SeqCst), 0);
    assert!(matches!(done_rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)), "claim completed while locked");
    *clock.time.lock().unwrap() = now + chrono::Duration::hours(2);
    assert!(Instant::now() < deadline, "lock release exceeded deadline");
    lock.execute_batch("COMMIT").unwrap();
    let (result, timeout_ms) = done_rx.recv_timeout(Duration::from_secs(4)).expect("claim did not complete");
    worker.join().unwrap();
    assert_eq!(timeout_ms, 5_000);
    assert!(matches!(result, Err(ProductError::Invariant(ref m)) if *m == "service authority is absent, expired, or revoked"), "unexpected claim result: {result:?}");
    assert_eq!(clock_rx.recv_timeout(Duration::from_millis(100)).unwrap(), ());
    assert_eq!(clock.samples.load(Ordering::SeqCst), 1);
    assert_eq!(selection_schema_observation(&path), baseline);
    assert_eq!(marker_state(&Connection::open(&path).unwrap()), marker);
    assert_eq!(recursive_snapshot(&dir.0), files);
    drop(lock);
    }
    {
    use foundry_ledger::{ProductCapability, ProductClock, ProductServiceAction, ProductServiceGrant, ScopeGrantManifest, ProductPollCursor};
    use foundry_ledger::product_identity::ProductPasswordKdf;
    let dir = PrivateDir::new();
    struct FixedClock {
        time: std::sync::Mutex<chrono::DateTime<chrono::Utc>>,
        samples: std::sync::atomic::AtomicUsize,
        signal: std::sync::mpsc::SyncSender<()>,
    }
    impl ProductClock for FixedClock {
        fn now(&self) -> chrono::DateTime<chrono::Utc> {
            self.samples.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.signal.try_send(()).unwrap();
            *self.time.lock().unwrap()
        }
    }
    let now = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00+00:00").unwrap().with_timezone(&chrono::Utc);
    let (clock_tx, clock_rx) = std::sync::mpsc::sync_channel(1);
    let clock = Arc::new(FixedClock { time: std::sync::Mutex::new(now), samples: std::sync::atomic::AtomicUsize::new(0), signal: clock_tx });
    let mut store = ProductStore::create_w1_capture_test_store(&dir.0, "v3.db", config(), clock.clone()).unwrap();
    let source = ScopeSourceManifest { connector_id: "gh".into(), source_instance_id: "source-instance-1".into(), generation: 1, identity: RepositoryIdentity::canonicalized("acme", "GitHub", "https://api.github.com", "42").unwrap(), api_owner: Some("acme".into()), api_repo: Some("proj".into()), legacy_repository_locator: "acme/proj".into(), scope_query: "open".into(), enabled: true };
    let grant = ScopeGrantManifest { actor_id: "owner".into(), source_instance_id: source.source_instance_id.clone(), capabilities: vec![ProductCapability::ReadOrder] };
    store.bootstrap_owner_scoped("owner", &ProductPasswordKdf::hash("test-password").unwrap(), &[source], &[grant]).unwrap();
    let session = store.login("owner", "test-password", now).unwrap().digest().to_owned();
    let grants: Vec<_> = [ProductServiceAction::IntakeRead, ProductServiceAction::AdmitDelivery, ProductServiceAction::AckDelivery, ProductServiceAction::DriveReview].into_iter().map(|action| ProductServiceGrant { source_instance_id: "source-instance-1".into(), action }).collect();
    let service_receipt = store.manage_product_service(&session, "service", &grants, true, now + chrono::Duration::hours(3), 0, "configure", now).unwrap();
    assert_eq!((service_receipt.service_id.as_str(), service_receipt.revision, service_receipt.enabled), ("service", 1, true));
    let token = store.issue_product_service_credential(&session, "service", 1, now + chrono::Duration::hours(2), "credential", now).unwrap().expose_once_to_file_writer().to_owned();
    let service = store.authenticate_product_service_for_action(&token, "gh", "https://api.github.com", "42", ProductServiceAction::DriveReview, now).unwrap();
    assert_eq!((service.service_id(), service.source_instance_id()), ("service", "source-instance-1"));
    let path = dir.0.join("v3.db");
    let issue = "issue:service-w1";
    let payload = "immutable local adapter task";
    let updated = now.to_rfc3339();
    let version = foundry_ledger::product::delivery_version_digest(issue, &updated, "dep-bump", payload);
    let delivery = foundry_ledger::ProductDelivery { delivery_id: foundry_ledger::product::github_delivery_id("w1-install", "gh", "https://api.github.com", "42", issue, &version), connector_id: "gh".into(), repository_id: "42".into(), issue_id: issue.into(), observed_updated_at: updated.clone(), version_digest: version, task_kind: "dep-bump".into(), payload: payload.into() };
    let cursor = ProductPollCursor { connector_id: "gh".into(), query_fingerprint: "v3-query".into(), sweep_id: "v3-sweep".into(), next_page: 1, page_ceiling: 1, updated_at: updated };
    store.persist_intake_page_for_service(&service, &cursor, &[delivery.clone()], &[], now).unwrap();
    let receipt = store.admit_intake_delivery_for_service(&service, &delivery.delivery_id, "service-order", now).unwrap();
    assert_eq!((receipt.order_id.as_str(), receipt.lifecycle), ("service-order", 1));
    assert!(!receipt.event_hash.is_empty());
    store.mark_intake_acked_for_service(&service, &delivery.delivery_id, &receipt, now).unwrap();
    let view = store.read_scoped_order(&session, &receipt.order_id, now).unwrap();
    assert_eq!((view.order_id.as_str(), view.lifecycle, view.source_instance_id.as_str(), view.admission_sequence), (receipt.order_id.as_str(), receipt.lifecycle, "source-instance-1", receipt.sequence));
    // Synthetic adapter selection: exactly this admitted row changes from waiting_unsupported to ready.
    let mut expected = selection_typed_snapshot(&path);
    let rows = expected.get_mut("product_work_backlog_v1").unwrap();
    let row = rows.iter_mut().find(|row| row[0] == rusqlite::types::Value::Text(receipt.order_id.clone()) && row[1] == rusqlite::types::Value::Integer(receipt.lifecycle) && row[2] == rusqlite::types::Value::Text(delivery.delivery_id.clone())).unwrap();
    assert_eq!(row[3], rusqlite::types::Value::Text("waiting_unsupported".into()));
    row[3] = rusqlite::types::Value::Text("ready".into());
    rows.sort_by_cached_key(|row| format!("{row:?}"));
    assert_eq!(Connection::open(&path).unwrap().execute("UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id=?1 AND lifecycle=?2 AND delivery_id=?3 AND state='waiting_unsupported'", rusqlite::params![receipt.order_id, receipt.lifecycle, delivery.delivery_id]).unwrap(), 1);
    assert_eq!(selection_typed_snapshot(&path), expected);
    let before = selection_typed_snapshot(&path);
    let files = recursive_snapshot(&dir.0);
    use rusqlite::types::Value as V;
    use sha2::{Digest, Sha256};
    use foundry_ledger::ProductWorkClaimResult;
    use foundry_exec::ReviewServiceBinding;
    let mut h = Sha256::new();
    h.update(b"foundry-product-intake-v1\0"); h.update(("dep-bump".len() as u64).to_be_bytes()); h.update(b"dep-bump"); h.update((payload.len() as u64).to_be_bytes()); h.update(payload.as_bytes());
    let digest = hex::encode(h.finalize());
    let sample = now + chrono::Duration::hours(1);
    let stale_caller = now - chrono::Duration::hours(1);
    assert_eq!(before["product_work_backlog_v1"][0][4], V::Text(now.to_rfc3339()));
    assert!(stale_caller < now && now <= sample);
    let lease = sample + chrono::Duration::minutes(5);
    let conn = Connection::open(&path).unwrap();
    // Every authority value is observed before the public claim, from its original rows.
    let sql = "SELECT o.order_id,o.lifecycle,1,?1+1,m.installation_id,m.database_id,b.delivery_id,'worker',?2,?3,?4,o.admission_seq,o.admission_hash,s.service_id,s.revision,s.lease_until,c.credential_revision,c.expires_at,g.action,g.grant_revision,i.source_instance_id,i.generation,i.revision,i.connector_id,ps.revision,r.api_origin,r.stable_repository_id,l.api_owner,l.api_repo FROM product_orders_v1 o JOIN product_work_backlog_v1 b ON b.order_id=o.order_id AND b.lifecycle=o.lifecycle JOIN product_scope_order_provenance_v1 p ON p.order_id=o.order_id AND p.lifecycle=o.lifecycle AND p.verified=1 AND p.admission_seq=o.admission_seq AND p.admission_hash=o.admission_hash JOIN product_scope_source_instances_v1 i ON i.source_instance_id=p.source_instance_id JOIN product_scope_repositories_v1 r ON r.repository_key=i.repository_key JOIN product_scope_api_locators_v1 l ON l.source_instance_id=i.source_instance_id JOIN product_sources_v1 ps ON ps.connector_id=i.connector_id JOIN product_service_grants_v1 g ON g.source_instance_id=i.source_instance_id AND g.action='drive_review' AND g.enabled=1 JOIN product_services_v1 s ON s.service_id=g.service_id JOIN product_service_credentials_v1 c ON c.service_id=s.service_id CROSS JOIN product_meta_v1 m WHERE o.order_id=?5 AND s.service_id='service' AND m.singleton=1";
    let old_fence = match &before["product_fence_counter_v1"][0][1] { V::Integer(n) => *n, _ => panic!("fence type") };
    let capture: Vec<V> = conn.query_row(sql, rusqlite::params![old_fence, sample.to_rfc3339(), lease.to_rfc3339(), digest, receipt.order_id], |r| (0..29).map(|i| r.get(i)).collect()).unwrap();
    let schema_before = selection_schema_observation(&path);
    let marker_before = marker_state(&conn);
    assert_eq!(marker_before, (MARKER_DDL.replace("version=2", "version=3"), vec![(1, 3)]));
    let observer_pragmas: (i64,i64) = (conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap(), conn.query_row("PRAGMA recursive_triggers", [], |r| r.get(0)).unwrap());
    assert_eq!(observer_pragmas, (1, 0));
    drop(conn);
    assert_ne!(capture[9], capture[15]); assert_ne!(capture[9], capture[17]); assert_ne!(capture[15], capture[17]);
    assert_eq!(capture[0], V::Text(receipt.order_id.clone())); assert_eq!(capture[1], V::Integer(receipt.lifecycle));
    assert_eq!(capture[6], V::Text(delivery.delivery_id.clone()));
    let mut expected = before.clone();
    expected.get_mut("product_fence_counter_v1").unwrap()[0][1] = V::Integer(old_fence + 1);
    let backlog = &mut expected.get_mut("product_work_backlog_v1").unwrap()[0];
    assert_eq!(backlog[3], V::Text("ready".into())); assert_eq!(backlog[5], V::Integer(0));
    backlog[3] = V::Text("claimed".into()); backlog[5] = V::Integer(1);
    expected.get_mut("product_work_claims_v1").unwrap().push(vec![capture[0].clone(),capture[1].clone(),V::Text("worker".into()),V::Integer(1),capture[3].clone(),V::Text("claimed".into()),capture[9].clone(),capture[24].clone(),V::Text(digest.clone())]);
    expected.get_mut("product_service_work_bindings_v1").unwrap().push(vec![capture[0].clone(),capture[1].clone(),capture[13].clone(),capture[14].clone(),capture[16].clone(),capture[20].clone()]);
    expected.get_mut("product_w1_claim_authority_v1").unwrap().push(capture.clone());


    *clock.time.lock().unwrap() = sample;
    let claim = match store.claim_product_work_for_service(&service, "worker", stale_caller, chrono::Duration::minutes(5)).unwrap() {
        ProductWorkClaimResult::Claimed(claim) => claim,
        other => panic!("expected available work at sampled time: {other:?}"),
    };
    assert_eq!(clock_rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(), ());
    assert_eq!(clock.samples.load(std::sync::atomic::Ordering::SeqCst), 1);
    let binding = claim.review_binding();
    assert_eq!((binding.order_id.as_str(), binding.lifecycle, binding.delivery_id.as_str(), binding.worker_id.as_str(), binding.epoch, binding.fence, binding.source_revision, binding.lease_until, binding.task_kind.as_str(), binding.payload_snapshot.as_str(), binding.payload_digest.as_str()), (receipt.order_id.as_str(), receipt.lifecycle, delivery.delivery_id.as_str(), "worker", 1, old_fence + 1, 1, lease, "dep-bump", payload, digest.as_str()));
    assert_eq!(binding.service_binding, Some(ReviewServiceBinding { service_id: "service".into(), service_revision: 1, credential_revision: 1, source_instance_id: "source-instance-1".into() }));
    let mut observation = schema_before.clone();
    observation.2 = expected;
    assert_eq!(selection_schema_observation(&path), observation);
    assert_eq!(marker_state(&Connection::open(&path).unwrap()), marker_before);
    let mut files = files;
    files.get_mut(&PathBuf::from("v3.db")).unwrap().1.clear();
    let files_without_db_bytes = || { let mut current = recursive_snapshot(&dir.0); current.get_mut(&PathBuf::from("v3.db")).unwrap().1.clear(); current };
    assert_eq!(files_without_db_bytes(), files);
    drop(store);
    let reopened = ProductStore::reopen_w1_capture_test_store(&dir.0, "v3.db", config(), clock.clone()).unwrap();
    assert_eq!(selection_schema_observation(&path), observation);
    assert_eq!(marker_state(&Connection::open(&path).unwrap()), marker_before);
    assert_eq!(files_without_db_bytes(), files);
    drop(reopened);
    }
}

#[test]
fn w1_capture_reauthorization() {
    use foundry_ledger::{ProductCapability, ProductClock, ProductServiceAction, ProductServiceGrant, ScopeGrantManifest, ProductPollCursor};
    use foundry_ledger::product_identity::ProductPasswordKdf;
    struct FixedClock {
        time: std::sync::Mutex<chrono::DateTime<chrono::Utc>>,
        samples: std::sync::atomic::AtomicUsize,
    }
    impl ProductClock for FixedClock {
        fn now(&self) -> chrono::DateTime<chrono::Utc> {
            self.samples.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *self.time.lock().unwrap()
        }
    }
    for case in ["disable", "rotate", "revoke", "rebind", "credential_expiry", "combined_expiry"] {
        let dir = PrivateDir::new();
        let now = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00+00:00").unwrap().with_timezone(&chrono::Utc);
        let clock = Arc::new(FixedClock { time: std::sync::Mutex::new(now), samples: std::sync::atomic::AtomicUsize::new(0) });
        let mut store = ProductStore::create_w1_capture_test_store(&dir.0, "v3.db", config(), clock.clone()).unwrap();
        let source = ScopeSourceManifest { connector_id: "gh".into(), source_instance_id: "source-instance-1".into(), generation: 1, identity: RepositoryIdentity::canonicalized("acme", "GitHub", "https://api.github.com", "42").unwrap(), api_owner: Some("acme".into()), api_repo: Some("proj".into()), legacy_repository_locator: "acme/proj".into(), scope_query: "open".into(), enabled: true };
        let grant = ScopeGrantManifest { actor_id: "owner".into(), source_instance_id: source.source_instance_id.clone(), capabilities: vec![ProductCapability::ReadOrder] };
        store.bootstrap_owner_scoped("owner", &ProductPasswordKdf::hash("test-password").unwrap(), &[source], &[grant]).unwrap();
        let session = store.login("owner", "test-password", now).unwrap().digest().to_owned();
        let grants: Vec<_> = [ProductServiceAction::IntakeRead, ProductServiceAction::AdmitDelivery, ProductServiceAction::AckDelivery, ProductServiceAction::DriveReview].into_iter().map(|action| ProductServiceGrant { source_instance_id: "source-instance-1".into(), action }).collect();
        let service_receipt = store.manage_product_service(&session, "service", &grants, true, now + chrono::Duration::hours(3), 0, "configure", now).unwrap();
        assert_eq!((service_receipt.service_id.as_str(), service_receipt.revision, service_receipt.enabled), ("service", 1, true));
        let credential_expiry = now + chrono::Duration::hours(if case == "combined_expiry" { 3 } else { 2 });
        let token = store.issue_product_service_credential(&session, "service", 1, credential_expiry, "credential", now).unwrap().expose_once_to_file_writer().to_owned();
        let service = store.authenticate_product_service_for_action(&token, "gh", "https://api.github.com", "42", ProductServiceAction::DriveReview, now).unwrap();
        assert_eq!((service.service_id(), service.source_instance_id()), ("service", "source-instance-1"));
        let path = dir.0.join("v3.db");
        let issue = "issue:service-w1";
        let payload = "immutable local adapter task";
        let updated = now.to_rfc3339();
        let version = foundry_ledger::product::delivery_version_digest(issue, &updated, "dep-bump", payload);
        let delivery = foundry_ledger::ProductDelivery { delivery_id: foundry_ledger::product::github_delivery_id("w1-install", "gh", "https://api.github.com", "42", issue, &version), connector_id: "gh".into(), repository_id: "42".into(), issue_id: issue.into(), observed_updated_at: updated.clone(), version_digest: version, task_kind: "dep-bump".into(), payload: payload.into() };
        let cursor = ProductPollCursor { connector_id: "gh".into(), query_fingerprint: "v3-query".into(), sweep_id: "v3-sweep".into(), next_page: 1, page_ceiling: 1, updated_at: updated };
        store.persist_intake_page_for_service(&service, &cursor, &[delivery.clone()], &[], now).unwrap();
        let receipt = store.admit_intake_delivery_for_service(&service, &delivery.delivery_id, "service-order", now).unwrap();
        assert_eq!((receipt.order_id.as_str(), receipt.lifecycle), ("service-order", 1));
        assert!(!receipt.event_hash.is_empty());
        store.mark_intake_acked_for_service(&service, &delivery.delivery_id, &receipt, now).unwrap();
        let view = store.read_scoped_order(&session, &receipt.order_id, now).unwrap();
        assert_eq!((view.order_id.as_str(), view.lifecycle, view.source_instance_id.as_str(), view.admission_sequence), (receipt.order_id.as_str(), receipt.lifecycle, "source-instance-1", receipt.sequence));
        // Synthetic adapter selection: exactly this admitted row changes from waiting_unsupported to ready.
        let mut expected = selection_typed_snapshot(&path);
        let rows = expected.get_mut("product_work_backlog_v1").unwrap();
        let row = rows.iter_mut().find(|row| row[0] == rusqlite::types::Value::Text(receipt.order_id.clone()) && row[1] == rusqlite::types::Value::Integer(receipt.lifecycle) && row[2] == rusqlite::types::Value::Text(delivery.delivery_id.clone())).unwrap();
        assert_eq!(row[3], rusqlite::types::Value::Text("waiting_unsupported".into()));
        row[3] = rusqlite::types::Value::Text("ready".into());
        rows.sort_by_cached_key(|row| format!("{row:?}"));
        assert_eq!(Connection::open(&path).unwrap().execute("UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id=?1 AND lifecycle=?2 AND delivery_id=?3 AND state='waiting_unsupported'", rusqlite::params![receipt.order_id, receipt.lifecycle, delivery.delivery_id]).unwrap(), 1);
        assert_eq!(selection_typed_snapshot(&path), expected);
        use foundry_ledger::ProductWorkClaimResult;
        assert!(matches!(store.claim_product_work_for_service(&service, "first", now, chrono::Duration::minutes(5)).unwrap(), ProductWorkClaimResult::Claimed(_)));
        let original = selection_typed_snapshot(&path)["product_w1_claim_authority_v1"].clone();
        assert_eq!(original.len(), 1);
        let revision: i64 = Connection::open(&path).unwrap().query_row("SELECT revision FROM product_services_v1 WHERE service_id='service'", [], |r| r.get(0)).unwrap();
        let claim_time = match case { "credential_expiry" => now + chrono::Duration::hours(2), "combined_expiry" => now + chrono::Duration::hours(3), _ => now + chrono::Duration::minutes(6) };
        match case {
            "disable" => { let receipt = store.manage_product_service(&session, "service", &grants, false, now + chrono::Duration::hours(3), revision, "disable", now).unwrap(); assert!(!receipt.enabled); assert_ne!(receipt.revision, revision); }
            "rotate" => { let old: i64 = Connection::open(&path).unwrap().query_row("SELECT credential_revision FROM product_service_credentials_v1 WHERE service_id='service'", [], |r| r.get(0)).unwrap(); let _token = store.issue_product_service_credential(&session, "service", revision, now + chrono::Duration::hours(2), "rotate", now).unwrap(); let new: i64 = Connection::open(&path).unwrap().query_row("SELECT credential_revision FROM product_service_credentials_v1 WHERE service_id='service'", [], |r| r.get(0)).unwrap(); assert_ne!(old, new); }
            "revoke" => { let old: i64 = Connection::open(&path).unwrap().query_row("SELECT grant_revision FROM product_service_grants_v1 WHERE service_id='service' AND action='drive_review'", [], |r| r.get(0)).unwrap(); let retained: Vec<_> = grants.iter().filter(|g| g.action != ProductServiceAction::DriveReview).cloned().collect(); let receipt = store.manage_product_service(&session, "service", &retained, true, now + chrono::Duration::hours(3), revision, "revoke", now).unwrap(); assert_ne!(receipt.revision, revision); let (new, enabled): (i64,i64) = Connection::open(&path).unwrap().query_row("SELECT grant_revision,enabled FROM product_service_grants_v1 WHERE service_id='service' AND action='drive_review'", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap(); assert_ne!(old,new); assert_eq!(enabled,0); }
            "rebind" => { let rev = store.list_scoped_sources(&session, now).unwrap().iter().find(|s| s.source.source_instance_id == "source-instance-1").unwrap().source_revision; let replacement = store.configure_scoped_source(&session, "gh", RepositoryIdentity::canonicalized("acme", "GitHub", "https://api.github.com", "43").unwrap(), "acme", "other", "acme/other", "open", true, rev, "rebind", now).unwrap(); assert_ne!(replacement.source_instance_id, "source-instance-1"); }
            "credential_expiry" => {}
            "combined_expiry" => {
                let (service_lease, credential_expiry): (String, String) = Connection::open(&path).unwrap().query_row("SELECT s.lease_until,c.expires_at FROM product_services_v1 s JOIN product_service_credentials_v1 c USING(service_id) WHERE s.service_id='service'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
                assert_eq!(service_lease, claim_time.to_rfc3339());
                assert_eq!(credential_expiry, claim_time.to_rfc3339());
                assert!(claim_time > now + chrono::Duration::minutes(5));
            }
            _ => unreachable!(),
        }
        *clock.time.lock().unwrap() = claim_time;
        let before = selection_schema_observation(&path);
        let marker = marker_state(&Connection::open(&path).unwrap());
        let files = recursive_snapshot(&dir.0);
        assert!(matches!(store.claim_product_work_for_service(&service, "second", now, chrono::Duration::minutes(5)), Err(ProductError::Invariant("service authority is absent, expired, or revoked"))), "case {case}");
        assert_eq!(selection_schema_observation(&path), before, "case {case}");
        assert_eq!(marker_state(&Connection::open(&path).unwrap()), marker, "case {case}");
        assert_eq!(recursive_snapshot(&dir.0), files, "case {case}");
        assert_eq!(selection_typed_snapshot(&path)["product_w1_claim_authority_v1"], original, "case {case}");
        if case == "revoke" {
            use rusqlite::types::Value as V;
            let conn = Connection::open(&path).unwrap();
            let (current_revision, revoked_grant): (i64,i64) = conn.query_row("SELECT s.revision,g.grant_revision FROM product_services_v1 s JOIN product_service_grants_v1 g USING(service_id) WHERE s.service_id='service' AND g.action='drive_review'", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
            drop(conn);
            let receipt = store.manage_product_service(&session, "service", &grants, true, now + chrono::Duration::hours(3), current_revision, "regrant", now).unwrap();
            assert_ne!(receipt.revision, current_revision);
            let token = store.issue_product_service_credential(&session, "service", receipt.revision, now + chrono::Duration::hours(2), "reissue", now).unwrap().expose_once_to_file_writer().to_owned();
            let fresh = store.authenticate_product_service_for_action(&token, "gh", "https://api.github.com", "42", ProductServiceAction::DriveReview, now).unwrap();
            let conn = Connection::open(&path).unwrap();
            let (service_revision, credential_revision, new_grant, enabled): (i64,i64,i64,i64) = conn.query_row("SELECT s.revision,c.credential_revision,g.grant_revision,g.enabled FROM product_services_v1 s JOIN product_service_credentials_v1 c USING(service_id) JOIN product_service_grants_v1 g USING(service_id) WHERE s.service_id='service' AND g.action='drive_review'", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
            drop(conn);
            assert_eq!(service_revision, receipt.revision); assert_eq!(enabled, 1);
            assert_ne!(new_grant, revoked_grant); assert_ne!(V::Integer(new_grant), original[0][19]);
            assert_ne!(V::Integer(service_revision), original[0][14]); assert_ne!(V::Integer(credential_revision), original[0][16]);
            let mut expected = selection_schema_observation(&path);
            let files_before_reclaim = recursive_snapshot(&dir.0);
            let old_fence = match expected.2["product_fence_counter_v1"][0][1] { V::Integer(n) => n, _ => panic!("fence type") };
            let new_lease = claim_time + chrono::Duration::minutes(5);
            let mut capture = original[0].clone();
            capture[2] = V::Integer(2); capture[3] = V::Integer(old_fence + 1); capture[7] = V::Text("second".into()); capture[8] = V::Text(claim_time.to_rfc3339()); capture[9] = V::Text(new_lease.to_rfc3339()); capture[14] = V::Integer(service_revision); capture[16] = V::Integer(credential_revision); capture[19] = V::Integer(new_grant);
            expected.2.get_mut("product_fence_counter_v1").unwrap()[0][1] = V::Integer(old_fence + 1);
            expected.2.get_mut("product_work_backlog_v1").unwrap()[0][5] = V::Integer(2);
            let claim = &mut expected.2.get_mut("product_work_claims_v1").unwrap()[0];
            claim[2] = V::Text("second".into()); claim[3] = V::Integer(2); claim[4] = V::Integer(old_fence + 1); claim[6] = V::Text(new_lease.to_rfc3339());
            let binding = &mut expected.2.get_mut("product_service_work_bindings_v1").unwrap()[0];
            binding[3] = V::Integer(service_revision); binding[4] = V::Integer(credential_revision);
            let captures = expected.2.get_mut("product_w1_claim_authority_v1").unwrap(); captures.push(capture); captures.sort_by_cached_key(|row| format!("{row:?}"));
            let reclaimed = match store.claim_product_work_for_service(&fresh, "second", now, chrono::Duration::minutes(5)).unwrap() { ProductWorkClaimResult::Claimed(claim) => claim, other => panic!("expected reclaim: {other:?}") };
            assert_eq!((reclaimed.review_binding().epoch, reclaimed.review_binding().fence), (2, old_fence + 1));
            assert_eq!(selection_schema_observation(&path), expected);
            let mut files_after_reclaim = recursive_snapshot(&dir.0); let mut expected_files = files_before_reclaim;
            files_after_reclaim.get_mut(&PathBuf::from("v3.db")).unwrap().1.clear(); expected_files.get_mut(&PathBuf::from("v3.db")).unwrap().1.clear(); assert_eq!(files_after_reclaim, expected_files);
            drop(store);
            let reopened = ProductStore::reopen_w1_capture_test_store(&dir.0, "v3.db", config(), clock.clone()).unwrap();
            assert_eq!(selection_schema_observation(&path), expected); drop(reopened);
        }
    }
}

// A fresh real service admission and public claim for each disposable schema case.
fn immutable_claim_fixture() -> (PrivateDir, ProductStore, foundry_ledger::ProductServiceContext, chrono::DateTime<chrono::Utc>) {
    use foundry_ledger::{ProductCapability, ProductClock, ProductServiceAction, ProductServiceGrant, ScopeGrantManifest, ProductPollCursor};
    use foundry_ledger::product_identity::ProductPasswordKdf;
    let dir = PrivateDir::new();
    struct FixedClock(chrono::DateTime<chrono::Utc>);
    impl ProductClock for FixedClock { fn now(&self) -> chrono::DateTime<chrono::Utc> { self.0 } }
    let now = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00+00:00").unwrap().with_timezone(&chrono::Utc);
    let clock = Arc::new(FixedClock(now));
    let mut store = ProductStore::create_w1_capture_test_store(&dir.0, "v3.db", config(), clock.clone()).unwrap();
    let source = ScopeSourceManifest { connector_id: "gh".into(), source_instance_id: "source-instance-1".into(), generation: 1, identity: RepositoryIdentity::canonicalized("acme", "GitHub", "https://api.github.com", "42").unwrap(), api_owner: Some("acme".into()), api_repo: Some("proj".into()), legacy_repository_locator: "acme/proj".into(), scope_query: "open".into(), enabled: true };
    let grant = ScopeGrantManifest { actor_id: "owner".into(), source_instance_id: source.source_instance_id.clone(), capabilities: vec![ProductCapability::ReadOrder] };
    store.bootstrap_owner_scoped("owner", &ProductPasswordKdf::hash("test-password").unwrap(), &[source], &[grant]).unwrap();
    let session = store.login("owner", "test-password", now).unwrap().digest().to_owned();
    let grants: Vec<_> = [ProductServiceAction::IntakeRead, ProductServiceAction::AdmitDelivery, ProductServiceAction::AckDelivery, ProductServiceAction::DriveReview].into_iter().map(|action| ProductServiceGrant { source_instance_id: "source-instance-1".into(), action }).collect();
    let service_receipt = store.manage_product_service(&session, "service", &grants, true, now + chrono::Duration::hours(3), 0, "configure", now).unwrap();
    assert_eq!((service_receipt.service_id.as_str(), service_receipt.revision, service_receipt.enabled), ("service", 1, true));
    let token = store.issue_product_service_credential(&session, "service", 1, now + chrono::Duration::hours(2), "credential", now).unwrap().expose_once_to_file_writer().to_owned();
    let service = store.authenticate_product_service_for_action(&token, "gh", "https://api.github.com", "42", ProductServiceAction::DriveReview, now).unwrap();
    assert_eq!((service.service_id(), service.source_instance_id()), ("service", "source-instance-1"));
    let path = dir.0.join("v3.db");
    let issue = "issue:service-w1";
    let payload = "immutable local adapter task";
    let updated = now.to_rfc3339();
    let version = foundry_ledger::product::delivery_version_digest(issue, &updated, "dep-bump", payload);
    let delivery = foundry_ledger::ProductDelivery { delivery_id: foundry_ledger::product::github_delivery_id("w1-install", "gh", "https://api.github.com", "42", issue, &version), connector_id: "gh".into(), repository_id: "42".into(), issue_id: issue.into(), observed_updated_at: updated.clone(), version_digest: version, task_kind: "dep-bump".into(), payload: payload.into() };
    let cursor = ProductPollCursor { connector_id: "gh".into(), query_fingerprint: "v3-query".into(), sweep_id: "v3-sweep".into(), next_page: 1, page_ceiling: 1, updated_at: updated };
    store.persist_intake_page_for_service(&service, &cursor, &[delivery.clone()], &[], now).unwrap();
    let receipt = store.admit_intake_delivery_for_service(&service, &delivery.delivery_id, "service-order", now).unwrap();
    assert_eq!((receipt.order_id.as_str(), receipt.lifecycle), ("service-order", 1));
    assert!(!receipt.event_hash.is_empty());
    store.mark_intake_acked_for_service(&service, &delivery.delivery_id, &receipt, now).unwrap();
    let view = store.read_scoped_order(&session, &receipt.order_id, now).unwrap();
    assert_eq!((view.order_id.as_str(), view.lifecycle, view.source_instance_id.as_str(), view.admission_sequence), (receipt.order_id.as_str(), receipt.lifecycle, "source-instance-1", receipt.sequence));
    // Synthetic adapter selection: exactly this admitted row changes from waiting_unsupported to ready.
    let mut expected = selection_typed_snapshot(&path);
    let rows = expected.get_mut("product_work_backlog_v1").unwrap();
    let row = rows.iter_mut().find(|row| row[0] == rusqlite::types::Value::Text(receipt.order_id.clone()) && row[1] == rusqlite::types::Value::Integer(receipt.lifecycle) && row[2] == rusqlite::types::Value::Text(delivery.delivery_id.clone())).unwrap();
    assert_eq!(row[3], rusqlite::types::Value::Text("waiting_unsupported".into()));
    row[3] = rusqlite::types::Value::Text("ready".into());
    rows.sort_by_cached_key(|row| format!("{row:?}"));
    assert_eq!(Connection::open(&path).unwrap().execute("UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id=?1 AND lifecycle=?2 AND delivery_id=?3 AND state='waiting_unsupported'", rusqlite::params![receipt.order_id, receipt.lifecycle, delivery.delivery_id]).unwrap(), 1);
    assert_eq!(selection_typed_snapshot(&path), expected);
    assert!(matches!(store.claim_product_work_for_service(&service, "worker", now, chrono::Duration::minutes(5)).unwrap(), foundry_ledger::ProductWorkClaimResult::Claimed(_)));
    assert_eq!(selection_typed_snapshot(&path)["product_w1_claim_authority_v1"].len(), 1);
    (dir, store, service, now)
}

#[test]
fn w1_capture_immutable_and_schema() {
    use rusqlite::types::Value;
    let (dir, mut store, service, now) = immutable_claim_fixture();
    let path = dir.0.join("v3.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA recursive_triggers=ON").unwrap();
    for pragma in ["PRAGMA foreign_keys", "PRAGMA recursive_triggers"] {
        assert_eq!(conn.query_row(pragma, [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    }
    let before = selection_schema_observation(&path);
    let marker = marker_state(&conn);
    let files = recursive_snapshot(&dir.0);
    let row = &before.2["product_w1_claim_authority_v1"][0];
    assert_eq!(row.len(), 29);
    let mut pk = row.clone();
    pk[3] = match row[3] { Value::Integer(n) => Value::Integer(n + 100), _ => panic!("fence type") };
    pk[7] = Value::Text("changed-pk-worker".into());
    let mut fence = row.clone();
    fence[0] = Value::Text("distinct-order".into());
    fence[7] = Value::Text("changed-fence-worker".into());
    let insert = format!("INSERT OR REPLACE INTO product_w1_claim_authority_v1 VALUES ({})", (1..=29).map(|n| format!("?{n}")).collect::<Vec<_>>().join(","));
    for case in 0..4 {
        let result = match case {
            0 => conn.execute("UPDATE product_w1_claim_authority_v1 SET worker_id='changed'", []),
            1 => conn.execute("DELETE FROM product_w1_claim_authority_v1", []),
            2 => conn.execute(&insert, rusqlite::params_from_iter(pk.iter())),
            _ => conn.execute(&insert, rusqlite::params_from_iter(fence.iter())),
        };
        assert!(matches!(result, Err(rusqlite::Error::SqliteFailure(ref e, Some(ref m))) if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER && m == "immutable claim authority"), "case {case}: {result:?}");
        assert_eq!(selection_schema_observation(&path), before);
        assert_eq!(marker_state(&conn), marker);
        assert_eq!(recursive_snapshot(&dir.0), files);
    }
    // A distinct, valid row can be inserted; roll it back to keep the fixture intact.
    let mut control = fence.clone();
    control[3] = pk[3].clone();
    conn.execute_batch("BEGIN").unwrap();
    assert_eq!(conn.execute(&insert, rusqlite::params_from_iter(control.iter())).unwrap(), 1);
    assert_eq!(conn.query_row("SELECT count(*) FROM product_w1_claim_authority_v1", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
    conn.execute_batch("ROLLBACK").unwrap();
    assert_eq!(selection_schema_observation(&path), before);
    assert_eq!(recursive_snapshot(&dir.0), files);
    // An actual SQLite writer timeout remains a Database error.
    let lock = Connection::open(&path).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let result = store.claim_product_work_for_service(&service, "worker", now, chrono::Duration::minutes(5));
    assert!(matches!(result, Err(ProductError::Database(rusqlite::Error::SqliteFailure(ref e, _))) if e.code == rusqlite::ErrorCode::DatabaseBusy), "busy: {result:?}");
    lock.execute_batch("ROLLBACK").unwrap();
    drop(lock);
    assert_eq!(selection_schema_observation(&path), before);
    assert_eq!(recursive_snapshot(&dir.0), files);
    drop(conn);
    drop(store);
    assert!(ProductStore::reopen_w1_capture_test_store(&dir.0, "v3.db", config(), Arc::new(SystemProductClock)).is_ok());

    for case in ["marker", "missing-marker", "missing-trigger", "changed-trigger", "missing-unique", "malformed-table", "forbidden-fk"] {
        let (dir, store, _service, _now) = immutable_claim_fixture();
        let path = dir.0.join("v3.db");
        drop(store);
        let conn = Connection::open(&path).unwrap();
        let original = selection_typed_snapshot(&path)["product_w1_claim_authority_v1"].clone();
        let ddl: String = conn.query_row("SELECT sql FROM sqlite_schema WHERE name='product_w1_claim_authority_v1'", [], |r| r.get(0)).unwrap();
        let triggers: Vec<String> = ["product_w1_claim_authority_no_update", "product_w1_claim_authority_no_delete", "product_w1_claim_authority_no_replace"].iter().map(|name| conn.query_row("SELECT sql FROM sqlite_schema WHERE name=?1", [name], |r| r.get(0)).unwrap()).collect();
        match case {
            "marker" => {
                conn.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE product_w1_isolated_marker_v1 SET version=4; PRAGMA ignore_check_constraints=OFF").unwrap();
                assert_eq!(marker_state(&conn).1, vec![(1, 4)]);
            }
            "missing-marker" => {
                conn.execute_batch("DROP TABLE product_w1_isolated_marker_v1").unwrap();
                assert_eq!(conn.query_row("SELECT count(*) FROM sqlite_schema WHERE name='product_w1_isolated_marker_v1'", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
            }
            "missing-trigger" => {
                conn.execute_batch("DROP TRIGGER product_w1_claim_authority_no_update").unwrap();
                assert_eq!(conn.query_row("SELECT count(*) FROM sqlite_schema WHERE name='product_w1_claim_authority_no_update'", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
            }
            "changed-trigger" => {
                conn.execute_batch("DROP TRIGGER product_w1_claim_authority_no_update; CREATE TRIGGER product_w1_claim_authority_no_update BEFORE UPDATE ON product_w1_claim_authority_v1 BEGIN SELECT RAISE(ABORT,'changed'); END").unwrap();
                let changed: String = conn.query_row("SELECT sql FROM sqlite_schema WHERE name='product_w1_claim_authority_no_update'", [], |r| r.get(0)).unwrap();
                assert_ne!(changed, triggers[0]);
            }
            _ => {
                let changed = match case {
                    "missing-unique" => ddl.replace(", UNIQUE(fence)", ""),
                    "malformed-table" => ddl.replace("worker_id TEXT NOT NULL", "worker_id BLOB NOT NULL"),
                    "forbidden-fk" => ddl.replace(", UNIQUE(fence))", ", UNIQUE(fence), FOREIGN KEY(order_id) REFERENCES product_orders_v1(order_id))"),
                    _ => unreachable!(),
                };
                assert_ne!(changed, ddl);
                conn.execute_batch("CREATE TEMP TABLE saved_capture AS SELECT * FROM product_w1_claim_authority_v1; DROP TABLE product_w1_claim_authority_v1").unwrap();
                conn.execute_batch(&changed).unwrap();
                conn.execute_batch("INSERT INTO product_w1_claim_authority_v1 SELECT * FROM saved_capture; DROP TABLE saved_capture").unwrap();
                for trigger in &triggers { conn.execute_batch(trigger).unwrap(); }
                let actual: String = conn.query_row("SELECT sql FROM sqlite_schema WHERE name='product_w1_claim_authority_v1'", [], |r| r.get(0)).unwrap();
                assert_eq!(actual, changed);
                if case == "forbidden-fk" {
                    assert_eq!(conn.query_row("SELECT count(*) FROM pragma_foreign_key_list('product_w1_claim_authority_v1')", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
                }
                if case == "missing-unique" {
                    assert_eq!(conn.query_row("SELECT count(*) FROM pragma_index_list('product_w1_claim_authority_v1') WHERE origin='u'", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
                }
            }
        }
        assert_eq!(selection_typed_snapshot(&path)["product_w1_claim_authority_v1"], original);
        drop(conn);
        let baseline = selection_schema_observation(&path);
        let files = recursive_snapshot(&dir.0);
        assert!(matches!(ProductStore::reopen_w1_capture_test_store(&dir.0, "v3.db", config(), Arc::new(SystemProductClock)), Err(ProductError::ContractMismatch)), "{case}");
        assert_eq!(selection_schema_observation(&path), baseline, "{case}");
        assert_eq!(recursive_snapshot(&dir.0), files, "{case}");
    }
}


fn scoped_version_denial_control(version: i64) {
    use foundry_ledger::{ProductCapability, ProductClock, ProductServiceAction, ProductServiceGrant, ScopeGrantManifest, ProductPollCursor};
    use foundry_ledger::product_identity::ProductPasswordKdf;
    use foundry_exec::{Attempt, Epoch, ExecutionIntent, Generation, HostError, OrderId, ReviewAdmissionRequest, ReviewAdmissionStore, RunId};
    struct FixedClock(chrono::DateTime<chrono::Utc>);
    impl ProductClock for FixedClock { fn now(&self) -> chrono::DateTime<chrono::Utc> { self.0 } }
    let dir = PrivateDir::new();
    let now = chrono::Utc::now();
    let caller = now + chrono::Duration::seconds(1);
    // Normal review validates with the store clock, which must not precede its claim.
    // V3 instead claims at the injected time, independently of the later caller time.
    let clock = Arc::new(FixedClock(if version == 0 { caller } else { now }));
    let mut store = if version == 0 { ProductStore::open_with_clock(&dir.0.join("v3.db"), config(), clock.clone()).unwrap() } else if version == 2 { ProductStore::create_isolated_test_store(&dir.0, "v3.db", config()).unwrap() } else { ProductStore::create_w1_capture_test_store(&dir.0, "v3.db", config(), clock.clone()).unwrap() };
    let source = ScopeSourceManifest { connector_id: "gh".into(), source_instance_id: "source-instance-1".into(), generation: 1, identity: RepositoryIdentity::canonicalized("acme", "GitHub", "https://api.github.com", "42").unwrap(), api_owner: Some("acme".into()), api_repo: Some("proj".into()), legacy_repository_locator: "acme/proj".into(), scope_query: "open".into(), enabled: true };
    let grant = ScopeGrantManifest { actor_id: "owner".into(), source_instance_id: source.source_instance_id.clone(), capabilities: vec![ProductCapability::ReadOrder] };
    store.bootstrap_owner_scoped("owner", &ProductPasswordKdf::hash("test-password").unwrap(), &[source], &[grant]).unwrap();
    let session = store.login("owner", "test-password", now).unwrap().digest().to_owned();
    let grants: Vec<_> = [ProductServiceAction::IntakeRead, ProductServiceAction::AdmitDelivery, ProductServiceAction::AckDelivery, ProductServiceAction::DriveReview].into_iter().map(|action| ProductServiceGrant { source_instance_id: "source-instance-1".into(), action }).collect();
    let service_receipt = store.manage_product_service(&session, "service", &grants, true, now + chrono::Duration::hours(3), 0, "configure", now).unwrap();
    assert_eq!((service_receipt.service_id.as_str(), service_receipt.revision, service_receipt.enabled), ("service", 1, true));
    let token = store.issue_product_service_credential(&session, "service", 1, now + chrono::Duration::hours(2), "credential", now).unwrap().expose_once_to_file_writer().to_owned();
    let service = store.authenticate_product_service_for_action(&token, "gh", "https://api.github.com", "42", ProductServiceAction::DriveReview, now).unwrap();
    assert_eq!((service.service_id(), service.source_instance_id()), ("service", "source-instance-1"));
    let path = dir.0.join("v3.db");
    let issue = "issue:service-w1";
    let payload = "immutable local adapter task";
    let updated = now.to_rfc3339();
    let delivery_version = foundry_ledger::product::delivery_version_digest(issue, &updated, "dep-bump", payload);
    let delivery = foundry_ledger::ProductDelivery { delivery_id: foundry_ledger::product::github_delivery_id("w1-install", "gh", "https://api.github.com", "42", issue, &delivery_version), connector_id: "gh".into(), repository_id: "42".into(), issue_id: issue.into(), observed_updated_at: updated.clone(), version_digest: delivery_version, task_kind: "dep-bump".into(), payload: payload.into() };
    let cursor = ProductPollCursor { connector_id: "gh".into(), query_fingerprint: "v3-query".into(), sweep_id: "v3-sweep".into(), next_page: 1, page_ceiling: 1, updated_at: updated };
    store.persist_intake_page_for_service(&service, &cursor, &[delivery.clone()], &[], now).unwrap();
    let receipt = store.admit_intake_delivery_for_service(&service, &delivery.delivery_id, "service-order", now).unwrap();
    assert_eq!((receipt.order_id.as_str(), receipt.lifecycle), ("service-order", 1));
    assert!(!receipt.event_hash.is_empty());
    store.mark_intake_acked_for_service(&service, &delivery.delivery_id, &receipt, now).unwrap();
    let view = store.read_scoped_order(&session, &receipt.order_id, now).unwrap();
    assert_eq!((view.order_id.as_str(), view.lifecycle, view.source_instance_id.as_str(), view.admission_sequence), (receipt.order_id.as_str(), receipt.lifecycle, "source-instance-1", receipt.sequence));
    use sha2::{Digest as ProvenanceDigest, Sha256 as ProvenanceSha256};
    let mut h = ProvenanceSha256::new();
    h.update(b"foundry-product-intake-v1\0"); h.update(("dep-bump".len() as u64).to_be_bytes()); h.update(b"dep-bump"); h.update((payload.len() as u64).to_be_bytes()); h.update(payload.as_bytes());
    let digest_for_provenance = hex::encode(h.finalize());
    let conn = Connection::open(&path).unwrap();
    let linked: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM product_intake_inbox_v1 i JOIN product_intake_source_bindings_v1 ib USING(delivery_id) JOIN product_work_backlog_v1 b USING(delivery_id) JOIN product_orders_v1 o ON o.order_id=b.order_id AND o.lifecycle=b.lifecycle JOIN product_scope_order_provenance_v1 p ON p.order_id=o.order_id AND p.lifecycle=o.lifecycle JOIN product_order_payload_digests_v1 d ON d.order_id=o.order_id WHERE i.delivery_id=?1 AND i.connector_id=?2 AND i.repository_id=?3 AND i.issue_id=?4 AND i.version_digest=?5 AND i.payload=?6 AND i.state='acked' AND ib.source_instance_id=?7 AND p.source_instance_id=?7 AND p.verified=1 AND o.connector_id=i.connector_id AND o.repository_id=i.repository_id AND o.issue_id=i.issue_id AND o.order_id=?8 AND o.lifecycle=?9 AND o.admission_seq=?10 AND o.admission_hash=?11 AND p.admission_seq=?10 AND p.admission_hash=?11 AND d.payload_digest=?12)",
        rusqlite::params![delivery.delivery_id, delivery.connector_id, delivery.repository_id, delivery.issue_id, delivery.version_digest, payload, "source-instance-1", receipt.order_id, receipt.lifecycle, receipt.sequence, receipt.event_hash, digest_for_provenance], |row| row.get(0),
    ).unwrap();
    assert!(linked, "intake, receipt, source, order and payload digest must refer to the same admission");
    drop(conn);
    // Synthetic adapter selection: exactly this admitted row changes from waiting_unsupported to ready.
    let mut expected = selection_typed_snapshot(&path);
    let rows = expected.get_mut("product_work_backlog_v1").unwrap();
    let row = rows.iter_mut().find(|row| row[0] == rusqlite::types::Value::Text(receipt.order_id.clone()) && row[1] == rusqlite::types::Value::Integer(receipt.lifecycle) && row[2] == rusqlite::types::Value::Text(delivery.delivery_id.clone())).unwrap();
    assert_eq!(row[3], rusqlite::types::Value::Text("waiting_unsupported".into()));
    row[3] = rusqlite::types::Value::Text("ready".into());
    rows.sort_by_cached_key(|row| format!("{row:?}"));
    assert_eq!(Connection::open(&path).unwrap().execute("UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id=?1 AND lifecycle=?2 AND delivery_id=?3 AND state='waiting_unsupported'", rusqlite::params![receipt.order_id, receipt.lifecycle, delivery.delivery_id]).unwrap(), 1);
    assert_eq!(selection_typed_snapshot(&path), expected);
    let effective = if version == 3 { now } else { caller };
    let before = selection_typed_snapshot(&path);
    assert_eq!(before["product_work_backlog_v1"][0][4], rusqlite::types::Value::Text(now.to_rfc3339()));
    assert!(now <= effective && effective <= caller);
    if version == 3 { assert_ne!(effective, caller); }
    let files = recursive_snapshot(&dir.0);
    use rusqlite::types::Value as V;
    use sha2::Sha256;
    use foundry_ledger::ProductWorkClaimResult;
    use foundry_exec::ReviewServiceBinding;
    let mut h = Sha256::new();
    h.update(b"foundry-product-intake-v1\0"); h.update(("dep-bump".len() as u64).to_be_bytes()); h.update(b"dep-bump"); h.update((payload.len() as u64).to_be_bytes()); h.update(payload.as_bytes());
    let digest = hex::encode(h.finalize());
    let lease = effective + chrono::Duration::minutes(5);
    let conn = Connection::open(&path).unwrap();
    // Every authority value is observed before the public claim, from its original rows.
    let sql = "SELECT o.order_id,o.lifecycle,1,?1+1,m.installation_id,m.database_id,b.delivery_id,'worker',?2,?3,?4,o.admission_seq,o.admission_hash,s.service_id,s.revision,s.lease_until,c.credential_revision,c.expires_at,g.action,g.grant_revision,i.source_instance_id,i.generation,i.revision,i.connector_id,ps.revision,r.api_origin,r.stable_repository_id,l.api_owner,l.api_repo FROM product_orders_v1 o JOIN product_work_backlog_v1 b ON b.order_id=o.order_id AND b.lifecycle=o.lifecycle JOIN product_scope_order_provenance_v1 p ON p.order_id=o.order_id AND p.lifecycle=o.lifecycle AND p.verified=1 AND p.admission_seq=o.admission_seq AND p.admission_hash=o.admission_hash JOIN product_scope_source_instances_v1 i ON i.source_instance_id=p.source_instance_id JOIN product_scope_repositories_v1 r ON r.repository_key=i.repository_key JOIN product_scope_api_locators_v1 l ON l.source_instance_id=i.source_instance_id JOIN product_sources_v1 ps ON ps.connector_id=i.connector_id JOIN product_service_grants_v1 g ON g.source_instance_id=i.source_instance_id AND g.action='drive_review' AND g.enabled=1 JOIN product_services_v1 s ON s.service_id=g.service_id JOIN product_service_credentials_v1 c ON c.service_id=s.service_id CROSS JOIN product_meta_v1 m WHERE o.order_id=?5 AND s.service_id='service' AND m.singleton=1";
    let old_fence = match &before["product_fence_counter_v1"][0][1] { V::Integer(n) => *n, _ => panic!("fence type") };
    let capture: Vec<V> = conn.query_row(sql, rusqlite::params![old_fence, effective.to_rfc3339(), lease.to_rfc3339(), digest, receipt.order_id], |r| (0..29).map(|i| r.get(i)).collect()).unwrap();
    let schema_before = selection_schema_observation(&path);
    let marker_before = if version == 0 { None } else { Some(marker_state(&conn)) };
    if version != 0 { assert_eq!(marker_before.as_ref().unwrap().1, vec![(1, version)]); }
    // These are properties of this observer connection, not the store connection.
    let observer_pragmas: (i64,i64) = (conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap(), conn.query_row("PRAGMA recursive_triggers", [], |r| r.get(0)).unwrap());
    assert_eq!(observer_pragmas, (1, 0));
    drop(conn);
    assert_ne!(capture[9], capture[15]); assert_ne!(capture[9], capture[17]); assert_ne!(capture[15], capture[17]);
    assert_eq!(capture[0], V::Text(receipt.order_id.clone())); assert_eq!(capture[1], V::Integer(receipt.lifecycle));
    assert_eq!(capture[6], V::Text(delivery.delivery_id.clone()));
    let mut expected = before.clone();
    expected.get_mut("product_fence_counter_v1").unwrap()[0][1] = V::Integer(old_fence + 1);
    let backlog = &mut expected.get_mut("product_work_backlog_v1").unwrap()[0];
    assert_eq!(backlog[3], V::Text("ready".into())); assert_eq!(backlog[5], V::Integer(0));
    backlog[3] = V::Text("claimed".into()); backlog[5] = V::Integer(1);
    expected.get_mut("product_work_claims_v1").unwrap().push(vec![capture[0].clone(),capture[1].clone(),V::Text("worker".into()),V::Integer(1),capture[3].clone(),V::Text("claimed".into()),capture[9].clone(),capture[24].clone(),V::Text(digest.clone())]);
    expected.get_mut("product_service_work_bindings_v1").unwrap().push(vec![capture[0].clone(),capture[1].clone(),capture[13].clone(),capture[14].clone(),capture[16].clone(),capture[20].clone()]);
    if version == 3 { expected.get_mut("product_w1_claim_authority_v1").unwrap().push(capture.clone()); } else { assert!(!before.contains_key("product_w1_claim_authority_v1")); }
    let claim = match store.claim_product_work_for_service(&service, "worker", caller, chrono::Duration::minutes(5)).unwrap() { ProductWorkClaimResult::Claimed(c) => c, other => panic!("expected scoped service claim: {other:?}") };
    let binding = claim.review_binding();
    assert_eq!((binding.order_id.as_str(),binding.lifecycle,binding.delivery_id.as_str(),binding.worker_id.as_str(),binding.epoch,binding.fence,binding.source_revision,binding.lease_until,binding.task_kind.as_str(),binding.payload_snapshot.as_str(),binding.payload_digest.as_str()), (receipt.order_id.as_str(),receipt.lifecycle,delivery.delivery_id.as_str(),"worker",1,old_fence+1,1,lease,"dep-bump",payload,digest.as_str()));
    assert_eq!(binding.service_binding, Some(ReviewServiceBinding { service_id: "service".into(), service_revision: 1, credential_revision: 1, source_instance_id: "source-instance-1".into() }));
    assert_eq!(selection_typed_snapshot(&path), expected);
    let after_schema = selection_schema_observation(&path);
    assert_eq!(after_schema.0, schema_before.0);
    assert_eq!(after_schema.1, schema_before.1);
    let observer = Connection::open(&path).unwrap();
    let observer_after: (i64,i64) = (observer.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap(), observer.query_row("PRAGMA recursive_triggers", [], |r| r.get(0)).unwrap());
    assert_eq!(observer_after, observer_pragmas);
    drop(observer);
    if version != 0 { assert_eq!(Some(marker_state(&Connection::open(&path).unwrap())), marker_before); }
    let mut after_files = recursive_snapshot(&dir.0); let mut before_files = files;
    after_files.get_mut(&PathBuf::from("v3.db")).unwrap().1.clear(); before_files.get_mut(&PathBuf::from("v3.db")).unwrap().1.clear();
    assert_eq!(after_files, before_files);
    let pack = "a".repeat(64);
    let configure = |store: &mut ProductStore| store.configure_review_backend(foundry_ledger::ProductReviewConfig { artifact_root: dir.0.join("artifacts"), session_root: dir.0.join("sessions"), claim_root: dir.0.join("claims"), pack_digest: pack.clone() }).unwrap();
    configure(&mut store);
    let request = ReviewAdmissionRequest { intent: ExecutionIntent { order: OrderId(receipt.order_id.clone()), attempt: Attempt(1), run: RunId(4), activation: 1, generation: Generation(0), epoch: Epoch(1), external_ref: format!("review:{}:1:base..target", receipt.order_id), recorded_at: caller }, base_commit: "base".into(), result_commit: "target".into(), pack_digest: pack.clone(), product_work: Some(binding) };
    if version == 0 {
        assert!(!expected.contains_key("product_w1_isolated_marker_v1"));
        let mut expected_admission = selection_typed_snapshot(&path);
        let schema = selection_schema_observation(&path).0;
        let mut before_files = recursive_snapshot(&dir.0);
        let key = serde_json::to_string(&foundry_exec::HostKey { order: receipt.order_id.clone(), attempt: 1, run: 4, activation: 1 }).unwrap();
        let intent_json = serde_json::to_string(&request.intent).unwrap();
        let t = |v: &str| V::Text(v.to_owned());
        let journal = "service-order:1:4:1";
        // SQLite unixepoch() supplies created_at, independently of the injected clock;
        // bracket the call with real UTC seconds and retain the exact observed value.
        let started = chrono::Utc::now().timestamp();
        ReviewAdmissionStore::validate_product_work_claim(&store, request.product_work.as_ref().unwrap(), &request.intent, caller).unwrap();
        let admitted = store.admit_review_activation(request.clone()).unwrap();
        let ended = chrono::Utc::now().timestamp();
        assert_eq!(admitted.journal_run, journal);
        let actual = selection_typed_snapshot(&path);
        let created = match actual["product_review_runs_v1"][0][17] { V::Integer(n) => n, _ => panic!("created_at type") };
        assert!((started..=ended).contains(&created));
        expected_admission.get_mut("product_review_epochs_v1").unwrap().push(vec![t("service-order"), V::Integer(1)]);
        expected_admission.get_mut("product_review_runs_v1").unwrap().push(vec![t("service-order"),V::Integer(1),t(journal),t("base"),t("target"),t(&pack),t("running"),t("{}"),V::Null,V::Null,V::Null,V::Null,V::Null,V::Integer(1),V::Integer(1),V::Integer(receipt.sequence),t(&receipt.event_hash),V::Integer(created)]);
        expected_admission.get_mut("product_review_work_bindings_v1").unwrap().push(vec![t("service-order"),V::Integer(1),t(journal),V::Integer(1),t(&delivery.delivery_id),t("worker"),V::Integer(1),V::Integer(old_fence+1),V::Integer(1),t(&digest),t("dep-bump"),t(payload)]);
        expected_admission.get_mut("product_review_host_accept_v1").unwrap().push(vec![t("service-order"),V::Integer(1),V::Integer(4),V::Integer(1)]);
        expected_admission.get_mut("product_review_bindings_v1").unwrap().push(vec![t(&key),t(&intent_json)]);
        expected_admission.get_mut("product_review_authority_v1").unwrap().push(vec![t(&key),t("service-order"),V::Integer(1),V::Integer(1),V::Integer(4),V::Integer(1),t(journal),V::Integer(1),capture[13].clone(),capture[14].clone(),capture[16].clone(),capture[19].clone(),capture[20].clone(),capture[21].clone(),capture[22].clone(),capture[24].clone()]);
        expected_admission.get_mut("product_review_active_v1").unwrap().push(vec![t("service-order"),t(&key),V::Integer(1),t(journal)]);
        assert_eq!(actual, expected_admission);
        assert_eq!(selection_schema_observation(&path).0, schema);
        let mut after_files = recursive_snapshot(&dir.0);
        before_files.get_mut(&PathBuf::from("v3.db")).unwrap().1.clear(); after_files.get_mut(&PathBuf::from("v3.db")).unwrap().1.clear();
        assert_eq!(after_files, before_files);
        return;
    }
    for reopen in [false, true] {
        if reopen { drop(store); store = if version == 2 { ProductStore::reopen_isolated_test_store(&dir.0, "v3.db", config()).unwrap() } else { ProductStore::reopen_w1_capture_test_store(&dir.0, "v3.db", config(), clock.clone()).unwrap() }; configure(&mut store); }
        let before = selection_schema_observation(&path); let marker = marker_state(&Connection::open(&path).unwrap()); let files = recursive_snapshot(&dir.0);
        let validation_time = caller;
        assert!(validation_time >= effective && validation_time < lease);
        ReviewAdmissionStore::validate_product_work_claim(&store, request.product_work.as_ref().unwrap(), &request.intent, validation_time).unwrap();
        assert!(matches!(store.admit_review_activation(request.clone()), Err(HostError::Acceptance(ref m)) if m == "isolated product review admission requires an atomic pin"));
        assert!(chrono::Utc::now() < lease);
        assert_eq!(selection_schema_observation(&path), before); assert_eq!(marker_state(&Connection::open(&path).unwrap()), marker); assert_eq!(recursive_snapshot(&dir.0), files);
    }
    drop(store);
    let before = selection_schema_observation(&path);
    let marker = marker_state(&Connection::open(&path).unwrap());
    let files = recursive_snapshot(&dir.0);
    if version == 2 {
        assert!(matches!(ProductStore::reopen_w1_capture_test_store(&dir.0, "v3.db", config(), clock), Err(ProductError::ContractMismatch)));
    } else {
        assert!(matches!(ProductStore::reopen_isolated_test_store(&dir.0, "v3.db", config()), Err(ProductError::ContractMismatch)));
    }
    assert_eq!(selection_schema_observation(&path), before); assert_eq!(marker_state(&Connection::open(&path).unwrap()), marker); assert_eq!(recursive_snapshot(&dir.0), files);
    assert!(matches!(ProductStore::open(&path, config()), Err(ProductError::ContractMismatch)));
    assert_eq!(selection_schema_observation(&path), before); assert_eq!(marker_state(&Connection::open(&path).unwrap()), marker); assert_eq!(recursive_snapshot(&dir.0), files);
}

// Preserve the original unscoped version and admission controls alongside scoped cases.
#[test]
fn w1_capture_version_and_denial_controls() {
    scoped_version_denial_control(2);
    scoped_version_denial_control(3);
    scoped_version_denial_control(0);
    use foundry_exec::{HostError, ReviewAdmissionStore};

    for version in [2, 3] {
        let dir = PrivateDir::new();
        let path = dir.0.join("control.db");
        let clock = Arc::new(SystemProductClock);
        let mut store = if version == 2 {
            ProductStore::create_isolated_test_store(&dir.0, "control.db", config()).unwrap()
        } else {
            ProductStore::create_w1_capture_test_store(&dir.0, "control.db", config(), clock.clone()).unwrap()
        };
        let request = prepare_review(&mut store, &dir.0, &path);
        let observe = || {
            let conn = Connection::open(&path).unwrap();
            let mut stmt = conn.prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name").unwrap();
            let schema: Vec<(String, String, String, Option<String>)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap().map(Result::unwrap).collect();
            (schema, marker_state(&conn), selection_typed_snapshot(&path), recursive_snapshot(&dir.0))
        };
        let baseline = observe();
        assert!(!baseline.0.is_empty());
        assert_eq!(baseline.1.1, vec![(1, version)]);
        assert!(!baseline.2["product_work_claims_v1"].is_empty());
        assert!(baseline.3.contains_key(&PathBuf::from("control.db")));
        let validation_time = chrono::Utc::now();
        ReviewAdmissionStore::validate_product_work_claim(&store, request.product_work.as_ref().unwrap(), &request.intent, validation_time).unwrap();
        assert!(matches!(store.admit_review_activation(request.clone()), Err(HostError::Acceptance(ref m)) if m == "isolated product review admission requires an atomic pin"));
        assert!(chrono::Utc::now() < request.product_work.as_ref().unwrap().lease_until);
        assert_eq!(observe(), baseline);
        drop(store);
        let before_open = observe();
        if version == 2 {
            assert!(matches!(ProductStore::reopen_w1_capture_test_store(&dir.0, "control.db", config(), clock.clone()), Err(ProductError::ContractMismatch)));
        } else {
            assert!(matches!(ProductStore::reopen_isolated_test_store(&dir.0, "control.db", config()), Err(ProductError::ContractMismatch)));
        }
        assert_eq!(observe(), before_open);
        assert!(matches!(ProductStore::open(&path, config()), Err(ProductError::ContractMismatch)));
        assert_eq!(observe(), before_open);
        let mut store = if version == 2 {
            ProductStore::reopen_isolated_test_store(&dir.0, "control.db", config()).unwrap()
        } else {
            ProductStore::reopen_w1_capture_test_store(&dir.0, "control.db", config(), clock).unwrap()
        };
        store.configure_review_backend(foundry_ledger::ProductReviewConfig { artifact_root: dir.0.join("artifacts"), session_root: dir.0.join("sessions"), claim_root: dir.0.join("claims"), pack_digest: "a".repeat(64) }).unwrap();
        assert_eq!(observe(), before_open);
        let validation_time = chrono::Utc::now();
        ReviewAdmissionStore::validate_product_work_claim(&store, request.product_work.as_ref().unwrap(), &request.intent, validation_time).unwrap();
        assert!(matches!(store.admit_review_activation(request.clone()), Err(HostError::Acceptance(ref m)) if m == "isolated product review admission requires an atomic pin"));
        assert!(chrono::Utc::now() < request.product_work.as_ref().unwrap().lease_until);
        assert_eq!(observe(), before_open);
    }
}

#[test]
fn w1_pin_v4_create_reopen_and_version_isolation() {
    let dir = PrivateDir::new();
    let clock = Arc::new(SystemProductClock);
    drop(ProductStore::create_isolated_test_store(&dir.0, "v2.db", config()).unwrap());
    drop(ProductStore::create_w1_capture_test_store(&dir.0, "v3.db", config(), clock.clone()).unwrap());
    drop(ProductStore::create_w1_pin_test_store(&dir.0, "v4.db", config()).unwrap());
    drop(ProductStore::open(&dir.0.join("normal.db"), config()).unwrap());
    for (file, version) in [("v2.db", 2), ("v3.db", 3), ("v4.db", 4)] {
        let path = dir.0.join(file);
        let conn = Connection::open(&path).unwrap();
        assert_eq!(conn.query_row("SELECT version FROM product_w1_isolated_marker_v1", [], |r| r.get::<_,i64>(0)).unwrap(), version);
        assert_eq!(conn.query_row("SELECT schema_version FROM product_meta_v1", [], |r| r.get::<_,i64>(0)).unwrap(), 3);
        assert_eq!(conn.query_row("SELECT schema_version FROM product_review_meta_v1", [], |r| r.get::<_,i64>(0)).unwrap(), 1);
        drop(conn);
        let before = snapshot(&dir.0);
        assert!(matches!(ProductStore::open(&path, config()), Err(ProductError::ContractMismatch)));
        if version != 2 { assert!(matches!(ProductStore::reopen_isolated_test_store(&dir.0, file, config()), Err(ProductError::ContractMismatch))); }
        if version != 3 { assert!(matches!(ProductStore::reopen_w1_capture_test_store(&dir.0, file, config(), clock.clone()), Err(ProductError::ContractMismatch))); }
        if version != 4 { assert!(matches!(ProductStore::reopen_w1_pin_test_store(&dir.0, file, config()), Err(ProductError::ContractMismatch))); }
        assert_eq!(snapshot(&dir.0), before);
    }
    assert!(matches!(ProductStore::reopen_w1_pin_test_store(&dir.0, "normal.db", config()), Err(ProductError::ContractMismatch)));
    let before_reopen = selection_schema_observation(&dir.0.join("v4.db"));
    drop(ProductStore::reopen_w1_pin_test_store(&dir.0, "v4.db", config()).unwrap());
    assert_eq!(selection_schema_observation(&dir.0.join("v4.db")), before_reopen);
}

#[test]
fn w1_pin_v4_exact_schema_and_immutability() {
    let dir = PrivateDir::new();
    let path = dir.0.join("pin.db");
    drop(ProductStore::create_w1_pin_test_store(&dir.0, "pin.db", config()).unwrap());
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA recursive_triggers=ON;").unwrap();
    let mut all = conn.prepare("SELECT type,name FROM sqlite_schema WHERE tbl_name LIKE 'product_w1_%' ORDER BY type,name").unwrap();
    let actual: Vec<(String,String)> = all.query_map([], |r| Ok((r.get(0)?,r.get(1)?))).unwrap().map(Result::unwrap).collect();
    let expected = [
        ("index","sqlite_autoindex_product_w1_claim_authority_v1_1"), ("index","sqlite_autoindex_product_w1_claim_authority_v1_2"),
        ("index","sqlite_autoindex_product_w1_lifecycle_pins_v1_1"),
        ("index","sqlite_autoindex_product_w1_operations_v1_1"), ("index","sqlite_autoindex_product_w1_operations_v1_2"), ("index","sqlite_autoindex_product_w1_operations_v1_3"), ("index","sqlite_autoindex_product_w1_operations_v1_4"),
        ("table","product_w1_claim_authority_v1"), ("table","product_w1_isolated_marker_v1"), ("table","product_w1_lifecycle_pins_v1"), ("table","product_w1_operations_v1"),
        ("trigger","product_w1_claim_authority_no_delete"), ("trigger","product_w1_claim_authority_no_replace"), ("trigger","product_w1_claim_authority_no_update"),
        ("trigger","product_w1_lifecycle_pins_no_delete"), ("trigger","product_w1_lifecycle_pins_no_replace"), ("trigger","product_w1_lifecycle_pins_no_update"),
        ("trigger","product_w1_operations_no_delete"), ("trigger","product_w1_operations_no_replace"), ("trigger","product_w1_operations_no_update"),
    ].map(|(a,b)| (a.to_string(), b.to_string()));
    assert_eq!(actual, expected);
    drop(all);
    assert_eq!(operation_objects(&conn), expected_operation_objects());
    assert!(conn.prepare("PRAGMA foreign_key_check").unwrap().query([]).unwrap().next().unwrap().is_none());
    let expected_ddl = [
        ("product_w1_lifecycle_pins_v1", "CREATE TABLE product_w1_lifecycle_pins_v1(order_id TEXT NOT NULL, lifecycle INTEGER NOT NULL CHECK(lifecycle>0), installation_id TEXT NOT NULL, database_id TEXT NOT NULL, source_instance_id TEXT NOT NULL, pack_digest TEXT NOT NULL, selection_revision INTEGER NOT NULL CHECK(selection_revision>0), referenced_approval_revision INTEGER NOT NULL CHECK(referenced_approval_revision>0), claim_epoch INTEGER NOT NULL CHECK(claim_epoch>0), claim_fence INTEGER NOT NULL CHECK(claim_fence>0), payload_digest TEXT NOT NULL, admission_seq INTEGER NOT NULL, admission_hash TEXT NOT NULL, PRIMARY KEY(order_id,lifecycle))"),
        ("product_w1_lifecycle_pins_no_update", "CREATE TRIGGER product_w1_lifecycle_pins_no_update BEFORE UPDATE ON product_w1_lifecycle_pins_v1 BEGIN SELECT RAISE(ABORT,'immutable lifecycle pin'); END"),
        ("product_w1_lifecycle_pins_no_delete", "CREATE TRIGGER product_w1_lifecycle_pins_no_delete BEFORE DELETE ON product_w1_lifecycle_pins_v1 BEGIN SELECT RAISE(ABORT,'immutable lifecycle pin'); END"),
        ("product_w1_lifecycle_pins_no_replace", "CREATE TRIGGER product_w1_lifecycle_pins_no_replace BEFORE INSERT ON product_w1_lifecycle_pins_v1 WHEN EXISTS(SELECT 1 FROM product_w1_lifecycle_pins_v1 WHERE order_id=NEW.order_id AND lifecycle=NEW.lifecycle) BEGIN SELECT RAISE(ABORT,'immutable lifecycle pin'); END"),
    ];
    for (name, ddl) in expected_ddl { assert_eq!(conn.query_row("SELECT sql FROM sqlite_schema WHERE name=?1", [name], |r| r.get::<_,String>(0)).unwrap(), ddl); }
    let mut stmt = conn.prepare("SELECT type,name FROM sqlite_schema WHERE tbl_name='product_w1_lifecycle_pins_v1' ORDER BY type,name").unwrap();
    let inventory: Vec<(String,String)> = stmt.query_map([], |r| Ok((r.get(0)?,r.get(1)?))).unwrap().map(Result::unwrap).collect();
    assert_eq!(inventory, [("index","sqlite_autoindex_product_w1_lifecycle_pins_v1_1"),("table","product_w1_lifecycle_pins_v1"),("trigger","product_w1_lifecycle_pins_no_delete"),("trigger","product_w1_lifecycle_pins_no_replace"),("trigger","product_w1_lifecycle_pins_no_update")].map(|(a,b)| (a.to_string(),b.to_string())));
    drop(stmt);
    let fk: i64 = conn.query_row("SELECT count(*) FROM pragma_foreign_key_list('product_w1_lifecycle_pins_v1')", [], |r| r.get(0)).unwrap();
    assert_eq!(fk, 0);
    for pragma in ["PRAGMA foreign_keys", "PRAGMA recursive_triggers"] { assert_eq!(conn.query_row(pragma, [], |r| r.get::<_,i64>(0)).unwrap(), 1); }
    // Synthetic raw row: this is not evidence of selection or claim reference guards.
    let insert = "INSERT INTO product_w1_lifecycle_pins_v1 VALUES('order',1,'install','db','source','pack',1,1,1,1,'payload',7,'hash')";
    conn.execute(insert, []).unwrap();
    assert!(conn.prepare("PRAGMA foreign_key_check").unwrap().query([]).unwrap().next().unwrap().is_none());
    let before = selection_typed_snapshot(&path);
    for sql in ["UPDATE product_w1_lifecycle_pins_v1 SET pack_digest='other'", "DELETE FROM product_w1_lifecycle_pins_v1", "INSERT OR REPLACE INTO product_w1_lifecycle_pins_v1 VALUES('order',1,'other','db','source','pack',1,1,1,1,'payload',7,'hash')"] {
        let err = conn.execute(sql, []).unwrap_err();
        assert!(matches!(err, rusqlite::Error::SqliteFailure(ref code, Some(ref message)) if code.extended_code == 1811 && message == "immutable lifecycle pin"));
        assert_eq!(selection_typed_snapshot(&path), before);
    }
    conn.execute("INSERT OR REPLACE INTO product_w1_lifecycle_pins_v1 VALUES('order',2,'install','db','source','pack',1,1,1,1,'payload',7,'hash')", []).unwrap();
    let mut expected = before;
    use rusqlite::types::Value as V;
    expected.get_mut("product_w1_lifecycle_pins_v1").unwrap().push(vec![V::Text("order".into()),V::Integer(2),V::Text("install".into()),V::Text("db".into()),V::Text("source".into()),V::Text("pack".into()),V::Integer(1),V::Integer(1),V::Integer(1),V::Integer(1),V::Text("payload".into()),V::Integer(7),V::Text("hash".into())]);
    expected.get_mut("product_w1_lifecycle_pins_v1").unwrap().sort_by_cached_key(|row| format!("{row:?}"));
    assert_eq!(selection_typed_snapshot(&path), expected);
    drop(conn);
    assert!(matches!(ProductStore::reopen_w1_pin_test_store(&dir.0, "pin.db", config()), Err(ProductError::ContractMismatch)));
}

#[test]
fn w1_pin_v4_admission_remains_fail_closed() {
    use foundry_exec::{HostError, ReviewAdmissionStore};
    let dir = PrivateDir::new();
    let path = dir.0.join("v4.db");
    let mut store = ProductStore::create_w1_pin_test_store(&dir.0, "v4.db", config()).unwrap();
    // A real normal-store claim supplies a well-formed request; it is not a valid v4 claim.
    let normal_dir = PrivateDir::new();
    let normal_path = normal_dir.0.join("normal.db");
    let mut normal = ProductStore::open(&normal_path, config()).unwrap();
    let request = prepare_review(&mut normal, &normal_dir.0, &normal_path);
    store.configure_review_backend(foundry_ledger::ProductReviewConfig { artifact_root: dir.0.join("artifacts"), session_root: dir.0.join("sessions"), claim_root: dir.0.join("claims"), pack_digest: "a".repeat(64) }).unwrap();
    let before = (selection_typed_snapshot(&path), recursive_snapshot(&dir.0));
    assert!(matches!(store.admit_review_activation(request.clone()), Err(HostError::Acceptance(ref m)) if m == "isolated product review admission requires an atomic pin"));
    assert_eq!((selection_typed_snapshot(&path), recursive_snapshot(&dir.0)), before);
    drop(store);
    let mut store = ProductStore::reopen_w1_pin_test_store(&dir.0, "v4.db", config()).unwrap();
    store.configure_review_backend(foundry_ledger::ProductReviewConfig { artifact_root: dir.0.join("artifacts"), session_root: dir.0.join("sessions"), claim_root: dir.0.join("claims"), pack_digest: "a".repeat(64) }).unwrap();
    let before = (selection_typed_snapshot(&path), recursive_snapshot(&dir.0));
    assert!(matches!(store.admit_review_activation(request.clone()), Err(HostError::Acceptance(ref m)) if m == "isolated product review admission requires an atomic pin"));
    assert_eq!((selection_typed_snapshot(&path), recursive_snapshot(&dir.0)), before);
    assert_eq!(normal.admit_review_activation(request).unwrap().journal_run, "review-order:1:4:1");
}

#[test]
fn w1_pin_v4_namespace_corruption_rejected() {
    for sql in [
        "CREATE TABLE product_w1_extra(x)",
        "CREATE VIEW product_w1_extra AS SELECT 1",
        "CREATE INDEX product_w1_extra ON product_meta_v1(schema_version)",
        "CREATE TRIGGER product_w1_extra AFTER UPDATE ON product_meta_v1 BEGIN SELECT 1; END",
        "DROP TRIGGER product_w1_lifecycle_pins_no_update",
        "DROP TRIGGER product_w1_lifecycle_pins_no_update; CREATE TRIGGER product_w1_lifecycle_pins_no_update BEFORE UPDATE ON product_w1_lifecycle_pins_v1 BEGIN SELECT 1; END",
    ] {
        let dir = PrivateDir::new();
        let path = dir.0.join("pin.db");
        drop(ProductStore::create_w1_pin_test_store(&dir.0, "pin.db", config()).unwrap());
        drop(ProductStore::reopen_w1_pin_test_store(&dir.0, "pin.db", config()).unwrap());
        Connection::open(&path).unwrap().execute_batch(sql).unwrap();
        let before = (selection_schema_observation(&path), recursive_snapshot(&dir.0));
        assert!(matches!(ProductStore::reopen_w1_pin_test_store(&dir.0, "pin.db", config()), Err(ProductError::ContractMismatch)), "{sql}");
        assert_eq!((selection_schema_observation(&path), recursive_snapshot(&dir.0)), before);
    }
}

#[test]
fn w1_pin_v4_mixed_case_namespace_corruption_rejected() {
    for (sql, kind, name, owner) in [
        ("CREATE TABLE Product_W1_extra(x)", "table", "Product_W1_extra", "Product_W1_extra"),
        ("CREATE VIEW Product_W1_extra AS SELECT 1", "view", "Product_W1_extra", "Product_W1_extra"),
        ("CREATE INDEX Product_W1_extra ON product_meta_v1(schema_version)", "index", "Product_W1_extra", "product_meta_v1"),
        ("CREATE TRIGGER Product_W1_extra AFTER UPDATE ON product_meta_v1 BEGIN SELECT 1; END", "trigger", "Product_W1_extra", "product_meta_v1"),
        ("CREATE TABLE Product_W1_owner(x); CREATE INDEX plain_extra ON Product_W1_owner(x)", "index", "plain_extra", "Product_W1_owner"),
        ("CREATE TABLE Product_W1_owner(x); CREATE TRIGGER plain_extra AFTER UPDATE ON Product_W1_owner BEGIN SELECT 1; END", "trigger", "plain_extra", "Product_W1_owner"),
    ] {
        let dir = PrivateDir::new();
        let path = dir.0.join("pin.db");
        drop(ProductStore::create_w1_pin_test_store(&dir.0, "pin.db", config()).unwrap());
        drop(ProductStore::reopen_w1_pin_test_store(&dir.0, "pin.db", config()).unwrap());
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(sql).unwrap();
        let actual: (String, String, String) = conn.query_row(
            "SELECT type,name,tbl_name FROM sqlite_schema WHERE name=?1 COLLATE BINARY", [name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).unwrap();
        assert_eq!(actual, (kind.into(), name.into(), owner.into()));
        drop(conn);
        let before = (selection_schema_observation(&path), recursive_snapshot(&dir.0));
        assert!(matches!(ProductStore::reopen_w1_pin_test_store(&dir.0, "pin.db", config()), Err(ProductError::ContractMismatch)), "{sql}");
        assert_eq!((selection_schema_observation(&path), recursive_snapshot(&dir.0)), before, "{sql}");
    }
}

fn pin_pending_scoped_fixture() -> (PrivateDir, ProductStore, foundry_ledger::ProductServiceContext, chrono::DateTime<chrono::Utc>) {
    use foundry_ledger::{ProductCapability, ProductServiceAction, ProductServiceGrant, ScopeGrantManifest, ProductPollCursor};
    use foundry_ledger::product_identity::ProductPasswordKdf;
    let dir = PrivateDir::new();
    let now = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00+00:00").unwrap().with_timezone(&chrono::Utc);
    let mut store = ProductStore::create_w1_pin_test_store(&dir.0, "v3.db", config()).unwrap();
    let source = ScopeSourceManifest { connector_id: "gh".into(), source_instance_id: "source-instance-1".into(), generation: 1, identity: RepositoryIdentity::canonicalized("acme", "GitHub", "https://api.github.com", "42").unwrap(), api_owner: Some("acme".into()), api_repo: Some("proj".into()), legacy_repository_locator: "acme/proj".into(), scope_query: "open".into(), enabled: true };
    let grant = ScopeGrantManifest { actor_id: "owner".into(), source_instance_id: source.source_instance_id.clone(), capabilities: vec![ProductCapability::ReadOrder] };
    store.bootstrap_owner_scoped("owner", &ProductPasswordKdf::hash("test-password").unwrap(), &[source], &[grant]).unwrap();
    let session = store.login("owner", "test-password", now).unwrap().digest().to_owned();
    let grants: Vec<_> = [ProductServiceAction::IntakeRead, ProductServiceAction::AdmitDelivery, ProductServiceAction::AckDelivery, ProductServiceAction::DriveReview].into_iter().map(|action| ProductServiceGrant { source_instance_id: "source-instance-1".into(), action }).collect();
    let service_receipt = store.manage_product_service(&session, "service", &grants, true, now + chrono::Duration::hours(3), 0, "configure", now).unwrap();
    assert_eq!((service_receipt.service_id.as_str(), service_receipt.revision, service_receipt.enabled), ("service", 1, true));
    let token = store.issue_product_service_credential(&session, "service", 1, now + chrono::Duration::hours(2), "credential", now).unwrap().expose_once_to_file_writer().to_owned();
    let service = store.authenticate_product_service_for_action(&token, "gh", "https://api.github.com", "42", ProductServiceAction::DriveReview, now).unwrap();
    assert_eq!((service.service_id(), service.source_instance_id()), ("service", "source-instance-1"));
    let path = dir.0.join("v3.db");
    let issue = "issue:service-w1";
    let payload = "immutable local adapter task";
    let updated = now.to_rfc3339();
    let version = foundry_ledger::product::delivery_version_digest(issue, &updated, "dep-bump", payload);
    let delivery = foundry_ledger::ProductDelivery { delivery_id: foundry_ledger::product::github_delivery_id("w1-install", "gh", "https://api.github.com", "42", issue, &version), connector_id: "gh".into(), repository_id: "42".into(), issue_id: issue.into(), observed_updated_at: updated.clone(), version_digest: version, task_kind: "dep-bump".into(), payload: payload.into() };
    let cursor = ProductPollCursor { connector_id: "gh".into(), query_fingerprint: "v3-query".into(), sweep_id: "v3-sweep".into(), next_page: 1, page_ceiling: 1, updated_at: updated };
    store.persist_intake_page_for_service(&service, &cursor, &[delivery.clone()], &[], now).unwrap();
    let receipt = store.admit_intake_delivery_for_service(&service, &delivery.delivery_id, "service-order", now).unwrap();
    assert_eq!((receipt.order_id.as_str(), receipt.lifecycle), ("service-order", 1));
    assert!(!receipt.event_hash.is_empty());
    store.mark_intake_acked_for_service(&service, &delivery.delivery_id, &receipt, now).unwrap();
    let view = store.read_scoped_order(&session, &receipt.order_id, now).unwrap();
    assert_eq!((view.order_id.as_str(), view.lifecycle, view.source_instance_id.as_str(), view.admission_sequence), (receipt.order_id.as_str(), receipt.lifecycle, "source-instance-1", receipt.sequence));
    // Synthetic adapter selection: exactly this admitted row changes from waiting_unsupported to ready.
    let mut expected = selection_typed_snapshot(&path);
    let rows = expected.get_mut("product_work_backlog_v1").unwrap();
    let row = rows.iter_mut().find(|row| row[0] == rusqlite::types::Value::Text(receipt.order_id.clone()) && row[1] == rusqlite::types::Value::Integer(receipt.lifecycle) && row[2] == rusqlite::types::Value::Text(delivery.delivery_id.clone())).unwrap();
    assert_eq!(row[3], rusqlite::types::Value::Text("waiting_unsupported".into()));
    row[3] = rusqlite::types::Value::Text("ready".into());
    rows.sort_by_cached_key(|row| format!("{row:?}"));
    assert_eq!(Connection::open(&path).unwrap().execute("UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id=?1 AND lifecycle=?2 AND delivery_id=?3 AND state='waiting_unsupported'", rusqlite::params![receipt.order_id, receipt.lifecycle, delivery.delivery_id]).unwrap(), 1);
    assert_eq!(selection_typed_snapshot(&path), expected);
    (dir, store, service, now)
}

#[test]
fn w1_pin_v4_claim_entrypoints_deny_pending_work() {
    for reopened in [false, true] {
        let (dir, mut store, service, now) = pin_pending_scoped_fixture();
        let path = dir.0.join("v3.db");
        if reopened { drop(store); store = ProductStore::reopen_w1_pin_test_store(&dir.0, "v3.db", config()).unwrap(); }
        let before = (selection_schema_observation(&path), selection_typed_snapshot(&path), recursive_snapshot(&dir.0));
        assert!(matches!(store.claim_product_work_for_service(&service, "worker", now, chrono::Duration::minutes(5)), Err(ProductError::ContractMismatch)));
        assert_eq!((selection_schema_observation(&path), selection_typed_snapshot(&path), recursive_snapshot(&dir.0)), before);
        let before = (selection_schema_observation(&path), selection_typed_snapshot(&path), recursive_snapshot(&dir.0));
        assert!(matches!(store.claim_product_work("worker", now, chrono::Duration::minutes(5)), Err(ProductError::ContractMismatch)));
        assert_eq!((selection_schema_observation(&path), selection_typed_snapshot(&path), recursive_snapshot(&dir.0)), before);
    }
}

#[test]
fn w1_pin_v4_claim_validators_deny_real_normal_claim() {
    use foundry_exec::{HostError, ReviewAdmissionStore};
    let normal_dir = PrivateDir::new();
    let mut normal = ProductStore::open(&normal_dir.0.join("normal.db"), config()).unwrap();
    let (request, claim) = prepare_review_with_claim(&mut normal, &normal_dir.0, &normal_dir.0.join("normal.db"));
    let now = chrono::Utc::now();
    assert!(claim.lease_until() > now);
    normal.validate_product_work_claim(&claim, now).unwrap();
    ReviewAdmissionStore::validate_product_work_claim(&normal, request.product_work.as_ref().unwrap(), &request.intent, now).unwrap();
    for reopened in [false, true] {
        let dir = PrivateDir::new();
        let path = dir.0.join("v4.db");
        let mut store = ProductStore::create_w1_pin_test_store(&dir.0, "v4.db", config()).unwrap();
        if reopened { drop(store); store = ProductStore::reopen_w1_pin_test_store(&dir.0, "v4.db", config()).unwrap(); }
        let before = (selection_schema_observation(&path), recursive_snapshot(&dir.0));
        assert!(matches!(store.validate_product_work_claim(&claim, now), Err(ProductError::ContractMismatch)));
        assert_eq!((selection_schema_observation(&path), recursive_snapshot(&dir.0)), before);
        let before = (selection_schema_observation(&path), recursive_snapshot(&dir.0));
        assert!(matches!(ReviewAdmissionStore::validate_product_work_claim(&store, request.product_work.as_ref().unwrap(), &request.intent, now), Err(HostError::Acceptance(ref m)) if m == "isolated product work claim requires an atomic pin"));
        assert_eq!((selection_schema_observation(&path), recursive_snapshot(&dir.0)), before);
    }
}

#[test]
fn w1_pin_v4_unscoped_claim_denies_pending_work() {
    for reopened in [false, true] {
        let dir = PrivateDir::new();
        let path = dir.0.join("v4.db");
        let mut store = ProductStore::create_w1_pin_test_store(&dir.0, "v4.db", config()).unwrap();
    store.bind_source("gh", "github", "https://api.github.com", "repo:42", "repo:42").unwrap();
    let now = chrono::Utc::now();
    let updated = now.to_rfc3339();
    let version = foundry_ledger::product::delivery_version_digest("issue:8", &updated, "dep-bump", "immutable task");
    let delivery = foundry_ledger::ProductDelivery {
        delivery_id: foundry_ledger::product::github_delivery_id("w1-install", "gh", "https://api.github.com", "repo:42", "issue:8", &version),
        connector_id: "gh".into(), repository_id: "repo:42".into(), issue_id: "issue:8".into(),
        observed_updated_at: updated.clone(), version_digest: version, task_kind: "dep-bump".into(), payload: "immutable task".into(),
    };
    store.persist_intake_page(&foundry_ledger::ProductPollCursor {
        connector_id: "gh".into(), query_fingerprint: "w1-test".into(), sweep_id: "sweep".into(),
        next_page: 1, page_ceiling: 1, updated_at: updated,
    }, &[delivery.clone()]).unwrap();
    let receipt = store.admit_intake_delivery(&delivery.delivery_id, "review-order", now).unwrap();
    // Synthetic adapter selection promotes the admitted backlog row to ready.
    assert_eq!(Connection::open(&path).unwrap().execute(
        "UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id=?1 AND lifecycle=?2 AND delivery_id=?3 AND state='waiting_unsupported'", rusqlite::params![receipt.order_id, receipt.lifecycle, delivery.delivery_id],
    ).unwrap(), 1);
        if reopened { drop(store); store = ProductStore::reopen_w1_pin_test_store(&dir.0, "v4.db", config()).unwrap(); }
        let conn = Connection::open(&path).unwrap();
        let mode: String = conn.query_row("SELECT mode FROM product_scope_policy_state_v1 WHERE singleton=1", [], |r| r.get(0)).unwrap();
        assert_ne!(mode, "enforced");
        let row: (String, i64, String, String) = conn.query_row("SELECT order_id,lifecycle,delivery_id,state FROM product_work_backlog_v1 WHERE order_id=?1 AND lifecycle=?2 AND delivery_id=?3", rusqlite::params![receipt.order_id, receipt.lifecycle, delivery.delivery_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
        assert_eq!(row, (receipt.order_id.clone(), receipt.lifecycle, delivery.delivery_id.clone(), "ready".into()));
        drop(conn);
        let before = (selection_schema_observation(&path), recursive_snapshot(&dir.0));
        assert!(matches!(store.claim_product_work("w1-worker", now, chrono::Duration::minutes(5)), Err(ProductError::ContractMismatch)));
        assert_eq!((selection_schema_observation(&path), recursive_snapshot(&dir.0)), before);
    }
}

// Constructibility only: claim authority is synthetic because v4 claims are denied.
fn pin_reference_constructibility_fixture() -> (PrivateDir, Vec<Vec<rusqlite::types::Value>>) {
    use foundry_ledger::{ProductCapability, ScopeGrantManifest};
    use foundry_ledger::product_identity::ProductPasswordKdf;
    use rusqlite::types::Value::{Integer, Text};
    let dir = PrivateDir::new();
    let path = dir.0.join("v4.db");
    let mut store = ProductStore::create_w1_pin_test_store(&dir.0, "v4.db", config()).unwrap();
    let source = ScopeSourceManifest { connector_id: "gh".into(), source_instance_id: "source-instance-1".into(), generation: 1, identity: RepositoryIdentity::canonicalized("acme", "GitHub", "https://api.github.com", "42").unwrap(), api_owner: Some("acme".into()), api_repo: Some("proj".into()), legacy_repository_locator: "acme/proj".into(), scope_query: "open".into(), enabled: true };
    let grant = ScopeGrantManifest { actor_id: "owner".into(), source_instance_id: source.source_instance_id.clone(), capabilities: vec![ProductCapability::ReadOrder] };
    store.bootstrap_owner_scoped("owner", &ProductPasswordKdf::hash("test-password").unwrap(), &[source], &[grant]).unwrap();
    let now = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z").unwrap().with_timezone(&chrono::Utc);
    let session = store.login("owner", "test-password", now).unwrap().digest().to_owned();
    assert_eq!(store.resolve_session(&session, now).unwrap().actor_id, "owner");
    let pack = "a".repeat(64);
    let synthetic_payload = "b".repeat(64);
    assert_ne!(pack, synthetic_payload);
    let approval = store.w1_change_pack_approval(&session, "source-instance-1", &pack, 0, true, "fixture-approve", now).unwrap();
    assert_eq!(approval.result_revision, 1);
    let mut pins = Vec::new();
    for (order, issue, epoch, fence, key) in [("fixture-order-1", "issue:1", 2, 7, "fixture-select-1"), ("fixture-order-2", "issue:2", 3, 8, "fixture-select-2")] {
        let admitted = store.admit_order(order, "gh", "42", issue, "dep-bump", "immutable task", now).unwrap();
        assert_eq!((admitted.order_id.as_str(), admitted.lifecycle), (order, 1));
        let view = store.read_scoped_order(&session, order, now).unwrap();
        assert_eq!((view.source_instance_id.as_str(), view.admission_sequence), ("source-instance-1", admitted.sequence));
        let selected = store.w1_select_pack(&session, order, 1, "source-instance-1", &pack, 0, key, now).unwrap();
        assert_eq!((selected.result_revision, selected.approval_revision), (1, Some(1)));
        pins.push(vec![Text(order.into()), Integer(1), Text("w1-install".into()), Text("w1-db".into()), Text("source-instance-1".into()), Text(pack.clone()), Integer(1), Integer(1), Integer(epoch), Integer(fence), Text(synthetic_payload.clone()), Integer(admitted.sequence), Text(admitted.event_hash)]);
    }
    drop(store);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA recursive_triggers=ON;").unwrap();
    for pragma in ["PRAGMA foreign_keys", "PRAGMA recursive_triggers"] { assert_eq!(conn.query_row(pragma, [], |r| r.get::<_, i64>(0)).unwrap(), 1); }
    for pin in &pins {
        // Deliberately synthetic payload digest and service/lease fields: this is not a real claim.
        // The order and admission coordinates come from the genuine scope provenance join.
        assert_eq!(conn.execute("INSERT INTO product_w1_claim_authority_v1 SELECT p.order_id,p.lifecycle,?3,?4,m.installation_id,m.database_id,'synthetic-delivery','synthetic-worker','2026-01-01T00:00:00Z','2026-01-01T01:00:00Z',?5,p.admission_seq,p.admission_hash,'synthetic-service',2,'2026-01-01T02:00:00Z',3,'2026-01-01T03:00:00Z','drive_review',4,p.source_instance_id,1,1,'gh',1,'https://api.github.com','42','acme','proj' FROM product_scope_order_provenance_v1 p JOIN product_orders_v1 o ON o.order_id=p.order_id AND o.lifecycle=p.lifecycle CROSS JOIN product_meta_v1 m WHERE p.order_id=?1 AND p.lifecycle=?2 AND p.source_instance_id='source-instance-1' AND p.admission_seq=?6 AND p.admission_hash=?7 AND m.singleton=1 AND m.installation_id='w1-install' AND m.database_id='w1-db'", rusqlite::params![pin[0], pin[1], pin[8], pin[9], pin[10], pin[11], pin[12]]).unwrap(), 1);
        let matched: i64 = conn.query_row("SELECT count(*) FROM product_w1_operations_v1 s JOIN product_w1_operations_v1 a ON a.source_instance_id=s.source_instance_id AND a.digest=s.digest AND a.approval_result_revision=s.referenced_approval_revision AND a.operation=s.referenced_operation JOIN product_w1_claim_authority_v1 c ON c.order_id=s.order_id AND c.lifecycle=s.lifecycle WHERE s.operation='select' AND s.actor_id='owner' AND a.actor_id='owner' AND a.operation='approve' AND s.order_id=?1 AND s.lifecycle=?2 AND c.installation_id=?3 AND c.database_id=?4 AND s.source_instance_id=?5 AND c.source_instance_id=?5 AND s.digest=?6 AND s.selection_result_revision=?7 AND s.referenced_approval_revision=?8 AND c.epoch=?9 AND c.fence=?10 AND c.payload_digest=?11 AND c.admission_seq=?12 AND c.admission_hash=?13", rusqlite::params_from_iter(pin.iter()), |r| r.get(0)).unwrap();
        assert_eq!(matched, 1);
    }
    drop(conn);
    (dir, pins)
}

#[test]
fn w1_pin_v4_reference_fixture_constructibility_only() {
    // No reference guard is installed. This demonstrates fixture constructibility only,
    // not reference checking, current authority, a valid claim, atomic admission,
    // host/runtime/external production, durable pinning, or W1 completion.
    let (dir, pins) = pin_reference_constructibility_fixture();
    let path = dir.0.join("v4.db");
    let non_db = || { let mut files = recursive_snapshot(&dir.0); files.retain(|p, _| !p.to_string_lossy().starts_with("v4.db")); files };
    let before = (selection_schema_observation(&path), non_db());
    let mut conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA recursive_triggers=ON;").unwrap();
    for pragma in ["PRAGMA foreign_keys", "PRAGMA recursive_triggers"] { assert_eq!(conn.query_row(pragma, [], |r| r.get::<_, i64>(0)).unwrap(), 1); }
    let tx = conn.transaction().unwrap();
    assert_eq!(tx.execute("INSERT INTO product_w1_lifecycle_pins_v1 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)", rusqlite::params_from_iter(pins[0].iter())).unwrap(), 1);
    let actual: Vec<rusqlite::types::Value> = tx.query_row("SELECT * FROM product_w1_lifecycle_pins_v1", [], |r| (0..13).map(|i| r.get(i)).collect()).unwrap();
    assert_eq!(actual, pins[0]);
    assert!(tx.prepare("PRAGMA foreign_key_check").unwrap().query([]).unwrap().next().unwrap().is_none());
    tx.rollback().unwrap();
    drop(conn);
    assert_eq!((selection_schema_observation(&path), non_db()), before);
    drop(ProductStore::reopen_w1_pin_test_store(&dir.0, "v4.db", config()).unwrap());
}
