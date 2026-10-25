//! Opt-in GitHub publisher for the durable ProductAtomicV1 publish consumer.

use crate::github_rest::{
    GithubApiError, GithubApiErrorKind, GithubHttpTransport, GithubPullRequest, GithubRestClient,
    GithubRestConfig,
};
use foundry_domain::Order;
use foundry_domain::ports::{PortError, PortResult, PublishOutcome, Publisher};
use foundry_line::product::{ProductPublishReconcileOutcome, ProductPublishReconciler};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone)]
pub struct GithubProductPublisher {
    config: GithubProductPublisherConfig,
    rest: GithubRestClient,
}

#[derive(Clone)]
pub struct GithubProductPublisherConfig {
    pub owner: String,
    pub repository: String,
    pub token_file: PathBuf,
    pub api_origin: String,
    pub base_branch: String,
    pub branch_namespace: String,
    pub workdir_root: PathBuf,
}

impl GithubProductPublisher {
    pub fn new(config: GithubProductPublisherConfig) -> Result<Self, String> {
        Self::with_rest_transport(config, None)
    }

    /// Construct with a mock HTTP transport for offline adapter tests.
    pub fn with_rest_transport(
        config: GithubProductPublisherConfig,
        transport: Option<Arc<dyn GithubHttpTransport>>,
    ) -> Result<Self, String> {
        if !valid_ref_component(&config.base_branch)
            || !valid_branch_namespace(&config.branch_namespace)
        {
            return Err("GitHub publisher configuration invalid".into());
        }
        let rest_config = GithubRestConfig {
            owner: config.owner.clone(),
            repository: config.repository.clone(),
            token_file: config.token_file.clone(),
            api_origin: config.api_origin.clone(),
        };
        let rest = match transport {
            Some(transport) => GithubRestClient::with_transport(rest_config, transport),
            None => GithubRestClient::new(rest_config),
        }
        .map_err(|_| "GitHub publisher configuration invalid".to_owned())?;
        Ok(Self { config, rest })
    }

    /// Resolve explicit production settings without reading the credential file.
    /// The adapter remains disabled unless FOUNDRY_GITHUB_PUBLISHER=enabled.
    pub fn from_env() -> Result<Option<Self>, String> {
        if !publisher_enabled(std::env::var("FOUNDRY_GITHUB_PUBLISHER").ok().as_deref()) {
            return Ok(None);
        }
        let names = [
            "FOUNDRY_GITHUB_OWNER",
            "FOUNDRY_GITHUB_REPOSITORY",
            "FOUNDRY_GITHUB_TOKEN_FILE",
        ];
        let values: Vec<_> = names.iter().map(|name| std::env::var(name).ok()).collect();
        if values.iter().all(Option::is_none) {
            return Ok(None);
        }
        if values.iter().any(Option::is_none) {
            return Err("GitHub publisher configuration incomplete".into());
        }
        let owner = values[0].clone().unwrap_or_default();
        let repository = values[1].clone().unwrap_or_default();
        let token_file = PathBuf::from(values[2].clone().unwrap_or_default());
        if owner.is_empty() || repository.is_empty() || token_file.as_os_str().is_empty() {
            return Err("GitHub publisher configuration incomplete".into());
        }
        let api_origin = std::env::var("FOUNDRY_GITHUB_API_ORIGIN")
            .unwrap_or_else(|_| "https://api.github.com".into());
        let base_branch =
            std::env::var("FOUNDRY_GITHUB_BASE_BRANCH").unwrap_or_else(|_| "main".into());
        let branch_namespace =
            std::env::var("FOUNDRY_GITHUB_BRANCH_NAMESPACE").unwrap_or_else(|_| "w7-verify".into());
        let workdir_root = std::env::var_os("FOUNDRY_GITHUB_WORKDIR_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(foundry_domain::workdir::WORKDIR_ROOT));
        Self::new(GithubProductPublisherConfig {
            owner,
            repository,
            token_file,
            api_origin,
            base_branch,
            branch_namespace,
            workdir_root,
        })
        .map(Some)
    }

    pub fn branch_for_operation(&self, operation_id: &str) -> String {
        let digest = Sha256::digest(operation_id.as_bytes());
        let run_id = hex::encode(digest);
        format!("{}/{run_id}", self.config.branch_namespace)
    }

    fn publish_blocking(
        &self,
        order_id: &str,
        attempt: u32,
        operation_id: &str,
        expected_digest: &str,
    ) -> PortResult<PublishOutcome> {
        if !valid_ref_component(order_id) || !is_git_sha(expected_digest) {
            return Err(PortError::Permanent(
                "approved publish binding is invalid".into(),
            ));
        }
        let repo = self
            .config
            .workdir_root
            .join(format!("foundry-die-{order_id}-{attempt}"))
            .join("repo");
        if !repo.is_dir() {
            return Err(PortError::Permanent("approved worktree unavailable".into()));
        }
        let head = git_output(&repo, &["rev-parse", "HEAD"])?;
        if head != expected_digest {
            return Err(PortError::Permanent(
                "approved artifact digest does not match worktree".into(),
            ));
        }

        let branch = self.branch_for_operation(operation_id);
        if let Some(existing) = self
            .rest
            .find_pull_request_by_head(&branch)
            .map_err(map_api_error)?
        {
            return existing_publish_outcome(existing, operation_id, expected_digest);
        }

        let remote = format!(
            "git@github.com:{}/{}.git",
            self.config.owner, self.config.repository
        );
        let remote_ref = git_output(&repo, &["ls-remote", "--heads", &remote, &branch])?;
        if let Some((remote_sha, _)) = remote_ref.split_once('\t')
            && remote_sha != expected_digest
        {
            return Err(PortError::Permanent(
                "operation branch is bound to a different approved digest".into(),
            ));
        }
        git_output(&repo, &["checkout", "-B", &branch, expected_digest])?;
        let head_before_push = git_output(&repo, &["rev-parse", "HEAD"])?;
        if head_before_push != expected_digest {
            return Err(PortError::Permanent(
                "approved worktree changed before GitHub push".into(),
            ));
        }
        let pinned_ref = format!("{expected_digest}:refs/heads/{branch}");
        git_output(&repo, &["push", &remote, &pinned_ref])?;

        let title = format!("foundry: {order_id}");
        let body = format!(
            "Automated Foundry publication.\n\n- order: `{order_id}`\n- approved digest: `{expected_digest}`\n- operation: `{operation_id}`"
        );
        let head_label = format!("{}:{branch}", self.config.owner);
        match self
            .rest
            .create_pull_request(&title, &head_label, &self.config.base_branch, &body)
        {
            Ok(pull) => existing_publish_outcome(pull, operation_id, expected_digest),
            Err(error) if error.kind == GithubApiErrorKind::InvalidResponse => {
                // GitHub can reject a concurrent duplicate create with 422.
                // Query the deterministic operation branch before treating it
                // as unresolved, so retries never manufacture another PR.
                if let Some(pull) = self
                    .rest
                    .find_pull_request_by_head(&branch)
                    .map_err(map_api_error)?
                {
                    existing_publish_outcome(pull, operation_id, expected_digest)
                } else {
                    Err(map_api_error(error))
                }
            }
            Err(error) => Err(map_api_error(error)),
        }
    }

    fn reconcile_blocking(
        &self,
        operation_id: &str,
        expected_digest: Option<&str>,
    ) -> PortResult<ProductPublishReconcileOutcome> {
        let branch = self.branch_for_operation(operation_id);
        match self
            .rest
            .find_pull_request_by_head(&branch)
            .map_err(map_api_error)?
        {
            None => Ok(ProductPublishReconcileOutcome::NotFound),
            Some(pull)
                if expected_digest
                    .is_some_and(|digest| pull.head_sha != digest || pull.head_ref != branch) =>
            {
                Ok(ProductPublishReconcileOutcome::Rejected {
                    pr_url: pull.html_url,
                })
            }
            Some(pull) if pull.merged => Ok(ProductPublishReconcileOutcome::Confirmed {
                pr_url: pull.html_url,
            }),
            Some(pull) if pull.state == "open" => Ok(ProductPublishReconcileOutcome::Confirmed {
                pr_url: pull.html_url,
            }),
            Some(pull) if pull.state == "closed" => Ok(ProductPublishReconcileOutcome::Rejected {
                pr_url: pull.html_url,
            }),
            Some(_) => Ok(ProductPublishReconcileOutcome::Unknown),
        }
    }
}

impl Publisher for GithubProductPublisher {
    fn publish(
        &self,
        order: &Order,
        operation_id: &str,
        expected_digest: &str,
    ) -> impl Future<Output = PortResult<PublishOutcome>> + Send {
        let this = self.clone();
        let order_id = order.id.clone();
        let attempt = order.attempt();
        let operation_id = operation_id.to_owned();
        let digest = expected_digest.to_owned();
        async move {
            let result = tokio::task::spawn_blocking(move || {
                this.publish_blocking(&order_id, attempt, &operation_id, &digest)
            })
            .await
            .map_err(|_| PortError::Retryable("GitHub publish worker failed".into()))?;
            if result.is_err() {
                tracing::warn!("GitHub publish failed");
            }
            result
        }
    }
}

impl ProductPublishReconciler for GithubProductPublisher {
    fn reconcile(
        &self,
        operation_id: &str,
    ) -> impl Future<Output = PortResult<ProductPublishReconcileOutcome>> + Send {
        let this = self.clone();
        let operation_id = operation_id.to_owned();
        async move {
            let result =
                tokio::task::spawn_blocking(move || this.reconcile_blocking(&operation_id, None))
                    .await
                    .map_err(|_| PortError::Retryable("GitHub reconcile worker failed".into()))?;
            if result.is_err() {
                tracing::warn!("GitHub reconciliation failed");
            }
            result
        }
    }

    fn reconcile_bound(
        &self,
        operation_id: &str,
        expected_digest: &str,
    ) -> impl Future<Output = PortResult<ProductPublishReconcileOutcome>> + Send {
        let this = self.clone();
        let operation_id = operation_id.to_owned();
        let expected_digest = expected_digest.to_owned();
        async move {
            let result = tokio::task::spawn_blocking(move || {
                this.reconcile_blocking(&operation_id, Some(&expected_digest))
            })
            .await
            .map_err(|_| PortError::Retryable("GitHub reconcile worker failed".into()))?;
            if result.is_err() {
                tracing::warn!("GitHub reconciliation failed");
            }
            result
        }
    }
}

fn existing_publish_outcome(
    pull: GithubPullRequest,
    operation_id: &str,
    expected_digest: &str,
) -> PortResult<PublishOutcome> {
    if pull.head_sha != expected_digest {
        return Err(PortError::Permanent(
            "existing operation PR is bound to a different approved digest".into(),
        ));
    }
    if pull.merged || pull.state == "open" {
        Ok(PublishOutcome::Confirmed {
            pr_url: pull.html_url,
        })
    } else {
        Ok(PublishOutcome::Unknown {
            operation_id: operation_id.to_owned(),
        })
    }
}

fn git_output(repo: &std::path::Path, args: &[&str]) -> PortResult<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|_| PortError::Retryable("Git SSH transport unavailable".into()))?;
    if !output.status.success() {
        return Err(PortError::Retryable("Git SSH operation failed".into()));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|_| PortError::Permanent("Git returned invalid output".into()))
}

fn map_api_error(error: GithubApiError) -> PortError {
    match error.kind {
        GithubApiErrorKind::Retryable => {
            let message = match error.retry_after {
                Some(delay) => format!("GitHub request retryable; Retry-After: {delay}"),
                None => "GitHub request retryable".into(),
            };
            PortError::Retryable(message)
        }
        GithubApiErrorKind::Unauthorized => {
            PortError::Permanent("GitHub authentication rejected".into())
        }
        GithubApiErrorKind::Forbidden => PortError::Permanent("GitHub request forbidden".into()),
        GithubApiErrorKind::NotFound => PortError::Permanent("GitHub resource not found".into()),
        GithubApiErrorKind::InvalidResponse => {
            PortError::Permanent("GitHub response invalid".into())
        }
        GithubApiErrorKind::Configuration => {
            PortError::Permanent("GitHub adapter configuration invalid".into())
        }
    }
}

fn valid_ref_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 120
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_./".contains(&byte))
        && !value.starts_with('/')
        && !value.ends_with('/')
        && !value.contains("..")
}

fn valid_branch_namespace(value: &str) -> bool {
    valid_ref_component(value) && !value.contains('@') && !value.contains("://")
}

fn is_git_sha(value: &str) -> bool {
    (value.len() == 40 || value.len() == 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn publisher_enabled(value: Option<&str>) -> bool {
    value == Some("enabled")
}

#[cfg(test)]
mod tests {
    use super::publisher_enabled;

    #[test]
    fn github_product_publisher_is_off_without_explicit_enable_setting() {
        assert!(!publisher_enabled(None));
        assert!(!publisher_enabled(Some("")));
        assert!(!publisher_enabled(Some("true")));
        assert!(publisher_enabled(Some("enabled")));
    }
}
