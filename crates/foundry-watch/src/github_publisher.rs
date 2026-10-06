//! GithubPublisher — the real Publisher port impl (M0 gate).
//!
//! Plan §11: the Publisher is the EXCLUSIVE owner of publish credentials.
//! The Die workdir already contains the approved result commit; publish()
//! re-verifies HEAD == expected digest (attestation binding, critique #1),
//! pushes a `foundry/<order>` branch, and opens a PR. Idempotency: an open
//! PR with the same head short-circuits (operation_id safety net, critique #2).

use foundry_domain::ports::{PortError, PortResult, PublishOutcome, Publisher};
use foundry_domain::Order;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct GithubPublisher {
    pub owner: String,
    pub repo: String,
    /// Fine-grained PAT (Contents: R/W, Pull requests: R/W). Never logged.
    pub token_path: PathBuf,
    /// Base branch for PRs.
    pub base: String,
}

impl Publisher for GithubPublisher {
    fn publish(
        &self,
        order: &Order,
        operation_id: &str,
        expected_digest: &str,
    ) -> impl Future<Output = PortResult<PublishOutcome>> + Send {
        let this = self.clone();
        let order_id = order.id.clone();
        let attempt = operation_id
            .rsplit(':')
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(1);
        let digest = expected_digest.to_string();

        async move {
            // reqwest::blocking must not drop inside the tokio runtime
            // (T9 measured-bug #3).
            tokio::task::spawn_blocking(move || {
                this.publish_blocking(&order_id, attempt, &digest)
            })
            .await
            .map_err(|e| PortError::Permanent(format!("publish join: {e}")))?
        }
    }
}

fn sanitize_branch(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    format!("foundry/{}", cleaned.trim_matches('-'))
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("git spawn: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

impl GithubPublisher {
    fn publish_blocking(
        &self,
        order_id: &str,
        attempt: u32,
        digest: &str,
    ) -> PortResult<PublishOutcome> {
        let token = std::fs::read_to_string(&self.token_path)
            .map(|s| s.trim().to_string())
            .map_err(|e| PortError::Permanent(format!("gh_token: {e}")))?;
        if token.is_empty() {
            return Err(PortError::Permanent("gh_token file is empty".into()));
        }
        let scrub = |s: String| s.replace(&token, "***");

        let workdir = PathBuf::from(format!("/tmp/foundry-die-{order_id}-{attempt}/repo"));
        if !workdir.exists() {
            return Err(PortError::Permanent(format!(
                "die workdir missing: {}",
                workdir.display()
            )));
        }

        // Attestation binding: the published commit must be the approved one.
        let head = git(&workdir, &["rev-parse", "HEAD"])
            .map_err(|e| PortError::Permanent(scrub(e)))?;
        if head != digest {
            return Err(PortError::Permanent(format!(
                "digest mismatch: workdir HEAD {head} != approved {digest}"
            )));
        }

        let branch = sanitize_branch(order_id);
        let remote = format!(
            "https://x-access-token:{token}@github.com/{}/{}.git",
            self.owner, self.repo
        );
        git(&workdir, &["checkout", "-B", &branch])
            .map_err(|e| PortError::Permanent(scrub(e)))?;
        if let Err(e) = git(&workdir, &["push", "-u", &remote, &branch]) {
            let msg = scrub(e);
            return Err(if msg.contains("Could not resolve") || msg.contains("timed out") {
                PortError::Retryable(msg)
            } else {
                PortError::Permanent(msg)
            });
        }

        let client = reqwest::blocking::Client::builder()
            .user_agent("foundry-m0")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| PortError::Permanent(format!("http client: {e}")))?;
        let api = |p: &str| format!("https://api.github.com/repos/{}/{}/{}", self.owner, self.repo, p);
        let head_label = format!("{}:{}", self.owner, branch);

        // Idempotency: open PR with this head already? → Confirmed (same PR).
        let existing: serde_json::Value = client
            .get(api(&format!("pulls?head={head_label}&state=open")))
            .bearer_auth(&token)
            .send()
            .map_err(|e| PortError::Retryable(scrub(format!("pr lookup: {e}"))))?
            .error_for_status()
            .map_err(|e| PortError::Retryable(scrub(format!("pr lookup status: {e}"))))?
            .json()
            .map_err(|e| PortError::Retryable(scrub(format!("pr lookup json: {e}"))))?;
        if let Some(pr) = existing.as_array().and_then(|a| a.first()) {
            let url = pr["html_url"].as_str().unwrap_or_default().to_string();
            tracing::info!(order = %order_id, %url, "PR already open — idempotent publish");
            return Ok(PublishOutcome::Confirmed { pr_url: url });
        }

        let payload = serde_json::json!({
            "title": format!("foundry: {}", order_id),
            "head": branch,
            "base": self.base,
            "body": format!(
                "Automated dep-bump by Foundry M0 gate.\n\n- order: `{}`\n- approved digest: `{}`\n- operation_id: `{}:{}`",
                order_id, digest, order_id, attempt
            ),
        });
        let resp = client
            .post(api("pulls"))
            .bearer_auth(&token)
            .json(&payload)
            .send()
            .map_err(|e| PortError::Retryable(scrub(format!("pr create: {e}"))))?;
        let status = resp.status();
        let body: serde_json::Value = resp
            .json()
            .map_err(|e| PortError::Permanent(scrub(format!("pr create json: {e}"))))?;
        if !status.is_success() {
            let msg = format!("pr create {}: {}", status, body["message"].as_str().unwrap_or("?"));
            return Err(if status.as_u16() >= 500 {
                PortError::Retryable(scrub(msg))
            } else {
                PortError::Permanent(scrub(msg))
            });
        }
        let url = body["html_url"].as_str().unwrap_or_default().to_string();
        tracing::info!(order = %order_id, %url, "PR created");
        Ok(PublishOutcome::Confirmed { pr_url: url })
    }
}
