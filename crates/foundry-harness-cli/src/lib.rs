//! foundry-harness-cli — Harness impl via external CLI subprocess (plan Task 6).
//! No source coupling with the worker binary; the contract is its CLI surface.
//!
//! The domain `Harness` trait returns `impl Future`, so implementers use RPITIT
//! (edition 2024). This crate drives the worker binary with an allowlisted env
//! (plan §11: publish credentials never reach the Die).
pub mod fake;

use foundry_domain::ports::{DieOutput, Harness, PortError, PortResult};
use foundry_domain::Order;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct CliHarness {
    /// Worker binary path (e.g. rustain). Resolved at construction.
    pub binary: PathBuf,
    /// Profile flag forwarded to the worker (plan: rustain profile system).
    pub profile: String,
    /// Per-attempt timeout in seconds.
    pub timeout_secs: u64,
}

impl CliHarness {
    pub fn new(binary: impl Into<PathBuf>, profile: impl Into<String>, timeout_secs: u64) -> Self {
        Self {
            binary: binary.into(),
            profile: profile.into(),
            timeout_secs,
        }
    }
}

impl Harness for CliHarness {
    fn run(
        &self,
        order: &Order,
        attempt: u32,
        workdir: &Path,
    ) -> impl Future<Output = PortResult<DieOutput>> + Send {
        let binary = self.binary.clone();
        let profile = self.profile.clone();
        let timeout_secs = self.timeout_secs;
        let order_id = order.id.clone();
        let dedup_key = order.external.dedup_key.clone();
        let workdir = workdir.to_path_buf();

        async move {
            if !binary.exists() {
                return Err(PortError::Permanent(format!(
                    "harness binary not found: {}",
                    binary.display()
                )));
            }

            let log_path = workdir.join(format!("attempt-{attempt}.log"));
            let log_file = std::fs::File::create(&log_path)
                .map_err(|e| PortError::Permanent(format!("create log: {e}")))?;
            let log_err = log_file
                .try_clone()
                .map_err(|e| PortError::Permanent(e.to_string()))?;

            // SECURITY (plan §11): env allowlist — minimal environment only.
            // No publish credentials ever reach the Die.
            let mut cmd = Command::new(&binary);
            cmd.current_dir(&workdir)
                .arg("--profile")
                .arg(&profile)
                .arg("--task")
                .arg(&dedup_key)
                .env_clear()
                .env("PATH", "/usr/bin:/bin:/usr/local/bin")
                .env("HOME", std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
                .env("FOUNDRY_ORDER_ID", &order_id)
                .env("FOUNDRY_ATTEMPT", attempt.to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::from(log_file))
                .stderr(Stdio::from(log_err));

            let child = tokio::time::timeout(
                std::time::Duration::from_secs(timeout_secs),
                // NOTE: cmd.output() would force stdout/stderr to pipes and we
                // would lose the log-file redirect. spawn() + wait() honors
                // the Stdio::from(file) bindings.
                async {
                    let mut child = cmd.spawn().map_err(|e| {
                        PortError::Retryable(format!("spawn: {e}"))
                    })?;
                    child.wait().await.map_err(|e| {
                        PortError::Retryable(format!("wait: {e}"))
                    })
                },
            )
            .await
            .map_err(|_| PortError::Retryable(format!("die timeout after {timeout_secs}s")))?;
            let status = child?;

            let base = git_head(&workdir);
            // The Die wrapper clones the gate repo into `workdir/repo`, so
            // HEAD may live one level down (measured 2026-10-07: git_head on
            // the bare workdir returned "" → empty digest → approval refused
            // 20/20). Refuse EARLY instead of binding an empty digest.
            let result_commit = {
                let h = git_head(&workdir);
                if h.is_empty() { git_head(&workdir.join("repo")) } else { h }
            };
            if result_commit.is_empty() {
                return Err(PortError::Permanent(format!(
                    "die produced no commit: no git HEAD in {} or {}/repo",
                    workdir.display(),
                    workdir.join("repo").display()
                )));
            }

            Ok(DieOutput {
                attempt,
                base_commit: base,
                result_commit,
                log_path: log_path.display().to_string(),
                exit_code: status.code().unwrap_or(-1),
            })
        }
    }
}

fn git_head(dir: &Path) -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}
