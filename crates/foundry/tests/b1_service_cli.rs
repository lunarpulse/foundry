use std::{
    io::Write,
    process::{Command, Stdio},
};

fn scratch() -> std::path::PathBuf {
    let root = std::env::var_os("TMPDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!(
            "foundry-b1-service-cli-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(&root).unwrap();
    root
}
fn run(root: &std::path::Path, args: &[&str], stdin: &str) -> std::process::Output {
    run_owned(root, args.iter().map(|v| (*v).to_owned()).collect(), stdin)
}
fn run_owned(root: &std::path::Path, args: Vec<String>, stdin: &str) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_foundry"))
        .args(args)
        .env("HOME", root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}
fn ok(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn service_cli_issues_private_credential_and_enforces_rotation_and_revoke() {
    let root = scratch();
    let db = root.join("product.sqlite");
    let session = root.join("owner.session");
    let credential = root.join("worker.credential");
    let config = foundry_ledger::ProductConfig {
        installation_id: "service-cli-install".into(),
        database_id: "service-cli-db".into(),
    };
    let source = foundry_ledger::ScopeSourceManifest {
        connector_id: "github-main".into(),
        source_instance_id: "github-main-generation-1".into(),
        generation: 1,
        identity: foundry_ledger::RepositoryIdentity::canonicalized(
            "acme",
            "github",
            "https://api.github.com",
            "42",
        )
        .unwrap(),
        api_owner: Some("acme".into()),
        api_repo: Some("repo".into()),
        legacy_repository_locator: "42".into(),
        scope_query: "open+foundry:make".into(),
        enabled: true,
    };
    let mut store = foundry_ledger::ProductStore::open(&db, config.clone()).unwrap();
    let phc =
        foundry_ledger::product_identity::ProductPasswordKdf::hash("service-cli-owner-password")
            .unwrap();
    store
        .bootstrap_owner_scoped("owner", &phc, std::slice::from_ref(&source), &[])
        .unwrap();
    drop(store);
    let login = run(
        &root,
        &[
            "product-login",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            "service-cli-install",
            "--database-id",
            "service-cli-db",
            "--session-file",
            session.to_str().unwrap(),
        ],
        "owner\nservice-cli-owner-password\n",
    );
    ok(&login);
    assert!(!String::from_utf8_lossy(&login.stdout).contains("owner.session"));
    let now = chrono::Utc::now();
    let lease = (now + chrono::Duration::hours(3)).to_rfc3339();
    let expiry = (now + chrono::Duration::hours(2)).to_rfc3339();
    let common = [
        "--db",
        db.to_str().unwrap(),
        "--installation-id",
        "service-cli-install",
        "--database-id",
        "service-cli-db",
        "--session-file",
        session.to_str().unwrap(),
        "--service-id",
        "github-reviewer",
    ];
    let mut configure_args = vec![
        "product-service".to_owned(),
        "--action".to_owned(),
        "configure".to_owned(),
    ];
    configure_args.extend(common.iter().map(|v| (*v).to_owned()));
    configure_args.extend([
        "--expected-revision".into(),
        "0".into(),
        "--request-key".into(),
        "configure-1".into(),
        "--lease-until".into(),
        lease.clone(),
        "--source-instance".into(),
        "github-main-generation-1".into(),
        "--actions".into(),
        "intake_read,admit_delivery,ack_delivery,drive_review".into(),
    ]);
    let configure = run_owned(&root, configure_args, "");
    ok(&configure);
    assert!(String::from_utf8_lossy(&configure.stdout).contains("enabled=true"));
    let issue = run(
        &root,
        &[
            "product-service",
            "--action",
            "issue",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            "service-cli-install",
            "--database-id",
            "service-cli-db",
            "--session-file",
            session.to_str().unwrap(),
            "--service-id",
            "github-reviewer",
            "--expected-revision",
            "1",
            "--request-key",
            "issue-1",
            "--expires-at",
            &expiry,
            "--credential-file",
            credential.to_str().unwrap(),
        ],
        "",
    );
    ok(&issue);
    assert!(
        !String::from_utf8_lossy(&issue.stdout).contains(
            &std::fs::read_to_string(&credential)
                .unwrap()
                .trim()
                .to_owned()
        )
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&credential).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let token = std::fs::read_to_string(&credential)
        .unwrap()
        .trim()
        .to_owned();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&credential).unwrap().permissions();
        permissions.set_mode(0o644);
        std::fs::set_permissions(&credential, permissions).unwrap();
    }
    let rejected = run(
        &root,
        &[
            "product-watch",
            "--once",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            "service-cli-install",
            "--database-id",
            "service-cli-db",
            "--repo",
            "acme/proj",
            "--repository-id",
            "42",
            "--api-origin",
            "https://api.github.com",
            "--service-file",
            credential.to_str().unwrap(),
        ],
        "",
    );
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("private service credential file rejected")
    );
    assert!(!String::from_utf8_lossy(&rejected.stderr).contains(&token));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&credential).unwrap().permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(&credential, permissions).unwrap();
    }
    let duplicate = run(
        &root,
        &[
            "product-service",
            "--action",
            "issue",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            "service-cli-install",
            "--database-id",
            "service-cli-db",
            "--session-file",
            session.to_str().unwrap(),
            "--service-id",
            "github-reviewer",
            "--expected-revision",
            "1",
            "--request-key",
            "issue-1",
            "--expires-at",
            &expiry,
            "--credential-file",
            credential.to_str().unwrap(),
        ],
        "",
    );
    assert!(!duplicate.status.success());
    assert!(!String::from_utf8_lossy(&duplicate.stderr).contains(&token));
    let new_credential = root.join("worker-rotated.credential");
    let rotate = run(
        &root,
        &[
            "product-service",
            "--action",
            "rotate",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            "service-cli-install",
            "--database-id",
            "service-cli-db",
            "--session-file",
            session.to_str().unwrap(),
            "--service-id",
            "github-reviewer",
            "--expected-revision",
            "1",
            "--request-key",
            "rotate-1",
            "--expires-at",
            &expiry,
            "--credential-file",
            new_credential.to_str().unwrap(),
        ],
        "",
    );
    ok(&rotate);
    let reopened = foundry_ledger::ProductStore::open(&db, config.clone()).unwrap();
    assert!(
        reopened
            .authenticate_product_service(
                &token,
                "github-main",
                "https://api.github.com",
                "42",
                chrono::Utc::now()
            )
            .is_err()
    );
    let rotated = std::fs::read_to_string(&new_credential).unwrap();
    let context = reopened
        .authenticate_product_service(
            rotated.trim(),
            "github-main",
            "https://api.github.com",
            "42",
            chrono::Utc::now(),
        )
        .unwrap();
    reopened
        .require_product_service(
            &context,
            foundry_ledger::ProductServiceAction::DriveReview,
            chrono::Utc::now(),
        )
        .unwrap();
    drop(reopened);
    let revoke = run(
        &root,
        &[
            "product-service",
            "--action",
            "revoke",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            "service-cli-install",
            "--database-id",
            "service-cli-db",
            "--session-file",
            session.to_str().unwrap(),
            "--service-id",
            "github-reviewer",
            "--expected-revision",
            "1",
            "--request-key",
            "revoke-1",
        ],
        "",
    );
    ok(&revoke);
    let reopened = foundry_ledger::ProductStore::open(&db, config).unwrap();
    assert!(
        reopened
            .authenticate_product_service(
                rotated.trim(),
                "github-main",
                "https://api.github.com",
                "42",
                chrono::Utc::now()
            )
            .is_err()
    );
    drop(reopened);
    drop(token);
    std::fs::remove_dir_all(root).unwrap();
}
