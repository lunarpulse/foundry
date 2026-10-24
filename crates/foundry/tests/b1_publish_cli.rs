#![cfg(feature = "p2-test-support")]

use foundry_ledger::{ProductConfig, ScopeSourceManifest};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[path = "../../foundry-line/tests/support/product_result_fixture.rs"]
mod product_result_fixture;
use product_result_fixture::{FixtureIdentity, genuine_host_done_with_bootstrap};

const PASSWORD: &str = "publish-cli-secret-canary";

fn run(
    args: &[&str],
    home: &Path,
    input: Option<&str>,
    registry: Option<&Path>,
    artifact_root: Option<&Path>,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_foundry"));
    command
        .args(args)
        .env("HOME", home)
        .env("FOUNDRY_INSTALLATION_ID", "publish-install")
        .env("FOUNDRY_DATABASE_ID", "publish-db")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(registry) = registry {
        command.env("B1_PUBLISH_TEST_DIR", registry);
    } else {
        command.env_remove("B1_PUBLISH_TEST_DIR");
    }
    if let Some(artifact_root) = artifact_root {
        command.env("FOUNDRY_PRODUCT_ARTIFACT_ROOT", artifact_root);
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

fn cli_bootstrap(
    root: &Path,
    db: &Path,
    source: &ScopeSourceManifest,
    installation: &str,
    database: &str,
) -> std::process::Output {
    let manifest_path = root.join("scope.json");
    let manifest = serde_json::json!({
        "sources": [source],
        "grants": [{
            "actor_id": "owner-publish",
            "source_instance_id": source.source_instance_id,
            "capabilities": ["read_order", "read_review", "approve_order", "publish_approved"]
        }]
    });
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    run(
        &[
            "product-bootstrap",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            installation,
            "--database-id",
            database,
            "--manifest",
            manifest_path.to_str().unwrap(),
        ],
        root,
        Some(&format!("owner-publish\n{PASSWORD}\n")),
        None,
        None,
    )
}

struct Approved {
    root: PathBuf,
    db: PathBuf,
    session: PathBuf,
    receipt: PathBuf,
    operation_id: String,
}

fn genuine_approved(suffix: &str) -> Approved {
    let installation = format!("publish-install-{suffix}");
    let database = format!("publish-db-{suffix}");
    let order = format!("publish-order-{suffix}");
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: installation.clone(),
            database_id: database.clone(),
        },
        "publish-source",
        order.clone(),
    );
    let mut fixture = genuine_host_done_with_bootstrap(
        identity,
        "owner-publish",
        PASSWORD,
        |root, db, _config, source, _now, _owner, _password| {
            let output = cli_bootstrap(root, db, source, &installation, &database);
            assert!(
                output.status.success(),
                "bootstrap: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        },
    );
    let root = fixture.root.clone();
    let db = fixture.db_path.clone();
    let artifact_root = fixture.artifact_root.clone();
    let expected = fixture.expected.clone();
    let result = fixture
        .result_store
        .take()
        .unwrap()
        .finalize_product_review_result(&fixture.service, &expected)
        .unwrap();
    let session = root.join("owner.session");
    let login = run(
        &[
            "product-login",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            &installation,
            "--database-id",
            &database,
            "--session-file",
            session.to_str().unwrap(),
        ],
        &root,
        Some(&format!("owner-publish\n{PASSWORD}\n")),
        None,
        None,
    );
    assert!(
        login.status.success(),
        "login: {}",
        String::from_utf8_lossy(&login.stderr)
    );

    let grant_file = root.join("grant");
    let grant = run(
        &[
            "product-grant",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            &installation,
            "--database-id",
            &database,
            "--session-file",
            session.to_str().unwrap(),
            "--order",
            &order,
            "--token-file",
            grant_file.to_str().unwrap(),
        ],
        &root,
        None,
        None,
        None,
    );
    assert!(
        grant.status.success(),
        "grant: {}",
        String::from_utf8_lossy(&grant.stderr)
    );
    let grant_id = String::from_utf8_lossy(&grant.stdout)
        .trim()
        .strip_prefix("approval grant: ")
        .unwrap()
        .to_owned();
    let approve = run(
        &[
            "product-approve",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            &installation,
            "--database-id",
            &database,
            "--session-file",
            session.to_str().unwrap(),
            "--order",
            &order,
            "--grant-id",
            &grant_id,
            "--grant-file",
            grant_file.to_str().unwrap(),
            "--expected-source-revision",
            "1",
            "--expected-attempt",
            "1",
            "--run-id",
            &fixture.approval.run_id.to_string(),
            "--generation",
            &result.generation.to_string(),
            "--artifact-digest",
            &fixture.terminal.as_ref().unwrap().result_commit,
            "--tuple-digest",
            &result.tuple_digest,
            "--intent",
            "publish cli fixture approval",
            "--idempotency-key",
            &format!("approve-{suffix}"),
            "--receipt-out",
            root.join("receipt.json").to_str().unwrap(),
        ],
        &root,
        None,
        None,
        Some(&artifact_root),
    );
    assert!(
        approve.status.success(),
        "approval: {}",
        String::from_utf8_lossy(&approve.stderr)
    );
    assert!(String::from_utf8_lossy(&approve.stdout).starts_with("approval recorded: "));
    let receipt = root.join("receipt.json");
    let receipt_json: foundry_ledger::ProductApprovalReceipt =
        serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
    assert_eq!(receipt_json.order_id, order);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&receipt).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    drop(fixture);
    Approved {
        root,
        db,
        session,
        receipt,
        operation_id: receipt_json.operation_id,
    }
}

fn publish_args(fixture: &Approved) -> Vec<String> {
    [
        "product".to_owned(),
        "publish".to_owned(),
        "--db".into(),
        fixture.db.display().to_string(),
        "--installation-id".into(),
        "".into(),
        "--database-id".into(),
        "".into(),
        "--session-file".into(),
        fixture.session.display().to_string(),
        "--receipt-file".into(),
        fixture.receipt.display().to_string(),
    ]
    .into_iter()
    .collect()
}

fn publish_args_for(fixture: &Approved, suffix: &str) -> Vec<String> {
    let mut args = publish_args(fixture);
    args[5] = format!("publish-install-{suffix}");
    args[7] = format!("publish-db-{suffix}");
    args
}

fn publish(fixture: &Approved, args: &[String], registry: Option<&Path>) -> std::process::Output {
    run(
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
        &fixture.root,
        None,
        registry,
        None,
    )
}

fn publish_rows(db: &Path) -> (i64, i64, Option<String>) {
    rusqlite::Connection::open(db).unwrap().query_row(
        "SELECT count(*),sum(state='confirmed'),max(receipt_json) FROM product_publish_outcomes_v1",
        [],
        |row| Ok((row.get(0)?, row.get::<_, Option<i64>>(1)?.unwrap_or(0), row.get(2)?)),
    ).unwrap()
}

fn registry_pr_count(registry: &Path) -> usize {
    std::fs::read_dir(registry)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "pr")
        })
        .count()
}

fn publisher_call_count(registry: &Path) -> usize {
    std::fs::read_to_string(registry.join("publisher-invocations.log"))
        .map(|calls| calls.lines().count())
        .unwrap_or(0)
}

#[test]
fn v1_positive_and_v2_confirmed_replay() {
    let approved = genuine_approved("positive");
    let registry = approved.root.join("test-pr-registry");
    let args = publish_args_for(&approved, "positive");
    let first = publish(&approved, &args, Some(&registry));
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let output = String::from_utf8_lossy(&first.stdout).trim().to_owned();
    assert_eq!(
        output,
        format!("https://example.invalid/pr/{}", approved.operation_id)
    );
    let (count, confirmed, receipt_json) = publish_rows(&approved.db);
    assert_eq!((count, confirmed), (1, 1));
    let expected = std::fs::read_to_string(&approved.receipt).unwrap();
    assert_eq!(
        receipt_json.as_deref(),
        Some(expected.trim_end_matches(['\r', '\n']))
    );
    assert_eq!(registry_pr_count(&registry), 1);
    assert_eq!(publisher_call_count(&registry), 1);
    let registry_files = std::fs::read_dir(&registry).unwrap().count();

    let replay = publish(&approved, &args, Some(&registry));
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&replay.stdout).trim(), output);
    assert_eq!(publish_rows(&approved.db).0, 1);
    assert_eq!(registry_pr_count(&registry), 1);
    assert_eq!(
        std::fs::read_dir(&registry).unwrap().count(),
        registry_files
    );
    assert_eq!(publisher_call_count(&registry), 1);
}

#[test]
fn v4_publish_denials_leave_no_rows_or_prs() {
    let approved = genuine_approved("denials-a");
    let foreign = genuine_approved("denials-b");
    let registry = approved.root.join("test-pr-registry");
    std::fs::create_dir_all(&registry).unwrap();
    let base = publish_args_for(&approved, "denials-a");

    let bad_session = approved.root.join("bad.session");
    std::fs::write(&bad_session, "not-a-session\n").unwrap();
    let mut args = base.clone();
    let at = args.iter().position(|arg| arg == "--session-file").unwrap() + 1;
    args[at] = bad_session.display().to_string();
    let result = publish(&approved, &args, Some(&registry));
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(publish_rows(&approved.db).0, 0);

    let corrupt = approved.root.join("corrupt.json");
    std::fs::write(&corrupt, b"{\"not\":\"a receipt\"").unwrap();
    let mut args = base.clone();
    let at = args.iter().position(|arg| arg == "--receipt-file").unwrap() + 1;
    args[at] = corrupt.display().to_string();
    let result = publish(&approved, &args, Some(&registry));
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(publish_rows(&approved.db).0, 0);

    let unknown = approved.root.join("unknown.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&approved.receipt).unwrap()).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("unexpected".into(), serde_json::json!(true));
    std::fs::write(&unknown, serde_json::to_vec(&value).unwrap()).unwrap();
    let mut args = base.clone();
    let at = args.iter().position(|arg| arg == "--receipt-file").unwrap() + 1;
    args[at] = unknown.display().to_string();
    let result = publish(&approved, &args, Some(&registry));
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(publish_rows(&approved.db).0, 0);

    let mut args = publish_args_for(&approved, "denials-a");
    let at = args.iter().position(|arg| arg == "--receipt-file").unwrap() + 1;
    args[at] = foreign.receipt.display().to_string();
    let result = publish(&approved, &args, Some(&registry));
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(publish_rows(&approved.db).0, 0);

    let result = publish(&approved, &base, None);
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("no publish adapter configured"));
    assert_eq!(publish_rows(&approved.db).0, 0);
    assert_eq!(registry_pr_count(&registry), 0);
    assert_eq!(publisher_call_count(&registry), 0);
}
