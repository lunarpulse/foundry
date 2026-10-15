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
