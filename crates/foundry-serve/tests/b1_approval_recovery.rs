use foundry_ledger::{
    ProductCapability, ProductConfig, ProductStore, RepositoryIdentity, ScopeGrantManifest,
    ScopeSourceManifest,
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn root() -> PathBuf {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "foundry-b1-intake-scope-{}-{n}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn product_intake_refuses_admission_without_provisioned_service_scope() {
    let root = root();
    let db = root.join("product.db");
    let config = ProductConfig {
        installation_id: "hold-route-install".into(),
        database_id: "hold-route-db".into(),
    };
    let mut store = ProductStore::open(&db, config).unwrap();
    let phc =
        foundry_ledger::product_identity::ProductPasswordKdf::hash("synthetic-owner-password")
            .unwrap();
    let source = ScopeSourceManifest {
        connector_id: "gh".into(),
        source_instance_id: "hold-route-source".into(),
        generation: 1,
        identity: RepositoryIdentity::canonicalized(
            "acme",
            "github",
            "https://api.github.com",
            "repo:1",
        )
        .unwrap(),
        api_owner: Some("acme".into()),
        api_repo: Some("repo".into()),
        legacy_repository_locator: "repo:1".into(),
        scope_query: "open+foundry:make".into(),
        enabled: true,
    };
    store
        .bootstrap_owner_scoped(
            "owner",
            &phc,
            &[source],
            &[ScopeGrantManifest {
                actor_id: "owner".into(),
                source_instance_id: "hold-route-source".into(),
                capabilities: vec![ProductCapability::ReadOrder, ProductCapability::ReadReview],
            }],
        )
        .unwrap();
    let now = chrono::Utc::now();
    let updated = now.to_rfc3339();
    let version = foundry_ledger::product::delivery_version_digest(
        "issue:5",
        &updated,
        "dep-bump",
        "synthetic immutable task",
    );
    let delivery = foundry_ledger::product_intake::ProductDelivery {
        delivery_id: foundry_ledger::product::github_delivery_id(
            "hold-route-install",
            "gh",
            "https://api.github.com",
            "repo:1",
            "issue:5",
            &version,
        ),
        connector_id: "gh".into(),
        repository_id: "repo:1".into(),
        issue_id: "issue:5".into(),
        observed_updated_at: updated.clone(),
        version_digest: version,
        task_kind: "dep-bump".into(),
        payload: "synthetic immutable task".into(),
    };
    let persist_error = store
        .persist_intake_page(
            &foundry_ledger::product_intake::ProductPollCursor {
                connector_id: "gh".into(),
                query_fingerprint: "synthetic-route-test".into(),
                sweep_id: "synthetic-route-sweep".into(),
                next_page: 1,
                page_ceiling: 1,
                updated_at: updated,
            },
            std::slice::from_ref(&delivery),
        )
        .unwrap_err();
    assert!(
        persist_error
            .to_string()
            .contains("intake service authority required")
    );

    // Scope policy is active, but no real trusted intake service principal is
    // provisioned. Do not invent service authority just to reach the old hold
    // continuation scenario; persisted delivery must remain unadmitted.
    let conn = rusqlite::Connection::open(&db).unwrap();
    let order_count: i64 = conn
        .query_row(
            "SELECT count(*) FROM product_orders_v1 WHERE order_id='hold-route-order'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let delivery_count: i64 = conn
        .query_row(
            "SELECT count(*) FROM product_intake_inbox_v1 WHERE delivery_id=?1",
            [&delivery.delivery_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(order_count, 0);
    assert_eq!(delivery_count, 0);
    drop(conn);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}
