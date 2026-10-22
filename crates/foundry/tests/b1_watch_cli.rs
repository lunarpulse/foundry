use std::process::Command;

#[test]
fn product_watch_cli_requires_a_private_service_credential_and_uses_explicit_home() {
    let root = std::env::var_os("TMPDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!(
            "foundry-b1-watch-cli-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("product.sqlite");
    let mut store = foundry_ledger::ProductStore::open(
        &db,
        foundry_ledger::ProductConfig {
            installation_id: "isolated-install".into(),
            database_id: "isolated-db".into(),
        },
    )
    .unwrap();
    let phc = foundry_ledger::product_identity::ProductPasswordKdf::hash("isolated-owner-password")
        .unwrap();
    store
        .bootstrap_owner_scoped("owner", &phc, &[], &[])
        .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_foundry"))
        .args([
            "product-watch",
            "--once",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            "isolated-install",
            "--database-id",
            "isolated-db",
            "--repo",
            "acme/proj",
            "--repository-id",
            "repo-42",
            "--api-origin",
            "https://api.github.com",
            "--service-file",
            root.join("missing-service.credential").to_str().unwrap(),
        ])
        .env("HOME", &root)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("private service credential file rejected"));
    assert!(!stderr.contains("token"));
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}
