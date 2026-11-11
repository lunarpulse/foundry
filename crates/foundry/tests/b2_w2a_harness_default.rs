use std::process::Command;

#[test]
fn default_binary_rejects_test_flag_before_opening_database() {
    let root = std::env::temp_dir().join(format!(
        "w2a-default-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db = root.join("missing.sqlite");
    let output = Command::new(env!("CARGO_BIN_EXE_foundry"))
        .args([
            "product-watch",
            "--a01-local-provider-test",
            "--once",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            "test",
            "--database-id",
            "test",
            "--repository-id",
            "42",
            "--service-file",
            root.join("missing").to_str().unwrap(),
        ])
        .env("HOME", &root)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("unexpected argument '--a01-local-provider-test'")
    );
    assert!(!root.exists());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("test_transport="));
}
