#![cfg(feature = "w1-isolated-test-support")]

use foundry_ledger::{
    ProductCapability, ProductConfig, ProductStore, RepositoryIdentity, ScopeGrantManifest,
    ScopeSourceManifest,
};
use rusqlite::{Connection, types::Value};
use std::{
    collections::BTreeMap,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct PrivateDir(PathBuf);

impl PrivateDir {
    fn new() -> Self {
        let base = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .expect("TMPDIR must name the configured private scratch directory");
        assert_ne!(base, Path::new("/tmp"), "tests must use configured scratch");
        let path = base.join(format!(
            "b2-w1-selection-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock before epoch")
                .as_nanos()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .expect("create private W1 fixture directory");
        Self(path)
    }
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

type Snapshot = BTreeMap<String, Vec<Vec<Value>>>;

fn logical_snapshot(path: &Path) -> Snapshot {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open W1 fixture snapshot");
    let mut tables = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .expect("list W1 tables");
    let names: Vec<String> = tables
        .query_map([], |row| row.get(0))
        .expect("query W1 table names")
        .map(Result::unwrap)
        .collect();
    names
        .into_iter()
        .map(|name| {
            let escaped = name.replace('"', "\"\"");
            let mut statement = conn
                .prepare(&format!("SELECT * FROM \"{escaped}\""))
                .expect("prepare W1 table snapshot");
            let columns = statement.column_count();
            let mut rows: Vec<Vec<Value>> = statement
                .query_map([], |row| (0..columns).map(|index| row.get(index)).collect())
                .expect("query W1 rows")
                .map(Result::unwrap)
                .collect();
            rows.sort_by_cached_key(|row| format!("{row:?}"));
            (name, rows)
        })
        .collect()
}

fn assert_snapshot_unchanged(before: &Snapshot, path: &Path) {
    let after = logical_snapshot(path);
    assert!(before == &after, "logical ProductStore snapshot changed");
}

fn config() -> ProductConfig {
    ProductConfig {
        installation_id: "b2-w1-install".into(),
        database_id: "b2-w1-database".into(),
    }
}

fn fixture() -> (
    PrivateDir,
    ProductStore,
    String,
    chrono::DateTime<chrono::Utc>,
) {
    let dir = PrivateDir::new();
    let mut store = ProductStore::create_isolated_test_store(&dir.0, "product.sqlite", config())
        .expect("create isolated W1 ProductStore");
    let source = ScopeSourceManifest {
        connector_id: "github-primary".into(),
        source_instance_id: "source-one".into(),
        generation: 1,
        identity: RepositoryIdentity::canonicalized(
            "acme",
            "GitHub",
            "https://api.github.com",
            "repo:1",
        )
        .expect("canonicalize fixture repository"),
        api_owner: Some("acme".into()),
        api_repo: Some("project".into()),
        legacy_repository_locator: "acme/project".into(),
        scope_query: "open".into(),
        enabled: true,
    };
    let owner_grant = ScopeGrantManifest {
        actor_id: "owner".into(),
        source_instance_id: source.source_instance_id.clone(),
        capabilities: vec![ProductCapability::ReadOrder],
    };
    let owner_phc = foundry_ledger::product_identity::ProductPasswordKdf::hash("owner-password")
        .expect("hash test-only owner password");
    store
        .bootstrap_owner_scoped("owner", &owner_phc, &[source], &[owner_grant])
        .expect("bootstrap owner and source");
    let now = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .expect("parse fixed fixture time")
        .with_timezone(&chrono::Utc);
    let owner_session = store
        .login("owner", "owner-password", now)
        .expect("login fixture owner")
        .digest()
        .to_owned();
    for (order, issue) in [("order-one", "issue:1"), ("order-two", "issue:2")] {
        store
            .admit_order(
                order,
                "github-primary",
                "repo:1",
                issue,
                "dep-bump",
                "fixture payload",
                now,
            )
            .expect("admit fixture order");
    }
    (dir, store, owner_session, now)
}

fn second_actor(
    store: &mut ProductStore,
    owner_session: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    use foundry_ledger::ScopeMutation;
    let phc = foundry_ledger::product_identity::ProductPasswordKdf::hash("reviewer-password")
        .expect("hash test-only reviewer password");
    store
        .create_account(owner_session, "reviewer", "approver", &phc, now)
        .expect("create non-owner actor");
    store
        .change_product_scope(
            owner_session,
            "reviewer",
            "source-one",
            ProductCapability::ReadOrder,
            ScopeMutation::Grant,
            0,
            "grant-reviewer-read",
            now,
        )
        .expect("grant scoped order read");
    store
        .login("reviewer", "reviewer-password", now)
        .expect("login non-owner actor")
        .digest()
        .to_owned()
}

#[test]
fn p01_selection_control_and_one_axis_denials_preserve_the_database() {
    // Observation layer: actual isolated ProductStore and its logical SQLite rows.
    let (dir, mut store, owner, now) = fixture();
    let path = dir.0.join("product.sqlite");
    let other_actor = second_actor(&mut store, &owner, now);
    let x = "a".repeat(64);
    let y = "b".repeat(64);

    let approval = store
        .w1_change_pack_approval(&owner, "source-one", &x, 0, true, "approve-x", now)
        .expect("owner approval control");
    assert_eq!(
        (approval.expected_revision, approval.result_revision),
        (0, 1)
    );
    let selection = store
        .w1_select_pack(&owner, "order-one", 1, "source-one", &x, 0, "select-x", now)
        .expect("approved selection control");
    assert_eq!(
        (selection.expected_revision, selection.result_revision),
        (0, 1)
    );
    assert_eq!(selection.order_id.as_deref(), Some("order-one"));

    // Each negative changes one input from the now-established normal control.
    let before = logical_snapshot(&path);
    assert!(
        store
            .w1_change_pack_approval(
                &other_actor,
                "source-one",
                &x,
                1,
                true,
                "reviewer-approval",
                now
            )
            .is_err()
    );
    assert_snapshot_unchanged(&before, &path);

    let before = logical_snapshot(&path);
    assert!(
        store
            .w1_select_pack(
                &owner,
                "order-one",
                1,
                "source-one",
                &y,
                0,
                "unapproved-y",
                now
            )
            .is_err()
    );
    assert_snapshot_unchanged(&before, &path);

    let before = logical_snapshot(&path);
    assert!(
        store
            .w1_select_pack(
                &owner,
                "order-one",
                99,
                "source-one",
                &x,
                0,
                "absent-lifecycle",
                now
            )
            .is_err()
    );
    assert_snapshot_unchanged(&before, &path);
}

#[test]
fn p01b_retry_is_actor_bound_and_preserves_historical_receipts() {
    // Observation layer: actual isolated ProductStore, receipt values and logical SQLite rows.
    let (dir, mut store, owner, now) = fixture();
    let path = dir.0.join("product.sqlite");
    let reviewer = second_actor(&mut store, &owner, now);
    let x = "c".repeat(64);
    let y = "d".repeat(64);
    let z = "e".repeat(64);

    let approval_x = store
        .w1_change_pack_approval(&owner, "source-one", &x, 0, true, "approval-x", now)
        .expect("owner approval control");
    assert_eq!(approval_x.result_revision, 1);
    let before_retry = logical_snapshot(&path);
    let approval_retry = store
        .w1_change_pack_approval(&owner, "source-one", &x, 0, true, "approval-x", now)
        .expect("exact approval retry returns original receipt");
    assert!(
        approval_retry == approval_x,
        "exact retry returned a different receipt"
    );
    assert_snapshot_unchanged(&before_retry, &path);

    let before = logical_snapshot(&path);
    assert!(
        store
            .w1_change_pack_approval(
                &reviewer,
                "source-one",
                &x,
                0,
                true,
                "reviewer-new-request",
                now
            )
            .is_err()
    );
    assert_snapshot_unchanged(&before, &path);

    let before = logical_snapshot(&path);
    assert!(
        store
            .w1_change_pack_approval(&owner, "source-one", &x, 0, false, "approval-x", now)
            .is_err()
    );
    assert_snapshot_unchanged(&before, &path);

    let selected_x = store
        .w1_select_pack(&owner, "order-one", 1, "source-one", &x, 0, "select-x", now)
        .expect("select X control");
    assert_eq!(selected_x.result_revision, 1);
    let before_select_retry = logical_snapshot(&path);
    let selected_retry = store
        .w1_select_pack(&owner, "order-one", 1, "source-one", &x, 0, "select-x", now)
        .expect("same actor exact selection retry returns original receipt");
    assert!(
        selected_retry == selected_x,
        "exact selection retry changed receipt"
    );
    assert_snapshot_unchanged(&before_select_retry, &path);

    store
        .w1_change_pack_approval(&owner, "source-one", &y, 0, true, "approval-y", now)
        .expect("approve replacement pack");
    let selected_y = store
        .w1_select_pack(&owner, "order-one", 1, "source-one", &y, 1, "select-y", now)
        .expect("owner moves order from X to Y at current selection revision");
    assert_eq!(
        (selected_y.expected_revision, selected_y.result_revision),
        (1, 2)
    );

    let approved_z = store
        .w1_change_pack_approval(&owner, "source-one", &z, 0, true, "approve-z-1", now)
        .expect("approve Z revision one");
    let revoked_z = store
        .w1_change_pack_approval(&owner, "source-one", &z, 1, false, "revoke-z", now)
        .expect("revoke Z");
    let reapproved_z = store
        .w1_change_pack_approval(&owner, "source-one", &z, 2, true, "approve-z-2", now)
        .expect("reapprove Z at a new revision");
    assert_eq!(
        (
            approved_z.result_revision,
            revoked_z.result_revision,
            reapproved_z.result_revision
        ),
        (1, 2, 3)
    );
    let before_historical_retry = logical_snapshot(&path);
    let historical = store
        .w1_change_pack_approval(&owner, "source-one", &z, 0, true, "approve-z-1", now)
        .expect("historical exact retry returns its original receipt");
    assert!(
        historical == approved_z,
        "historical receipt was not preserved"
    );
    assert_snapshot_unchanged(&before_historical_retry, &path);
}
