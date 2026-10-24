use foundry_ledger::{ProductApprovalReceipt, ProductConfig, ProductStore};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn scratch() -> PathBuf {
    let base = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .expect("TMPDIR must point at configured private scratch");
    assert_ne!(base, Path::new("/tmp"));
    let root = base.join(format!(
        "foundry-b1-publish-default-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn run(args: &[&str], root: &Path, input: Option<&str>) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_foundry"));
    command
        .args(args)
        .env("HOME", root)
        .env("FOUNDRY_INSTALLATION_ID", "publish-default-install")
        .env("FOUNDRY_DATABASE_ID", "publish-default-db")
        .env_remove("B1_PUBLISH_TEST_DIR")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    if let Some(input) = input {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    child.wait_with_output().unwrap()
}

#[test]
fn v3_default_publish_fails_closed_before_durable_start() {
    let root = scratch();
    let db = root.join("product.db");
    let bootstrap = run(
        &[
            "product-bootstrap",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            "publish-default-install",
            "--database-id",
            "publish-default-db",
        ],
        &root,
        Some("owner-default\ndefault-publish-secret\n"),
    );
    assert!(
        bootstrap.status.success(),
        "{}",
        String::from_utf8_lossy(&bootstrap.stderr)
    );
    let session = root.join("session");
    let login = run(
        &[
            "product-login",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            "publish-default-install",
            "--database-id",
            "publish-default-db",
            "--session-file",
            session.to_str().unwrap(),
        ],
        &root,
        Some("owner-default\ndefault-publish-secret\n"),
    );
    assert!(
        login.status.success(),
        "{}",
        String::from_utf8_lossy(&login.stderr)
    );

    let receipt = root.join("receipt.json");
    let value = ProductApprovalReceipt {
        operation_id: "uncommitted-operation".into(),
        actor_id: "owner-default".into(),
        order_id: "uncommitted-order".into(),
        lifecycle: 1,
        sequence: 1,
        event_hash: "a".repeat(64),
        artifact_digest: "b".repeat(64),
        attempt: 1,
        run_id: 1,
        generation: 1,
        tuple_digest: "c".repeat(64),
        source_revision: 1,
    };
    std::fs::write(&receipt, serde_json::to_vec(&value).unwrap()).unwrap();
    let publish = run(
        &[
            "product",
            "publish",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            "publish-default-install",
            "--database-id",
            "publish-default-db",
            "--session-file",
            session.to_str().unwrap(),
            "--receipt-file",
            receipt.to_str().unwrap(),
        ],
        &root,
        None,
    );
    assert_eq!(publish.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&publish.stderr)
            .contains("no publish adapter configured — external publishing arrives in W6")
    );
    let store = ProductStore::open(
        &db,
        ProductConfig {
            installation_id: "publish-default-install".into(),
            database_id: "publish-default-db".into(),
        },
    )
    .unwrap();
    drop(store);
    let count: i64 = rusqlite::Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM product_publish_outcomes_v1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}
