#![allow(clippy::type_complexity)] // W6a clippy debt: preserve this existing test/example logic, assertions, and cell sequence

use chrono::{Duration, Utc};
use foundry_ledger::product_identity::ProductPasswordKdf;
use foundry_ledger::{
    ProductCapability, ProductConfig, ProductStore, RepositoryIdentity, ScopeGrantManifest,
    ScopeMigrationManifest, ScopeMutation, ScopeSourceManifest, VerifiedOrderProvenance,
};
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};

fn scratch(label: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "foundry-b1-scope-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("product.sqlite");
    (root, db)
}

fn config() -> ProductConfig {
    ProductConfig {
        installation_id: "scope-test-install".into(),
        database_id: "scope-test-db".into(),
    }
}

fn identity(org: &str, origin: &str, stable_id: &str) -> RepositoryIdentity {
    RepositoryIdentity::canonicalized(org, "GitHub", origin, stable_id).unwrap()
}

fn owner(path: &Path, now: chrono::DateTime<Utc>) -> (ProductStore, String) {
    let mut store = ProductStore::open(path, config()).unwrap();
    let phc = ProductPasswordKdf::hash("owner-password-strong").unwrap();
    store.bootstrap_owner("owner", &phc).unwrap();
    let session = store.login("owner", "owner-password-strong", now).unwrap();
    (store, session.digest().to_owned())
}

#[test]
fn explicit_bootstrap_enforces_even_zero_grants_and_accepts_multiple_capabilities() {
    let (root, empty_path) = scratch("zero-bootstrap");
    let now = Utc::now();
    let phc = ProductPasswordKdf::hash("owner-password-strong").unwrap();
    let mut empty = ProductStore::open(&empty_path, config()).unwrap();
    empty
        .bootstrap_owner_scoped("owner", &phc, &[], &[])
        .unwrap();
    let owner_session = empty.login("owner", "owner-password-strong", now).unwrap();
    assert!(
        empty
            .list_scoped_orders(owner_session.digest(), None, 100, now)
            .is_ok()
    );
    assert!(
        empty
            .read_scoped_order(owner_session.digest(), "unknown", now)
            .is_err()
    );
    assert!(
        empty
            .bind_source("github", "github", "https://api.github.com", "42", "open")
            .is_err()
    );
    drop(empty);
    std::fs::remove_dir_all(root).unwrap();

    let (root, path) = scratch("multi-cap-bootstrap");
    let mut store = ProductStore::open(&path, config()).unwrap();
    let source = ScopeSourceManifest {
        connector_id: "github-main".into(),
        source_instance_id: "bootstrap-source-one".into(),
        generation: 1,
        identity: identity("acme", "https://api.github.com", "42"),
        api_owner: Some("acme".into()),
        api_repo: Some("repo".into()),
        legacy_repository_locator: "42".into(),
        scope_query: "open+foundry:make".into(),
        enabled: true,
    };
    let grant = ScopeGrantManifest {
        actor_id: "owner".into(),
        source_instance_id: source.source_instance_id.clone(),
        capabilities: vec![
            ProductCapability::ReadOrder,
            ProductCapability::ReadReview,
            ProductCapability::ApproveOrder,
        ],
    };
    store
        .bootstrap_owner_scoped("owner", &phc, std::slice::from_ref(&source), &[grant])
        .unwrap();
    let session = store.login("owner", "owner-password-strong", now).unwrap();
    let sources = store.list_scoped_sources(session.digest(), now).unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].source_revision, 1);
    assert!(sources[0].current);
    let grants = store
        .list_product_scope_grants(session.digest(), "owner", &source.source_instance_id, now)
        .unwrap();
    assert_eq!(grants.len(), 3);
    assert!(grants.iter().all(|grant| grant.enabled));
    let revisions: std::collections::BTreeSet<_> =
        grants.iter().map(|grant| grant.scope_revision).collect();
    assert_eq!(revisions.len(), 1);
    assert_eq!(*revisions.iter().next().unwrap(), 3);
    store
        .admit_order(
            "order-visible",
            "github-main",
            "42",
            "issue:1",
            "dep-bump",
            "snapshot",
            now,
        )
        .unwrap();
    assert_eq!(
        store
            .list_scoped_orders(session.digest(), None, 100, now)
            .unwrap()
            .len(),
        1
    );
    assert!(
        store
            .list_scoped_orders(session.digest(), Some("foreign-id"), 100, now)
            .is_err()
    );
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn scoped_grants_are_authenticated_repository_bound_and_revisioned() {
    let (root, path) = scratch("grant");
    let now = Utc::now();
    let (mut store, owner_session) = owner(&path, now);
    let viewer_phc = ProductPasswordKdf::hash("viewer-password-strong").unwrap();
    store
        .create_account(&owner_session, "viewer", "viewer", &viewer_phc, now)
        .unwrap();
    let viewer_session = store
        .login("viewer", "viewer-password-strong", now)
        .unwrap()
        .digest()
        .to_owned();

    let source = store
        .configure_scoped_source(
            &owner_session,
            "github-main",
            identity("acme", "HTTPS://API.GITHUB.COM:443/", "42"),
            "acme",
            "repo",
            "42",
            "open+foundry:make",
            true,
            0,
            "source-create-1",
            now,
        )
        .unwrap();
    let first = store
        .admit_order(
            "order-acme-1",
            "github-main",
            "42",
            "issue:7",
            "dep-bump",
            "immutable payload",
            now,
        )
        .unwrap();
    assert_eq!(first.lifecycle, 1);

    // Owners manage grants but have no implicit data access. A second
    // authenticated account also sees no order until explicitly granted.
    assert!(
        store
            .read_scoped_order(&owner_session, "order-acme-1", now)
            .is_err()
    );
    assert!(
        store
            .read_scoped_order(&viewer_session, "order-acme-1", now)
            .is_err()
    );
    assert!(
        store
            .change_product_scope(
                &owner_session,
                "owner",
                &source.source_instance_id,
                ProductCapability::ReadOrder,
                ScopeMutation::Grant,
                0,
                "owner-self-grant",
                now,
            )
            .is_err()
    );
    assert!(
        store
            .change_product_scope(
                &owner_session,
                "viewer",
                &source.source_instance_id,
                ProductCapability::ApproveOrder,
                ScopeMutation::Grant,
                0,
                "viewer-over-ceiling",
                now,
            )
            .is_err(),
        "Viewer's role ceiling must reject ApproveOrder"
    );

    // A grant/event/revision update is one transaction. A failure at the
    // append-only audit event leaves the account scope unchanged and retryable.
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER scope_event_failure BEFORE INSERT ON product_scope_events_v1
             BEGIN SELECT RAISE(ABORT,'injected scope event failure'); END;",
        )
        .unwrap();
    assert!(
        store
            .change_product_scope(
                &owner_session,
                "viewer",
                &source.source_instance_id,
                ProductCapability::ReadOrder,
                ScopeMutation::Grant,
                0,
                "viewer-grant-after-fault",
                now,
            )
            .is_err()
    );
    let conn = Connection::open(&path).unwrap();
    let grants_after_fault: i64 = conn
        .query_row("SELECT count(*) FROM product_scope_grants_v1", [], |r| {
            r.get(0)
        })
        .unwrap();
    let revisions_after_fault: i64 = conn
        .query_row(
            "SELECT count(*) FROM product_scope_subject_revisions_v1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn);
    assert_eq!(grants_after_fault, 0);
    assert_eq!(revisions_after_fault, 0);
    Connection::open(&path)
        .unwrap()
        .execute_batch("DROP TRIGGER scope_event_failure;")
        .unwrap();

    let receipt = store
        .change_product_scope(
            &owner_session,
            "viewer",
            &source.source_instance_id,
            ProductCapability::ReadOrder,
            ScopeMutation::Grant,
            0,
            "viewer-grant-1",
            now,
        )
        .unwrap();
    assert_eq!(receipt.scope_revision, 1);
    assert_eq!(
        store
            .change_product_scope(
                &owner_session,
                "viewer",
                &source.source_instance_id,
                ProductCapability::ReadOrder,
                ScopeMutation::Grant,
                0,
                "viewer-grant-1",
                now + Duration::seconds(1),
            )
            .unwrap(),
        receipt
    );
    let view = store
        .read_scoped_order(&viewer_session, "order-acme-1", now)
        .unwrap();
    assert_eq!(view.source_instance_id, source.source_instance_id);
    assert_eq!(view.state, "queued");
    assert!(
        store
            .read_scoped_order(&viewer_session, "order-foreign", now)
            .is_err()
    );

    // A changed request cannot reuse an idempotency key. Revoke/regrant uses
    // a monotonically increasing source-scoped revision and new grant ID.
    assert!(
        store
            .change_product_scope(
                &owner_session,
                "viewer",
                &source.source_instance_id,
                ProductCapability::ReadOrder,
                ScopeMutation::Revoke,
                0,
                "viewer-grant-1",
                now,
            )
            .is_err()
    );
    let revoked = store
        .change_product_scope(
            &owner_session,
            "viewer",
            &source.source_instance_id,
            ProductCapability::ReadOrder,
            ScopeMutation::Revoke,
            1,
            "viewer-revoke-1",
            now + Duration::seconds(2),
        )
        .unwrap();
    assert_eq!(revoked.scope_revision, 2);
    assert!(
        store
            .read_scoped_order(&viewer_session, "order-acme-1", now)
            .is_err()
    );
    let restored = store
        .change_product_scope(
            &owner_session,
            "viewer",
            &source.source_instance_id,
            ProductCapability::ReadOrder,
            ScopeMutation::Grant,
            2,
            "viewer-regrant-1",
            now + Duration::seconds(3),
        )
        .unwrap();
    assert_eq!(restored.scope_revision, 3);
    assert_eq!(restored.grant_revision, 3);
    assert!(
        store
            .read_scoped_order(&viewer_session, "order-acme-1", now)
            .is_ok()
    );

    // Role changes revoke existing scope grants in the same authority
    // transaction; promotion and re-login do not revive them.
    store
        .set_account_authority(&owner_session, "viewer", "operator", true, now)
        .unwrap();
    let after_demotion = store
        .login(
            "viewer",
            "viewer-password-strong",
            now + Duration::seconds(4),
        )
        .unwrap()
        .digest()
        .to_owned();
    assert!(
        store
            .read_scoped_order(&after_demotion, "order-acme-1", now)
            .is_err()
    );
    store
        .set_account_authority(&owner_session, "viewer", "viewer", true, now)
        .unwrap();
    let after_promotion = store
        .login(
            "viewer",
            "viewer-password-strong",
            now + Duration::seconds(5),
        )
        .unwrap()
        .digest()
        .to_owned();
    assert!(
        store
            .read_scoped_order(&after_promotion, "order-acme-1", now)
            .is_err()
    );

    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn canonical_repository_origin_and_rebind_keep_grants_on_old_instance() {
    let a = identity("Acme", "https://api.github.com:443/", "42");
    let b = identity("acme", "https://API.GITHUB.COM", "42");
    let c = identity("acme", "https://github.example", "42");
    assert_eq!(a, b);
    assert_ne!(a.key(), c.key());
    assert_eq!(
        identity("acme", "https://EXAMPLE.COM.:0443/", "42"),
        identity("acme", "https://example.com", "42")
    );
    for invalid in [
        "https://example.com:",
        "https://example.com:65536",
        "https://example..com",
        "https://example.com/api",
        "https://user@example.com",
        "https://example.com?query=1",
        "https://[not-ipv6]",
    ] {
        assert!(
            RepositoryIdentity::canonicalized("acme", "github", invalid, "42").is_err(),
            "accepted invalid origin: {invalid}"
        );
    }

    let (root, path) = scratch("rebind");
    let now = Utc::now();
    let (mut store, owner_session) = owner(&path, now);
    let viewer_phc = ProductPasswordKdf::hash("viewer-password-strong").unwrap();
    store
        .create_account(&owner_session, "viewer", "viewer", &viewer_phc, now)
        .unwrap();
    let viewer_session = store
        .login("viewer", "viewer-password-strong", now)
        .unwrap()
        .digest()
        .to_owned();
    let first = store
        .configure_scoped_source(
            &owner_session,
            "github-main",
            a,
            "acme",
            "repo",
            "42",
            "open",
            true,
            0,
            "source-1",
            now,
        )
        .unwrap();
    store
        .admit_order(
            "order-old",
            "github-main",
            "42",
            "issue:1",
            "dep-bump",
            "old",
            now,
        )
        .unwrap();
    store
        .change_product_scope(
            &owner_session,
            "viewer",
            &first.source_instance_id,
            ProductCapability::ReadOrder,
            ScopeMutation::Grant,
            0,
            "grant-old",
            now,
        )
        .unwrap();

    // Same connector label and numeric repository id at a new origin is a
    // distinct source instance. Existing grants are not copied.
    let second = store
        .configure_scoped_source(
            &owner_session,
            "github-main",
            c,
            "acme",
            "repo",
            "42",
            "open",
            true,
            1,
            "source-rebind",
            now + Duration::seconds(1),
        )
        .unwrap();
    assert_ne!(first.source_instance_id, second.source_instance_id);
    assert!(
        store
            .read_scoped_order(&viewer_session, "order-old", now)
            .is_ok()
    );
    store
        .admit_order(
            "order-new",
            "github-main",
            "42",
            "issue:2",
            "dep-bump",
            "new",
            now,
        )
        .unwrap();
    assert!(
        store
            .read_scoped_order(&viewer_session, "order-new", now)
            .is_err()
    );

    // Rebinding back to the original identity is another generation, so an
    // old grant cannot be revived by source ABA.
    let third = store
        .configure_scoped_source(
            &owner_session,
            "github-main",
            identity("acme", "https://api.github.com", "42"),
            "acme",
            "repo",
            "42",
            "open",
            true,
            2,
            "source-aba",
            now + Duration::seconds(2),
        )
        .unwrap();
    assert_ne!(first.source_instance_id, third.source_instance_id);
    assert_eq!(third.generation, 3);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn v1_maintenance_migration_is_explicit_atomic_and_quarantines_unknown_orders() {
    let (root, path) = scratch("migration");
    let now = Utc::now();
    let mut store = ProductStore::open(&path, config()).unwrap();
    store
        .bind_source(
            "gh",
            "github",
            "https://api.github.com",
            "acme/proj",
            "open",
        )
        .unwrap();
    let verified = store
        .admit_order(
            "order-verified",
            "gh",
            "acme/proj",
            "issue:1",
            "dep-bump",
            "snapshot-1",
            now,
        )
        .unwrap();
    store
        .admit_order(
            "order-unknown",
            "gh",
            "acme/proj",
            "issue:2",
            "dep-bump",
            "snapshot-2",
            now,
        )
        .unwrap();
    let owner_phc = ProductPasswordKdf::hash("owner-password-strong").unwrap();
    store.bootstrap_owner("owner", &owner_phc).unwrap();
    let old_session = store.login("owner", "owner-password-strong", now).unwrap();
    store
        .issue_approval_grant(
            old_session.digest(),
            "old-grant",
            &"a".repeat(64),
            "order-verified",
            now,
        )
        .unwrap();
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE product_grants_v1 SET state='reserved',fence=9 WHERE grant_id='old-grant'",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO product_operations_v1(operation_id,actor_id,request_key,request_digest,order_id,grant_id,phase,owner_id,epoch,fence,prepared_at,expires_at)
             VALUES('old-prepared','owner','old-request',?1,'order-verified','old-grant','prepared','old-owner',0,9,?2,?3)",
            params!["b".repeat(64),now.to_rfc3339(),(now+Duration::minutes(5)).to_rfc3339()],
        ).unwrap();
    }
    drop(store);

    let before: Vec<(
        i64,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        String,
    )> = {
        let conn = Connection::open(&path).unwrap();
        let mut stmt = conn
            .prepare("SELECT seq,order_id,from_state,to_state,event,actor,intent,at,event_json,hash FROM ledger ORDER BY seq")
            .unwrap();
        stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    };
    let tuple_history_before: Vec<(String, i64, i64, i64, String, String, String)> = {
        let conn = Connection::open(&path).unwrap();
        let mut stmt = conn.prepare(
            "SELECT order_id,attempt,run_id,generation,immutable_commit,artifact_location,generations_json FROM artifact_tuples ORDER BY order_id,attempt,run_id,generation",
        ).unwrap();
        stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    };
    assert!(
        tuple_history_before.is_empty(),
        "the legacy fixture has no tuple history to census"
    );
    use foundry_ledger::product_identity::ProductPasswordKdf;
    // Reconstruct the v1 input contract for this isolated fixture. The
    // v1→v2 migration creates scope tables; the independent v2→v3 migration
    // requires the v3 result-contract tables to be absent. Keep the v2 review
    // tables and artifact_tuples table (which the v3 migration censuses).
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP TRIGGER product_result_meta_no_update;
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
             UPDATE product_meta_v1 SET schema_version=1 WHERE singleton=1;",
        )
        .unwrap();
    }
    assert!(
        ProductStore::open(&path, config()).is_err(),
        "ordinary open must refuse v1"
    );
    {
        let conn = Connection::open(&path).unwrap();
        let (version, result_meta): (i64, i64) = conn.query_row(
            "SELECT schema_version,(SELECT count(*) FROM sqlite_master WHERE type='table' AND name='product_result_meta_v1') FROM product_meta_v1 WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!((version, result_meta), (1, 0));
    }
    let source = ScopeSourceManifest {
        connector_id: "gh".into(),
        source_instance_id: "source-instance-1".into(),
        generation: 1,
        identity: identity("acme", "https://api.github.com", "42"),
        api_owner: Some("acme".into()),
        api_repo: Some("proj".into()),
        legacy_repository_locator: "acme/proj".into(),
        scope_query: "open".into(),
        enabled: true,
    };
    let manifest = ScopeMigrationManifest {
        manifest_id: "reviewed-migration-1".into(),
        reviewed_by: "local-admin-review-2026-10".into(),
        sources: vec![source.clone()],
        verified_orders: vec![VerifiedOrderProvenance {
            order_id: "order-verified".into(),
            source_instance_id: source.source_instance_id.clone(),
            lifecycle: verified.lifecycle,
            admission_sequence: verified.sequence,
            admission_hash: verified.event_hash.clone(),
        }],
        grants: vec![ScopeGrantManifest {
            actor_id: "owner".into(),
            source_instance_id: source.source_instance_id.clone(),
            capabilities: vec![ProductCapability::ReadOrder],
        }],
    };
    let mut invalid = manifest.clone();
    invalid.verified_orders[0].admission_hash = "0".repeat(64);
    assert!(ProductStore::migrate_scope_v1(&path, &config(), &invalid, now).is_err());
    {
        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .query_row(
                "SELECT schema_version FROM product_meta_v1 WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            version, 1,
            "failed migration must roll back the version gate"
        );
    }
    let report = ProductStore::migrate_scope_v1(&path, &config(), &manifest, now).unwrap();
    assert_eq!(report.verified_orders, 1);
    assert_eq!(report.quarantined_orders, 1);
    assert_eq!(report.grants, 1);
    let after_scope_migration: Vec<(
        i64,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        String,
    )> = {
        let conn = Connection::open(&path).unwrap();
        let mut stmt = conn
            .prepare("SELECT seq,order_id,from_state,to_state,event,actor,intent,at,event_json,hash FROM ledger ORDER BY seq")
            .unwrap();
        stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    };
    assert_eq!(
        before, after_scope_migration,
        "v1→v2 preserves all S1 event/evidence bytes and hashes"
    );
    // Scope migration ends at v2. A normal open must still refuse it and
    // leave the v2 history/version gate untouched until result migration.
    assert!(
        ProductStore::open(&path, config()).is_err(),
        "ordinary open must refuse v2"
    );
    {
        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .query_row(
                "SELECT schema_version FROM product_meta_v1 WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(version, 2);
    }
    let after_v2_open_refusal: Vec<(
        i64,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        String,
    )> = {
        let conn = Connection::open(&path).unwrap();
        let mut stmt = conn
            .prepare("SELECT seq,order_id,from_state,to_state,event,actor,intent,at,event_json,hash FROM ledger ORDER BY seq")
            .unwrap();
        stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    };
    assert_eq!(
        before, after_v2_open_refusal,
        "refusing v2 open performs no S1 writes"
    );
    assert!(ProductStore::migrate_scope_v1(&path, &config(), &manifest, now).is_err());
    {
        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .query_row(
                "SELECT schema_version FROM product_meta_v1 WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(version, 2, "repeated v1 migration must not alter v2");
    }
    ProductStore::migrate_product_results_v2(&path, &config()).unwrap();
    {
        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .query_row(
                "SELECT schema_version FROM product_meta_v1 WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(version, 3);
    }
    ProductStore::migrate_product_results_v2(&path, &config()).unwrap();
    let mut migrated = ProductStore::open(&path, config()).unwrap();
    assert!(migrated.resolve_session(old_session.digest(), now).is_err());
    let new_session = migrated
        .login("owner", "owner-password-strong", now + Duration::seconds(1))
        .unwrap();
    let read = migrated
        .read_scoped_order(new_session.digest(), "order-verified", now)
        .unwrap();
    assert_eq!(read.order_id, "order-verified");
    assert!(
        migrated
            .read_scoped_order(new_session.digest(), "order-unknown", now)
            .is_err()
    );
    let after: Vec<(
        i64,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        String,
    )> = {
        let conn = Connection::open(&path).unwrap();
        let mut stmt = conn
            .prepare("SELECT seq,order_id,from_state,to_state,event,actor,intent,at,event_json,hash FROM ledger ORDER BY seq")
            .unwrap();
        stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    };
    assert_eq!(
        before, after,
        "both migrations must preserve every S1 event/evidence byte and hash"
    );
    let conn = Connection::open(&path).unwrap();
    let quarantine: i64 = conn
        .query_row(
            "SELECT count(*) FROM product_scope_quarantine_v1 WHERE order_id='order-unknown'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let old_grant_state: String = conn
        .query_row(
            "SELECT state FROM product_grants_v1 WHERE grant_id='old-grant'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let token_consumptions: i64 = conn
        .query_row(
            "SELECT count(*) FROM product_token_consumptions_v1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let old_operation_phase: String = conn
        .query_row(
            "SELECT phase FROM product_operations_v1 WHERE operation_id='old-prepared'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(quarantine, 1);
    assert_eq!(old_grant_state, "invalidated");
    assert_eq!(
        token_consumptions, 0,
        "migration does not fake token consumption"
    );
    assert_eq!(old_operation_phase, "fenced");
    let tuple_history_after: Vec<(String, i64, i64, i64, String, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT order_id,attempt,run_id,generation,immutable_commit,artifact_location,generations_json FROM artifact_tuples ORDER BY order_id,attempt,run_id,generation",
        ).unwrap();
        stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    };
    assert_eq!(
        tuple_history_before, tuple_history_after,
        "legacy tuple history is unchanged"
    );
    let (result_version, legacy_hosts, legacy_tuples): (i64, i64, i64) = conn.query_row(
        "SELECT result_contract_version,(SELECT count(*) FROM product_result_legacy_hosts_v1),(SELECT count(*) FROM product_result_legacy_tuples_v1) FROM product_result_meta_v1 WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).unwrap();
    assert_eq!((result_version, legacy_hosts, legacy_tuples), (1, 0, 0));
    drop(conn);
    drop(migrated);
    drop(ProductStore::open(&path, config()).unwrap());
    std::fs::remove_dir_all(root).unwrap();
}
