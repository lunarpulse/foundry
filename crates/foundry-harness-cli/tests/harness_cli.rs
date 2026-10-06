//! T6 — CLI adapter tests: env isolation + FakeHarness determinism.
use foundry_domain::ports::Harness;
use foundry_domain::{ExternalRef, Order};
use foundry_harness_cli::fake::FakeHarness;
use foundry_harness_cli::CliHarness;
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
    assert!(matches!(err, foundry_domain::ports::PortError::Permanent(_)));
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
        std::process::Command::new("git").args(args).current_dir(&repo).output().unwrap();
    };
    g(&["init", "-q"]);
    g(&["config", "user.email", "t@t.local"]);
    g(&["config", "user.name", "t"]);
    std::fs::write(repo.join("f.txt"), "x").unwrap();
    g(&["add", "-A"]);
    g(&["commit", "-qm", "seed"]);

    let sh = dir.join("die.sh");
    std::fs::write(&sh, "#!/bin/sh\nexec /usr/bin/env\n").unwrap();
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
