//! foundry-harness-cli — Harness impl via external CLI subprocess (plan Task 6).
//! No source coupling with the worker binary; the contract is its CLI surface.
//!
//! The domain `Harness` trait returns `impl Future`, so implementers use RPITIT
//! (edition 2024). This crate drives the worker binary with an allowlisted env
//! (plan §11: publish credentials never reach the Die).
pub mod fake;

use foundry_domain::Order;
use foundry_domain::ports::{DieOutput, Harness, PortError, PortResult};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BaseSelection {
    #[default]
    Latest,
    Origin,
    Commit(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DieProtocol {
    pub base: BaseSelection,
    pub fix_context: bool,
    pub context: Vec<String>,
}

/// Stable input contract written before invoking the external Die.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputManifest {
    pub order_id: String,
    pub attempt: u32,
    pub task: String,
    pub profile: String,
    pub base: String,
    pub fix_context: bool,
    pub context: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CliHarness {
    /// Worker binary path (e.g. rustain). Resolved at construction.
    pub binary: PathBuf,
    /// Profile flag forwarded to the worker (plan: rustain profile system).
    pub profile: String,
    /// Per-attempt timeout in seconds.
    pub timeout_secs: u64,
    /// Pack/caller supplied Die protocol configuration.
    pub protocol: DieProtocol,
}

impl CliHarness {
    pub fn new(binary: impl Into<PathBuf>, profile: impl Into<String>, timeout_secs: u64) -> Self {
        Self {
            binary: binary.into(),
            profile: profile.into(),
            timeout_secs,
            protocol: DieProtocol::default(),
        }
    }

    pub fn with_protocol(mut self, protocol: DieProtocol) -> Self {
        self.protocol = protocol;
        self
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
        let protocol = self.protocol.clone();
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

            let base_name = match &protocol.base {
                BaseSelection::Latest => "latest",
                BaseSelection::Origin => "origin",
                BaseSelection::Commit(commit) => commit.as_str(),
            };
            // Origin must be pinned before Die runs. Latest retains the legacy
            // post-Die workdir HEAD lookup below.
            let origin_base = match &protocol.base {
                BaseSelection::Latest => None,
                BaseSelection::Origin => Some(git_repo_commit(
                    &workdir.join("repo"), "refs/remotes/origin/main^{commit}",
                )?),
                BaseSelection::Commit(_) => None,
            };
            let manifest = InputManifest {
                order_id: order_id.clone(),
                attempt,
                task: dedup_key.clone(),
                profile: profile.clone(),
                base: base_name.to_string(),
                fix_context: protocol.fix_context,
                context: protocol.context.clone(),
            };
            write_manifest_atomic(&workdir, attempt, &manifest)?;

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
                .arg(&manifest.profile)
                .arg("--task")
                .arg(&manifest.task)
                .env_clear()
                .env("PATH", "/usr/bin:/bin:/usr/local/bin")
                .env(
                    "HOME",
                    std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()),
                )
                .env("FOUNDRY_ORDER_ID", &order_id)
                .env("FOUNDRY_ATTEMPT", attempt.to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::from(log_file))
                .stderr(Stdio::from(log_err));
            if manifest.fix_context {
                cmd.arg("--fix-context");
            }
            cmd.arg("--base").arg(&manifest.base);

            let child = tokio::time::timeout(
                std::time::Duration::from_secs(timeout_secs),
                // NOTE: cmd.output() would force stdout/stderr to pipes and we
                // would lose the log-file redirect. spawn() + wait() honors
                // the Stdio::from(file) bindings.
                async {
                    let mut child = cmd
                        .spawn()
                        .map_err(|e| PortError::Retryable(format!("spawn: {e}")))?;
                    child
                        .wait()
                        .await
                        .map_err(|e| PortError::Retryable(format!("wait: {e}")))
                },
            )
            .await
            .map_err(|_| PortError::Retryable(format!("die timeout after {timeout_secs}s")))?;
            let status = child?;

            // The Die wrapper clones the gate repo into `workdir/repo`, so
            // HEAD may live one level down (measured 2026-10-07: git_head on
            // the bare workdir returned "" → empty digest → approval refused
            // 20/20). Refuse EARLY instead of binding an empty digest.
            let base_commit = origin_base.unwrap_or_else(|| git_head(&workdir));
            let result_commit = if protocol.base == BaseSelection::Origin {
                git_repo_commit(&workdir.join("repo"), "HEAD^{commit}")?
            } else {
                let h = git_head(&workdir);
                if h.is_empty() {
                    git_head(&workdir.join("repo"))
                } else {
                    h
                }
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
                base_commit,
                result_commit,
                log_path: log_path.display().to_string(),
                exit_code: status.code().unwrap_or(-1),
            })
        }
    }
}

fn write_manifest_atomic(workdir: &Path, attempt: u32, manifest: &InputManifest) -> PortResult<()> {
    let target = workdir.join("input-manifest.json");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = workdir.join(format!(
        ".input-manifest-{}-{attempt}-{nonce}.tmp",
        std::process::id()
    ));
    let result = (|| {
        let bytes = serde_json::to_vec(manifest)
            .map_err(|e| PortError::Permanent(format!("serialize input manifest: {e}")))?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| PortError::Permanent(format!("create input manifest temp: {e}")))?;
        use std::io::Write;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| PortError::Permanent(format!("write input manifest: {e}")))?;
        std::fs::rename(&temporary, &target)
            .map_err(|e| PortError::Permanent(format!("rename input manifest: {e}")))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn git_head(dir: &Path) -> String {
    git_ref(dir, "HEAD")
}

fn git_ref(dir: &Path, reference: &str) -> String {
    std::process::Command::new("git")
        .args(["rev-parse", reference])
        .current_dir(dir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}


/// Unlike the legacy workdir lookup, Origin is bound to this exact clone.
/// Do not inherit GIT_DIR/GIT_WORK_TREE (or Git config env) from the host.
fn git_repo_commit(repo: &Path, reference: &str) -> PortResult<String> {
    let run = |args: &[&str]| -> PortResult<Vec<u8>> {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()))
            .output()
            .map_err(|e| PortError::Permanent(format!("git {reference} in {}: {e}", repo.display())))?;
        if !output.status.success() {
            return Err(PortError::Permanent(format!(
                "git {reference} failed in {}", repo.display()
            )));
        }
        Ok(output.stdout)
    };
    // Git otherwise walks up into an unrelated parent repository when repo/
    // exists but has not been cloned yet. Worktree .git files remain supported.
    let prefix = run(&["rev-parse", "--show-prefix"])?;
    if prefix != b"\n" && !prefix.is_empty() {
        return Err(PortError::Permanent(format!(
            "die repository is not rooted at {}", repo.display()
        )));
    }
    let bytes = run(&["rev-parse", "--verify", reference])?;
    let commit = std::str::from_utf8(&bytes).unwrap_or_default().trim();
    // Accept surrounding whitespace, but never multiple revisions or a
    // non-object response. Git supports both SHA-1 and SHA-256 repositories.
    if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(PortError::Permanent(format!(
            "invalid git {reference} output in {}", repo.display()
        )));
    }
    Ok(commit.to_string())
}
