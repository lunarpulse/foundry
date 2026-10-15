use foundry_line::{DieStageExecutor, StageExecutor, StageTask};
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

fn root() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("die-executor-{nonce}"));
    fs::create_dir_all(&path).unwrap();
    path
}

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn repo(root: &std::path::Path) -> String {
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "test@example.invalid"]);
    git(&repo, &["config", "user.name", "test"]);
    fs::write(repo.join("base.txt"), "base\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "base"]);
    git(&repo, &["rev-parse", "HEAD"])
}

fn make_executor(
    root: &std::path::Path,
    repo_path: &std::path::Path,
    mode: &str,
) -> DieStageExecutor {
    let binary = root.join("fake-die.sh");
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-die.sh");
    let script = fs::read_to_string(fixture)
        .unwrap()
        .replace("FAKE_DIE_MODE=normal", &format!("FAKE_DIE_MODE={mode}"));
    fs::write(&binary, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    }
    DieStageExecutor::new(
        binary,
        "coding".into(),
        10,
        repo_path.to_path_buf(),
        root.join("sessions"),
    )
}

fn task(root: &std::path::Path, base: &str, fix_context: Option<String>) -> StageTask {
    StageTask {
        stage_id: "implement-g1".into(),
        role: "implement".into(),
        lens_bytes: vec![],
        session_id: "order-1:implement".into(),
        fix_context,
        base_commit: base.into(),
        workdir: root.join("work"),
        artifact_commit: base.into(),
        inputs: BTreeMap::from([
            ("input".into(), "value".into()),
            ("alpha".into(), "first".into()),
        ]),
    }
}

#[test]
fn fake_die_commits_and_writes_the_manifest_bound_output() {
    let root = root();
    let base = repo(&root);
    let mut executor = make_executor(&root, &root.join("repo"), "normal");
    executor.ensure_available().unwrap();
    let output = executor.execute(&task(&root, &base, None)).unwrap();
    let commit = output.produced_commit.as_deref().unwrap();
    assert_eq!(git(&root.join("work"), &["rev-parse", "HEAD"]), commit);
    assert_eq!(git(&root.join("work"), &["rev-parse", "HEAD^"]), base);
    assert!(!output.findings);
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("work/input-manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["task"], "implement:implement-g1");
    assert_eq!(manifest["base"], base);
    assert_eq!(
        manifest["context"],
        serde_json::json!(["alpha=first", "input=value"])
    );
    assert!(root.join("sessions/order-1:implement.json").exists());
    let resumed_workdir = root.join("resumed-work");
    executor
        .restore_session("order-1:implement", &resumed_workdir)
        .unwrap();
    let mut resumed_task = task(&root, &base, None);
    resumed_task.stage_id = "implement-g2".into();
    resumed_task.workdir = resumed_workdir.clone();
    executor.execute(&resumed_task).unwrap();
    assert!(
        resumed_workdir
            .join(".die-session/order-1:implement.json")
            .exists()
    );
    let resumed_manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(resumed_workdir.join("input-manifest.json")).unwrap())
            .unwrap();
    assert!(
        resumed_manifest["context"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry
                .as_str()
                .unwrap()
                .starts_with("previous_task=implement:implement-g1"))
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn forged_commit_is_rejected_and_unrelated_fix_base_is_binding_mismatch() {
    let root = root();
    let base = repo(&root);
    let mut executor = make_executor(&root, &root.join("repo"), "forged");
    let error = executor.execute(&task(&root, &base, None)).unwrap_err();
    assert!(!error.is_empty(), "forged produced_commit must be rejected");

    let root2 = root.join("mismatch");
    fs::create_dir_all(&root2).unwrap();
    let base2 = repo(&root2);
    let mut executor2 = make_executor(&root2, &root2.join("repo"), "normal");
    let other = "b".repeat(40);
    let error = executor2
        .execute(&task(&root2, &base2, Some(other)))
        .unwrap_err();
    assert!(error.contains("BindingMismatch"), "{error}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn fake_die_failure_mode_returns_an_execution_error() {
    let root = root();
    let base = repo(&root);
    let mut executor = make_executor(&root, &root.join("repo"), "fail");
    let error = executor.execute(&task(&root, &base, None)).unwrap_err();
    assert!(error.contains("exited with code 9"), "{error}");
    let _ = fs::remove_dir_all(root);
}

#[tokio::test(flavor = "current_thread")]
async fn execution_is_safe_inside_the_serve_runtime() {
    let root = root();
    let base = repo(&root);
    let mut executor = make_executor(&root, &root.join("repo"), "normal");
    executor.execute(&task(&root, &base, None)).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn die_processes_cannot_keep_writing_after_success_or_timeout() {
    for mode in ["timeout", "background"] {
        let root = root();
        let base = repo(&root);
        make_executor(&root, &root.join("repo"), mode);
        let mut executor = DieStageExecutor::new(
            root.join("fake-die.sh"),
            "coding".into(),
            1,
            root.join("repo"),
            root.join("sessions"),
        );
        let task = task(&root, &base, None);
        let result = executor.execute(&task);
        if mode == "timeout" {
            assert!(result.unwrap_err().contains("timeout"));
        } else {
            result.unwrap();
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
        assert!(
            !task.workdir.join("late-write").exists(),
            "{mode} left a writer alive"
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn verified_generations_are_available_to_fresh_workdirs_after_restart() {
    let root = root();
    let base = repo(&root);
    let mut executor = make_executor(&root, &root.join("repo"), "normal");
    let first = executor
        .execute(&task(&root, &base, None))
        .unwrap()
        .produced_commit
        .unwrap();
    drop(executor);
    let mut verifier = make_executor(&root, &root.join("repo"), "verify");
    let mut check = task(&root, &base, None);
    check.role = "verify".into();
    check.stage_id = "verify-g1".into();
    check.base_commit = first.clone();
    check.artifact_commit = first.clone();
    check.workdir = root.join("verify-work");
    assert_eq!(
        verifier.execute(&check).unwrap().inspected_commit,
        Some(first.clone())
    );
    let mut executor = make_executor(&root, &root.join("repo"), "normal");
    let mut next = task(&root, &first, Some(first.clone()));
    next.stage_id = "implement-g2".into();
    next.workdir = root.join("next-work");
    executor
        .restore_session(&next.session_id, &next.workdir)
        .unwrap();
    let second = executor.execute(&next).unwrap().produced_commit.unwrap();
    assert_eq!(git(&next.workdir, &["rev-parse", "HEAD^"]), first);
    assert_eq!(git(&root.join("repo"), &["rev-parse", "HEAD"]), base);
    assert_ne!(second, base);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn fresh_output_and_unmodified_manifest_are_required() {
    for mode in ["missing", "malformed", "manifest", "unrelated", "replace"] {
        let root = root();
        let base = repo(&root);
        let mut executor = make_executor(&root, &root.join("repo"), mode);
        let error = executor.execute(&task(&root, &base, None)).unwrap_err();
        assert!(
            !root.join("sessions/order-1:implement.json").exists(),
            "{mode}"
        );
        assert!(!root.join("escaped.output.json").exists(), "{mode}");
        if matches!(mode, "unrelated" | "replace") {
            assert!(error.contains("BindingMismatch"), "{error}");
        }
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn checked_in_output_cannot_substitute_for_a_missing_die_result() {
    let root = root();
    repo(&root);
    let source = root.join("repo");
    fs::write(
        source.join("die-output.json"),
        r#"{"findings":false,"verdict_categories":[]}"#,
    )
    .unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "-qm", "stale output"]);
    let base = git(&source, &["rev-parse", "HEAD"]);
    let mut executor = make_executor(&root, &source, "missing");
    let mut check = task(&root, &base, None);
    check.role = "verify".into();
    assert!(
        executor
            .execute(&check)
            .unwrap_err()
            .contains("die-output.json")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn nonempty_workdir_is_preserved_and_missing_restore_fails_closed() {
    let root = root();
    let base = repo(&root);
    let mut executor = make_executor(&root, &root.join("repo"), "normal");
    let task = task(&root, &base, None);
    fs::create_dir_all(&task.workdir).unwrap();
    fs::write(task.workdir.join("keep"), "precious").unwrap();
    assert!(executor.execute(&task).is_err());
    assert_eq!(
        fs::read_to_string(task.workdir.join("keep")).unwrap(),
        "precious"
    );
    assert!(
        executor
            .restore_session("missing", &root.join("restore"))
            .is_err()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn verification_requires_its_role_schema() {
    let root = root();
    let base = repo(&root);
    let mut executor = make_executor(&root, &root.join("repo"), "schema");
    let mut check = task(&root, &base, None);
    check.role = "verify".into();
    assert!(
        executor
            .execute(&check)
            .unwrap_err()
            .contains("verdict_categories")
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn real_die_adapter_reviews_and_promotes_the_final_fix_generation() {
    exercise_adapter("review").await;
}

#[tokio::test(flavor = "current_thread")]
async fn forged_fix_is_held_without_issuing_a_tuple() {
    exercise_adapter("review-forged").await;
}

async fn exercise_adapter(mode: &str) {
    use foundry_exec::{
        Attempt, Epoch, OrderId, ReviewRunPort, ReviewRunRequest, RunId, SqliteTupleStore,
        TupleStore,
    };
    use std::sync::Arc;
    let root = root();
    let base = repo(&root);
    let source = root.join("repo");
    fs::write(source.join("needs-fix"), "review this change").unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "-qm", "initial artifact"]);
    let result = git(&source, &["rev-parse", "HEAD"]);
    let executor = make_executor(&root, &source, mode);
    let db = root.join("host.db");
    let pack = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
    let host = foundry_line::PackReviewHost::new(pack, executor, &db)
        .unwrap()
        .with_source_commit("7ec94ef837db9282e5a3f604fb0282d27ce8071e")
        .unwrap();
    let tuples =
        Arc::new(SqliteTupleStore::open(root.join("tuples.db")).unwrap()) as Arc<dyn TupleStore>;
    let mut runner = foundry_line::durable_review_run(host, &db, tuples.clone()).unwrap();
    let outcome = runner
        .run(ReviewRunRequest {
            order: OrderId("die-e2e".into()),
            attempt: Attempt(1),
            run: RunId(1),
            base_commit: base,
            result_commit: result.clone(),
            artifact_digest: result.clone(),
            epoch: Epoch(1),
            activation: 1,
        })
        .unwrap();
    if mode == "review-forged" {
        assert!(matches!(outcome, foundry_exec::ReviewRunOutcome::Held(_)));
        assert!(
            tuples
                .latest(&OrderId("die-e2e".into()), Attempt(1))
                .unwrap()
                .is_none()
        );
        assert!(
            tuples
                .latest_promoted(&OrderId("die-e2e".into()), Attempt(1))
                .unwrap()
                .is_none()
        );
    } else {
        assert!(
            matches!(outcome, foundry_exec::ReviewRunOutcome::Completed(e)
        if e.verdict == foundry_exec::ReviewVerdict::Passed)
        );
        let tuple = tuples
            .latest_promoted(&OrderId("die-e2e".into()), Attempt(1))
            .unwrap()
            .unwrap();
        assert_eq!(tuple.generations.len(), 2);
        assert_eq!(tuple.generations[0].commit, result);
        assert_eq!(tuple.immutable_commit.0, tuple.generations[1].commit);
        assert_ne!(tuple.immutable_commit.0, result);
    }
    let connection = rusqlite::Connection::open(&db).unwrap();
    let context: String = connection.query_row(
        "SELECT json_extract(output_json,'$.input_manifest.context') FROM review_stage_attempts WHERE stage_id='claims' ORDER BY sequence LIMIT 1",
        [], |row| row.get(0),
    ).unwrap();
    let context: Vec<String> = serde_json::from_str(&context).unwrap();
    assert!(
        context
            .iter()
            .any(|entry| entry == "previous_task=lens:edge")
    );
    let output_path = context
        .iter()
        .find_map(|entry| entry.strip_prefix("previous_die_output="))
        .unwrap();
    let prior: serde_json::Value = serde_json::from_slice(&fs::read(output_path).unwrap()).unwrap();
    assert_eq!(prior["inspected_commit"], result);
    drop(connection);
    assert_eq!(git(&source, &["rev-parse", "HEAD"]), result);
    drop(runner);
    fs::remove_dir_all(root).unwrap();
}
