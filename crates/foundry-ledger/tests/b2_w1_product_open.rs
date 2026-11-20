use foundry_ledger::{ProductConfig, ProductError, ProductStore};
use rusqlite::{Connection, types::Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

type LogicalSnapshot = BTreeMap<String, Vec<Vec<Value>>>;
type SchemaSnapshot = Vec<(String, String, String, Option<String>)>;

fn logical_snapshot(path: &Path) -> LogicalSnapshot {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open logical snapshot observer");
    let mut tables = conn
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap();
    let names: Vec<String> = tables
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    names
        .into_iter()
        .map(|name| {
            let escaped = name.replace('"', "\"\"");
            let mut statement = conn
                .prepare(&format!("SELECT * FROM \"{escaped}\""))
                .unwrap();
            let columns = statement.column_count();
            let mut rows: Vec<Vec<Value>> = statement
                .query_map([], |row| (0..columns).map(|index| row.get(index)).collect())
                .unwrap()
                .map(Result::unwrap)
                .collect();
            rows.sort_by_cached_key(|row| format!("{row:?}"));
            (name, rows)
        })
        .collect()
}

fn schema_snapshot(path: &Path) -> SchemaSnapshot {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open schema snapshot observer");
    let mut statement = conn
        .prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name,tbl_name,sql")
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn product_meta(path: &Path) -> (String, String, i64, i64) {
    Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap()
        .query_row(
            "SELECT installation_id,database_id,schema_version,recovery_epoch FROM product_meta_v1 WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap()
}

fn manifest_path(path: &Path) -> PathBuf {
    path.with_extension("manifest")
}

fn scratch_path(name: &str) -> PathBuf {
    let root =
        std::path::PathBuf::from(std::env::var_os("TMPDIR").unwrap_or_else(|| "/tmp".into()));
    root.join(format!(
        "{name}-{}-{}.sqlite",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ))
}

#[test]
fn reopen_feature_off_rejected_v4() {
    let path = scratch_path("b2-w1-product-open-v4");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE foundry_contract(singleton INTEGER PRIMARY KEY CHECK(singleton=1),contract TEXT NOT NULL); INSERT INTO foundry_contract VALUES(1,'ProductAtomicV1'); CREATE TABLE product_meta_v1(singleton INTEGER PRIMARY KEY CHECK(singleton=1),installation_id TEXT NOT NULL,database_id TEXT NOT NULL,schema_version INTEGER NOT NULL,recovery_epoch INTEGER NOT NULL DEFAULT 0); INSERT INTO product_meta_v1 VALUES(1,'install','database',4,0);").unwrap();
    drop(conn);
    let result = ProductStore::open(
        &path,
        ProductConfig {
            installation_id: "install".into(),
            database_id: "database".into(),
        },
    );
    #[cfg(not(feature = "product-pack-pins"))]
    assert!(matches!(result, Err(ProductError::ContractMismatch)));
    #[cfg(feature = "product-pack-pins")]
    assert!(matches!(result, Err(ProductError::ContractMismatch)));
    let _ = std::fs::remove_file(path);
}

#[cfg(not(feature = "product-pack-pins"))]
#[test]
fn fresh_v3_reopen_is_untouched_and_has_no_w1_schema() {
    let path = scratch_path("b2-w1-genuine-v3");
    let config = ProductConfig {
        installation_id: "v3-install".into(),
        database_id: "v3-database".into(),
    };
    let store =
        ProductStore::open(&path, config.clone()).expect("create genuine feature-off v3 store");
    drop(store);
    let before_state = logical_snapshot(&path);
    let before_schema = schema_snapshot(&path);
    let before_meta = product_meta(&path);
    assert_eq!(
        before_meta,
        ("v3-install".into(), "v3-database".into(), 3, 0)
    );
    let reopened = ProductStore::open(&path, config).expect("reopen genuine v3 store");
    drop(reopened);
    assert_eq!(logical_snapshot(&path), before_state);
    assert_eq!(schema_snapshot(&path), before_schema);
    assert_eq!(product_meta(&path), before_meta);
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name GLOB 'product_w1_*'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    drop(conn);
    let _ = std::fs::remove_file(path);
}

#[cfg(not(feature = "product-pack-pins"))]
#[test]
fn emit_genuine_feature_off_v3_preimage() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let path = PathBuf::from(
        std::env::var_os("W1_V3_PREIMAGE").expect("W1_V3_PREIMAGE must name the private v3 DB"),
    );
    assert!(path.is_absolute(), "v3 preimage path must be absolute");
    let parent = path.parent().expect("v3 preimage has a parent");
    if !parent.exists() {
        std::fs::create_dir_all(parent).expect("create private v3 preimage parent");
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .expect("make v3 preimage parent private");
    }
    let parent_meta = std::fs::symlink_metadata(parent).expect("inspect v3 preimage parent");
    assert!(parent_meta.file_type().is_dir() && !parent_meta.file_type().is_symlink());
    assert_eq!(
        parent_meta.mode() & 0o077,
        0,
        "v3 preimage parent must be private"
    );
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        assert!(meta.file_type().is_file() && !meta.file_type().is_symlink());
        assert_eq!(meta.uid(), parent_meta.uid(), "v3 preimage owner changed");
    }

    let config = ProductConfig {
        installation_id: "w1-genuine-v3-install".into(),
        database_id: "w1-genuine-v3-database".into(),
    };
    let existed = path.exists();
    let before = existed.then(|| (logical_snapshot(&path), schema_snapshot(&path)));
    let store = ProductStore::open(&path, config.clone())
        .expect("feature-off ProductStore creates or reopens the genuine v3 preimage");
    drop(store);
    assert_eq!(
        product_meta(&path),
        (
            config.installation_id.clone(),
            config.database_id.clone(),
            3,
            0,
        )
    );
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name GLOB 'product_w1_*'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        0
    );
    drop(conn);
    if let Some((state, schema)) = before {
        assert_eq!(logical_snapshot(&path), state);
        assert_eq!(schema_snapshot(&path), schema);
    }
    let bound_path = path
        .canonicalize()
        .expect("canonicalize genuine v3 preimage path");
    let bound_path = bound_path
        .to_str()
        .expect("v3 preimage path must be valid UTF-8 for its durable manifest");
    assert!(
        !bound_path.contains('\n') && !bound_path.contains('\r'),
        "v3 preimage path must fit one manifest field"
    );
    let manifest = format!(
        "producer_cell=F:no-default-features\nproducer_selector=emit_genuine_feature_off_v3_preimage\npreimage_path={bound_path}\ninstallation_id={}\ndatabase_id={}\nproduct_schema_version=3\n",
        config.installation_id, config.database_id,
    );
    let manifest_path = manifest_path(&path);
    if manifest_path.exists() {
        assert_eq!(std::fs::read_to_string(&manifest_path).unwrap(), manifest);
    } else {
        std::fs::write(&manifest_path, manifest).expect("write durable v3 provenance manifest");
        std::fs::set_permissions(&manifest_path, std::fs::Permissions::from_mode(0o600))
            .expect("make v3 provenance manifest private");
    }
}

#[cfg(feature = "product-pack-pins")]
mod product_pins {
    use super::*;
    use chrono::{TimeZone, Utc};
    use foundry_ledger::{
        PackRegistryConfig, ProductCapability, RepositoryIdentity, ScopeGrantManifest,
        ScopeSourceManifest,
    };
    use std::{
        os::unix::fs::DirBuilderExt,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn feature_on_genuine_v3_is_not_promoted() {
        let path = PathBuf::from(
            std::env::var_os("W1_V3_PREIMAGE")
                .expect("producer provenance missing: W1_V3_PREIMAGE is not set"),
        );
        assert!(path.is_absolute());
        let bound_path = path
            .canonicalize()
            .expect("producer preimage path must exist")
            .to_str()
            .expect("producer preimage path must be valid UTF-8")
            .to_owned();
        let expected_manifest = format!(
            "producer_cell=F:no-default-features\nproducer_selector=emit_genuine_feature_off_v3_preimage\npreimage_path={bound_path}\ninstallation_id=w1-genuine-v3-install\ndatabase_id=w1-genuine-v3-database\nproduct_schema_version=3\n"
        );
        assert_eq!(
            std::fs::read_to_string(manifest_path(&path))
                .expect("producer provenance manifest must exist"),
            expected_manifest
        );
        let config = ProductConfig {
            installation_id: "w1-genuine-v3-install".into(),
            database_id: "w1-genuine-v3-database".into(),
        };
        assert_eq!(
            product_meta(&path),
            (
                config.installation_id.clone(),
                config.database_id.clone(),
                3,
                0
            )
        );
        let before_state = logical_snapshot(&path);
        let before_schema = schema_snapshot(&path);
        let store = ProductStore::open(&path, config)
            .expect("feature-on normal open accepts genuine v3 without promotion");
        drop(store);
        assert_eq!(logical_snapshot(&path), before_state);
        assert_eq!(schema_snapshot(&path), before_schema);
        assert_eq!(product_meta(&path).2, 3);
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name GLOB 'product_w1_*'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            0
        );
    }

    struct Fixture {
        root: PathBuf,
        path: PathBuf,
        config: ProductConfig,
        digest: String,
        admission: foundry_ledger::AdmissionReceipt,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            if std::thread::panicking() {
                eprintln!("retained W1-b open fixture: {}", self.root.display());
            } else {
                std::fs::remove_dir_all(&self.root).expect("remove successful open fixture");
            }
        }
    }
    fn fixture() -> Fixture {
        let base = PathBuf::from(std::env::var_os("TMPDIR").expect("configured scratch"));
        assert_ne!(base, PathBuf::from("/tmp"));
        let root = base.join(format!(
            "b2-w1-open-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let path = root.join("product.sqlite");
        let config = ProductConfig {
            installation_id: "open-install".into(),
            database_id: "open-database".into(),
        };
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let source = ScopeSourceManifest {
            connector_id: "github-primary".into(),
            source_instance_id: "open-source".into(),
            generation: 1,
            identity: RepositoryIdentity::canonicalized(
                "acme",
                "github",
                "https://api.github.com",
                "repo:1",
            )
            .unwrap(),
            api_owner: Some("acme".into()),
            api_repo: Some("project".into()),
            legacy_repository_locator: "repo:1".into(),
            scope_query: "open".into(),
            enabled: true,
        };
        let phc = foundry_ledger::product_identity::ProductPasswordKdf::hash("open-owner-password")
            .unwrap();
        let mut store = ProductStore::create_product_pin_store(&path, &config).unwrap();
        store
            .bootstrap_owner_scoped(
                "owner",
                &phc,
                std::slice::from_ref(&source),
                &[ScopeGrantManifest {
                    actor_id: "owner".into(),
                    source_instance_id: "open-source".into(),
                    capabilities: vec![ProductCapability::ReadOrder],
                }],
            )
            .unwrap();
        let owner = store
            .login("owner", "open-owner-password", now)
            .unwrap()
            .digest()
            .to_owned();
        let admission = store
            .admit_order(
                "order-open",
                "github-primary",
                "repo:1",
                "issue:open",
                "dep-bump",
                "payload",
                now,
            )
            .unwrap();
        let pack = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../skills/bmad-review-full/v1")
            .canonicalize()
            .unwrap();
        let digest = foundry_pack::load_pack(&pack).unwrap().digest().to_owned();
        store = store.with_product_registry(PackRegistryConfig {
            snapshots: [(digest.clone(), pack)].into_iter().collect(),
        });
        store
            .change_pack_approval(&owner, "open-source", &digest, 0, true, "approve-open", now)
            .unwrap();
        store
            .select_order_pack(
                &owner,
                "order-open",
                admission.lifecycle,
                "open-source",
                &digest,
                0,
                "select-open",
                now,
            )
            .unwrap();
        drop(store);
        Fixture {
            root,
            path,
            config,
            digest,
            admission,
        }
    }
    // Raw insertion is confined to corruption fixtures below. Positive reopen
    // coverage is the genuine atomic selector in b2_w1_runtime:
    // p04_atomic_selected_activation_reopens_state_schema_and_replays_exactly.
    fn inject_pin_for_tamper_test(f: &Fixture) {
        let c = Connection::open(&f.path).unwrap();
        c.execute("INSERT INTO product_w1_lifecycle_pins_v1(order_id,lifecycle,installation_id,database_id,source_instance_id,pack_digest,selection_revision,referenced_approval_revision,service_id,request_key,claim_epoch,claim_fence,payload_digest,admission_seq,admission_hash) VALUES(?1,?2,?3,?4,'open-source',?5,1,1,'open-service','open-pin',1,1,?6,?7,?8)",rusqlite::params![f.admission.order_id,f.admission.lifecycle,f.config.installation_id,f.config.database_id,f.digest,"c".repeat(64),f.admission.sequence,f.admission.event_hash]).unwrap();
    }
    fn check_corrupt_value(f: &Fixture, trigger: &str, read: &str, update: &str, corrupt: Value) {
        drop(ProductStore::open(&f.path, f.config.clone()).expect("valid control must reopen"));
        let schema = schema_snapshot(&f.path);
        let mut c = Connection::open(&f.path).unwrap();
        let tx = c.transaction().unwrap();
        let original: Value = tx.query_row(read, [], |row| row.get(0)).unwrap();
        assert_ne!(original, corrupt);
        let ddl: String = tx
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE type='trigger' AND name=?1",
                [trigger],
                |row| row.get(0),
            )
            .unwrap();
        tx.execute_batch(&format!("DROP TRIGGER {trigger}"))
            .unwrap();
        assert_eq!(tx.execute(update, [&corrupt]).unwrap(), 1);
        tx.execute_batch(&ddl).unwrap();
        tx.commit().unwrap();
        assert_eq!(
            c.query_row(read, [], |row| row.get::<_, Value>(0)).unwrap(),
            corrupt
        );
        drop(c);
        assert_eq!(schema_snapshot(&f.path), schema);
        assert!(matches!(
            ProductStore::open(&f.path, f.config.clone()),
            Err(ProductError::ContractMismatch)
        ));
    }

    #[test]
    fn reopen_tampered_pin_rejected_contract_mismatch() {
        let f = fixture();
        inject_pin_for_tamper_test(&f);
        check_corrupt_value(
            &f,
            "product_w1_lifecycle_pins_no_update",
            "SELECT pack_digest FROM product_w1_lifecycle_pins_v1 WHERE order_id='order-open'",
            "UPDATE product_w1_lifecycle_pins_v1 SET pack_digest=?1 WHERE order_id='order-open'",
            Value::Text("b".repeat(64)),
        );
    }

    #[test]
    fn reopen_tampered_operation_history_rejected() {
        let f = fixture();
        check_corrupt_value(
            &f,
            "product_w1_operations_no_update",
            "SELECT receipt FROM product_w1_operations_v1 WHERE request_key='select-open'",
            "UPDATE product_w1_operations_v1 SET receipt=?1 WHERE request_key='select-open'",
            Value::Blob(vec![1, 2, 3]),
        );
    }

    #[cfg(feature = "w1-isolated-test-support")]
    #[test]
    fn open_fixture_retains_closed_database_on_failure() {
        let f = fixture();
        let root = f.root.clone();
        let before = logical_snapshot(&f.path);
        let failure = std::panic::catch_unwind(move || {
            let _fixture = f;
            panic!("intentional open fixture custody probe");
        });
        assert!(failure.is_err());
        assert_eq!(logical_snapshot(&root.join("product.sqlite")), before);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(feature = "w1-isolated-test-support")]
    #[test]
    fn isolated_v4_remains_separate_from_normal_v4() {
        let f = fixture();
        let normal_state = logical_snapshot(&f.path);
        let normal_schema = schema_snapshot(&f.path);
        assert_eq!(
            product_meta(&f.path).2,
            4,
            "normal pin profile uses Product schema v4"
        );
        let normal_conn = Connection::open(&f.path).unwrap();
        assert_eq!(
            normal_conn
                .query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE name='product_w1_isolated_marker_v1'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        drop(normal_conn);
        let normal_reopened = ProductStore::open(&f.path, f.config.clone())
            .expect("normal v4 profile reopens normally");
        drop(normal_reopened);
        assert_eq!(logical_snapshot(&f.path), normal_state);
        assert_eq!(schema_snapshot(&f.path), normal_schema);

        let root = f.root.join("isolated");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let isolated_path = root.join("isolated.sqlite");
        let config = ProductConfig {
            installation_id: "isolated-install".into(),
            database_id: "isolated-database".into(),
        };
        let isolated =
            ProductStore::create_w1_pin_test_store(&root, "isolated.sqlite", config.clone())
                .unwrap();
        drop(isolated);
        assert_eq!(
            product_meta(&isolated_path).2,
            3,
            "isolated profile keeps Product schema v3"
        );
        let marker_version: i64 = Connection::open(&isolated_path)
            .unwrap()
            .query_row(
                "SELECT version FROM product_w1_isolated_marker_v1 WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(marker_version, 4, "isolated profile marker version is v4");
        let isolated_state = logical_snapshot(&isolated_path);
        let isolated_schema = schema_snapshot(&isolated_path);
        assert!(matches!(
            ProductStore::open(&isolated_path, config.clone()),
            Err(ProductError::ContractMismatch)
        ));
        assert_eq!(logical_snapshot(&isolated_path), isolated_state);
        assert_eq!(schema_snapshot(&isolated_path), isolated_schema);
        let isolated_reopened =
            ProductStore::reopen_w1_pin_test_store(&root, "isolated.sqlite", config)
                .expect("isolated profile reopens through isolated authority");
        drop(isolated_reopened);
        assert_eq!(logical_snapshot(&isolated_path), isolated_state);
        assert_eq!(schema_snapshot(&isolated_path), isolated_schema);
        std::fs::remove_dir_all(root).unwrap();
    }
}
