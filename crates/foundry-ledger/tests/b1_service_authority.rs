use chrono::{Duration, TimeZone, Utc};
use foundry_ledger::product_identity::ProductPasswordKdf;
use foundry_ledger::{
    ProductConfig, ProductDelivery, ProductPollCursor, ProductServiceAction, ProductServiceGrant,
    ProductStore, RepositoryIdentity, ScopeSourceManifest,
};
use std::path::PathBuf;

fn scratch() -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "foundry-b1-service-{}-{}",
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
fn cfg() -> ProductConfig {
    ProductConfig {
        installation_id: "service-test-install".into(),
        database_id: "service-test-db".into(),
    }
}
fn source(connector: &str, instance: &str, origin: &str, id: &str) -> ScopeSourceManifest {
    ScopeSourceManifest {
        connector_id: connector.into(),
        source_instance_id: instance.into(),
        generation: 1,
        identity: RepositoryIdentity::canonicalized("acme", "github", origin, id).unwrap(),
        api_owner: Some("acme".into()),
        api_repo: Some("repo".into()),
        legacy_repository_locator: id.into(),
        scope_query: "open+foundry:make".into(),
        enabled: true,
    }
}
fn open_scoped(
    db: &std::path::Path,
    sources: &[ScopeSourceManifest],
    now: chrono::DateTime<Utc>,
) -> (ProductStore, String) {
    let mut store = ProductStore::open(db, cfg()).unwrap();
    let phc = ProductPasswordKdf::hash("service-test-owner-password").unwrap();
    store
        .bootstrap_owner_scoped("owner", &phc, sources, &[])
        .unwrap();
    let session = store
        .login("owner", "service-test-owner-password", now)
        .unwrap()
        .digest()
        .to_owned();
    (store, session)
}

#[test]
fn service_credentials_are_source_action_bound_and_revoke_across_handles() {
    let (root, db) = scratch();
    let now = Utc.with_ymd_and_hms(2026, 10, 18, 0, 0, 0).unwrap();
    let a = source("github-a", "source-a", "https://api.github.com", "42");
    let b = source("github-b", "source-b", "https://api.github.com", "43");
    let (mut owner, session) = open_scoped(&db, &[a.clone(), b.clone()], now);
    let grants = vec![
        ProductServiceGrant {
            source_instance_id: a.source_instance_id.clone(),
            action: ProductServiceAction::IntakeRead,
        },
        ProductServiceGrant {
            source_instance_id: a.source_instance_id.clone(),
            action: ProductServiceAction::AdmitDelivery,
        },
        ProductServiceGrant {
            source_instance_id: a.source_instance_id.clone(),
            action: ProductServiceAction::AckDelivery,
        },
        ProductServiceGrant {
            source_instance_id: a.source_instance_id.clone(),
            action: ProductServiceAction::DriveReview,
        },
    ];
    let lease = now + Duration::hours(4);
    let first = owner
        .manage_product_service(
            &session,
            "github-worker",
            &grants,
            true,
            lease,
            0,
            "register-1",
            now,
        )
        .unwrap();
    assert_eq!(first.revision, 1);
    assert_eq!(
        owner
            .manage_product_service(
                &session,
                "github-worker",
                &grants,
                true,
                lease,
                0,
                "register-1",
                now
            )
            .unwrap(),
        first,
        "exact management retry returns the recorded receipt"
    );
    assert!(
        owner
            .manage_product_service(
                &session,
                "github-worker",
                &[],
                false,
                lease,
                0,
                "register-1",
                now
            )
            .is_err(),
        "same key with changed body conflicts"
    );
    let credential = owner
        .issue_product_service_credential(
            &session,
            "github-worker",
            1,
            now + Duration::hours(2),
            "issue-1",
            now,
        )
        .unwrap();
    let token = credential.expose_once_to_file_writer().to_owned();
    let ctx = owner
        .authenticate_product_service(
            &token,
            &a.connector_id,
            &a.identity.api_origin,
            &a.identity.stable_repository_id,
            now,
        )
        .unwrap();
    owner
        .require_product_service(&ctx, ProductServiceAction::IntakeRead, now)
        .unwrap();
    owner
        .require_product_service(&ctx, ProductServiceAction::DriveReview, now)
        .unwrap();
    assert!(
        owner
            .authenticate_product_service(
                &token,
                &b.connector_id,
                &b.identity.api_origin,
                &b.identity.stable_repository_id,
                now
            )
            .is_err(),
        "service has no implicit grant to another source instance"
    );

    // Revoke through an independent connection; the old in-process context is
    // rechecked against persisted service revision and fails immediately.
    let (mut admin2, session2) = open_existing(&db, now);
    let revoked = admin2
        .manage_product_service(
            &session2,
            "github-worker",
            &[],
            false,
            lease,
            1,
            "revoke-1",
            now,
        )
        .unwrap();
    assert!(!revoked.enabled);
    assert!(
        owner
            .require_product_service(&ctx, ProductServiceAction::IntakeRead, now)
            .is_err()
    );
    assert!(
        owner
            .authenticate_product_service(
                &token,
                &a.connector_id,
                &a.identity.api_origin,
                &a.identity.stable_repository_id,
                now
            )
            .is_err()
    );
    drop(admin2);
    drop(owner);
    std::fs::remove_dir_all(root).unwrap();
}

fn open_existing(db: &std::path::Path, now: chrono::DateTime<Utc>) -> (ProductStore, String) {
    let mut store = ProductStore::open(db, cfg()).unwrap();
    let session = store
        .login("owner", "service-test-owner-password", now)
        .unwrap()
        .digest()
        .to_owned();
    (store, session)
}

#[test]
fn revoked_or_expired_service_cannot_reclaim_and_service_actions_do_not_become_human_authority() {
    let (root, db) = scratch();
    let now = Utc.with_ymd_and_hms(2026, 10, 18, 0, 0, 0).unwrap();
    let src = source("github-a", "source-a", "https://api.github.com", "42");
    let (mut store, session) = open_scoped(&db, std::slice::from_ref(&src), now);
    let grants = vec![ProductServiceGrant {
        source_instance_id: src.source_instance_id.clone(),
        action: ProductServiceAction::IntakeRead,
    }];
    store
        .manage_product_service(
            &session,
            "intake-only",
            &grants,
            true,
            now + Duration::hours(1),
            0,
            "config-1",
            now,
        )
        .unwrap();
    let credential = store
        .issue_product_service_credential(
            &session,
            "intake-only",
            1,
            now + Duration::minutes(30),
            "credential-1",
            now,
        )
        .unwrap();
    let token = credential.expose_once_to_file_writer();
    let ctx = store
        .authenticate_product_service(
            token,
            &src.connector_id,
            &src.identity.api_origin,
            &src.identity.stable_repository_id,
            now,
        )
        .unwrap();
    assert!(
        store
            .require_product_service(&ctx, ProductServiceAction::DriveReview, now)
            .is_err()
    );
    assert!(
        store
            .claim_product_work_for_service(&ctx, "worker", now, Duration::minutes(1))
            .is_err()
    );
    assert!(
        store
            .authenticate_product_service(
                token,
                &src.connector_id,
                &src.identity.api_origin,
                &src.identity.stable_repository_id,
                now + Duration::minutes(30)
            )
            .is_err(),
        "expiry is exclusive at the boundary"
    );
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn service_revoke_regrant_does_not_reauthenticate_old_bearer_after_reopen() {
    let (root, db) = scratch();
    let now = Utc.with_ymd_and_hms(2026, 10, 18, 0, 0, 0).unwrap();
    let src = source("github-a", "source-a", "https://api.github.com", "42");
    let (mut store, session) = open_scoped(&db, std::slice::from_ref(&src), now);
    let grants = all_intake_actions(&src);
    let lease = now + Duration::hours(4);
    store
        .manage_product_service(
            &session,
            "github-worker",
            &grants,
            true,
            lease,
            0,
            "configure-1",
            now,
        )
        .unwrap();
    let credential = store
        .issue_product_service_credential(
            &session,
            "github-worker",
            1,
            now + Duration::hours(2),
            "issue-1",
            now,
        )
        .unwrap();
    let old_token = credential.expose_once_to_file_writer().to_owned();

    store
        .manage_product_service(
            &session,
            "github-worker",
            &[],
            false,
            lease,
            1,
            "revoke-1",
            now,
        )
        .unwrap();
    store
        .manage_product_service(
            &session,
            "github-worker",
            &grants,
            true,
            lease,
            2,
            "regrant-1",
            now,
        )
        .unwrap();
    drop(store);
    let reopened = ProductStore::open(&db, cfg()).unwrap();
    assert!(
        reopened
            .authenticate_product_service(
                &old_token,
                &src.connector_id,
                &src.identity.api_origin,
                &src.identity.stable_repository_id,
                now
            )
            .is_err(),
        "a service grant revoke/regrant must not make the old bearer authenticate again"
    );
    drop(reopened);

    let mut fresh = ProductStore::open(&db, cfg()).unwrap();
    let token = fresh
        .issue_product_service_credential(
            &session,
            "github-worker",
            3,
            now + Duration::hours(2),
            "issue-2",
            now,
        )
        .unwrap();
    assert!(
        fresh
            .authenticate_product_service(
                token.expose_once_to_file_writer(),
                &src.connector_id,
                &src.identity.api_origin,
                &src.identity.stable_repository_id,
                now
            )
            .is_ok(),
        "only explicit fresh issuance restores service authentication"
    );
    drop(fresh);
    std::fs::remove_dir_all(root).unwrap();
}

fn all_intake_actions(source: &ScopeSourceManifest) -> Vec<ProductServiceGrant> {
    [
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
    .collect()
}

fn delivery(
    store: &ProductStore,
    origin: &str,
    issue: &str,
    updated: &str,
    payload: &str,
) -> ProductDelivery {
    let task = "github-issue";
    let version = foundry_ledger::product::delivery_version_digest(issue, updated, task, payload);
    ProductDelivery {
        delivery_id: foundry_ledger::product::github_delivery_id(
            store.installation_id(),
            "github-a",
            origin,
            "42",
            issue,
            &version,
        ),
        connector_id: "github-a".into(),
        repository_id: "42".into(),
        issue_id: issue.into(),
        observed_updated_at: updated.into(),
        version_digest: version,
        task_kind: task.into(),
        payload: payload.into(),
    }
}

fn cursor(now: chrono::DateTime<Utc>, sweep: &str) -> ProductPollCursor {
    ProductPollCursor {
        connector_id: "github-a".into(),
        query_fingerprint: "query-v1".into(),
        sweep_id: sweep.into(),
        next_page: 1,
        page_ceiling: 1000,
        updated_at: now.to_rfc3339(),
    }
}

#[test]
fn rebound_source_uses_separate_revisions_and_rejects_foreign_order_provenance_atomically() {
    let (root, db) = scratch();
    let now = Utc.with_ymd_and_hms(2026, 10, 18, 0, 0, 0).unwrap();
    let first = source("github-a", "source-a", "https://api.github.com", "42");
    let (mut store, session) = open_scoped(&db, std::slice::from_ref(&first), now);
    let lease = now + Duration::hours(4);
    store
        .manage_product_service(
            &session,
            "github-worker",
            &all_intake_actions(&first),
            true,
            lease,
            0,
            "configure-a",
            now,
        )
        .unwrap();
    let credential = store
        .issue_product_service_credential(
            &session,
            "github-worker",
            1,
            now + Duration::hours(2),
            "credential-a",
            now,
        )
        .unwrap();
    let token_a = credential.expose_once_to_file_writer().to_owned();
    let old = store
        .authenticate_product_service(
            &token_a,
            &first.connector_id,
            &first.identity.api_origin,
            &first.identity.stable_repository_id,
            now,
        )
        .unwrap();
    store
        .require_product_service(&old, ProductServiceAction::IntakeRead, now)
        .unwrap();

    let first_delivery = delivery(
        &store,
        &first.identity.api_origin,
        "issue:7",
        "2026-10-18T00:00:00Z",
        "first immutable snapshot",
    );
    store
        .persist_intake_page_for_service(
            &old,
            &cursor(now, "sweep-a"),
            std::slice::from_ref(&first_delivery),
            &[],
            now,
        )
        .unwrap();
    let receipt = store
        .admit_intake_delivery_for_service(&old, &first_delivery.delivery_id, "order-issue-7", now)
        .unwrap();
    let retry = store
        .admit_intake_delivery_for_service(&old, &first_delivery.delivery_id, "order-issue-7", now)
        .unwrap();
    assert_eq!(
        retry, receipt,
        "an exact admitted retry returns the exact original receipt"
    );

    let edited = delivery(
        &store,
        &first.identity.api_origin,
        "issue:7",
        "2026-10-18T00:01:00Z",
        "edited snapshot",
    );
    store
        .persist_intake_page_for_service(
            &old,
            &cursor(now, "sweep-edit"),
            std::slice::from_ref(&edited),
            &[],
            now,
        )
        .unwrap();
    assert_eq!(
        store
            .admit_intake_delivery_for_service(
                &old,
                &edited.delivery_id,
                "ignored-new-order-id",
                now
            )
            .unwrap(),
        receipt,
        "an edit creates a delivery version but retains the admitted order snapshot and receipt"
    );

    let second_identity =
        RepositoryIdentity::canonicalized("acme", "github", "https://api.github.example", "42")
            .unwrap();
    let second = store
        .configure_scoped_source(
            &session,
            "github-a",
            second_identity,
            "acme",
            "renamed",
            "acme/renamed",
            "open+foundry:make",
            true,
            1,
            "rebind-b",
            now + Duration::seconds(1),
        )
        .unwrap();
    assert_eq!(second.generation, 2);
    let grants = all_intake_actions(&second);
    store
        .manage_product_service(
            &session,
            "github-worker",
            &grants,
            true,
            lease,
            1,
            "configure-b",
            now + Duration::seconds(1),
        )
        .unwrap();
    let credential = store
        .issue_product_service_credential(
            &session,
            "github-worker",
            2,
            now + Duration::hours(2),
            "credential-b",
            now + Duration::seconds(1),
        )
        .unwrap();
    let token_b = credential.expose_once_to_file_writer().to_owned();
    assert!(
        store
            .authenticate_product_service(
                &token_a,
                &first.connector_id,
                &first.identity.api_origin,
                "42",
                now + Duration::seconds(1)
            )
            .is_err(),
        "source rebind invalidates the old credential/context binding"
    );
    assert!(
        store
            .require_product_service(
                &old,
                ProductServiceAction::IntakeRead,
                now + Duration::seconds(1)
            )
            .is_err(),
        "an already authenticated old context is rechecked and rejected"
    );
    assert!(
        store
            .persist_intake_page_for_service(
                &old,
                &cursor(now, "stale-old-context"),
                &[],
                &[],
                now + Duration::seconds(1)
            )
            .is_err(),
        "the old source context cannot advance a durable page after rebind"
    );
    let current = store
        .authenticate_product_service(
            &token_b,
            &second.connector_id,
            &second.identity.api_origin,
            "42",
            now + Duration::seconds(1),
        )
        .unwrap();
    store
        .require_product_service(
            &current,
            ProductServiceAction::IntakeRead,
            now + Duration::seconds(1),
        )
        .unwrap();
    store
        .require_product_service(
            &current,
            ProductServiceAction::AdmitDelivery,
            now + Duration::seconds(1),
        )
        .unwrap();

    // The same connector/repository/issue tuple already belongs to generation
    // one. Generation two may durably observe the issue, but cannot receive or
    // mutate generation one's receipt.
    let foreign = delivery(
        &store,
        &second.identity.api_origin,
        "issue:7",
        "2026-10-18T00:02:00Z",
        "foreign generation snapshot",
    );
    store
        .persist_intake_page_for_service(
            &current,
            &cursor(now, "sweep-b"),
            std::slice::from_ref(&foreign),
            &[],
            now + Duration::seconds(1),
        )
        .unwrap();
    let before = store
        .due_intake_deliveries_for_service(&current, now + Duration::seconds(1), 10)
        .unwrap();
    assert!(
        before
            .iter()
            .any(|d| d.delivery.delivery_id == foreign.delivery_id && d.state == "fetched")
    );
    assert!(
        store
            .admit_intake_delivery_for_service(
                &current,
                &foreign.delivery_id,
                "order-foreign",
                now + Duration::seconds(1)
            )
            .is_err(),
        "foreign immutable source provenance conflicts instead of returning the old receipt"
    );
    let after = store
        .due_intake_deliveries_for_service(&current, now + Duration::seconds(1), 10)
        .unwrap();
    assert!(
        after
            .iter()
            .any(|d| d.delivery.delivery_id == foreign.delivery_id && d.state == "fetched"),
        "the failed admission leaves the new delivery unchanged"
    );
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}
