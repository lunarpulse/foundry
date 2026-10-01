//! GitHub label Intake — polls issues labeled `foundry:make` (plan §3).
//! At-least-once + dedup_key (issue number + updated_at) = effectively-once.
//! M0 uses the public REST API (no auth needed for public repos, 60 req/h).
use foundry_domain::ports::{Intake, OrderDraft, PortError, PortResult};
use std::collections::HashSet;

pub const LABEL: &str = "foundry:make";

#[derive(Debug, Clone)]
pub struct IntakeConfig {
    pub owner: String,
    pub repo: String,
    /// GithubIssuesIntake only (M0). Jira etc. are M4 enterprise ports.
    pub poll_secs: u64,
}

pub struct GithubLabelIntake {
    cfg: IntakeConfig,
    client: reqwest::blocking::Client,
    /// Dedup: drafts already delivered in this process lifetime.
    seen: HashSet<String>,
}

#[derive(serde::Deserialize)]
struct GhIssue {
    number: u64,
    title: String,
    updated_at: String,
    labels: Vec<GhLabel>,
    body: Option<String>,
}

#[derive(serde::Deserialize)]
struct GhLabel {
    name: String,
}

impl GithubLabelIntake {
    pub fn new(cfg: IntakeConfig) -> PortResult<Self> {
        let client = reqwest::blocking::Client::builder()
            .user_agent("foundry-m0")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| PortError::Permanent(format!("http client: {e}")))?;
        Ok(Self { cfg, client, seen: HashSet::new() })
    }

}

impl Intake for GithubLabelIntake {
    fn poll(&mut self) -> PortResult<Vec<OrderDraft>> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/issues?labels={}&state=open&per_page=20",
            self.cfg.owner, self.cfg.repo, LABEL
        );
        let issues: Vec<GhIssue> = self
            .client
            .get(&url)
            .send()
            .map_err(|e| PortError::Retryable(format!("poll: {e}")))?
            .error_for_status()
            .map_err(|e| PortError::Retryable(format!("poll status: {e}")))?
            .json()
            .map_err(|e| PortError::Retryable(format!("poll json: {e}")))?;

        let mut drafts = Vec::new();
        for issue in issues {
            let key = format!("{}#{}:{}", self.cfg.repo, issue.number, issue.updated_at);
            if self.seen.contains(&key) {
                continue;
            }
            self.seen.insert(key.clone());
            drafts.push(OrderDraft {
                draft_id: key,
                external: foundry_domain::ExternalRef::github(
                    &format!("{}/{}", self.cfg.owner, self.cfg.repo),
                    &issue.number.to_string(),
                    format!("{}/{}#{}:{}", self.cfg.owner, self.cfg.repo, issue.number, issue.updated_at),
                ),
                task_kind: "dep-bump".into(), // M0 first task class
                payload: issue.body.unwrap_or_default(),
            });
        }
        Ok(drafts)
    }

    fn ack(&mut self, draft_id: &str) -> PortResult<()> {
        // M0: removal from label happens via the Publisher's PR description
        // link or manually; ack here marks delivery complete in-process.
        // M1: durable ack (SQLite) so restarts don't re-deliver.
        self.seen.insert(draft_id.to_string());
        Ok(())
    }
}
