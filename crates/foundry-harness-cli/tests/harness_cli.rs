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
    // Distinguish the remote base from HEAD so a HEAD-for-Origin regression fails.
    let advanced = std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-qm", "advanced-head"])
        .current_dir(dir.join("repo")).status().unwrap();
    assert!(advanced.success());
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


#[tokio::test]
async fn latest_keeps_post_die_workdir_head_semantics() {
    let dir = temp_dir("latest-head");
    let before = seed_repo(&dir, false);
    let binary = executable_script(&dir, "#!/bin/sh\n/usr/bin/git -c user.name=t -c user.email=t@t.local commit --allow-empty -qm die\n");
    let out = CliHarness::new(binary, "coding", 30).run(&order(), 1, &dir).await.unwrap();
    assert_ne!(out.result_commit, before);
    assert_eq!(out.base_commit, out.result_commit, "Latest must retain the pre-T30 post-Die lookup");
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn origin_refuses_parent_repository_discovery() {
    let dir = temp_dir("origin-parent");
    seed_repo(&dir, true);
    std::fs::create_dir(dir.join("repo")).unwrap();
    let binary = executable_script(&dir, "#!/bin/sh\ntouch die-ran\n");
    let h = CliHarness::new(binary, "coding", 30).with_protocol(DieProtocol {
        base: BaseSelection::Origin,
        ..DieProtocol::default()
    });
    let error = h.run(&order(), 1, &dir).await.unwrap_err();
    assert!(matches!(error, foundry_domain::ports::PortError::Permanent(_)));
    assert!(!dir.join("die-ran").exists());
    assert!(!dir.join("input-manifest.json").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn origin_result_uses_the_same_nested_repository_as_base() {
    let dir = temp_dir("origin-result");
    seed_repo(&dir, true);
    let before = seed_repo(&dir.join("repo"), true);
    let binary = executable_script(&dir, "#!/bin/sh\ncd repo\n/usr/bin/git -c user.name=t -c user.email=t@t.local commit --allow-empty -qm die\n");
    let h = CliHarness::new(binary, "coding", 30).with_protocol(DieProtocol {
        base: BaseSelection::Origin,
        ..DieProtocol::default()
    });
    let out = h.run(&order(), 1, &dir).await.unwrap();
    let result = std::process::Command::new("git").args(["rev-parse", "HEAD"])
        .current_dir(dir.join("repo")).output().unwrap();
    assert!(result.status.success());
    assert_eq!(out.base_commit, before);
    assert_eq!(out.result_commit, String::from_utf8(result.stdout).unwrap().trim());
    assert_ne!(out.result_commit, out.base_commit);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn manifest_rename_failure_cleans_up_and_can_be_retried() {
    let dir = temp_dir("rename-failure");
    seed_repo(&dir.join("repo"), false);
    let target = dir.join("input-manifest.json");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("old"), "untouched").unwrap();
    let binary = executable_script(&dir, "#!/bin/sh\ntouch die-ran\n");
    let h = CliHarness::new(binary, "coding", 30);
    for _ in 0..2 {
        let err = h.run(&order(), 1, &dir).await.unwrap_err();
        assert!(matches!(err, foundry_domain::ports::PortError::Permanent(ref message) if message.contains("rename input manifest")));
        assert_eq!(std::fs::read_to_string(target.join("old")).unwrap(), "untouched");
        assert!(!dir.join("die-ran").exists());
        assert!(std::fs::read_dir(&dir).unwrap().all(|entry| !entry.unwrap().file_name().to_string_lossy().ends_with(".tmp")));
    }
    std::fs::remove_dir_all(target).unwrap();
    h.run(&order(), 1, &dir).await.unwrap();
    assert!(dir.join("die-ran").exists());
    let manifest: InputManifest = serde_json::from_slice(&std::fs::read(dir.join("input-manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest.attempt, 1);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn manifest_replacement_keeps_open_readers_on_the_complete_old_document() {
    use std::io::Read;
    let dir = temp_dir("manifest-reader");
    seed_repo(&dir.join("repo"), false);
    let binary = executable_script(&dir, "#!/bin/sh\ncp input-manifest.json consumed.json\n");
    let h = CliHarness::new(binary, "coding", 30);
    h.run(&order(), 1, &dir).await.unwrap();
    let target = dir.join("input-manifest.json");
    let old = std::fs::read(&target).unwrap();
    let mut reader = std::fs::File::open(&target).unwrap();
    let protocol = DieProtocol { context: vec!["x".repeat(1024 * 1024)], ..DieProtocol::default() };
    h.with_protocol(protocol.clone()).run(&order(), 2, &dir).await.unwrap();
    let mut held_bytes = Vec::new();
    reader.read_to_end(&mut held_bytes).unwrap();
    assert_eq!(held_bytes, old, "an open reader must never observe an in-place partial replacement");
    let manifest: InputManifest = serde_json::from_slice(&std::fs::read(&target).unwrap()).unwrap();
    assert_eq!(manifest.attempt, 2);
    assert_eq!(manifest.context, protocol.context);
    assert_eq!(std::fs::read(dir.join("consumed.json")).unwrap(), std::fs::read(&target).unwrap(), "Die must read the complete new manifest at launch");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn input_manifest_rejects_unknown_fields() {
    let valid = serde_json::json!({
        "order_id": "o-h1", "attempt": 1, "task": "task", "profile": "coding",
        "base": "latest", "fix_context": false, "context": []
    });
    assert!(serde_json::from_value::<InputManifest>(valid.clone()).is_ok());
    let mut extra = valid;
    extra["unrecognized"] = serde_json::json!(true);
    assert!(serde_json::from_value::<InputManifest>(extra).is_err());
}

// Environment mutations stay in an isolated test process, avoiding unsafe set_var
// and races with the rest of the Cargo test suite.
#[test]
fn origin_output_and_environment_are_checked_in_isolated_processes() {
    let dir = temp_dir("origin-isolation");
    let expected = seed_repo(&dir.join("repo"), true);
    let foreign = dir.join("foreign");
    seed_repo(&foreign, true);
    let foreign_commit = std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-qm", "foreign"])
        .current_dir(&foreign).status().unwrap();
    assert!(foreign_commit.success());
    let foreign_ref = std::process::Command::new("git")
        .args(["update-ref", "refs/remotes/origin/main", "HEAD"])
        .current_dir(&foreign).status().unwrap();
    assert!(foreign_ref.success());
    let mockbin = dir.join("mockbin");
    std::fs::create_dir(&mockbin).unwrap();
    let git = mockbin.join("git");
    std::fs::write(&git, format!("#!/bin/sh\ncase \"$*\" in\n  *origin/main*) /bin/cat '{}'; exit 0 ;;\nesac\nexec /usr/bin/git \"$@\"\n", dir.join("git-output").display())).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut failures = Vec::new();
    for (mode, output) in [
        ("padded", format!(" \t{expected}\n\t ")),
        ("multiline", format!("{expected}\n{expected}\n")),
        ("empty", " \n\t".into()),
        ("malformed", "not-a-commit\n".into()),
        ("environment", expected.clone()),
        ("allowlist", expected.clone()),
    ] {
        std::fs::write(dir.join("git-output"), output).unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", "origin_isolated_child", "--nocapture"])
            .env("FOUNDRY_REVIEW_MODE", mode).env("FOUNDRY_REVIEW_DIR", &dir)
            .env("FOUNDRY_REVIEW_EXPECTED", &expected)
            .env("GITHUB_TOKEN", "review-secret-github")
            .env("OPENROUTER_API_KEY", "review-secret-router")
            .env("FOUNDRY_BASE", "protocol-env-poison")
            .env("FOUNDRY_CONTEXT", "protocol-env-poison")
            .env("FOUNDRY_FIX_CONTEXT", "protocol-env-poison");
        if mode == "environment" {
            command.env("GIT_DIR", foreign.join(".git")).env("GIT_WORK_TREE", &foreign);
        } else {
            command.env("PATH", format!("{}:/usr/bin:/bin", mockbin.display()));
        }
        let result = command.output().unwrap();
        if !result.status.success() {
            failures.push(format!("{mode}: {}\n{}", String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr)));
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn origin_isolated_child() {
    let Ok(mode) = std::env::var("FOUNDRY_REVIEW_MODE") else { return; };
    let dir = PathBuf::from(std::env::var_os("FOUNDRY_REVIEW_DIR").unwrap());
    let binary = executable_script(&dir, "#!/bin/sh\ntouch die-ran\n/usr/bin/env\n");
    let _ = std::fs::remove_file(dir.join("die-ran"));
    let h = CliHarness::new(binary, "coding", 30).with_protocol(DieProtocol {
        base: BaseSelection::Origin, fix_context: true, context: vec!["context-sentinel".into()],
    });
    let result = h.run(&order(), 1, &dir).await;
    if matches!(mode.as_str(), "multiline" | "empty" | "malformed") {
        assert!(matches!(result, Err(foundry_domain::ports::PortError::Permanent(_))), "{mode}: {result:?}");
        assert!(!dir.join("die-ran").exists());
    } else {
        let out = result.unwrap();
        assert_eq!(out.base_commit, std::env::var("FOUNDRY_REVIEW_EXPECTED").unwrap());
        if mode == "environment" {
            assert_eq!(out.result_commit, out.base_commit);
        }
        if mode == "allowlist" {
            let log = std::fs::read_to_string(out.log_path).unwrap();
            for line in log.lines() {
                let (key, value) = line.split_once('=').unwrap();
                assert!(["PATH", "HOME", "FOUNDRY_ORDER_ID", "FOUNDRY_ATTEMPT", "PWD"].contains(&key), "unexpected env: {key}");
                assert!(!value.contains("review-secret") && !value.contains("protocol-env-poison") && !value.contains("context-sentinel"));
            }
            assert!(log.lines().any(|line| line == "PATH=/usr/bin:/bin:/usr/local/bin"));
            assert!(log.lines().any(|line| line == "FOUNDRY_ORDER_ID=o-h1"));
            assert!(log.lines().any(|line| line == "FOUNDRY_ATTEMPT=1"));
        }
    }
}


#[tokio::test]
async fn manifest_and_argv_agree_for_every_protocol_switch_combination() {
    for base in [BaseSelection::Latest, BaseSelection::Origin, BaseSelection::Commit("a".repeat(40))] {
        for fix_context in [false, true] {
            let dir = temp_dir("protocol-parity");
            seed_repo(&dir.join("repo"), true);
            let binary = executable_script(&dir, "#!/bin/sh\nprintf '%s\\0' \"$@\" > argv.bin\ncp input-manifest.json consumed.json\n");
            let protocol = DieProtocol {
                base: base.clone(), fix_context, context: vec!["src/a path.rs".into(), "README.md".into()],
            };
            let test_order = order();
            let h = CliHarness::new(binary, "coding profile ; '$()'", 30).with_protocol(protocol.clone());
            h.run(&test_order, 3, &dir).await.unwrap();
            let consumed = std::fs::read(dir.join("consumed.json")).unwrap();
            assert_eq!(consumed, std::fs::read(dir.join("input-manifest.json")).unwrap());
            let manifest: InputManifest = serde_json::from_slice(&consumed).unwrap();
            assert_eq!(manifest.order_id, test_order.id);
            assert_eq!(manifest.attempt, 3);
            assert_eq!(manifest.task, test_order.external.dedup_key);
            assert_eq!(manifest.profile, h.profile);
            assert_eq!(manifest.context, protocol.context);
            assert_eq!(manifest.fix_context, fix_context);
            let expected_base=match &base { BaseSelection::Latest=>"latest",BaseSelection::Origin=>"origin",BaseSelection::Commit(commit)=>commit.as_str() };
            assert_eq!(manifest.base, expected_base);
            let raw_args = std::fs::read(dir.join("argv.bin")).unwrap();
            assert_eq!(raw_args.last(), Some(&0));
            let actual: Vec<&[u8]> = raw_args[..raw_args.len() - 1].split(|b| *b == 0).collect();
            let mut expected = vec!["--profile", &manifest.profile, "--task", &manifest.task];
            if manifest.fix_context { expected.push("--fix-context"); }
            expected.extend(["--base", &manifest.base]);
            assert_eq!(actual, expected.iter().map(|s| s.as_bytes()).collect::<Vec<_>>());
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
