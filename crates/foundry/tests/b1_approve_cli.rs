#![allow(dead_code)] // W6a clippy debt: preserve this existing test/example logic, assertions, and cell sequence
#![allow(clippy::too_many_arguments)] // W6a clippy debt: preserve this existing test/example logic, assertions, and cell sequence

#[cfg(feature = "p2-test-support")]
use foundry_exec::DispatchHost;
use foundry_ledger::{ProductConfig, ProductStore, RepositoryIdentity, ScopeSourceManifest};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(feature = "p2-test-support")]
#[path = "../../foundry-line/tests/support/product_result_fixture.rs"]
mod product_result_fixture;

static NEXT: AtomicU64 = AtomicU64::new(0);

fn scratch() -> (PathBuf, PathBuf) {
    let base = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .expect("TMPDIR must point at configured private scratch");
    assert_ne!(base, Path::new("/tmp"));
    let root = base.join(format!(
        "foundry-b1-approve-cli-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).unwrap();
    (root.clone(), root.join("product.db"))
}

fn run(
    args: &[&str],
    home: &Path,
    input: Option<&str>,
    artifact_root: Option<&Path>,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_foundry"));
    command
        .args(args)
        .env("HOME", home)
        .env("FOUNDRY_INSTALLATION_ID", "cli-install")
        .env("FOUNDRY_DATABASE_ID", "cli-db")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(root) = artifact_root {
        command.env("FOUNDRY_PRODUCT_ARTIFACT_ROOT", root);
    }
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

fn private_file(path: &Path, value: &str) {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(value.as_bytes()).unwrap();
}

fn bootstrap_source() -> ScopeSourceManifest {
    ScopeSourceManifest {
        connector_id: "github-install".into(),
        source_instance_id: "cli-source".into(),
        generation: 1,
        identity: RepositoryIdentity::canonicalized(
            "acme",
            "github",
            "https://api.github.com",
            "repo:42",
        )
        .unwrap(),
        api_owner: Some("acme".into()),
        api_repo: Some("repo".into()),
        legacy_repository_locator: "repo:42".into(),
        scope_query: "open+foundry:make".into(),
        enabled: true,
    }
}

fn cli_bootstrap(
    root: &Path,
    db: &Path,
    source: &ScopeSourceManifest,
    password: &str,
) -> std::process::Output {
    let manifest_path = root.join("scope.json");
    let manifest = serde_json::json!({
        "sources": [source],
        "grants": [{
            "actor_id": "owner-cli",
            "source_instance_id": source.source_instance_id,
            "capabilities": ["read_order", "read_review", "approve_order"]
        }]
    });
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    run(
        &[
            "product-bootstrap",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            "cli-install",
            "--database-id",
            "cli-db",
            "--manifest",
            manifest_path.to_str().unwrap(),
        ],
        root,
        Some(&format!("owner-cli\n{password}\n")),
        None,
    )
}

fn named_arg(args: &[String], flag: &str) -> usize {
    args.iter()
        .position(|arg| arg == flag)
        .expect("named CLI flag")
        + 1
}

fn cli_approval_args(
    db: &Path,
    session: &Path,
    order: &str,
    grant_id: &str,
    grant_file: &Path,
    run_id: i64,
    generation: i64,
    artifact_digest: &str,
    tuple_digest: &str,
    request_key: &str,
) -> Vec<String> {
    [
        "product-approve".into(),
        "--db".into(),
        db.display().to_string(),
        "--installation-id".into(),
        "cli-install".into(),
        "--database-id".into(),
        "cli-db".into(),
        "--session-file".into(),
        session.display().to_string(),
        "--order".into(),
        order.into(),
        "--grant-id".into(),
        grant_id.into(),
        "--grant-file".into(),
        grant_file.display().to_string(),
        "--expected-source-revision".into(),
        "1".into(),
        "--expected-attempt".into(),
        "1".into(),
        "--run-id".into(),
        run_id.to_string(),
        "--generation".into(),
        generation.to_string(),
        "--artifact-digest".into(),
        artifact_digest.into(),
        "--tuple-digest".into(),
        tuple_digest.into(),
        "--intent".into(),
        "approve verified local review result".into(),
        "--idempotency-key".into(),
        request_key.into(),
    ]
    .into_iter()
    .collect()
}

#[cfg(feature = "p2-test-support")]
#[test]
fn product_approve_cli_consumes_genuine_result_and_replays_exact_receipt() {
    use foundry_ledger::ProductConfig;
    use product_result_fixture::{FixtureIdentity, genuine_host_done_with_bootstrap};
    use std::sync::atomic::Ordering as AtomicOrdering;

    let password = "approval-cli-secret-canary";
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: "cli-install".into(),
            database_id: "cli-db".into(),
        },
        "cli-source",
        "order-cli",
    );
    let mut fixture = genuine_host_done_with_bootstrap(
        identity,
        "owner-cli",
        password,
        |root, db, _config, source, _now, _owner, password| {
            let bootstrap = cli_bootstrap(root, db, source, password);
            assert!(
                bootstrap.status.success(),
                "CLI bootstrap exited {:?}: {}",
                bootstrap.status.code(),
                String::from_utf8_lossy(&bootstrap.stderr)
            );
            assert!(!String::from_utf8_lossy(&bootstrap.stdout).contains(password));
            assert!(!String::from_utf8_lossy(&bootstrap.stderr).contains(password));
        },
    );
    fixture.assert_genuine_private_handoff();
    let root = fixture.root.clone();
    let db = fixture.db_path.clone();
    let dbs = db.to_str().unwrap();
    let artifact_root = fixture.artifact_root.clone();
    let original_root_identity = fixture.artifact_root_device_inode;
    let expected = fixture.expected.clone();
    let terminal = fixture.terminal.as_ref().unwrap().clone();
    let calls = fixture.calls.clone();
    let restores = fixture.restores.clone();
    let mut result_store = fixture.result_store.take().unwrap();
    let result_info = result_store
        .finalize_product_review_result(&fixture.service, &expected)
        .expect("real ProductStore finalizer creates genuine result");
    let initial: (String, String, i64, i64, i64) = {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.query_row(
            "SELECT b.receipt_json,b.binding_digest,(SELECT count(*) FROM artifact_tuples),(SELECT count(*) FROM product_review_host_results_v1),(SELECT count(*) FROM product_review_runs_v1) FROM product_result_bindings_v1 b WHERE b.result_id=?1",
            [&result_info.result_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        ).unwrap()
    };
    let pinned_root: (String, i64, i64) = rusqlite::Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT r.root_path,i.device,i.inode FROM product_artifact_root_v1 r JOIN product_artifact_root_identity_v1 i USING(singleton)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert!(artifact_root.is_absolute());
    assert_eq!(PathBuf::from(&pinned_root.0), artifact_root);
    assert_eq!(
        (pinned_root.1 as u64, pinned_root.2 as u64),
        original_root_identity
    );
    assert_eq!(
        product_result_fixture::directory_identity(&artifact_root),
        original_root_identity,
    );
    drop(result_store);

    let session_file = root.join("session");
    let session_path = session_file.to_str().unwrap();
    let login = run(
        &[
            "product-login",
            "--db",
            dbs,
            "--installation-id",
            "cli-install",
            "--database-id",
            "cli-db",
            "--session-file",
            session_path,
        ],
        &root,
        Some(&format!("owner-cli\n{password}\n")),
        Some(&artifact_root),
    );
    assert!(
        login.status.success(),
        "{}",
        String::from_utf8_lossy(&login.stderr)
    );
    assert!(!String::from_utf8_lossy(&login.stdout).contains(password));
    assert!(!String::from_utf8_lossy(&login.stderr).contains(password));
    let session_bearer = std::fs::read_to_string(&session_file).unwrap();

    let holds = run(
        &[
            "product-review",
            "--db",
            dbs,
            "--installation-id",
            "cli-install",
            "--database-id",
            "cli-db",
            "--session-file",
            session_path,
            "--action",
            "holds",
        ],
        &root,
        None,
        Some(&artifact_root),
    );
    assert!(
        holds.status.success(),
        "{}",
        String::from_utf8_lossy(&holds.stderr)
    );
    assert!(!String::from_utf8_lossy(&holds.stdout).contains(&session_bearer));
    assert!(!String::from_utf8_lossy(&holds.stdout).contains(password));

    let grant_file = root.join("grant");
    let grant = run(
        &[
            "product-grant",
            "--db",
            dbs,
            "--installation-id",
            "cli-install",
            "--database-id",
            "cli-db",
            "--session-file",
            session_path,
            "--order",
            "order-cli",
            "--token-file",
            grant_file.to_str().unwrap(),
        ],
        &root,
        None,
        Some(&artifact_root),
    );
    assert!(
        grant.status.success(),
        "{}",
        String::from_utf8_lossy(&grant.stderr)
    );
    let grant_id = String::from_utf8_lossy(&grant.stdout)
        .trim()
        .strip_prefix("approval grant: ")
        .unwrap()
        .to_owned();
    let token = std::fs::read_to_string(&grant_file).unwrap();
    assert!(!String::from_utf8_lossy(&grant.stdout).contains(&token));
    let args = cli_approval_args(
        &db,
        &session_file,
        "order-cli",
        &grant_id,
        &grant_file,
        fixture.approval.run_id,
        result_info.generation,
        &terminal.result_commit,
        &result_info.tuple_digest,
        "cli-approval-idempotency",
    );
    let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let first = run(&arg_refs, &root, None, Some(&artifact_root));
    assert!(
        first.status.success(),
        "genuine CLI approval exited {:?}: {}",
        first.status.code(),
        String::from_utf8_lossy(&first.stderr)
    );
    let first_out = String::from_utf8_lossy(&first.stdout).trim().to_owned();
    assert!(first_out.starts_with("approval recorded: "));
    assert!(!first_out.contains(&token));
    assert!(!String::from_utf8_lossy(&first.stderr).contains(&token));

    // Every retry is a fresh CARGO_BIN_EXE_foundry process reopening the same
    // DB while the original pinned root remains available at its original path.
    let replay = run(&arg_refs, &root, None, Some(&artifact_root));
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&replay.stdout).trim(), first_out);

    let mut mismatch = args.clone();
    let tuple_operand = named_arg(&mismatch, "--tuple-digest");
    mismatch[tuple_operand] = "a".repeat(64);
    assert_eq!(mismatch[tuple_operand - 1], "--tuple-digest");
    assert_eq!(mismatch[tuple_operand], "a".repeat(64));
    let denied = run(
        &mismatch.iter().map(String::as_str).collect::<Vec<_>>(),
        &root,
        None,
        Some(&artifact_root),
    );
    assert!(!denied.status.success());
    let mismatch_error = String::from_utf8_lossy(&denied.stderr);
    assert!(
        mismatch_error.contains("product approval unavailable"),
        "{mismatch_error}"
    );
    assert!(!mismatch_error.contains(&token));

    let second_grant_file = root.join("second-grant");
    let second_grant = run(
        &[
            "product-grant",
            "--db",
            dbs,
            "--installation-id",
            "cli-install",
            "--database-id",
            "cli-db",
            "--session-file",
            session_path,
            "--order",
            "order-cli",
            "--token-file",
            second_grant_file.to_str().unwrap(),
        ],
        &root,
        None,
        Some(&artifact_root),
    );
    assert!(
        second_grant.status.success(),
        "{}",
        String::from_utf8_lossy(&second_grant.stderr)
    );
    let second_grant_id = String::from_utf8_lossy(&second_grant.stdout)
        .trim()
        .strip_prefix("approval grant: ")
        .unwrap()
        .to_owned();
    let revoked_args = cli_approval_args(
        &db,
        &session_file,
        "order-cli",
        &second_grant_id,
        &second_grant_file,
        fixture.approval.run_id,
        result_info.generation,
        &terminal.result_commit,
        &result_info.tuple_digest,
        "revoked-session-operation",
    );
    let reset = run(
        &[
            "product-reset",
            "--db",
            dbs,
            "--installation-id",
            "cli-install",
            "--database-id",
            "cli-db",
        ],
        &root,
        Some(&format!("owner-cli\n{password}\n")),
        Some(&artifact_root),
    );
    assert!(
        reset.status.success(),
        "{}",
        String::from_utf8_lossy(&reset.stderr)
    );
    let revoked_attempt = run(
        &revoked_args.iter().map(String::as_str).collect::<Vec<_>>(),
        &root,
        None,
        Some(&artifact_root),
    );
    assert!(
        !revoked_attempt.status.success(),
        "credential reset revokes prior CLI session"
    );
    let foreign_session_file = root.join("foreign-session");
    private_file(&foreign_session_file, &"e".repeat(64));
    let mut foreign_args = revoked_args.clone();
    let session_operand = named_arg(&foreign_args, "--session-file");
    foreign_args[session_operand] = foreign_session_file.display().to_string();
    assert_eq!(foreign_args[session_operand - 1], "--session-file");
    assert_eq!(
        foreign_args[session_operand],
        foreign_session_file.display().to_string()
    );
    let operation_operand = named_arg(&foreign_args, "--idempotency-key");
    foreign_args[operation_operand] = "foreign-session-operation".into();
    assert_eq!(foreign_args[operation_operand - 1], "--idempotency-key");
    assert_eq!(foreign_args[operation_operand], "foreign-session-operation");
    let foreign_attempt = run(
        &foreign_args.iter().map(String::as_str).collect::<Vec<_>>(),
        &root,
        None,
        Some(&artifact_root),
    );
    assert!(!foreign_attempt.status.success());
    assert!(
        String::from_utf8_lossy(&foreign_attempt.stderr).contains("product approval unavailable")
    );

    let conn = rusqlite::Connection::open(&db).unwrap();
    let counts: (i64, i64, i64, i64) = conn.query_row(
        "SELECT (SELECT count(*) FROM product_token_consumptions_v1),(SELECT count(*) FROM product_outcomes_v1),(SELECT count(*) FROM ledger WHERE to_state='publishing'),(SELECT count(*) FROM product_approval_result_bindings_v1)",
        [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
    ).unwrap();
    assert_eq!(counts, (1, 1, 1, 1));
    let final_result: (String, String, i64, i64, i64) = conn.query_row(
        "SELECT b.receipt_json,b.binding_digest,(SELECT count(*) FROM artifact_tuples),(SELECT count(*) FROM product_review_host_results_v1),(SELECT count(*) FROM product_review_runs_v1) FROM product_result_bindings_v1 b WHERE b.result_id=?1",
        [&result_info.result_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
    ).unwrap();
    assert_eq!(
        final_result, initial,
        "approval/replay leaves result receipt, tuple and execution evidence unchanged"
    );
    let final_root: (String, i64, i64) = conn
        .query_row(
            "SELECT r.root_path,i.device,i.inode FROM product_artifact_root_v1 r JOIN product_artifact_root_identity_v1 i USING(singleton)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        final_root, pinned_root,
        "approval CLI leaves original pinned artifact root intact"
    );
    let side: (String, String, String) = conn
        .query_row(
            "SELECT result_id,binding_digest,tuple_digest FROM product_approval_result_bindings_v1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        side,
        (
            result_info.result_id,
            result_info.binding_digest,
            result_info.tuple_digest
        )
    );
    let transition: (String, String, String, String) = conn.query_row(
        "SELECT r.operation,r.event_hash,l.hash,l.event_json FROM product_transition_receipts_v1 r JOIN ledger l ON l.seq=r.seq WHERE r.operation='human-approval'",
        [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
    ).unwrap();
    assert_eq!(transition.0, "human-approval");
    assert_eq!(transition.1, transition.2);
    assert!(transition.3.contains("\"type\":\"approved\""));
    drop(conn);
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 4);
    assert_eq!(restores.load(AtomicOrdering::SeqCst), 0);
    assert_eq!(fixture.host.as_ref().unwrap().dispatch_count(), 1);
    assert_eq!(
        product_result_fixture::directory_identity(&artifact_root),
        original_root_identity
    );
    drop(token);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn default_cli_bootstrap_login_and_unbound_result_rejection_are_preserved() {
    let (root, db) = scratch();
    let password = "default-cli-secret-canary";
    let source = bootstrap_source();
    let bootstrap = cli_bootstrap(&root, &db, &source, password);
    assert!(
        bootstrap.status.success(),
        "{}",
        String::from_utf8_lossy(&bootstrap.stderr)
    );
    assert!(!String::from_utf8_lossy(&bootstrap.stdout).contains(password));
    assert!(!String::from_utf8_lossy(&bootstrap.stderr).contains(password));

    let config = ProductConfig {
        installation_id: "cli-install".into(),
        database_id: "cli-db".into(),
    };
    let mut store = ProductStore::open(&db, config).unwrap();
    store
        .admit_order(
            "order-unbound-cli",
            "github-install",
            "repo:42",
            "issue:7",
            "dep-bump",
            "immutable unbound CLI test order",
            chrono::Utc::now(),
        )
        .unwrap();
    drop(store);

    let session_file = root.join("session");
    let login = run(
        &[
            "product-login",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            "cli-install",
            "--database-id",
            "cli-db",
            "--session-file",
            session_file.to_str().unwrap(),
        ],
        &root,
        Some(&format!("owner-cli\n{password}\n")),
        None,
    );
    assert!(
        login.status.success(),
        "{}",
        String::from_utf8_lossy(&login.stderr)
    );
    assert!(!String::from_utf8_lossy(&login.stdout).contains(password));
    assert!(!String::from_utf8_lossy(&login.stderr).contains(password));
    let holds = run(
        &[
            "product-review",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            "cli-install",
            "--database-id",
            "cli-db",
            "--session-file",
            session_file.to_str().unwrap(),
            "--action",
            "holds",
        ],
        &root,
        None,
        None,
    );
    assert!(
        holds.status.success(),
        "{}",
        String::from_utf8_lossy(&holds.stderr)
    );

    let grant_file = root.join("grant");
    let grant = run(
        &[
            "product-grant",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            "cli-install",
            "--database-id",
            "cli-db",
            "--session-file",
            session_file.to_str().unwrap(),
            "--order",
            "order-unbound-cli",
            "--token-file",
            grant_file.to_str().unwrap(),
        ],
        &root,
        None,
        None,
    );
    assert!(
        grant.status.success(),
        "{}",
        String::from_utf8_lossy(&grant.stderr)
    );
    let grant_id = String::from_utf8_lossy(&grant.stdout)
        .trim()
        .strip_prefix("approval grant: ")
        .unwrap()
        .to_owned();
    let args = cli_approval_args(
        &db,
        &session_file,
        "order-unbound-cli",
        &grant_id,
        &grant_file,
        1,
        1,
        &"f".repeat(40),
        &"a".repeat(64),
        "unbound-result-must-fail",
    );
    let denied = run(
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
        &root,
        None,
        None,
    );
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("product approval unavailable"));
    let conn = rusqlite::Connection::open(&db).unwrap();
    let counts: (i64, i64, i64, i64) = conn.query_row(
        "SELECT (SELECT count(*) FROM product_token_consumptions_v1),(SELECT count(*) FROM product_outcomes_v1),(SELECT count(*) FROM ledger WHERE to_state='publishing'),(SELECT count(*) FROM product_result_bindings_v1)",
        [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
    ).unwrap();
    assert_eq!(counts, (0, 0, 0, 0));
    std::fs::remove_dir_all(root).unwrap();
}
