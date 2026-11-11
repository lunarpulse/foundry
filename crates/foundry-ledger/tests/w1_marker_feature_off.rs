#![cfg(not(feature = "w1-isolated-test-support"))]

use foundry_ledger::{ProductConfig, ProductError, ProductStore, RepositoryIdentity, ScopeMigrationManifest, ScopeSourceManifest, SystemProductClock};
use rusqlite::Connection;
use std::{collections::BTreeMap, path::{Path, PathBuf}, sync::Arc};

struct TestDir(PathBuf);
impl TestDir {
    fn new() -> Self {
        let path = PathBuf::from("/home/cosmo/.hermes/cache/scratch").join(format!("foundry-w1-off-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TestDir { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
fn config() -> ProductConfig { ProductConfig { installation_id: "w1-install".into(), database_id: "w1-db".into() } }
fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(dir).unwrap().map(|entry| {
        let entry = entry.unwrap();
        (entry.file_name().into_string().unwrap(), std::fs::read(entry.path()).unwrap())
    }).collect()
}

#[test]
fn ordinary_store_feature_off_has_no_w1_schema() {
    let dir = TestDir::new();
    let path = dir.0.join("nested").join("ordinary.db");
    let mut store = ProductStore::open(&path, config()).unwrap();
    store.bind_source("gh", "github", "https://api.github.com", "repo:42", "repo:42").unwrap();
    drop(store);
    drop(ProductStore::open(&path, config()).unwrap());
    let conn = Connection::open(&path).unwrap();
    let count: i64 = conn.query_row("SELECT count(*) FROM sqlite_master WHERE type IN ('table','trigger') AND name LIKE 'product_w1_%'", [], |row| row.get(0)).unwrap();
    assert_eq!(count, 0);
    drop(conn);

    let marked_dir = TestDir::new();
    let marked = marked_dir.0.join("marked.db");
    let conn = Connection::open(&marked).unwrap();
    conn.execute_batch("CREATE TABLE product_w1_isolated_marker_v1(singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=2)); INSERT INTO product_w1_isolated_marker_v1 VALUES(1,2);").unwrap();
    drop(conn);
    let manifest = ScopeMigrationManifest {
        manifest_id: "reviewed".into(), reviewed_by: "tester".into(),
        sources: vec![ScopeSourceManifest {
            connector_id: "gh".into(), source_instance_id: "source-instance-1".into(), generation: 1,
            identity: RepositoryIdentity::canonicalized("acme", "GitHub", "https://api.github.com", "42").unwrap(),
            api_owner: Some("acme".into()), api_repo: Some("proj".into()),
            legacy_repository_locator: "acme/proj".into(), scope_query: "open".into(), enabled: true,
        }], verified_orders: vec![], grants: vec![],
    };
    let before = snapshot(&marked_dir.0);
    assert!(matches!(ProductStore::open(&marked, config()), Err(ProductError::ContractMismatch)));
    assert_eq!(snapshot(&marked_dir.0), before);
    assert!(matches!(ProductStore::open_with_clock(&marked, config(), Arc::new(SystemProductClock)), Err(ProductError::ContractMismatch)));
    assert_eq!(snapshot(&marked_dir.0), before);
    assert!(matches!(ProductStore::migrate_scope_v1(&marked, &config(), &manifest, chrono::Utc::now()), Err(ProductError::ContractMismatch)));
    assert_eq!(snapshot(&marked_dir.0), before);
}

#[test]
fn v3_marker_rejected_feature_off() {
    let dir = TestDir::new();
    let path = dir.0.join("v3.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE product_w1_isolated_marker_v1(singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=3)); INSERT INTO product_w1_isolated_marker_v1 VALUES(1,3);").unwrap();
    drop(conn);
    let before = snapshot(&dir.0);
    assert!(matches!(ProductStore::open(&path, config()), Err(ProductError::ContractMismatch)));
    assert_eq!(snapshot(&dir.0), before);
    assert!(matches!(ProductStore::open_with_clock(&path, config(), Arc::new(SystemProductClock)), Err(ProductError::ContractMismatch)));
    assert_eq!(snapshot(&dir.0), before);
}

#[test]
fn v4_marker_rejected_feature_off() {
    let dir = TestDir::new();
    let path = dir.0.join("v4.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE product_w1_isolated_marker_v1(singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=4)); INSERT INTO product_w1_isolated_marker_v1 VALUES(1,4);").unwrap();
    drop(conn);
    let before = snapshot(&dir.0);
    assert!(matches!(ProductStore::open(&path, config()), Err(ProductError::ContractMismatch)));
    assert_eq!(snapshot(&dir.0), before);
    assert!(matches!(ProductStore::open_with_clock(&path, config(), Arc::new(SystemProductClock)), Err(ProductError::ContractMismatch)));
    assert_eq!(snapshot(&dir.0), before);
}
