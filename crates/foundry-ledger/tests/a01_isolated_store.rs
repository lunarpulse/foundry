#![cfg(all(feature = "a01-harness-support", unix))]

use chrono::{DateTime, Duration, Utc};
use foundry_exec::{HostError, ReviewAdmissionRequest, ReviewAdmissionStore};
use foundry_ledger::{ProductClock, ProductConfig, ProductError, ProductStore};
use rusqlite::{Connection, params, types::Value};
use std::collections::BTreeMap;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;

// All test failures use fixed diagnostics, including fallible fixture setup.
#[track_caller]
fn require<T, E>(result: Result<T, E>) -> T {
    match result { Ok(value) => value, Err(_) => panic!("A01 test operation failed") }
}

#[track_caller]
fn require_fixture<T>(
    result: rusqlite::Result<T>,
    case: &'static str,
    stage: &'static str,
) -> T {
    match result {
        Ok(value) => value,
        Err(rusqlite::Error::SqliteFailure(code, _)) => panic!(
            "A01 fixture operation failed: case={case} stage={stage} sqlite_code={}",
            code.extended_code
        ),
        Err(_) => panic!(
            "A01 fixture operation failed: case={case} stage={stage} sqlite_code=none"
        ),
    }
}

struct PrivateDir(PathBuf);
impl PrivateDir {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let stamp = require(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)).as_nanos();
        let path = std::env::temp_dir().join(format!("foundry-a01-{}-{stamp}-{serial}", std::process::id()));
        require(std::fs::DirBuilder::new().mode(0o700).create(&path));
        Self(require(path.canonicalize()))
    }
}
impl Drop for PrivateDir {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

struct FixedClock;
impl ProductClock for FixedClock { fn now(&self) -> DateTime<Utc> { now() } }
fn now() -> DateTime<Utc> {
    require(DateTime::parse_from_rfc3339("2026-01-01T00:00:00+00:00")).with_timezone(&Utc)
}
fn clock() -> Arc<dyn ProductClock> { Arc::new(FixedClock) }
fn config() -> ProductConfig {
    ProductConfig { installation_id: "a01-test-installation".into(), database_id: "a01-test-database".into() }
}
fn create(dir: &PrivateDir) -> ProductStore {
    require(ProductStore::create_a01_isolated_test_store(&dir.0, "store.db", config(), clock()))
}
fn reopen(dir: &PrivateDir) -> ProductStore {
    require(ProductStore::reopen_a01_isolated_test_store(&dir.0, "store.db", config(), clock()))
}
fn read_db(path: &Path) -> Connection {
    require(Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY))
}
fn count(path: &Path, table: &str) -> i64 {
    require(read_db(path).query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)))
}

type Files = BTreeMap<PathBuf, (u32, u32, u64, Vec<u8>)>;
fn files(dir: &Path, database_bytes: bool) -> Files {
    fn visit(root: &Path, dir: &Path, database_bytes: bool, out: &mut Files) {
        for entry in require(std::fs::read_dir(dir)) {
            let entry = require(entry);
            let path = entry.path();
            let meta = require(std::fs::symlink_metadata(&path));
            let relative = require(path.strip_prefix(root)).to_path_buf();
            let bytes = if meta.file_type().is_symlink() {
                use std::os::unix::ffi::OsStrExt;
                require(std::fs::read_link(&path)).as_os_str().as_bytes().to_vec()
            } else if meta.is_file() && (database_bytes || relative != Path::new("store.db")) {
                require(std::fs::read(&path))
            } else { Vec::new() };
            out.insert(relative, (meta.mode(), meta.uid(), meta.nlink(), bytes));
            if meta.is_dir() { visit(root, &path, database_bytes, out); }
        }
    }
    let mut out = BTreeMap::new();
    visit(dir, dir, database_bytes, &mut out);
    out
}

// Keep rows and schema private. Never use assert_eq! on these observations.
fn logical(path: &Path) -> BTreeMap<String, Vec<Vec<Value>>> {
    let conn = read_db(path);
    let mut names = require(conn.prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name"));
    let names: Vec<String> = require(require(names.query_map([], |r| r.get(0))).collect());
    let mut out = BTreeMap::new();
    for name in names {
        let quoted = name.replace('"', "\"\"");
        let mut st = require(conn.prepare(&format!("SELECT * FROM \"{quoted}\"")));
        let columns = st.column_count();
        let mut rows: Vec<Vec<Value>> = require(require(st.query_map([], |r| {
            (0..columns).map(|i| r.get(i)).collect::<rusqlite::Result<_>>()
        })).collect());
        rows.sort_by_cached_key(|row| format!("{row:?}"));
        out.insert(name, rows);
    }
    let mut st = require(conn.prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name"));
    out.insert("schema definitions".into(), require(require(st.query_map([], |r| {
        (0..4).map(|i| r.get(i)).collect::<rusqlite::Result<_>>()
    })).collect()));
    out
}
fn preserved(dir: &PrivateDir) -> (BTreeMap<String, Vec<Vec<Value>>>, Files) {
    (logical(&dir.0.join("store.db")), files(&dir.0, false))
}
fn invalid(result: Result<ProductStore, ProductError>) {
    assert!(matches!(result, Err(ProductError::InvalidConfig)), "A01 path denial mismatch");
}
fn mismatch(result: Result<ProductStore, ProductError>) {
    assert!(matches!(result, Err(ProductError::ContractMismatch)), "A01 contract denial mismatch");
}

#[test]
fn a01_fresh_create_preserves_product_review_and_result_contracts() {
    // Frozen public four-argument signatures are also feature-on compile controls.
    type Open = fn(&Path, &str, ProductConfig, Arc<dyn ProductClock>) -> Result<ProductStore, ProductError>;
    let _: Open = ProductStore::create_a01_isolated_test_store;
    let _: Open = ProductStore::reopen_a01_isolated_test_store;
    let dir = PrivateDir::new();
    let store = create(&dir);
    assert!(store.installation_id() == config().installation_id && store.database_id() == config().database_id,
        "A01 identity mismatch");
    let conn = read_db(&dir.0.join("store.db"));
    let contract: String = require(conn.query_row("SELECT contract FROM foundry_contract WHERE singleton=1", [], |r| r.get(0)));
    assert!(contract == "ProductAtomicV1", "A01 contract mismatch");
    for (table, column, expected) in [
        ("product_meta_v1", "schema_version", 0x0A01),
        ("product_review_meta_v1", "schema_version", 1),
        ("product_result_meta_v1", "result_contract_version", 1),
    ] {
        let row: (String, String, i64) = require(conn.query_row(
            &format!("SELECT installation_id,database_id,{column} FROM {table} WHERE singleton=1"),
            [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))));
        assert!(row == (config().installation_id, config().database_id, expected), "A01 metadata mismatch");
    }
    assert!(require(require(conn.prepare("PRAGMA foreign_key_check")).query([])).next().map(|r| r.is_none()).unwrap_or(false),
        "A01 foreign key integrity mismatch");
    assert!(count(&dir.0.join("store.db"), "product_work_claims_v1") == 0, "A01 unexpected work");
    drop(store);
    drop(reopen(&dir));
    // Exactly two replacements and one additional trigger distinguish marked schema.
    let normal = PrivateDir::new();
    drop(require(ProductStore::open(&normal.0.join("store.db"), config())));
    let a = logical(&dir.0.join("store.db"));
    let b = logical(&normal.0.join("store.db"));
    foundation_schema_delta(&a, &b);
}

#[test]
fn a01_reopen_preserves_identity_and_rejects_unrelated_identity() {
    let dir = PrivateDir::new();
    drop(create(&dir));
    let path = dir.0.join("store.db");
    let meta = require(std::fs::metadata(&path));
    let before = preserved(&dir);
    let store = reopen(&dir);
    assert!(store.installation_id() == config().installation_id && store.database_id() == config().database_id,
        "A01 logical identity mismatch");
    let after = require(std::fs::metadata(&path));
    assert!(meta.dev() == after.dev() && meta.ino() == after.ino(), "A01 physical identity mismatch");
    drop(store);
    assert!(preserved(&dir) == before, "A01 reopen changed state");
    for installation in [true, false] {
        let mut wrong = config();
        if installation { wrong.installation_id = "unrelated-installation".into(); }
        else { wrong.database_id = "unrelated-database".into(); }
        let before = files(&dir.0, true);
        mismatch(ProductStore::reopen_a01_isolated_test_store(&dir.0, "store.db", wrong, clock()));
        assert!(files(&dir.0, true) == before, "A01 identity denial changed files");
    }
    // The assembly's additional expected-inode check is outside this API.
    let copy = PrivateDir::new();
    require(std::fs::copy(&path, copy.0.join("store.db")));
    let copied = require(std::fs::metadata(copy.0.join("store.db")));
    assert!((meta.dev(), meta.ino()) != (copied.dev(), copied.ino()), "A01 copy control lacks distinct identity");
}

#[test]
fn a01_normal_openers_and_feature_reopen_reject_wrong_mode_without_mutation() {
    let dir = PrivateDir::new();
    drop(create(&dir));
    let path = dir.0.join("store.db");
    let before = files(&dir.0, true);
    mismatch(ProductStore::open(&path, config()));
    assert!(files(&dir.0, true) == before, "A01 ordinary denial changed files");
    mismatch(ProductStore::open_with_clock(&path, config(), clock()));
    assert!(files(&dir.0, true) == before, "A01 clock opener denial changed files");
    let normal = PrivateDir::new();
    drop(require(ProductStore::open(&normal.0.join("store.db"), config())));
    // Satisfy the A01 filesystem predicate so mode, rather than permissions,
    // is the independently changed negative condition.
    require(std::fs::set_permissions(normal.0.join("store.db"), std::fs::Permissions::from_mode(0o600)));
    let before = files(&normal.0, true);
    mismatch(ProductStore::reopen_a01_isolated_test_store(&normal.0, "store.db", config(), clock()));
    assert!(files(&normal.0, true) == before, "A01 wrong-mode denial changed files");
}

#[test]
fn a01_create_rejects_existing_invalid_symlink_and_dangling_paths_without_mutation() {
    let control = PrivateDir::new();
    drop(create(&control));
    for kind in 0..4 {
        let dir = PrivateDir::new();
        let path = dir.0.join("store.db");
        match kind {
            0 => require(std::fs::write(&path, b"existing entry")),
            1 => { require(std::fs::write(dir.0.join("target"), b"existing target")); require(symlink("target", &path)); }
            2 => require(symlink("missing", &path)),
            _ => require(std::fs::create_dir(&path)),
        }
        // Every destination, including absolute names and "..", is contained
    // within this owned disposable outer directory.
    let before = files(&dir.0, true);
        invalid(ProductStore::create_a01_isolated_test_store(&dir.0, "store.db", config(), clock()));
        assert!(files(&dir.0, true) == before, "A01 existing-entry denial changed files");
    }
    let dir = PrivateDir::new();
    let store_directory = dir.0.join("filename-cases");
    require(std::fs::DirBuilder::new().mode(0o700).create(&store_directory));
    require(std::fs::DirBuilder::new().mode(0o700).create(
        store_directory.join("nested"),
    ));
    let absolute_destination = dir.0.join("absolute-destination.db");
    let absolute_name = require(absolute_destination.to_str().ok_or(()));
    let nested_destination = store_directory.join("nested/store.db");
    for name in ["", ".", "..", "nested/store.db", absolute_name, "store.db/", "bad\0name"] {
        let before = files(&dir.0, true);
        invalid(ProductStore::create_a01_isolated_test_store(&store_directory, name, config(), clock()));
        assert!(before == files(&dir.0, true), "A01 invalid-name creation changed fixture destinations");
        invalid(ProductStore::reopen_a01_isolated_test_store(&store_directory, name, config(), clock()));
        assert!(files(&dir.0, true) == before, "A01 filename denial changed files");

        assert!(std::fs::symlink_metadata(&absolute_destination).is_err(),
            "A01 invalid absolute destination changed");
        assert!(std::fs::symlink_metadata(&nested_destination).is_err(),
            "A01 invalid nested destination changed");
    }
    let alias = PrivateDir::new();
    require(symlink(&control.0, alias.0.join("linked")));
    require(std::fs::DirBuilder::new().mode(0o700).create(control.0.join("child")));
    let before = files(&control.0, true);
    let alias_before = files(&alias.0, true);
    let filename_parent_before = files(&dir.0, true);
    for directory in [alias.0.join("linked"), alias.0.join("linked/child"),
        control.0.join("child/.."), control.0.join("."), store_directory.join("..")]
    {
        invalid(ProductStore::create_a01_isolated_test_store(&directory, "other.db", config(), clock()));
        assert!(before == files(&control.0, true), "A01 invalid-directory creation changed fixture destinations");
        assert!(alias_before == files(&alias.0, true), "A01 invalid-directory creation changed fixture destinations");
        assert!(filename_parent_before == files(&dir.0, true), "A01 invalid-directory creation changed fixture destinations");
        invalid(ProductStore::reopen_a01_isolated_test_store(&directory, "store.db", config(), clock()));
        assert!(before == files(&control.0, true), "A01 invalid-directory creation changed fixture destinations");
        assert!(alias_before == files(&alias.0, true), "A01 invalid-directory creation changed fixture destinations");
        assert!(filename_parent_before == files(&dir.0, true), "A01 invalid-directory creation changed fixture destinations");
    }
    assert!(files(&control.0, true) == before, "A01 directory denial changed files");
}

#[test]
fn a01_reopen_rejects_hardlinks_and_insecure_modes_without_mutation() {
    for kind in 0..6 {
        let dir = PrivateDir::new();
        drop(create(&dir));
        let path = dir.0.join("store.db");
        match kind {
            0 => require(std::fs::hard_link(&path, dir.0.join("alias.db"))),
            1 => require(std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640))),
            2 => require(std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(0o750))),
            3 => require(std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o604))),
            4 => { require(std::fs::rename(&path, dir.0.join("target"))); require(symlink("target", &path)); }
            _ => { require(std::fs::remove_file(&path)); require(symlink("missing", &path)); }
        }
        let before = files(&dir.0, true);
        invalid(ProductStore::reopen_a01_isolated_test_store(&dir.0, "store.db", config(), clock()));
        assert!(files(&dir.0, true) == before, "A01 file-mode denial changed files");
        if kind == 2 {
            invalid(ProductStore::create_a01_isolated_test_store(&dir.0, "fresh.db", config(), clock()));
            assert!(files(&dir.0, true) == before, "A01 directory-mode denial changed files");
        }
    }
}

#[test]
#[ignore = "NOT-RUN: nonowned private directory requires a separately provisioned ownership fixture"]
fn a01_nonowned_directory_is_rejected_when_constructible() {
    // Never change ownership or probe unrelated directories during this suite.
    // This selector remains explicitly NOT-RUN until custody supplies a fixture.
    panic!("A01 nonowned ownership fixture is not provisioned");
}

#[test]
fn a01_create_and_reopen_reject_each_sidecar_without_mutation() {
    for reopening in [false, true] {
        for suffix in ["-journal", "-wal", "-shm"] {
            for kind in 0..3 {
                let dir = PrivateDir::new();
                if reopening { drop(create(&dir)); drop(reopen(&dir)); }
                else { let control = PrivateDir::new(); drop(create(&control)); }
                let path = dir.0.join(format!("store.db{suffix}"));
                match kind {
                    0 => require(std::fs::write(&path, b"sidecar sentinel")),
                    1 => { require(std::fs::write(dir.0.join("target"), b"link sentinel")); require(symlink("target", &path)); }
                    _ => require(symlink("missing", &path)),
                }
                let before = files(&dir.0, true);
                if reopening { invalid(ProductStore::reopen_a01_isolated_test_store(&dir.0, "store.db", config(), clock())); }
                else { invalid(ProductStore::create_a01_isolated_test_store(&dir.0, "store.db", config(), clock())); }
                assert!(files(&dir.0, true) == before, "A01 sidecar denial changed files");
            }
        }
    }
}

#[test]
fn a01_reopen_rejects_marker_identity_schema_and_fk_damage_without_repair() {
    for (case, damage) in [
        ("product-meta-missing", "DELETE FROM product_meta_v1"),
        ("product-meta-version", "UPDATE product_meta_v1 SET schema_version=3"),
        ("product-meta-installation", "UPDATE product_meta_v1 SET installation_id='changed'"),
        ("product-meta-database", "UPDATE product_meta_v1 SET database_id='changed'"),
        ("review-meta-installation", "UPDATE product_review_meta_v1 SET installation_id='changed'"),
        ("review-meta-database", "UPDATE product_review_meta_v1 SET database_id='changed'"),
        ("review-table-missing", "DROP TABLE product_review_sessions_v1"),
        ("result-trigger-missing", "DROP TRIGGER product_result_bindings_no_update"),
        ("result-trigger-altered", "DROP TRIGGER product_result_bindings_no_update; CREATE TRIGGER product_result_bindings_no_update BEFORE UPDATE ON product_result_bindings_v1 BEGIN SELECT 1; END"),
        ("intake-meta-orphan", "INSERT INTO product_intake_delivery_meta_v1 VALUES('orphan','timestamp','unsupported')"),
    ] {
        let dir = PrivateDir::new();
        drop(create(&dir));
        drop(reopen(&dir)); // Otherwise identical successful control for every mutation.
        let path = dir.0.join("store.db");
        let conn = require_fixture(Connection::open(&path), case, "open");
        require_fixture(conn.pragma_update(None, "foreign_keys", 0), case, "disable-foreign-keys");
        require_fixture(conn.execute_batch(damage), case, "mutate");
        drop(conn);
        let before = preserved(&dir);
        let bytes = files(&dir.0, true);
        mismatch(ProductStore::reopen_a01_isolated_test_store(&dir.0, "store.db", config(), clock()));
        assert!(preserved(&dir) == before, "A01 malformed reopen repaired state");
        assert!(files(&dir.0, true) == bytes, "A01 pre-open contract denial changed files");
    }
    for column in ["installation_id", "database_id"] {
        let case = match column {
            "installation_id" => "result-meta-installation",
            "database_id" => "result-meta-database",
            _ => unreachable!(),
        };
        let dir = PrivateDir::new();
        drop(create(&dir));
        drop(reopen(&dir));
        let conn = require_fixture(Connection::open(dir.0.join("store.db")), case, "open");
        let trigger: String = require_fixture(conn.query_row(
            "SELECT sql FROM sqlite_schema WHERE name='product_result_meta_no_update'", [], |r| r.get(0)), case, "read-trigger");
        // Disposable corruption fixture: restore the identical trigger body so
        // this case varies only the result identity, not a schema definition.
        require_fixture(conn.execute_batch("DROP TRIGGER product_result_meta_no_update"), case, "drop-trigger");
        require_fixture(conn.execute(&format!("UPDATE product_result_meta_v1 SET {column}='changed'"), []), case, "mutate");
        require_fixture(conn.execute_batch(&trigger), case, "restore-trigger");
        drop(conn);
        let before = preserved(&dir);
        let bytes = files(&dir.0, true);
        mismatch(ProductStore::reopen_a01_isolated_test_store(&dir.0, "store.db", config(), clock()));
        assert!(preserved(&dir) == before && files(&dir.0, true) == bytes, "A01 result identity denial repaired state");
    }

    // Keep schema definitions intact while damaging metadata cardinality.
    // Parent deletion/movement and result singleton movement/insertion also damage FKs.
    for (table, columns) in [
        ("foundry_contract", "contract"),
        ("product_meta_v1", "installation_id,database_id,schema_version"),
        ("product_review_meta_v1", "installation_id,database_id,schema_version"),
        ("product_result_meta_v1", "installation_id,database_id,result_contract_version,legacy_approval_cutoff_seq"),
    ] {
        for damage in ["missing", "moved", "extra"] {
            let case = match (table, damage) {
                ("foundry_contract", "missing") => "contract-missing",
                ("foundry_contract", "moved") => "contract-moved",
                ("foundry_contract", "extra") => "contract-extra",
                ("product_meta_v1", "missing") => "product-meta-missing",
                ("product_meta_v1", "moved") => "product-meta-moved",
                ("product_meta_v1", "extra") => "product-meta-extra",
                ("product_review_meta_v1", "missing") => "review-meta-missing",
                ("product_review_meta_v1", "moved") => "review-meta-moved",
                ("product_review_meta_v1", "extra") => "review-meta-extra",
                ("product_result_meta_v1", "missing") => "result-meta-missing",
                ("product_result_meta_v1", "moved") => "result-meta-moved",
                ("product_result_meta_v1", "extra") => "result-meta-extra",
                _ => unreachable!(),
            };
            let damages_fk = matches!(
                (table, damage),
                ("product_meta_v1", "missing" | "moved")
                    | ("product_result_meta_v1", "moved" | "extra")
            );
            let dir = PrivateDir::new();
            drop(create(&dir));
            drop(reopen(&dir));
            let path = dir.0.join("store.db");
            let schema_before = require(logical(&path).remove("schema definitions").ok_or(()));
            let conn = require_fixture(Connection::open(&path), case, "open");
            let triggers: Vec<(String, String)> = {
                let mut st = require_fixture(conn.prepare(
                    "SELECT name,sql FROM sqlite_schema WHERE type='trigger' AND tbl_name=?1 ORDER BY name",
                ), case, "prepare-triggers");
                let rows = require_fixture(st.query_map([table], |row| Ok((row.get(0)?, row.get(1)?))), case, "read-triggers");
                require_fixture(rows.collect::<Result<Vec<_>, _>>(), case, "collect-triggers")
            };
            if damages_fk {
                require_fixture(conn.pragma_update(None, "foreign_keys", 0), case, "disable-foreign-keys");
            }
            require_fixture(conn.pragma_update(None, "ignore_check_constraints", 1), case, "disable-checks");
            let tx = require_fixture(conn.unchecked_transaction(), case, "begin");
            // Restore each original trigger verbatim before taking the corruption
            // baseline, so schema damage cannot cause the expected rejection.
            for (name, _) in &triggers {
                require_fixture(tx.execute_batch(&format!("DROP TRIGGER \"{}\"", name.replace('"', "\"\""))), case, "drop-trigger");
            }
            let sql = match damage {
                "missing" => format!("DELETE FROM {table}"),
                "moved" => format!("UPDATE {table} SET singleton=2"),
                "extra" => format!(
                    "INSERT INTO {table}(singleton,{columns}) SELECT 2,{columns} FROM {table} WHERE singleton=1",
                ),
                _ => unreachable!("A01 metadata damage case invalid"),
            };
            require_fixture(tx.execute_batch(&sql), case, "mutate");
            for (_, definition) in &triggers {
                require_fixture(tx.execute_batch(definition), case, "restore-trigger");
            }
            require_fixture(tx.commit(), case, "commit");
            require_fixture(conn.pragma_update(None, "ignore_check_constraints", 0), case, "restore-checks");
            if damages_fk {
                require_fixture(conn.pragma_update(None, "foreign_keys", 1), case, "restore-foreign-keys");
            }
            let checks: i64 = require_fixture(conn.query_row(
                "PRAGMA ignore_check_constraints", [], |row| row.get(0),
            ), case, "verify-checks");
            assert!(checks == 0, "A01 metadata corruption left CHECK enforcement disabled");
            let shape: (i64, i64) = require_fixture(conn.query_row(
                &format!("SELECT count(*),count(CASE WHEN singleton=1 THEN 1 END) FROM {table}"),
                [], |row| Ok((row.get(0)?, row.get(1)?)),
            ), case, "verify-cardinality");
            let expected = match damage {
                "missing" => (0, 0),
                "moved" => (1, 0),
                "extra" => (2, 1),
                _ => unreachable!("A01 metadata damage case invalid"),
            };
            assert!(shape == expected, "A01 metadata corruption was not established");
            let has_fk_damage = {
                let mut fk = require_fixture(conn.prepare("PRAGMA foreign_key_check"), case, "prepare-foreign-keys");
                require_fixture(require_fixture(fk.query([]), case, "read-foreign-keys").next(), case, "read-foreign-key-row").is_some()
            };
            assert!(
                has_fk_damage == damages_fk,
                "A01 fixture case={case} stage=verify-foreign-keys"
            );
            drop(conn);
            assert!(Some(&schema_before) == logical(&path).get("schema definitions"),
                "A01 metadata corruption changed schema definitions");
            let before = preserved(&dir);
            let bytes = files(&dir.0, true);
            mismatch(ProductStore::reopen_a01_isolated_test_store(
                &dir.0, "store.db", config(), clock(),
            ));
            assert!(before == preserved(&dir), "A01 metadata rejection repaired logical state");
            assert!(bytes == files(&dir.0, true), "A01 metadata rejection changed fixture bytes");
        }
    }
}

// Genuine setup and provider admission. The ONLY synthetic row change is the
// exact admitted backlog row's waiting_unsupported -> ready state. No A01 claim,
// acceptance, host result, output, bridge, tuple or receipt is ever seeded.
fn ready_scoped(store: &mut ProductStore, path: &Path) -> foundry_ledger::ProductServiceContext {
    use foundry_ledger::{ProductCapability, ProductServiceAction, ProductServiceGrant, ScopeGrantManifest, ScopeSourceManifest, RepositoryIdentity};
    use foundry_ledger::product_identity::ProductPasswordKdf;
    let source = ScopeSourceManifest {
        connector_id: "gh".into(), source_instance_id: "a01-source".into(), generation: 1,
        identity: require(RepositoryIdentity::canonicalized("acme", "GitHub", "https://api.github.com", "42")),
        api_owner: Some("acme".into()), api_repo: Some("project".into()),
        legacy_repository_locator: "acme/project".into(), scope_query: "open".into(), enabled: true,
    };
    let grant = ScopeGrantManifest { actor_id: "owner".into(), source_instance_id: source.source_instance_id.clone(), capabilities: vec![ProductCapability::ReadOrder] };
    require(store.bootstrap_owner_scoped("owner", &require(ProductPasswordKdf::hash("a01-private-fixture-password")), &[source], &[grant]));
    let session = require(store.login("owner", "a01-private-fixture-password", now())).digest().to_owned();
    let grants: Vec<_> = [ProductServiceAction::IntakeRead, ProductServiceAction::AdmitDelivery, ProductServiceAction::AckDelivery, ProductServiceAction::DriveReview]
        .into_iter().map(|action| ProductServiceGrant { source_instance_id: "a01-source".into(), action }).collect();
    require(store.manage_product_service(&session, "a01-service", &grants, true, now() + Duration::hours(3), 0, "configure", now()));
    let token = require(store.issue_product_service_credential(&session, "a01-service", 1, now() + Duration::hours(2), "credential", now()))
        .expose_once_to_file_writer().to_owned();
    let service = require(store.authenticate_product_service_for_action(&token, "gh", "https://api.github.com", "42", ProductServiceAction::DriveReview, now()));
    let updated = now().to_rfc3339();
    let version = foundry_ledger::product::delivery_version_digest("issue:a01", &updated, "dep-bump", "immutable task");
    let delivery = foundry_ledger::ProductDelivery {
        delivery_id: foundry_ledger::product::github_delivery_id(&config().installation_id, "gh", "https://api.github.com", "42", "issue:a01", &version),
        connector_id: "gh".into(), repository_id: "42".into(), issue_id: "issue:a01".into(),
        observed_updated_at: updated.clone(), version_digest: version, task_kind: "dep-bump".into(), payload: "immutable task".into(),
    };
    let cursor = foundry_ledger::ProductPollCursor { connector_id: "gh".into(), query_fingerprint: "a01-query".into(), sweep_id: "a01-sweep".into(), next_page: 1, page_ceiling: 1, updated_at: updated };
    require(store.persist_intake_page_for_service(&service, &cursor, &[delivery.clone()], &[], now()));
    let receipt = require(store.admit_intake_delivery_for_service(&service, &delivery.delivery_id, "a01-order", now()));
    require(store.mark_intake_acked_for_service(&service, &delivery.delivery_id, &receipt, now()));
    let view = require(store.read_scoped_order(&session, &receipt.order_id, now()));
    assert!(view.admission_sequence == receipt.sequence && view.source_instance_id == "a01-source", "A01 genuine admission control failed");
    let mut expected = logical(path);
    let rows = match expected.get_mut("product_work_backlog_v1") { Some(rows) => rows, None => panic!("A01 readiness table missing") };
    let row = match rows.iter_mut().find(|r| r[0] == Value::Text(receipt.order_id.clone()) && r[1] == Value::Integer(receipt.lifecycle) && r[2] == Value::Text(delivery.delivery_id.clone())) {
        Some(row) => row, None => panic!("A01 admitted readiness row missing"),
    };
    assert!(row[3] == Value::Text("waiting_unsupported".into()), "A01 readiness precondition mismatch");
    row[3] = Value::Text("ready".into());
    rows.sort_by_cached_key(|row| format!("{row:?}"));
    let changed = require(require(Connection::open(path)).execute(
        "UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id=?1 AND lifecycle=?2 AND delivery_id=?3 AND state='waiting_unsupported'",
        params![receipt.order_id, receipt.lifecycle, delivery.delivery_id]));
    assert!(changed == 1 && logical(path) == expected, "A01 readiness-only preparation mismatch");
    service
}

fn request(claim: &foundry_ledger::ProductWorkClaim) -> ReviewAdmissionRequest {
    use foundry_exec::{Attempt, Epoch, ExecutionIntent, Generation, OrderId, RunId};
    ReviewAdmissionRequest {
        intent: ExecutionIntent { order: OrderId(claim.order_id().into()), attempt: Attempt(1), run: RunId(4),
            activation: 1, generation: Generation(0), epoch: Epoch(1),
            external_ref: "review:a01-order:1:base..target".into(), recorded_at: now() },
        base_commit: "base".into(), result_commit: "target".into(), pack_digest: "a".repeat(64),
        product_work: Some(claim.review_binding()),
    }
}

fn ready_legacy(store: &mut ProductStore, path: &Path) {
    require(store.bind_source("gh", "github", "https://api.github.com", "42", "42"));
    let updated = now().to_rfc3339();
    let version = foundry_ledger::product::delivery_version_digest("issue:a01", &updated, "dep-bump", "immutable task");
    let delivery = foundry_ledger::ProductDelivery {
        delivery_id: foundry_ledger::product::github_delivery_id(&config().installation_id, "gh", "https://api.github.com", "42", "issue:a01", &version),
        connector_id: "gh".into(), repository_id: "42".into(), issue_id: "issue:a01".into(),
        observed_updated_at: updated.clone(), version_digest: version, task_kind: "dep-bump".into(), payload: "immutable task".into(),
    };
    let cursor = foundry_ledger::ProductPollCursor { connector_id: "gh".into(), query_fingerprint: "a01-query".into(), sweep_id: "a01-sweep".into(), next_page: 1, page_ceiling: 1, updated_at: updated };
    require(store.persist_intake_page(&cursor, &[delivery.clone()]));
    let receipt = require(store.admit_intake_delivery(&delivery.delivery_id, "a01-order", now()));
    let mut expected = logical(path);
    let rows = match expected.get_mut("product_work_backlog_v1") { Some(rows) => rows, None => panic!("A01 readiness table missing") };
    let row = match rows.iter_mut().find(|r| r[0] == Value::Text(receipt.order_id.clone()) && r[1] == Value::Integer(receipt.lifecycle) && r[2] == Value::Text(delivery.delivery_id.clone())) {
        Some(row) => row, None => panic!("A01 admitted readiness row missing"),
    };
    assert!(row[3] == Value::Text("waiting_unsupported".into()), "A01 readiness precondition mismatch");
    row[3] = Value::Text("ready".into());
    rows.sort_by_cached_key(|row| format!("{row:?}"));
    let changed = require(require(Connection::open(path)).execute(
        "UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id=?1 AND lifecycle=?2 AND delivery_id=?3 AND state='waiting_unsupported'",
        params![receipt.order_id, receipt.lifecycle, delivery.delivery_id]));
    assert!(changed == 1 && logical(path) == expected, "A01 legacy readiness-only preparation mismatch");
}

fn genuine_claim() -> (PrivateDir, ProductStore, foundry_ledger::ProductWorkClaim) {
    let dir = PrivateDir::new();
    let path = dir.0.join("store.db");
    let mut store = require(ProductStore::open_with_clock(&path, config(), clock()));
    let service = ready_scoped(&mut store, &path);
    let claim = match require(store.claim_product_work_for_service(&service, "worker", now(), Duration::minutes(5))) {
        foundry_ledger::ProductWorkClaimResult::Claimed(claim) => claim,
        _ => panic!("A01 normal-store claim control failed"),
    };
    require(store.validate_product_work_claim(&claim, now()));
    let request = request(&claim);
    require(<ProductStore as ReviewAdmissionStore>::validate_product_work_claim(&store, &claim.review_binding(), &request.intent, now()));
    (dir, store, claim)
}

#[test]
fn a01_legacy_and_scoped_claim_guards_are_nonvacuous_on_create_and_reopen() {
    let (_control_dir, _control, _claim) = genuine_claim();
    let legacy_dir = PrivateDir::new();
    let legacy_path = legacy_dir.0.join("store.db");
    let mut legacy = require(ProductStore::open_with_clock(&legacy_path, config(), clock()));
    ready_legacy(&mut legacy, &legacy_path);
    let legacy_claim = match require(legacy.claim_product_work("worker", now(), Duration::minutes(5))) {
        foundry_ledger::ProductWorkClaimResult::Claimed(claim) => claim,
        _ => panic!("A01 normal legacy claim control failed"),
    };
    require(legacy.validate_product_work_claim(&legacy_claim, now()));
    for reopening in [false, true] {
        let unscoped_dir = PrivateDir::new();
        let mut unscoped = create(&unscoped_dir);
        ready_legacy(&mut unscoped, &unscoped_dir.0.join("store.db"));
        if reopening { drop(unscoped); unscoped = reopen(&unscoped_dir); }
        let before = preserved(&unscoped_dir);
        assert!(matches!(unscoped.claim_product_work("worker", now(), Duration::minutes(5)), Err(ProductError::Invariant("A01 isolated store work is disabled"))), "A01 unscoped legacy guard mismatch");
        assert!(preserved(&unscoped_dir) == before && count(&unscoped_dir.0.join("store.db"), "product_work_claims_v1") == 0, "A01 unscoped legacy denial changed state");
        let dir = PrivateDir::new();
        let mut store = create(&dir);
        let path = dir.0.join("store.db");
        let service = ready_scoped(&mut store, &path);
        if reopening { drop(store); store = reopen(&dir); }
        let before = preserved(&dir);
        assert!(matches!(store.claim_product_work("worker", now(), Duration::minutes(5)), Err(ProductError::Invariant("A01 isolated store work is disabled"))), "A01 legacy guard mismatch");
        assert!(preserved(&dir) == before, "A01 legacy denial changed state");
        assert!(matches!(store.claim_product_work_for_service(&service, "worker", now(), Duration::minutes(5)), Err(ProductError::Invariant("A01 isolated store work is disabled"))), "A01 scoped guard mismatch");
        assert!(preserved(&dir) == before && count(&path, "product_work_claims_v1") == 0, "A01 claim denial changed state");
    }
    // Readiness is genuine except the explicitly approved state-only change.
    // This does not assert the unresolved full valid-A01-state S7 criterion.
}

#[test]
fn a01_foreign_claim_three_way_guard_precedence() {
    let (_origin_dir, origin, claim) = genuine_claim();
    let request = request(&claim);
    require(origin.validate_product_work_claim(&claim, now()));
    require(<ProductStore as ReviewAdmissionStore>::validate_product_work_claim(&origin, &claim.review_binding(), &request.intent, now()));
    let other_dir = PrivateDir::new();
    let other = require(ProductStore::open_with_clock(&other_dir.0.join("store.db"), config(), clock()));
    assert!(matches!(other.validate_product_work_claim(&claim, now()), Err(ProductError::Invariant("work claim is stale, expired, or no longer authorized"))), "A01 downstream ledger control mismatch");
    assert!(matches!(<ProductStore as ReviewAdmissionStore>::validate_product_work_claim(&other, &claim.review_binding(), &request.intent, now()), Err(HostError::Acceptance(ref message)) if message == "product work claim was reclaimed or its source changed"), "A01 downstream host control mismatch");
    for reopening in [false, true] {
        let dir = PrivateDir::new();
        let mut store = create(&dir);
        if reopening { drop(store); store = reopen(&dir); }
        let before = preserved(&dir);
        assert!(matches!(store.validate_product_work_claim(&claim, now()), Err(ProductError::Invariant("A01 isolated store work is disabled"))), "A01 ledger guard precedence mismatch");
        assert!(matches!(<ProductStore as ReviewAdmissionStore>::validate_product_work_claim(&store, &claim.review_binding(), &request.intent, now()), Err(HostError::Acceptance(ref message)) if message == "A01 isolated store claim validation is disabled"), "A01 host guard precedence mismatch");
        assert!(preserved(&dir) == before && count(&dir.0.join("store.db"), "product_work_claims_v1") == 0, "A01 foreign claim changed state");
    }
    // Foreign-claim precedence only. Full S7 valid-A01-state remains NOT-RUN.
}

#[test]
fn a01_host_claim_and_review_admission_guards_are_nonvacuous_on_create_and_reopen() {
    let (_origin_dir, mut origin, claim) = genuine_claim();
    let roots = PrivateDir::new();
    require(origin.configure_review_backend(foundry_ledger::ProductReviewConfig {
        artifact_root: roots.0.join("artifacts"), session_root: roots.0.join("sessions"),
        claim_root: roots.0.join("claims"), pack_digest: "a".repeat(64),
    }));
    let admission = require(<ProductStore as ReviewAdmissionStore>::admit_review_activation(&mut origin, request(&claim)));
    assert!(!admission.journal_run.is_empty(), "A01 ordinary admission control failed");
    for reopening in [false, true] {
        let dir = PrivateDir::new();
        let mut store = create(&dir);
        if reopening { drop(store); store = reopen(&dir); }
        let before = preserved(&dir);
        let req = request(&claim);
        assert!(matches!(<ProductStore as ReviewAdmissionStore>::validate_product_work_claim(&store, &claim.review_binding(), &req.intent, now()), Err(HostError::Acceptance(ref message)) if message == "A01 isolated store claim validation is disabled"), "A01 host claim guard mismatch");
        // No configured roots: the A01 guard must precede configuration lookup.
        assert!(matches!(<ProductStore as ReviewAdmissionStore>::admit_review_activation(&mut store, req), Err(HostError::Acceptance(ref message)) if message == "A01 isolated store review admission is disabled"), "A01 admission guard precedence mismatch");
        assert!(preserved(&dir) == before, "A01 host denial changed state");
    }
    // A genuine ordinary admission is the control, not a valid A01 admission.
}

#[test]
fn a01_failure_diagnostics_are_sanitized() {
    // Partial S8 evidence: Display and caught payload only. Actual panic-hook
    // output is not captured here. Stable Rust offers a process-global hook;
    // a child-process alternative needs separate custody review. Neither is
    // introduced into this fixture, so emitted-diagnostic coverage stays open.

    let dir = PrivateDir::new();
    let mut store = create(&dir);
    let marker = "private-a01-input-must-not-appear";
    let result = store.claim_product_work(marker, now(), Duration::minutes(5));
    let error = match result { Err(error) => error, Ok(_) => panic!("A01 denial missing") };
    let diagnostic = format!("{error}");
    assert!(diagnostic == "product store invariant failed: A01 isolated store work is disabled", "A01 diagnostic mismatch");
    assert!(!diagnostic.contains(marker) && !diagnostic.contains(&config().installation_id) && !diagnostic.contains(&config().database_id), "A01 diagnostic disclosed input");
    // Exercise the actual panic path and its installed hook using a fixed
    // message. Neither the panic payload nor the hook receives the error value.
    let payload = match std::panic::catch_unwind(|| require::<(), _>(Err(marker))) {
        Err(payload) => payload,
        Ok(_) => panic!("A01 panic control did not unwind"),
    };
    assert!(payload.downcast_ref::<&str>() == Some(&"A01 test operation failed"), "A01 panic payload mismatch");
}


fn foundation_schema_delta(
    marked: &BTreeMap<String, Vec<Vec<Value>>>,
    ordinary: &BTreeMap<String, Vec<Vec<Value>>>,
) {
    let definitions = |snapshot: &BTreeMap<String, Vec<Vec<Value>>>| {
        let rows = require(snapshot.get("schema definitions").ok_or(()));
        let mut definitions = BTreeMap::new();
        for row in rows {
            let name = match row.get(1) {
                Some(Value::Text(name)) => name.clone(),
                _ => panic!("A01 schema identity unavailable"),
            };
            assert!(definitions.insert(name, row.clone()).is_none(), "A01 duplicate schema definition");
        }
        definitions
    };
    let mut actual = definitions(marked);
    let ordinary = definitions(ordinary);
    let expected: [(&str, &str); 3] = [
        ("product_result_bindings_no_replace", r#"CREATE TRIGGER product_result_bindings_no_replace
BEFORE INSERT ON product_result_bindings_v1
WHEN EXISTS(
  SELECT 1
  FROM product_result_bindings_v1 r
  WHERE r.result_id=NEW.result_id
     OR (
       r.order_id=NEW.order_id
       AND r.lifecycle=NEW.lifecycle
       AND r.attempt=NEW.attempt
       AND r.run_id=NEW.run_id
       AND r.activation=NEW.activation
       AND r.generation=NEW.generation
     )
     OR (
       r.order_id=NEW.order_id
       AND r.attempt=NEW.attempt
       AND r.run_id=NEW.run_id
       AND r.generation=NEW.generation
     )
     OR (
       r.result_id=NEW.result_id
       AND r.binding_digest=NEW.binding_digest
       AND r.tuple_digest=NEW.tuple_digest
     )
)
BEGIN
  SELECT RAISE(ROLLBACK,'product result already exists');
END"#),
("product_result_bindings_no_legacy_tuple", r#"CREATE TRIGGER product_result_bindings_no_legacy_tuple
BEFORE INSERT ON product_result_bindings_v1
WHEN EXISTS(
  SELECT 1
  FROM product_result_legacy_tuples_v1 t
  WHERE t.order_id=NEW.order_id
    AND t.attempt=NEW.attempt
    AND t.run_id=NEW.run_id
    AND t.generation=NEW.generation
)
BEGIN
  SELECT RAISE(ROLLBACK,'legacy tuple cannot be adopted');
END"#),
("product_result_bindings_a01_deny_all", r#"CREATE TRIGGER product_result_bindings_a01_deny_all
BEFORE INSERT ON product_result_bindings_v1
BEGIN
  SELECT RAISE(ROLLBACK,'A01 result binding is disabled');
END"#)
    ];
    let tokens = |sql: &str| sql.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    for (index, (name, sql)) in expected.iter().enumerate() {
        let row = require(actual.remove(*name).ok_or(()));
        assert!(row == vec![
            Value::Text("trigger".into()), Value::Text((*name).into()),
            Value::Text("product_result_bindings_v1".into()), Value::Text((*sql).into()),
        ], "A01 foundation definition differs");
        if index < 2 {
            let original = require(ordinary.get(*name).ok_or(()));
            let original_sql = match original.get(3) {
                Some(Value::Text(sql)) => sql,
                _ => panic!("A01 ordinary definition unavailable"),
            };
            assert!(original_sql.contains("RAISE(ABORT"), "A01 ordinary rollback action changed");
            assert!(tokens(original_sql) == tokens(&sql.replace("RAISE(ROLLBACK", "RAISE(ABORT")),
                "A01 replacement changed ordinary predicate");
            actual.insert((*name).into(), original.clone());
        } else {
            assert!(!ordinary.contains_key(*name), "A01 denial leaked into ordinary schema");
        }
    }
    assert!(actual == ordinary, "A01 changed another schema definition");
}


#[test]
fn a01_binding_foundation_reopen_rejects_enforcement_tamper_without_repair() {
    for case in 0..12 {
        let dir = PrivateDir::new();
        drop(create(&dir));
        drop(reopen(&dir));
        let conn = require(Connection::open(dir.0.join("store.db")));
        let names = ["product_result_bindings_no_replace", "product_result_bindings_no_legacy_tuple", "product_result_bindings_a01_deny_all"];
        if case < 9 {
            let index = match case { 0..=2 => case, 3..=4 => case - 3,
                5..=6 => case - 5, 7 => 2, _ => 0 };
            let name = names[index];
            let mut sql: String = require(conn.query_row(
                "SELECT sql FROM sqlite_schema WHERE type='trigger' AND name=?1",
                params![name], |row| row.get(0),
            ));
            require(conn.execute_batch(&format!("DROP TRIGGER {name}")));
            if case >= 3 {
                sql = match case {
                    3..=4 => sql.replace("RAISE(ROLLBACK", "RAISE(ABORT"),
                    5..=6 => sql.replacen("WHEN", "WHEN 0 AND", 1),
                    7 => sql.replacen("BEGIN", "WHEN 1 BEGIN", 1),
                    _ => sql.replacen(name, &format!("{name}_changed"), 1),
                };
                require(conn.execute_batch(&sql));
            }
        } else {
            require(conn.execute_batch(match case {
                9 => "CREATE TRIGGER foundation_extra BEFORE INSERT ON product_result_bindings_v1 BEGIN SELECT 1; END",
                10 => "CREATE INDEX foundation_extra ON product_result_bindings_v1(result_id)",
                _ => "ALTER TABLE artifact_tuples ADD COLUMN foundation_extra TEXT",
            }));
        }
        require(conn.close());
        let before = preserved(&dir);
        mismatch(ProductStore::reopen_a01_isolated_test_store(
            &dir.0, "store.db", config(), clock(),
        ));
        assert!(preserved(&dir) == before, "A01 foundation rejection modified fixture");
    }
}

fn foundation_binding_census(store: &ProductStore) {
    assert_eq!(count(store.path(), "product_result_bindings_v1"), 0, "A01 foundation binding census mismatch");
}

#[test]
fn a01_binding_foundation_create_and_reopen_are_equivalent() {
    let dir = PrivateDir::new();
    let store = create(&dir);
    foundation_binding_census(&store);
    assert!(store.installation_id() == config().installation_id
        && store.database_id() == config().database_id, "A01 foundation create identity differs");
    drop(store);
    let before = preserved(&dir);
    let normal = PrivateDir::new();
    drop(require(ProductStore::open(&normal.0.join("ordinary.sqlite"), config())));
    foundation_schema_delta(&before.0, &logical(&normal.0.join("ordinary.sqlite")));
    let store = reopen(&dir);
    foundation_binding_census(&store);
    assert!(store.installation_id() == config().installation_id
        && store.database_id() == config().database_id, "A01 foundation reopen identity differs");
    drop(store);
    assert!(preserved(&dir) == before, "A01 foundation reopen changed baseline");
}


use foundry_ledger::product::{A01AuthorizeTaskRequest, A01RevokeTaskRequest, A01TaskAdmissionExpectation,
    A01TaskOwnerExpectation, A01TaskServiceExpectation, A01TaskStoreRef, A01TaskStoreIdentity, A01TaskOperation};

struct TaskClock(std::sync::Mutex<DateTime<Utc>>);
impl ProductClock for TaskClock { fn now(&self) -> DateTime<Utc> { *require(self.0.lock()) } }
impl TaskClock { fn set(&self, value: DateTime<Utc>) { *require(self.0.lock())=value; } }

// Transport is simulated. Admission, authentication and every positive write
// use the real ledger APIs. This fixture never seeds ready or claim evidence.
struct TaskClaimFixture {
    dir: PrivateDir,
    store: ProductStore,
    identity: A01TaskStoreIdentity,
    clock: Arc<TaskClock>,
    session: String,
    admin: String,
    service: foundry_ledger::ProductServiceContext,
    authorization: A01AuthorizeTaskRequest,
}
impl TaskClaimFixture {
    fn new() -> Self {
        use foundry_ledger::{ProductCapability, ProductServiceAction, ProductServiceGrant, ScopeGrantManifest, ScopeSourceManifest, RepositoryIdentity};
        use foundry_ledger::product_identity::ProductPasswordKdf;
        let dir=PrivateDir::new();
        let clock=Arc::new(TaskClock(std::sync::Mutex::new(now())));
        let (mut store,identity)=require(ProductStore::create_a01_task_claim_test_store(&dir.0,"store.db",config(),clock.clone()));
        let source=ScopeSourceManifest{connector_id:"gh".into(),source_instance_id:"task-source".into(),generation:1,
            identity:require(RepositoryIdentity::canonicalized("acme","GitHub","https://api.github.com","42")),
            api_owner:Some("acme".into()),api_repo:Some("project".into()),legacy_repository_locator:"acme/project".into(),scope_query:"open".into(),enabled:true};
        let grants=[ScopeGrantManifest{actor_id:"owner".into(),source_instance_id:"task-source".into(),capabilities:vec![ProductCapability::ReadOrder]}];
        require(store.bootstrap_owner_scoped("owner",&require(ProductPasswordKdf::hash("task-profile-fixture-password")),&[source],&grants));
        let session=require(store.login("owner","task-profile-fixture-password",now())).digest().to_owned();
        require(store.create_account(&session,"bridge-admin","owner",&require(ProductPasswordKdf::hash("task-admin-fixture-password")),now()));
        let admin=require(store.login("bridge-admin","task-admin-fixture-password",now())).digest().to_owned();
        let db=read_db(store.path());
        let scope_revision:i64=require(db.query_row("SELECT revision FROM product_scope_subject_revisions_v1 WHERE subject_id='owner' AND source_instance_id='task-source'",[],|r|r.get(0)));
        drop(db);
        require(store.change_product_scope(&admin,"owner","task-source",ProductCapability::A01BridgePassed,foundry_ledger::ScopeMutation::Grant,scope_revision,"bridge-grant",now()));
        let bridge_permission=require(require(store.read_a01_bridge_permission(&session,"task-source")).ok_or(()));
        let grants:Vec<_>=[ProductServiceAction::IntakeRead,ProductServiceAction::AdmitDelivery,ProductServiceAction::AckDelivery,ProductServiceAction::DriveReview]
            .into_iter().map(|action|ProductServiceGrant{source_instance_id:"task-source".into(),action}).collect();
        require(store.manage_product_service(&session,"task-service",&grants,true,now()+Duration::hours(4),0,"service",now()));
        let credential=require(store.issue_product_service_credential(&session,"task-service",1,now()+Duration::hours(3),"credential",now()));
        let service=require(store.authenticate_product_service_for_action(credential.expose_once_to_file_writer(),"gh","https://api.github.com","42",ProductServiceAction::DriveReview,now()));
        let payload=r#"{"issue_number":17,"title":"A bounded task","body":"original input"}"#;
        let updated=now().to_rfc3339();
        let version=foundry_ledger::product::delivery_version_digest("issue:17",&updated,"github-issue",payload);
        let delivery=foundry_ledger::ProductDelivery{delivery_id:foundry_ledger::product::github_delivery_id(&config().installation_id,"gh","https://api.github.com","42","issue:17",&version),connector_id:"gh".into(),repository_id:"42".into(),issue_id:"issue:17".into(),observed_updated_at:updated.clone(),version_digest:version,task_kind:"github-issue".into(),payload:payload.into()};
        let cursor=foundry_ledger::ProductPollCursor{connector_id:"gh".into(),query_fingerprint:"task-query".into(),sweep_id:"task-sweep".into(),next_page:1,page_ceiling:1,updated_at:updated};
        require(store.persist_intake_page_for_service(&service,&cursor,&[delivery.clone()],&[],now()));
        let receipt=require(store.admit_intake_delivery_for_service(&service,&delivery.delivery_id,"task-order",now()));
        require(store.mark_intake_acked_for_service(&service,&delivery.delivery_id,&receipt,now()));
        let view=require(store.read_scoped_order(&session,&receipt.order_id,now()));
        let db=read_db(store.path());
        let account_revision=require(db.query_row("SELECT revision FROM product_accounts_v1 WHERE actor_id='owner'",[],|r|r.get(0)));
        let (source_generation,source_instance_revision,connector_revision,provider,api_origin,repository_id,api_owner,api_repo):(i64,i64,i64,String,String,String,String,String)=require(db.query_row("SELECT i.generation,i.revision,s.revision,r.provider,r.api_origin,r.stable_repository_id,l.api_owner,l.api_repo FROM product_scope_source_instances_v1 i JOIN product_scope_repositories_v1 r USING(repository_key) JOIN product_scope_api_locators_v1 l USING(source_instance_id) JOIN product_sources_v1 s ON s.connector_id=i.connector_id WHERE i.source_instance_id='task-source'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))));
        let (service_revision,credential_revision,drive_review_grant_revision):(i64,i64,i64)=require(db.query_row("SELECT s.revision,c.credential_revision,g.grant_revision FROM product_services_v1 s JOIN product_service_credentials_v1 c USING(service_id) JOIN product_service_grants_v1 g USING(service_id) WHERE g.action='drive_review'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))));
        let raw_payload_digest=task_raw_digest(&delivery.task_kind,&delivery.payload);
        let authorization=A01AuthorizeTaskRequest{bridge_permission,request_key:"authorize".into(),expected_revision:0,
            admission:A01TaskAdmissionExpectation{store:A01TaskStoreRef{installation_id:config().installation_id,database_id:config().database_id},order_id:receipt.order_id,lifecycle:receipt.lifecycle,delivery_id:delivery.delivery_id,admission_seq:receipt.sequence,admission_hash:receipt.event_hash,raw_kind:delivery.task_kind,raw_payload_digest,source_instance_id:view.source_instance_id,source_generation,source_instance_revision,connector_id:"gh".into(),connector_revision,repository_key:view.repository_key,repository_provider:provider,api_origin,stable_repository_id:repository_id,api_owner,api_repo},
            owner:A01TaskOwnerExpectation{account_revision,scope_revision:view.scope_revision,read_order_grant_revision:view.grant_revision},
            service:A01TaskServiceExpectation{service_id:"task-service".into(),service_revision,credential_revision,drive_review_grant_revision,service_lease_until_ns:require((now()+Duration::hours(4)).timestamp_nanos_opt().ok_or(())),credential_expires_at_ns:require((now()+Duration::hours(3)).timestamp_nanos_opt().ok_or(()))},
            expires_at_ns:require((now()+Duration::hours(2)).timestamp_nanos_opt().ok_or(()))};
        drop(db);
        Self {dir,store,identity,clock,session,admin,service,authorization}
    }
    fn authorize(&mut self) -> foundry_ledger::product::A01TaskReceipt {
        require(self.store.authorize_a01_task(&self.session,&self.service,&self.authorization))
    }
    fn claim(&mut self) -> foundry_ledger::ProductWorkClaim {
        match require(self.store.claim_product_work_for_service(&self.service,"task-worker",now()-Duration::days(1),Duration::minutes(10))) {
            foundry_ledger::ProductWorkClaimResult::Claimed(c)=>c,_=>panic!("A01 task claim missing"),
        }
    }
    fn revoke_request(&self, revision: i64) -> A01RevokeTaskRequest {
        A01RevokeTaskRequest{request_key:format!("revoke-{revision}"),expected_revision:revision,admission:self.authorization.admission.clone(),owner:self.authorization.owner.clone()}
    }
    fn reopen(self) -> Self {
        let Self{dir,store,identity,clock,session,admin,service,authorization}=self;
        drop(store);
        let store=require(ProductStore::reopen_a01_task_claim_test_store(&dir.0,"store.db",config(),clock.clone(),&identity));
        Self{dir,store,identity,clock,session,admin,service,authorization}
    }
    fn empty(&mut self) {
        assert!(matches!(require(self.store.claim_product_work_for_service(&self.service,"another-worker",now(),Duration::minutes(5))),foundry_ledger::ProductWorkClaimResult::Empty),"A01 duplicate acquisition");
    }
}
fn task_raw_digest(kind: &str,payload: &str) -> String {
    use sha2::{Digest,Sha256};
    let mut h=Sha256::new();h.update(b"foundry-product-intake-v1\0");
    for bytes in [kind.as_bytes(),payload.as_bytes()] {h.update((bytes.len() as u64).to_be_bytes());h.update(bytes);}
    hex::encode(h.finalize())
}

// Matrix diagnostics deliberately expose only fixed labels, categories, and booleans.
fn task_matrix_result<T>(case: &'static str, result: Result<T, ProductError>) -> Result<T, ProductError> {
    let category = match &result {
        Ok(_) => "Ok",
        Err(ProductError::ContractMismatch) => "ContractMismatch",
        Err(ProductError::InvalidConfig) => "InvalidConfig",
        Err(ProductError::Invariant(_)) => "Invariant",
        Err(ProductError::Database(_)) => "Database",
    };
    eprintln!("a01_task_profile_cross_open_matrix case={case} result={category}");
    result
}

fn task_matrix_preconditions(case: &'static str, dir: &PrivateDir) {
    let metadata = std::fs::symlink_metadata(dir.0.join("store.db"));
    let metadata_known = metadata.is_ok();
    let mode_0600 = matches!(&metadata, Ok(metadata) if metadata.mode() & 0o7777 == 0o600);
    // False also covers an inspection error: absence must be positively established.
    let sidecar_absent = |suffix: &str| {
        matches!(
            std::fs::symlink_metadata(dir.0.join(format!("store.db{suffix}"))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        )
    };
    let wal_absent = sidecar_absent("-wal");
    let shm_absent = sidecar_absent("-shm");
    let journal_absent = sidecar_absent("-journal");
    eprintln!(
        "a01_task_profile_cross_open_matrix case={case} preconditions metadata_known={metadata_known} mode_0600={mode_0600} wal_absent={wal_absent} shm_absent={shm_absent} journal_absent={journal_absent}"
    );
}

#[test]
fn a01_task_profile_cross_open_matrix() {
    task_cross_profile_same_inode_and_metadata();
    let mut f=TaskClaimFixture::new();
    let before=preserved(&f.dir);
    let diagnostic_case = "task-to-foundation";
    task_matrix_preconditions(diagnostic_case, &f.dir);
    mismatch(task_matrix_result(diagnostic_case, ProductStore::reopen_a01_isolated_test_store(&f.dir.0,"store.db",config(),clock())));
    let diagnostic_case = "task-to-normal";
    task_matrix_preconditions(diagnostic_case, &f.dir);
    mismatch(task_matrix_result(diagnostic_case, ProductStore::open(f.store.path(),config())));
    assert!(before==preserved(&f.dir),"A01 cross open mutated task profile");
    let other=TaskClaimFixture::new();
    let diagnostic_case = "task-foreign-retained-identity";
    task_matrix_preconditions(diagnostic_case, &f.dir);
    mismatch(task_matrix_result(diagnostic_case, ProductStore::reopen_a01_task_claim_test_store(&f.dir.0,"store.db",config(),clock(),&other.identity)));
    let copy=PrivateDir::new();
    require(std::fs::copy(f.store.path(),copy.0.join("store.db")));
    require(std::fs::set_permissions(copy.0.join("store.db"),std::fs::Permissions::from_mode(0o600)));
    let diagnostic_case = "task-copy-retained-identity";
    task_matrix_preconditions(diagnostic_case, &copy);
    mismatch(task_matrix_result(diagnostic_case, ProductStore::reopen_a01_task_claim_test_store(&copy.0,"store.db",config(),clock(),&f.identity)));
    for profile in ["normal","foundation"] {
        let dir=PrivateDir::new();
        let mut store=if profile=="normal" {require(ProductStore::open_with_clock(&dir.0.join("store.db"),config(),clock()))} else {create(&dir)};
        let snapshot=preserved(&dir);
        let diagnostic_case = match profile { "normal" => "normal-authorize-task", _ => "foundation-authorize-task" };
        task_matrix_preconditions(diagnostic_case, &dir);
        assert!(matches!(task_matrix_result(diagnostic_case, store.authorize_a01_task(&f.session,&f.service,&f.authorization)),Err(ProductError::ContractMismatch)),"A01 non-task authorization exposed");
        let diagnostic_case = match profile { "normal" => "normal-revoke-task", _ => "foundation-revoke-task" };
        task_matrix_preconditions(diagnostic_case, &dir);
        assert!(matches!(task_matrix_result(diagnostic_case, store.revoke_a01_task(&f.session,&f.revoke_request(1))),Err(ProductError::ContractMismatch)),"A01 non-task revoke exposed");
        let diagnostic_case = match profile { "normal" => "normal-read-task-receipt", _ => "foundation-read-task-receipt" };
        task_matrix_preconditions(diagnostic_case, &dir);
        assert!(matches!(task_matrix_result(diagnostic_case, store.read_a01_task_receipt(&f.session,"authorize")),Err(ProductError::ContractMismatch)),"A01 non-task receipt exposed");
        let diagnostic_case = match profile { "normal" => "normal-read-task-claim", _ => "foundation-read-task-claim" };
        task_matrix_preconditions(diagnostic_case, &dir);
        assert!(matches!(task_matrix_result(diagnostic_case, store.read_a01_task_claim(&f.session,"task-order",1)),Err(ProductError::ContractMismatch)),"A01 non-task claim inspection exposed");
        let diagnostic_case = match profile { "normal" => "normal-to-task-foreign-retained-identity", _ => "foundation-to-task-foreign-retained-identity" };
        task_matrix_preconditions(diagnostic_case, &dir);
        mismatch(task_matrix_result(diagnostic_case, ProductStore::reopen_a01_task_claim_test_store(&dir.0,"store.db",config(),clock(),&f.identity)));
        assert!(snapshot==preserved(&dir),"A01 non-task denial mutated store");
    }
    #[cfg(feature="w1-isolated-test-support")]
    task_w1_cross_open(&f);
    f=f.reopen(); f.authorize(); f=f.reopen(); f.claim(); f=f.reopen(); f.empty();
    for (diagnostic_index, sql) in ["DROP TRIGGER product_a01_task_no_update","DROP TRIGGER product_a01_profile_no_delete; DELETE FROM product_a01_profile_v1"].into_iter().enumerate() {
        let f=TaskClaimFixture::new();
        require(require(Connection::open(f.store.path())).execute_batch(sql));
        let snapshot=preserved(&f.dir);let bytes=files(&f.dir.0,true);
        let diagnostic_case = ["task-trigger-removed", "task-profile-row-removed"][diagnostic_index];
        task_matrix_preconditions(diagnostic_case, &f.dir);
        mismatch(task_matrix_result(diagnostic_case, ProductStore::reopen_a01_task_claim_test_store(&f.dir.0,"store.db",config(),clock(),&f.identity)));
        assert!(snapshot==preserved(&f.dir) && bytes==files(&f.dir.0,true),"A01 task reopen repaired damage");
    }
}

#[test]
fn a01_task_authorize_ready_claim_is_atomic() {
    let mut f=TaskClaimFixture::new();let original=logical(f.store.path());
    assert!(count(f.store.path(),"product_a01_task_revisions_v1")==0,"A01 fixture seeded authority");
    let r=f.authorize();
    assert!(r.revision==1 && r.authorized_at_ns<r.expires_at_ns && r.expires_at_ns-r.authorized_at_ns>3_600_000_000_000,"A01 task grant duration changed");
    let ready=logical(f.store.path());
    let mut expected_backlog=original.get("product_work_backlog_v1").cloned().unwrap_or_else(||panic!("A01 original backlog missing"));
    assert!(expected_backlog.len()==1 && expected_backlog[0][3]==Value::Text("waiting_unsupported".into()) && expected_backlog[0][5]==Value::Integer(0),"A01 unsupported admission precondition missing");
    expected_backlog[0][3]=Value::Text("ready".into());
    assert!(ready.get("product_work_backlog_v1")==Some(&expected_backlog),"A01 authorization changed backlog beyond readiness");
    for table in ["ledger","product_orders_v1","product_intake_inbox_v1","product_order_payload_digests_v1","product_scope_order_provenance_v1"] {
        assert!(original.get(table)==ready.get(table),"A01 ready changed original admission");
    }
    let c=f.claim();
    assert!(c.task_kind()=="github-issue" && c.review_binding().payload_digest==r.admission.raw_payload_digest && c.payload_snapshot().contains("original input"),"A01 claim replaced original input");
    for table in ["product_work_claims_v1","product_service_work_bindings_v1","product_a01_claim_snapshots_v1"] {assert!(count(f.store.path(),table)==1,"A01 real claim evidence missing");}
    assert!(count(f.store.path(),"product_a01_task_revisions_v1")==1 && count(f.store.path(),"product_review_runs_v1")==0 && count(f.store.path(),"product_result_bindings_v1")==0,"A01 claim created downstream authority");
    f.empty();
}

#[test]
fn a01_task_source_revision_mismatch_denies_claim_without_mutation() {
    let mut control = TaskClaimFixture::new();
    control.authorize();
    control.claim();

    let sql = "UPDATE product_scope_source_instances_v1 SET revision=revision+1";
    let mut f=TaskClaimFixture::new();f.authorize();
    require(require(Connection::open(f.store.path())).execute_batch(sql));let before=preserved(&f.dir);
    assert!(f.store.claim_product_work_for_service(&f.service,"worker",now(),Duration::minutes(1)).is_err(),"A01 stale live authority accepted");
    assert!(before==preserved(&f.dir),"A01 live-authority denial changed state");
}

#[test]
fn a01_task_disabled_source_denies_claim_without_mutation() {
    let mut control = TaskClaimFixture::new();
    control.authorize();
    control.claim();

    let mut f=TaskClaimFixture::new();f.authorize();
    let case = "disabled-source";
    let conn = require_fixture(Connection::open(f.store.path()), case, "open");
    // Source disablement must advance the revision. This also leaves the
    // authorized context stale, so this case does not isolate the enabled check.
    // Authorization requires an enabled source; supported disablement cannot
    // produce disabled authority with the same revision as the ready receipt.
    let changed = require_fixture(conn.execute(
        "UPDATE product_scope_source_instances_v1 SET enabled=0,revision=revision+1 WHERE source_instance_id=?1",
        params![f.authorization.admission.source_instance_id],
    ), case, "setup-disable");
    assert!(changed == 1, "A01 disabled-source setup failed: category=affected-row-count");
    let (enabled, revision): (bool, i64) = require_fixture(conn.query_row(
        "SELECT enabled,revision FROM product_scope_source_instances_v1 WHERE source_instance_id=?1",
        params![f.authorization.admission.source_instance_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ), case, "setup-state");
    assert!(
        !enabled && revision == f.authorization.admission.source_instance_revision + 1,
        "A01 disabled-source setup failed: category=state"
    );
    drop(conn);
    let before=preserved(&f.dir);
    match f.store.claim_product_work_for_service(&f.service,"worker",now(),Duration::minutes(1)) {
        Err(ProductError::Invariant("A01 authorization unavailable")) => {}
        Err(ProductError::Database(error)) => {
            require_fixture::<()>(Err(error), case, "claim");
        }
        Err(ProductError::Invariant(_)) => panic!("A01 disabled-source claim failed: category=unexpected-invariant"),
        Err(ProductError::InvalidConfig) => panic!("A01 disabled-source claim failed: category=invalid-config"),
        Err(ProductError::ContractMismatch) => panic!("A01 disabled-source claim failed: category=contract-mismatch"),
        Ok(_) => panic!("A01 disabled-source claim failed: category=unexpected-success"),
    }
    assert!(before==preserved(&f.dir),"A01 live-authority denial changed state");
}

#[test]
fn a01_task_authority_and_original_input_denials() {
    let f=TaskClaimFixture::new();
    let mutations:Vec<Box<dyn Fn(&mut A01AuthorizeTaskRequest)>>=vec![
        Box::new(|r|r.bridge_permission.grant_id="different".into()),
        Box::new(|r|r.bridge_permission.grant_revision+=1),
        Box::new(|r|r.bridge_permission.subject_id="different".into()),
        Box::new(|r|r.bridge_permission.source_instance_id="different".into()),
        Box::new(|r|r.owner.account_revision+=1),Box::new(|r|r.owner.scope_revision+=1),Box::new(|r|r.owner.read_order_grant_revision+=1),
        Box::new(|r|r.service.service_revision+=1),Box::new(|r|r.service.credential_revision+=1),Box::new(|r|r.service.drive_review_grant_revision+=1),
        Box::new(|r|r.admission.source_instance_revision+=1),Box::new(|r|r.admission.source_generation+=1),Box::new(|r|r.admission.connector_revision+=1),
        Box::new(|r|r.admission.delivery_id="different".into()),Box::new(|r|r.admission.admission_seq+=1),Box::new(|r|r.admission.admission_hash="a".repeat(64)),
        Box::new(|r|r.admission.lifecycle+=1),Box::new(|r|r.admission.raw_kind="substituted".into()),Box::new(|r|r.admission.raw_payload_digest="b".repeat(64)),
        Box::new(|r|r.admission.store.database_id="different".into()),
    ];
    let mut f=f;
    for mutate in mutations {
        let mut r=f.authorization.clone();mutate(&mut r);let before=preserved(&f.dir);
        assert!(f.store.authorize_a01_task(&f.session,&f.service,&r).is_err(),"A01 substituted authority accepted");
        assert!(before==preserved(&f.dir),"A01 denial changed evidence");
    }
    let before=preserved(&f.dir);
    assert!(f.store.authorize_a01_task(&"0".repeat(64),&f.service,&f.authorization).is_err(),"A01 missing session accepted");
    assert!(before==preserved(&f.dir),"A01 missing session mutated store");
    for sql in ["UPDATE product_accounts_v1 SET enabled=0 WHERE actor_id='owner'","UPDATE product_accounts_v1 SET role='observer' WHERE actor_id='owner'","UPDATE product_accounts_v1 SET revision=revision+1 WHERE actor_id='owner'","UPDATE product_scope_subject_revisions_v1 SET revision=revision+1","UPDATE product_scope_grants_v1 SET grant_revision=grant_revision+1","UPDATE product_sources_v1 SET revision=revision+1","UPDATE product_services_v1 SET revision=revision+1","UPDATE product_service_credentials_v1 SET credential_revision=credential_revision+1","UPDATE product_service_grants_v1 SET grant_revision=grant_revision+1 WHERE action='drive_review'"] {
        let mut f=TaskClaimFixture::new();f.authorize();
        require(require(Connection::open(f.store.path())).execute_batch(sql));let before=preserved(&f.dir);
        assert!(f.store.claim_product_work_for_service(&f.service,"worker",now(),Duration::minutes(1)).is_err(),"A01 stale live authority accepted");
        assert!(before==preserved(&f.dir),"A01 live-authority denial changed state");
    }
    // A provider issue edit is another delivery, not another task input.
    let r=f.authorize();
    let updated=(now()+Duration::seconds(1)).to_rfc3339();let payload=r#"{"issue_number":17,"title":"edited","body":"edited input"}"#;
    let version=foundry_ledger::product::delivery_version_digest("issue:17",&updated,"github-issue",payload);
    let d=foundry_ledger::ProductDelivery{delivery_id:foundry_ledger::product::github_delivery_id(&config().installation_id,"gh","https://api.github.com","42","issue:17",&version),connector_id:"gh".into(),repository_id:"42".into(),issue_id:"issue:17".into(),observed_updated_at:updated.clone(),version_digest:version,task_kind:"github-issue".into(),payload:payload.into()};
    let cursor=foundry_ledger::ProductPollCursor{connector_id:"gh".into(),query_fingerprint:"task-query".into(),sweep_id:"edit-sweep".into(),next_page:1,page_ceiling:1,updated_at:updated};
    require(f.store.persist_intake_page_for_service(&f.service,&cursor,&[d.clone()],&[],now()));
    let edit=require(f.store.admit_intake_delivery_for_service(&f.service,&d.delivery_id,"ignored-order",now()));
    require(f.store.mark_intake_acked_for_service(&f.service,&d.delivery_id,&edit,now()));
    assert!(edit.order_id==r.admission.order_id,"A01 edit changed order identity");
    assert!(f.claim().payload_snapshot().contains("original input"),"A01 edit replaced original input");
}

#[test]
fn a01_task_replay_revoke_and_aba() {
    task_actor_namespace_and_rebind();
    let mut f=TaskClaimFixture::new();let first=f.authorize();let before=preserved(&f.dir);
    assert!(f.authorize()==first && before==preserved(&f.dir),"A01 replay wrote evidence");
    let mut changed=f.authorization.clone();changed.expires_at_ns+=1;
    assert!(matches!(f.store.authorize_a01_task(&f.session,&f.service,&changed),Err(ProductError::Invariant("A01 request conflict"))),"A01 changed request replay accepted");
    let revoke=f.revoke_request(1);let revoked=require(f.store.revoke_a01_task(&f.session,&revoke));
    assert!(revoked.operation==A01TaskOperation::Revoke && revoked.revision==2,"A01 revoke revision mismatch");f.empty();
    f.authorization.request_key="regrant".into();f.authorization.expected_revision=2;f.authorize();
    let c=f.claim();let before=preserved(&f.dir);
    f.empty();assert!(before==preserved(&f.dir),"A01 second claimer wrote evidence");
    let claim_evidence=logical(f.store.path());
    let revoke=f.revoke_request(3);require(f.store.revoke_a01_task(&f.session,&revoke));
    let revoked_evidence=logical(f.store.path());
    for table in ["product_work_claims_v1","product_service_work_bindings_v1","product_work_backlog_v1","product_a01_claim_snapshots_v1"] {
        assert!(claim_evidence.get(table)==revoked_evidence.get(table),"A01 revoke changed already claimed evidence");
    }
    f.authorization.request_key="after-claim".into();f.authorization.expected_revision=4;
    assert!(f.store.authorize_a01_task(&f.session,&f.service,&f.authorization).is_err(),"A01 regrant allowed second claim");
    f=f.reopen();f.clock.set(now()+Duration::minutes(20));f.empty();
    let v=require(f.store.read_a01_task_claim(&f.session,c.order_id(),c.lifecycle())).unwrap_or_else(||panic!("A01 historical claim missing"));
    assert!(v.fence==c.review_binding().fence && v.task_revision==3,"A01 historical snapshot changed");
    let mut f=TaskClaimFixture::new();let first=f.authorize();
    require(f.store.revoke_session(&f.session,now()));f.claim();
    let session=require(f.store.login("owner","task-profile-fixture-password",now())).digest().to_owned();f.session=session;
    require(require(Connection::open(f.store.path())).execute_batch("UPDATE product_scope_source_instances_v1 SET enabled=0,revision=revision+1"));
    let before=preserved(&f.dir);
    assert!(f.authorize()==first,"A01 historical replay required live source");
    assert!(require(f.store.read_a01_task_receipt(&f.session,"authorize"))==Some(first) && before==preserved(&f.dir),"A01 receipt inspection changed history");
    let revoke=f.revoke_request(1);require(f.store.revoke_a01_task(&f.session,&revoke));
    f=f.reopen();assert!(require(f.store.read_a01_task_claim(&f.session,"task-order",1)).is_some(),"A01 retired source hid immutable claim");
}

#[test]
fn a01_task_time_and_claim_inspection() {
    task_deadline_and_clock_edges();
    let mut f=TaskClaimFixture::new();f.authorize();
    f.clock.set(now()-Duration::nanoseconds(1));f.empty();
    f.clock.set(now()+Duration::hours(2));f.empty();
    f.clock.set(now());
    let before=preserved(&f.dir);
    assert!(matches!(f.store.claim_product_work_for_service(&f.service,"worker",now(),Duration::hours(1)+Duration::nanoseconds(1)),Err(ProductError::InvalidConfig)),"A01 excessive lease accepted");
    assert!(before==preserved(&f.dir),"A01 excessive lease changed state");
    let db=require(Connection::open(f.store.path()));
    require(db.execute("UPDATE product_work_backlog_v1 SET available_at=?1",["2026-01-01T10:00:00.000000001+10:00"]));drop(db);
    f.empty();f.clock.set(now()+Duration::nanoseconds(1));let c=f.claim();
    assert!(c.lease_until()==now()+Duration::minutes(10)+Duration::nanoseconds(1),"A01 clock precision lost");
    f=f.reopen();
    assert!(require(f.store.read_a01_task_receipt(&f.session,"authorize")).is_some(),"A01 uncertain response lost receipt");
    let v=require(f.store.read_a01_task_claim(&f.session,"task-order",1)).unwrap_or_else(||panic!("A01 uncertain response lost snapshot"));
    assert!(v.lease_until_text.ends_with("000000001Z"),"A01 lease canonical precision changed");
    let mut f=TaskClaimFixture::new();f.authorization.expires_at_ns=require((now()+Duration::seconds(7)).timestamp_nanos_opt().ok_or(()));f.authorize();
    assert!(f.claim().lease_until()==now()+Duration::seconds(7),"A01 task deadline did not shorten claim");
    for bad in ["2026-01-01T00:00:00.1234567890Z","2016-12-31T23:59:60Z","1969-12-31T23:59:59Z","2262-04-11T23:47:16.854775808Z","2026-01-01t00:00:00z"] {
        let mut f=TaskClaimFixture::new();
        require(require(Connection::open(f.store.path())).execute("UPDATE product_sessions_v1 SET expires_at=?1",[bad]));let before=preserved(&f.dir);
        assert!(f.store.authorize_a01_task(&f.session,&f.service,&f.authorization).is_err(),"A01 permissive session timestamp accepted");
        assert!(before==preserved(&f.dir),"A01 malformed session changed state");
    }
}

#[test]
fn a01_task_downstream_denials() {
    let mut f=TaskClaimFixture::new();f.authorize();let claim=f.claim();let before=preserved(&f.dir);
    assert!(matches!(f.store.validate_product_work_claim(&claim,now()),Err(ProductError::Invariant("A01 isolated store work is disabled"))),"A01 public validation authorized task execution");
    let req=request(&claim);
    assert!(matches!(<ProductStore as ReviewAdmissionStore>::validate_product_work_claim(&f.store,&claim.review_binding(),&req.intent,now()),Err(HostError::Acceptance(_))),"A01 host validation authorized execution");
    assert!(matches!(<ProductStore as ReviewAdmissionStore>::admit_review_activation(&mut f.store,req),Err(HostError::Acceptance(_))),"A01 review admission authorized execution");
    assert!(f.store.claim_product_work("legacy",now(),Duration::minutes(1)).is_err(),"A01 legacy acquisition authorized task");
    assert!(before==preserved(&f.dir),"A01 downstream denial changed evidence");
    let db=require(Connection::open(f.store.path()));
    require(db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA recursive_triggers=ON; BEGIN IMMEDIATE; UPDATE product_fence_counter_v1 SET next=next+1"));
    let columns:Vec<(String,String)>= {
        let mut st=require(db.prepare("PRAGMA table_info(product_result_bindings_v1)"));
        require(require(st.query_map([],|r|Ok((r.get(1)?,r.get(2)?)))).collect())
    };
    let names=columns.iter().map(|(n,_)|n.as_str()).collect::<Vec<_>>().join(",");
    let values=columns.iter().map(|(_,t)|if t=="INTEGER" {"1"} else {"'denied'"}).collect::<Vec<_>>().join(",");
    assert!(db.execute_batch(&format!("INSERT INTO product_result_bindings_v1({names}) VALUES({values})")).is_err() && db.is_autocommit(),"A01 binding guard failed to roll back");drop(db);
    assert!(before==preserved(&f.dir),"A01 binding denial committed unrelated write");
    let dir=PrivateDir::new();let foundation=create(&dir);assert!(count(foundation.path(),"product_result_bindings_v1")==0,"A01 foundation census changed");
}


#[cfg(feature="w1-isolated-test-support")]
fn task_w1_cross_open(f: &TaskClaimFixture) {
    for version in [2,3,4] {
        let dir=PrivateDir::new();
        let mut w=match version {
            2=>require(ProductStore::create_isolated_test_store(&dir.0,"store.db",config())),
            3=>require(ProductStore::create_w1_capture_test_store(&dir.0,"store.db",config(),clock())),
            _=>require(ProductStore::create_w1_pin_test_store(&dir.0,"store.db",config())),
        };
        let before=preserved(&dir);
        let diagnostic_case = match version { 2 => "w1-v2-to-task", 3 => "w1-v3-to-task", _ => "w1-v4-to-task" };
        task_matrix_preconditions(diagnostic_case, &dir);
        mismatch(task_matrix_result(diagnostic_case, ProductStore::reopen_a01_task_claim_test_store(&dir.0,"store.db",config(),clock(),&f.identity)));
        let diagnostic_case = match version { 2 => "w1-v2-to-foundation", 3 => "w1-v3-to-foundation", _ => "w1-v4-to-foundation" };
        task_matrix_preconditions(diagnostic_case, &dir);
        mismatch(task_matrix_result(diagnostic_case, ProductStore::reopen_a01_isolated_test_store(&dir.0,"store.db",config(),clock())));
        let diagnostic_case = match version { 2 => "w1-v2-authorize-task", 3 => "w1-v3-authorize-task", _ => "w1-v4-authorize-task" };
        task_matrix_preconditions(diagnostic_case, &dir);
        assert!(matches!(task_matrix_result(diagnostic_case, w.authorize_a01_task(&f.session,&f.service,&f.authorization)),Err(ProductError::ContractMismatch)),"A01 task API exposed on W1");
        assert!(before==preserved(&dir),"A01 W1 refusal mutated source");
        let before=preserved(&f.dir);
        let diagnostic_case = match version { 2 => "w1-v2-from-task", 3 => "w1-v3-from-task", _ => "w1-v4-from-task" };
        task_matrix_preconditions(diagnostic_case, &f.dir);
        let result=match version {
            2=>ProductStore::reopen_isolated_test_store(&f.dir.0,"store.db",config()),
            3=>ProductStore::reopen_w1_capture_test_store(&f.dir.0,"store.db",config(),clock()),
            _=>ProductStore::reopen_w1_pin_test_store(&f.dir.0,"store.db",config()),
        };
        mismatch(task_matrix_result(diagnostic_case, result));assert!(before==preserved(&f.dir),"A01 W1 refusal mutated task store");
    }
}


fn task_actor_namespace_and_rebind() {
    use foundry_ledger::{ProductCapability,ScopeMutation,RepositoryIdentity};
    use foundry_ledger::product_identity::ProductPasswordKdf;
    let mut f=TaskClaimFixture::new();let first=f.authorize();
    require(f.store.create_account(&f.session,"second-owner","owner",&require(ProductPasswordKdf::hash("second-task-fixture-password")),now()));
    let grant=require(f.store.change_product_scope(&f.session,"second-owner","task-source",ProductCapability::ReadOrder,ScopeMutation::Grant,0,"second-read",now()));
    let second=require(f.store.login("second-owner","second-task-fixture-password",now())).digest().to_owned();
    assert!(require(f.store.read_a01_task_receipt(&second,"authorize")).is_none(),"A01 actor guessed another receipt");
    let second_principal=require(f.store.resolve_session(&second,now()));
    let revoke=A01RevokeTaskRequest{request_key:"authorize".into(),expected_revision:1,admission:f.authorization.admission.clone(),owner:A01TaskOwnerExpectation{account_revision:second_principal.authority_revision,scope_revision:grant.scope_revision,read_order_grant_revision:grant.grant_revision}};
    let second_receipt=require(f.store.revoke_a01_task(&second,&revoke));
    assert!(second_receipt.actor_id!=first.actor_id && second_receipt.request_key==first.request_key,"A01 actor-local key failed");
    assert!(require(f.store.read_a01_task_receipt(&second,"authorize"))==Some(second_receipt),"A01 actor-local lookup returned wrong receipt");
    assert!(require(f.store.read_a01_task_receipt(&f.session,"authorize"))==Some(first),"A01 second actor overwrote first receipt");
    let mut f=TaskClaimFixture::new();let first=f.authorize();
    require(f.store.configure_scoped_source(&f.session,"gh",require(RepositoryIdentity::canonicalized("acme","GitHub","https://api.github.com","42")),"acme","project","acme/project","open",false,1,"retire",now()));
    require(f.store.configure_scoped_source(&f.session,"gh",require(RepositoryIdentity::canonicalized("acme","GitHub","https://api.github.com","99")),"acme","replacement","acme/replacement","open",true,2,"rebind",now()));
    let before=preserved(&f.dir);assert!(f.authorize()==first && before==preserved(&f.dir),"A01 historical replay required current binding");
    let r=f.revoke_request(1);require(f.store.revoke_a01_task(&f.session,&r));
    let f=f.reopen();assert!(count(f.store.path(),"product_a01_task_revisions_v1")==2,"A01 rebind history did not reopen");
    let mut f=TaskClaimFixture::new();let first=f.authorize();
    let revoke=require(f.store.change_product_scope(&f.session,"owner","task-source",ProductCapability::ReadOrder,ScopeMutation::Revoke,f.authorization.owner.scope_revision,"hide-read",now()));
    assert!(require(f.store.read_a01_task_receipt(&f.session,"authorize")).is_none(),"A01 receipt ignored current visibility");
    require(f.store.change_product_scope(&f.admin,"owner","task-source",ProductCapability::ReadOrder,ScopeMutation::Grant,revoke.scope_revision,"restore-read",now()));
    let before=preserved(&f.dir);assert!(f.authorize()==first && before==preserved(&f.dir),"A01 historical replay required captured scope revisions");
    assert!(f.store.claim_product_work_for_service(&f.service,"worker",now(),Duration::minutes(1)).is_err(),"A01 revoked/regranted owner scope restored stale task");
}
fn task_deadline_and_clock_edges() {
    for value in ["1969-12-31T23:59:59Z","2263-01-01T00:00:00Z","2016-12-31T23:59:60Z"] {
        let mut f=TaskClaimFixture::new();f.clock.set(require(DateTime::parse_from_rfc3339(value)).with_timezone(&Utc));let before=preserved(&f.dir);
        assert!(f.store.authorize_a01_task(&f.session,&f.service,&f.authorization).is_err(),"A01 invalid clock authorized task");assert!(before==preserved(&f.dir),"A01 invalid clock changed state");
    }
    let mut f=TaskClaimFixture::new();f.authorization.expires_at_ns=require((now()+Duration::hours(4)).timestamp_nanos_opt().ok_or(()));f.authorize();
    f.clock.set(now()+Duration::hours(3));let before=preserved(&f.dir);
    assert!(f.store.claim_product_work_for_service(&f.service,"worker",now(),Duration::minutes(10)).is_err(),"A01 credential expiry equality accepted");assert!(before==preserved(&f.dir),"A01 expired credential wrote claim");
    f.clock.set(now()+Duration::hours(3)-Duration::minutes(1));
    assert!(f.claim().lease_until()==now()+Duration::hours(3),"A01 credential deadline did not shorten lease");
    let mut f=TaskClaimFixture::new();let end=now()+Duration::minutes(2);
    require(require(Connection::open(f.store.path())).execute("UPDATE product_services_v1 SET lease_until=?1",[end.to_rfc3339_opts(chrono::SecondsFormat::Nanos,true)]));
    f.authorization.service.service_lease_until_ns=require(end.timestamp_nanos_opt().ok_or(()));f.authorize();
    assert!(f.claim().lease_until()==end,"A01 service deadline did not shorten lease");
    let mut f=TaskClaimFixture::new();
    require(require(Connection::open(f.store.path())).execute("UPDATE product_sessions_v1 SET expires_at=?1",[now().to_rfc3339()]));
    let before=preserved(&f.dir);assert!(f.store.authorize_a01_task(&f.session,&f.service,&f.authorization).is_err(),"A01 session expiry equality accepted");assert!(before==preserved(&f.dir),"A01 expired session changed state");
    let mut f=TaskClaimFixture::new();f.authorize();let end=now()+Duration::hours(4);
    f.clock.set(end);let before=preserved(&f.dir);
    assert!(f.store.claim_product_work_for_service(&f.service,"worker",now(),Duration::minutes(1)).is_err(),"A01 service expiry equality accepted");assert!(before==preserved(&f.dir),"A01 expired service changed state");
}


fn task_cross_profile_same_inode_and_metadata() {
    // Keeping the target inode forces profile validation to be exercised after
    // the retained physical-identity check, rather than only testing that check.
    #[cfg(feature="w1-isolated-test-support")]
    let profiles=vec!["normal","foundation",
        "w1-v2",
        "w1-v3",
        "w1-v4",
    ];
    #[cfg(not(feature="w1-isolated-test-support"))]
    let profiles=vec!["normal","foundation",
    ];
    for profile in profiles {
        let foreign=PrivateDir::new();
        let foreign_store=match profile {
            "normal"=>require(ProductStore::open_with_clock(&foreign.0.join("store.db"),config(),clock())),
            "foundation"=>create(&foreign),
            #[cfg(feature="w1-isolated-test-support")]
            "w1-v2"=>require(ProductStore::create_isolated_test_store(&foreign.0,"store.db",config())),
            #[cfg(feature="w1-isolated-test-support")]
            "w1-v3"=>require(ProductStore::create_w1_capture_test_store(&foreign.0,"store.db",config(),clock())),
            #[cfg(feature="w1-isolated-test-support")]
            "w1-v4"=>require(ProductStore::create_w1_pin_test_store(&foreign.0,"store.db",config())),
            _=>panic!("A01 profile fixture case invalid"),
        };
        drop(foreign_store);let bytes=require(std::fs::read(foreign.0.join("store.db")));
        let f=TaskClaimFixture::new();let TaskClaimFixture{dir,store,identity,clock,..}=f;drop(store);
        require(std::fs::write(dir.0.join("store.db"),bytes));
        let before=preserved(&dir);let bytes=files(&dir.0,true);
        let diagnostic_case = match profile { "normal" => "normal-to-task-same-inode", "foundation" => "foundation-to-task-same-inode", "w1-v2" => "w1-v2-to-task-same-inode", "w1-v3" => "w1-v3-to-task-same-inode", _ => "w1-v4-to-task-same-inode" };
        task_matrix_preconditions(diagnostic_case, &dir);
        mismatch(task_matrix_result(diagnostic_case, ProductStore::reopen_a01_task_claim_test_store(&dir.0,"store.db",config(),clock,&identity)));
        assert!(before==preserved(&dir) && bytes==files(&dir.0,true),"A01 same-inode cross-profile rejection mutated fixture");
    }
    for (diagnostic_index, sql) in ["UPDATE product_a01_profile_v1 SET version=2","UPDATE product_a01_profile_v1 SET profile='unknown'","UPDATE product_a01_profile_v1 SET installation_id='other'","UPDATE product_a01_profile_v1 SET database_id='other'","UPDATE product_a01_profile_v1 SET singleton=2","INSERT INTO product_a01_profile_v1 SELECT 2,version,profile,installation_id,database_id FROM product_a01_profile_v1"].into_iter().enumerate() {
        let f=TaskClaimFixture::new();let mut db=require(Connection::open(f.store.path()));
        require(db.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA ignore_check_constraints=ON"));
        let triggers:Vec<(String,String)>={let mut st=require(db.prepare("SELECT name,sql FROM sqlite_schema WHERE type='trigger' AND tbl_name='product_a01_profile_v1'"));require(require(st.query_map([],|r|Ok((r.get(0)?,r.get(1)?)))).collect())};
        let tx=require(db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate));
        for (name,_) in &triggers {require(tx.execute_batch(&format!("DROP TRIGGER \"{name}\"")));}
        require(tx.execute_batch(sql));for (_,ddl) in &triggers {require(tx.execute_batch(ddl));}require(tx.commit());drop(db);
        let before=preserved(&f.dir);let bytes=files(&f.dir.0,true);
        let diagnostic_case = ["task-metadata-version-changed", "task-metadata-profile-changed", "task-metadata-installation-changed", "task-metadata-database-changed", "task-metadata-singleton-moved", "task-metadata-second-singleton"][diagnostic_index];
        task_matrix_preconditions(diagnostic_case, &f.dir);
        mismatch(task_matrix_result(diagnostic_case, ProductStore::reopen_a01_task_claim_test_store(&f.dir.0,"store.db",config(),f.clock.clone(),&f.identity)));
        assert!(before==preserved(&f.dir) && bytes==files(&f.dir.0,true),"A01 altered profile metadata was repaired");
    }
}
