use std::{
    io::Write,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

#[test]
fn product_bootstrap_cli_reads_bounded_credentials_from_stdin_under_isolated_home() {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("foundry-b1-cli-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("product.db");
    let password = "cli-canary-never-argv";
    let mut child = Command::new(env!("CARGO_BIN_EXE_foundry"))
        .args([
            "product-bootstrap",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            "isolated-install",
            "--database-id",
            "isolated-db",
        ])
        .env("HOME", &root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("owner-cli\n{password}\n").as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains(password));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(password));

    let session_file = root.join("session.token");
    let mut login = Command::new(env!("CARGO_BIN_EXE_foundry"))
        .args([
            "product-login",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            "isolated-install",
            "--database-id",
            "isolated-db",
            "--session-file",
            session_file.to_str().unwrap(),
        ])
        .env("HOME", &root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    login
        .stdin
        .take()
        .unwrap()
        .write_all(format!("owner-cli\n{password}\n").as_bytes())
        .unwrap();
    let logged_in = login.wait_with_output().unwrap();
    assert!(
        logged_in.status.success(),
        "{}",
        String::from_utf8_lossy(&logged_in.stderr)
    );
    assert!(!String::from_utf8_lossy(&logged_in.stdout).contains(password));
    let session = std::fs::read_to_string(&session_file).unwrap();
    assert_eq!(session.trim().len(), 64);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&session_file)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    let new_password = "reset-canary-not-argv";
    let mut reset = Command::new(env!("CARGO_BIN_EXE_foundry"))
        .args([
            "product-reset",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            "isolated-install",
            "--database-id",
            "isolated-db",
        ])
        .env("HOME", &root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    reset
        .stdin
        .take()
        .unwrap()
        .write_all(format!("owner-cli\n{new_password}\n").as_bytes())
        .unwrap();
    let reset = reset.wait_with_output().unwrap();
    assert!(
        reset.status.success(),
        "{}",
        String::from_utf8_lossy(&reset.stderr)
    );
    assert!(!String::from_utf8_lossy(&reset.stdout).contains(new_password));
    use sha2::{Digest, Sha256};
    let session_digest = format!("{:x}", Sha256::digest(session.trim().as_bytes()));
    let store = foundry_ledger::ProductStore::open(
        &db,
        foundry_ledger::ProductConfig {
            installation_id: "isolated-install".into(),
            database_id: "isolated-db".into(),
        },
    )
    .unwrap();
    assert!(
        store
            .resolve_session(&session_digest, chrono::Utc::now())
            .is_err(),
        "local reset revokes old sessions by revision"
    );

    let mut second = Command::new(env!("CARGO_BIN_EXE_foundry"))
        .args([
            "product-bootstrap",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            "isolated-install",
            "--database-id",
            "isolated-db",
        ])
        .env("HOME", &root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    second
        .stdin
        .take()
        .unwrap()
        .write_all(format!("owner-cli\n{password}\n").as_bytes())
        .unwrap();
    let denied = second.wait_with_output().unwrap();
    assert!(
        !denied.status.success(),
        "bootstrap must not reopen over HTTP/CLI"
    );
    assert!(!String::from_utf8_lossy(&denied.stderr).contains(password));
    std::fs::remove_dir_all(root).unwrap();
}
