//! T6 — CLI adapter tests: env isolation + FakeHarness determinism.
use foundry_domain::ports::Harness;
use foundry_domain::{ExternalRef, Order};
use foundry_harness_cli::{BaseSelection, CliHarness, DieProtocol, InputManifest};
use foundry_harness_cli::fake::FakeHarness;
use std::path::PathBuf;

fn order() -> Order {
    Order::new(
        "o-h1".into(),
        ExternalRef::cron("dep", "serde"),
        chrono::Utc::now(),
    )
}

#[tokio::test]
async fn fake_harness_records_calls_and_succeeds() {
    let h = FakeHarness::default();
    let o = order();
    let out = h.run(&o, 1, &PathBuf::from("/tmp")).await.unwrap();
    assert_eq!(out.attempt, 1);
    assert_eq!(out.exit_code, 0);
    assert_eq!(h.calls(), vec![("o-h1".to_string(), 1)]);
}

#[tokio::test]
async fn fake_harness_scripted_failures_are_retryable() {
    let h = FakeHarness::fail_first(2);
    let o = order();
    assert!(h.run(&o, 1, &PathBuf::from("/tmp")).await.is_err());
    assert!(h.run(&o, 2, &PathBuf::from("/tmp")).await.is_err());
    assert!(h.run(&o, 3, &PathBuf::from("/tmp")).await.is_ok());
    assert_eq!(h.calls().len(), 3);
}

#[tokio::test]
async fn missing_binary_is_permanent_error() {
    let h = CliHarness::new("/nonexistent/foundry-die", "coding", 30);
    let o = order();
    let err = h.run(&o, 1, &PathBuf::from("/tmp")).await.unwrap_err();
    assert!(matches!(
        err,
        foundry_domain::ports::PortError::Permanent(_)
    ));
}

#[tokio::test]
async fn die_env_is_allowlisted() {
    // Wrapper script ignores harness args and dumps ITS env via /usr/bin/env.
    // Assert: FOUNDRY_* present, publish credentials absent.
    let dir = std::env::temp_dir().join(format!("foundry-h-env-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();

    // 2026-10-07 contract: a Die exiting 0 with NO commit anywhere is a
    // Permanent error (no silent no-ops). Seed dir/repo so this wrapper
    // yields a HEAD — this also exercises the nested-repo HEAD resolution.
    let g = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap();
    };
    g(&["init", "-q"]);
    g(&["config", "user.email", "t@t.local"]);
    g(&["config", "user.name", "t"]);
    std::fs::write(repo.join("f.txt"), "x").unwrap();
    g(&["add", "-A"]);
    g(&["commit", "-qm", "seed"]);

    let sh = dir.join("die.sh");
    std::fs::write(&sh, "#!/bin/sh\n/usr/bin/env\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let h = CliHarness::new(&sh, "coding", 30);
    let o = order();
    let out = h.run(&o, 1, &dir).await.unwrap();
    assert_eq!(out.exit_code, 0, "wrapper must succeed");
    assert!(
        !out.result_commit.is_empty(),
        "HEAD must be resolved from workdir/repo when workdir is not a repo"
    );
    let log = std::fs::read_to_string(&out.log_path).unwrap_or_default();

    assert!(
        log.contains("FOUNDRY_ORDER_ID=o-h1"),
        "FOUNDRY_ORDER_ID must be in Die env; log:\n{log}"
    );
    assert!(
        log.contains("FOUNDRY_ATTEMPT=1"),
        "FOUNDRY_ATTEMPT must be in Die env; log:\n{log}"
    );
    assert!(
        !log.contains("OPENROUTER_API_KEY") && !log.contains("GITHUB_TOKEN"),
        "publish credentials must NOT leak into Die env:\n{log}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "foundry-h-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn seed_repo(repo: &std::path::Path, origin_main: bool) -> String {
    std::fs::create_dir_all(repo).unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        output
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t.local"]);
    git(&["config", "user.name", "t"]);
    std::fs::write(repo.join("f.txt"), "seed").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "seed"]);
    let head = String::from_utf8(git(&["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_string();
    if origin_main {
        git(&["update-ref", "refs/remotes/origin/main", &head]);
    }
    head
}

fn executable_script(dir: &std::path::Path, script: &str) -> PathBuf {
    let path = dir.join("die.sh");
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

#[tokio::test]
async fn manifest_is_atomically_written_with_protocol_values() {
    let dir = temp_dir("manifest");
    seed_repo(&dir.join("repo"), false);
    let binary = executable_script(&dir, "#!/bin/sh\nexit 0\n");
    let protocol = DieProtocol {
        base: BaseSelection::Latest,
        fix_context: true,
        context: vec!["src/main.rs".into(), "README.md".into()],
    };
    let h = CliHarness::new(binary, "coding", 30).with_protocol(protocol);
    let test_order = order();
    let expected_task = test_order.external.dedup_key.clone();
    let out = h.run(&test_order, 7, &dir).await.unwrap();
    let bytes = std::fs::read(dir.join("input-manifest.json")).unwrap();
    let manifest: InputManifest = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        manifest,
        InputManifest {
            order_id: "o-h1".into(),
            attempt: 7,
            task: expected_task,
            profile: "coding".into(),
            base: "latest".into(),
            fix_context: true,
            context: vec!["src/main.rs".into(), "README.md".into()],
        }
    );
    assert!(out.base_commit.is_empty(), "latest preserves existing workdir HEAD semantics");
    assert!(std::fs::read_dir(&dir)
        .unwrap()
        .all(|entry| !entry.unwrap().file_name().to_string_lossy().ends_with(".tmp")));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn fix_context_is_forwarded_as_cli_flag() {
    let dir = temp_dir("fix-context");
    seed_repo(&dir.join("repo"), false);
    let binary = executable_script(&dir, "#!/bin/sh\nprintf '%s\\n' \"$@\"\nexit 0\n");
    let h = CliHarness::new(binary, "coding", 30).with_protocol(DieProtocol {
        fix_context: true,
        ..DieProtocol::default()
    });
    h.run(&order(), 1, &dir).await.unwrap();
    let args = std::fs::read_to_string(dir.join("attempt-1.log")).unwrap();
    assert!(args.lines().any(|line| line == "--fix-context"), "args: {args}");
    assert!(args.contains("--base\nlatest"), "args: {args}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn origin_base_uses_origin_main_commit() {
    let dir = temp_dir("origin-base");
    let expected = seed_repo(&dir.join("repo"), true);
    let binary = executable_script(&dir, "#!/bin/sh\nexit 0\n");
    let h = CliHarness::new(binary, "coding", 30).with_protocol(DieProtocol {
        base: BaseSelection::Origin,
        ..DieProtocol::default()
    });
    let out = h.run(&order(), 1, &dir).await.unwrap();
    assert_eq!(out.base_commit, expected);
    let manifest: InputManifest = serde_json::from_slice(
        &std::fs::read(dir.join("input-manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest.base, "origin");
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn missing_origin_main_is_permanent_and_does_not_run_die() {
    let dir = temp_dir("origin-missing");
    seed_repo(&dir.join("repo"), false);
    let marker = dir.join("die-ran");
    let binary = executable_script(
        &dir,
        &format!("#!/bin/sh\ntouch '{}'\nexit 0\n", marker.display()),
    );
    let h = CliHarness::new(binary, "coding", 30).with_protocol(DieProtocol {
        base: BaseSelection::Origin,
        ..DieProtocol::default()
    });
    let err = h.run(&order(), 1, &dir).await.unwrap_err();
    assert!(matches!(err, foundry_domain::ports::PortError::Permanent(_)));
    assert!(!marker.exists(), "Die must not run without origin/main");
    assert!(!dir.join("input-manifest.json").exists());
    std::fs::remove_dir_all(dir).unwrap();
}
