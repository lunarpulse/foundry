//! GitHub label Intake — polls issues labeled `foundry:make` (plan §3).
//! At-least-once + dedup_key (issue number + updated_at) = effectively-once.
//! M0 uses the public REST API (no auth needed for public repos, 60 req/h).
#![allow(clippy::collapsible_if)] // W6a clippy debt: preserve explicit branch and short-circuit ordering
#![allow(clippy::manual_clamp)] // W6a clippy debt: preserve the existing bound handling and edge semantics
#![allow(clippy::redundant_closure)] // W6a clippy debt: preserve the callback boundary and captured behavior
#![allow(clippy::too_many_arguments)] // W6a clippy debt: preserve the established operation binding and call shape

use foundry_domain::ports::{Intake, OrderDraft, PortError, PortResult};
use std::collections::HashSet;

/// ProductAtomicV1 GitHub snapshot polling configuration. `repository_id` is
/// GitHub's stable numeric repository identity; owner/name are locators only.
#[derive(Debug, Clone)]
pub struct ProductGithubConfig {
    pub connector_id: String,
    pub api_origin: String,
    pub owner: String,
    pub repo: String,
    pub repository_id: String,
    pub private: bool,
    pub credential_ref: Option<String>,
    /// Explicit opt-in for loopback-only fake-server tests. Product CLI sets
    /// this to false; production endpoints still require HTTPS.
    pub allow_loopback_http: bool,
    pub max_pages_per_tick: u32,
    pub now: chrono::DateTime<chrono::Utc>,
}

/// Clock used by the real product poller. A single captured instant is used
/// for each store transaction, while HTTP requests and post-response commits
/// sample again so a batch cannot carry a stale authorization timestamp.
pub trait ProductClock: Send + Sync {
    fn now(&self) -> chrono::DateTime<chrono::Utc>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProductClock;

impl ProductClock for SystemProductClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }
}

struct FixedProductClock(chrono::DateTime<chrono::Utc>);
impl ProductClock for FixedProductClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductPollReport {
    pub pages: u32,
    pub fetched: usize,
    pub next_page: u32,
    pub sweep_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ProductPollError {
    #[error("GitHub intake configuration rejected")]
    Configuration,
    #[error("GitHub intake credential unavailable")]
    Credential,
    #[error("GitHub intake request failed (retryable)")]
    Retryable,
    #[error("GitHub intake is durably backed off until {0}")]
    Backoff(String),
    #[error("GitHub intake request rejected (permanent)")]
    Permanent,
    #[error("GitHub authentication rejected")]
    Unauthorized,
    #[error("GitHub access forbidden or source scope denied")]
    Forbidden,
    #[error(transparent)]
    Store(#[from] foundry_ledger::ProductError),
}

#[derive(serde::Deserialize)]
struct ProductGhIssue {
    id: u64,
    number: u64,
    title: String,
    updated_at: String,
    state: String,
    labels: Vec<GhLabel>,
    body: Option<String>,
    pull_request: Option<serde_json::Value>,
}

/// Fetch and persist a bounded page batch. Construct, use, and drop this
/// blocking client on a blocking worker (including when called from watch).
pub fn poll_product_github_page_batch(
    store: &mut foundry_ledger::ProductStore,
    cfg: &ProductGithubConfig,
    secrets: &dyn foundry_domain::ports::SecretProvider,
) -> Result<ProductPollReport, ProductPollError> {
    poll_product_github_page_batch_inner(store, cfg, secrets, None, &FixedProductClock(cfg.now))
}

pub fn poll_product_github_page_batch_for_service(
    store: &mut foundry_ledger::ProductStore,
    cfg: &ProductGithubConfig,
    secrets: &dyn foundry_domain::ports::SecretProvider,
    service: &foundry_ledger::ProductServiceContext,
) -> Result<ProductPollReport, ProductPollError> {
    poll_product_github_page_batch_inner(
        store,
        cfg,
        secrets,
        Some(service),
        &FixedProductClock(cfg.now),
    )
}

pub fn poll_product_github_page_batch_for_service_with_clock(
    store: &mut foundry_ledger::ProductStore,
    cfg: &ProductGithubConfig,
    secrets: &dyn foundry_domain::ports::SecretProvider,
    service: &foundry_ledger::ProductServiceContext,
    clock: &dyn ProductClock,
) -> Result<ProductPollReport, ProductPollError> {
    poll_product_github_page_batch_inner(store, cfg, secrets, Some(service), clock)
}

fn poll_product_github_page_batch_inner(
    store: &mut foundry_ledger::ProductStore,
    cfg: &ProductGithubConfig,
    secrets: &dyn foundry_domain::ports::SecretProvider,
    service: Option<&foundry_ledger::ProductServiceContext>,
    clock: &dyn ProductClock,
) -> Result<ProductPollReport, ProductPollError> {
    use foundry_ledger::{ProductDelivery, ProductPollCursor};
    use sha2::{Digest, Sha256};
    if cfg.connector_id.is_empty()
        || cfg.owner.is_empty()
        || cfg.repo.is_empty()
        || cfg.repository_id.is_empty()
        || cfg.max_pages_per_tick == 0
        || cfg.max_pages_per_tick > 5
    {
        return Err(ProductPollError::Configuration);
    }
    let origin =
        reqwest::Url::parse(&cfg.api_origin).map_err(|_| ProductPollError::Configuration)?;
    let host = origin.host_str().unwrap_or_default();
    let loopback = host == "127.0.0.1" || host == "localhost" || host == "[::1]";
    if origin.path() != "/"
        || origin.query().is_some()
        || origin.fragment().is_some()
        || !origin.username().is_empty()
        || origin.password().is_some()
        || !(origin.scheme() == "https"
            || (cfg.allow_loopback_http && loopback && origin.scheme() == "http"))
    {
        return Err(ProductPollError::Configuration);
    }
    if cfg.private && cfg.credential_ref.as_deref().unwrap_or("").is_empty() {
        return Err(ProductPollError::Credential);
    }
    if matches!(store.scope_policy_enforced(), Ok(true)) && service.is_none() {
        return Err(ProductPollError::Store(
            foundry_ledger::ProductError::Invariant("intake service authority required"),
        ));
    }
    if let Some(service) = service {
        store.validate_intake_source_for_service(
            service,
            &cfg.connector_id,
            &cfg.api_origin,
            &cfg.owner,
            &cfg.repo,
            &cfg.repository_id,
            clock.now(),
        )?;
    } else {
        store.validate_intake_source(&cfg.connector_id, &cfg.api_origin, &cfg.repository_id)?;
    }
    let check_service = |store: &foundry_ledger::ProductStore| -> Result<(), ProductPollError> {
        if let Some(service) = service {
            store.require_product_service(
                service,
                foundry_ledger::ProductServiceAction::IntakeRead,
                clock.now(),
            )?;
        }
        Ok(())
    };
    let fingerprint = {
        let mut h = Sha256::new();
        h.update(b"foundry-github-query-v1\0");
        for part in [&cfg.api_origin, &cfg.owner, &cfg.repo, "open+foundry:make"] {
            h.update((part.len() as u64).to_be_bytes());
            h.update(part.as_bytes());
        }
        hex::encode(h.finalize())
    };
    let prior_retry = if let Some(service) = service {
        store.intake_poll_retry_at_for_service(service, &fingerprint, clock.now())?
    } else {
        store.intake_poll_retry_at(&cfg.connector_id, &fingerprint)?
    };
    if let Some(retry_at) = prior_retry {
        if retry_at > clock.now() {
            return Err(ProductPollError::Backoff(retry_at.to_rfc3339()));
        }
    }
    let token = if cfg.private {
        Some(
            secrets
                .resolve(cfg.credential_ref.as_deref().unwrap())
                .map_err(|_| ProductPollError::Credential)?,
        )
    } else {
        None
    };
    let mut client_builder = reqwest::blocking::Client::builder()
        .user_agent("foundry-product-intake/1")
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none());
    if loopback {
        client_builder = client_builder.no_proxy();
    }
    let client = client_builder
        .build()
        .map_err(|_| ProductPollError::Configuration)?;
    // The provider API still addresses issue pages by owner/name. Verify that
    // locator before fetching and again after each page before its durable
    // cursor/inbox commit. This detects a rename/name-reuse spanning the page
    // request, while leaving the remaining remote TOCTOU race explicit.
    verify_repository_locator(
        store,
        &client,
        cfg,
        token.as_deref(),
        service,
        &origin,
        &fingerprint,
        clock,
    )?;
    let prior = if let Some(service) = service {
        store.intake_cursor_for_service(service, &fingerprint, clock.now())?
    } else {
        store.intake_cursor(&cfg.connector_id, &fingerprint)?
    };
    let (mut page, sweep_id, ceiling) = match prior {
        Some(c) if c.next_page <= c.page_ceiling => (c.next_page, c.sweep_id, c.page_ceiling),
        _ => (
            1,
            format!(
                "s-{}",
                clock.now().timestamp_nanos_opt().unwrap_or_default()
            ),
            1000,
        ),
    };
    let mut fetched = 0usize;
    let mut pages = 0u32;
    while pages < cfg.max_pages_per_tick && page <= ceiling {
        let mut url = origin.clone();
        url.set_path(&format!("/repos/{}/{}/issues", cfg.owner, cfg.repo));
        url.query_pairs_mut()
            .append_pair("labels", LABEL)
            .append_pair("state", "open")
            .append_pair("per_page", "100")
            .append_pair("page", &page.to_string());
        let mut request = client
            .get(url.clone())
            .header(reqwest::header::ACCEPT, "application/vnd.github+json");
        if let Some(secret) = token.as_deref() {
            request = request.bearer_auth(secret);
        }
        check_service(store)?;
        let response = request
            .send()
            .map_err(|_| retry_poll_error(store, cfg, &fingerprint, None, service, clock))?;
        let response_now = clock.now();
        let retry_at = github_retry_at(response.headers(), response_now);
        let status = response.status();
        if status.is_redirection() {
            return Err(ProductPollError::Permanent);
        }
        if status.as_u16() == 401 {
            return Err(ProductPollError::Unauthorized);
        }
        if status.as_u16() == 400 {
            return Err(ProductPollError::Permanent);
        }
        if status.as_u16() == 429 || status.as_u16() >= 500 {
            return Err(retry_poll_error(
                store,
                cfg,
                &fingerprint,
                retry_at,
                service,
                clock,
            ));
        }
        if status.as_u16() == 403 {
            if response
                .headers()
                .get("x-ratelimit-remaining")
                .is_some_and(|v| v == "0")
            {
                return Err(retry_poll_error(
                    store,
                    cfg,
                    &fingerprint,
                    retry_at,
                    service,
                    clock,
                ));
            }
            return Err(ProductPollError::Forbidden);
        }
        if !status.is_success() {
            return Err(ProductPollError::Permanent);
        }
        let next_link = response
            .headers()
            .get(reqwest::header::LINK)
            .map(|v| {
                v.to_str()
                    .map(str::to_owned)
                    .map_err(|_| ProductPollError::Permanent)
            })
            .transpose()?;
        if let Some(link) = next_link.as_deref() {
            validate_github_next_link(
                link,
                &origin,
                &cfg.owner,
                &cfg.repo,
                page.saturating_add(1),
            )?;
        }
        let body = read_bounded_body(response, 8 * 1024 * 1024)
            .map_err(|_| retry_poll_error(store, cfg, &fingerprint, retry_at, service, clock))?;
        let values: Vec<serde_json::Value> = serde_json::from_slice(&body)
            .map_err(|_| retry_poll_error(store, cfg, &fingerprint, retry_at, service, clock))?;
        if values.len() > 100 {
            return Err(ProductPollError::Permanent);
        }
        verify_repository_locator(
            store,
            &client,
            cfg,
            token.as_deref(),
            service,
            &origin,
            &fingerprint,
            clock,
        )?;
        let mut deliveries = Vec::new();
        let mut rejected_item_digests = Vec::new();
        for value in values {
            // A malformed item is isolated so it cannot starve later pages.
            // Keep only a digest in local diagnostics; never persist response
            // snippets which may contain issue text or other private data.
            let Ok(issue) = serde_json::from_value::<ProductGhIssue>(value.clone()) else {
                rejected_item_digests
                    .push(hex::encode(Sha256::digest(value.to_string().as_bytes())));
                continue;
            };
            if issue.pull_request.is_some()
                || issue.state != "open"
                || !issue.labels.iter().any(|l| l.name == LABEL)
            {
                continue;
            }
            let issue_id = issue.id.to_string();
            // An issue body is not itself a validated dep-bump task. Keep the
            // immutable source snapshot and route it to the explicit
            // unsupported-adapter state until a real task adapter exists.
            let task_kind = "github-issue".to_string();
            let payload = serde_json::json!({"issue_number": issue.number, "title": issue.title, "body": issue.body.unwrap_or_default()}).to_string();
            let version_digest = foundry_ledger::product::delivery_version_digest(
                &issue_id,
                &issue.updated_at,
                &task_kind,
                &payload,
            );
            let delivery_id = foundry_ledger::product::github_delivery_id(
                store.installation_id(),
                &cfg.connector_id,
                &cfg.api_origin,
                &cfg.repository_id,
                &issue_id,
                &version_digest,
            );
            deliveries.push(ProductDelivery {
                delivery_id,
                connector_id: cfg.connector_id.clone(),
                repository_id: cfg.repository_id.clone(),
                issue_id,
                observed_updated_at: issue.updated_at,
                version_digest,
                task_kind,
                payload,
            });
        }
        fetched += deliveries.len();
        let has_next = next_link.as_deref().is_some_and(|v| link_has_rel_next(v));
        let next_page = if has_next { page.saturating_add(1) } else { 1 };
        let cursor = ProductPollCursor {
            connector_id: cfg.connector_id.clone(),
            query_fingerprint: fingerprint.clone(),
            sweep_id: sweep_id.clone(),
            next_page,
            page_ceiling: ceiling,
            updated_at: clock.now().to_rfc3339(),
        };
        if let Some(service) = service {
            check_service(store)?;
            store.persist_intake_page_for_service(
                service,
                &cursor,
                &deliveries,
                &rejected_item_digests,
                clock.now(),
            )?;
        } else {
            store.persist_intake_page_with_rejections(
                &cursor,
                &deliveries,
                &rejected_item_digests,
            )?;
        }
        pages += 1;
        if !has_next {
            page = 1;
            break;
        }
        page = next_page;
    }
    Ok(ProductPollReport {
        pages,
        fetched,
        next_page: page,
        sweep_id,
    })
}

fn verify_repository_locator(
    store: &mut foundry_ledger::ProductStore,
    client: &reqwest::blocking::Client,
    cfg: &ProductGithubConfig,
    token: Option<&str>,
    service: Option<&foundry_ledger::ProductServiceContext>,
    origin: &reqwest::Url,
    fingerprint: &str,
    clock: &dyn ProductClock,
) -> Result<(), ProductPollError> {
    if let Some(service) = service {
        store.require_product_service(
            service,
            foundry_ledger::ProductServiceAction::IntakeRead,
            clock.now(),
        )?;
    }
    let mut repo_url = origin.clone();
    repo_url.set_path(&format!("/repos/{}/{}", cfg.owner, cfg.repo));
    let mut request = client.get(repo_url);
    if let Some(secret) = token {
        request = request.bearer_auth(secret);
    }
    let response = request
        .send()
        .map_err(|_| retry_poll_error(store, cfg, fingerprint, None, service, clock))?;
    let response_now = clock.now();
    let retry_at = github_retry_at(response.headers(), response_now);
    let status = response.status();
    if status.is_redirection() {
        return Err(ProductPollError::Permanent);
    }
    if status.as_u16() == 429 || status.as_u16() >= 500 {
        return Err(retry_poll_error(
            store,
            cfg,
            fingerprint,
            retry_at,
            service,
            clock,
        ));
    }
    if status.as_u16() == 403
        && response
            .headers()
            .get("x-ratelimit-remaining")
            .is_some_and(|v| v == "0")
    {
        return Err(retry_poll_error(
            store,
            cfg,
            fingerprint,
            retry_at,
            service,
            clock,
        ));
    }
    if status.as_u16() == 401 {
        return Err(ProductPollError::Unauthorized);
    }
    if status.as_u16() == 403 {
        return Err(ProductPollError::Forbidden);
    }
    if !status.is_success() {
        return Err(ProductPollError::Permanent);
    }
    let body = read_bounded_body(response, 64 * 1024)
        .map_err(|_| retry_poll_error(store, cfg, fingerprint, retry_at, service, clock))?;
    let value: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|_| retry_poll_error(store, cfg, fingerprint, retry_at, service, clock))?;
    let id_matches = value
        .get("id")
        .and_then(serde_json::Value::as_u64)
        .map(|id| id.to_string())
        .as_deref()
        == Some(cfg.repository_id.as_str());
    let expected_name = format!("{}/{}", cfg.owner, cfg.repo);
    let locator_matches = value
        .get("full_name")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|name| name.eq_ignore_ascii_case(&expected_name));
    if !id_matches || !locator_matches {
        return Err(ProductPollError::Permanent);
    }
    Ok(())
}

fn read_bounded_body(response: reqwest::blocking::Response, max: usize) -> Result<Vec<u8>, ()> {
    use std::io::Read;
    if response
        .content_length()
        .is_some_and(|length| length > max as u64)
    {
        return Err(());
    }
    let mut body = Vec::new();
    response
        .take(max as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|_| ())?;
    if body.len() > max {
        return Err(());
    }
    Ok(body)
}

fn link_has_rel_next(header: &str) -> bool {
    split_link_header(header).iter().any(|part| {
        let Some((_, params)) = part.split_once('>') else {
            return false;
        };
        params.split(';').skip(1).any(|param| {
            let Some((name, value)) = param.trim().split_once('=') else {
                return false;
            };
            name.trim().eq_ignore_ascii_case("rel")
                && value
                    .trim()
                    .trim_matches('"')
                    .split_ascii_whitespace()
                    .any(|rel| rel == "next")
        })
    })
}

fn split_link_header(header: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut in_target = false;
    for (index, byte) in header.bytes().enumerate() {
        match byte {
            b'<' if !in_target => in_target = true,
            b'>' if in_target => in_target = false,
            b',' if !in_target => {
                parts.push(header[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    if !in_target {
        parts.push(header[start..].trim());
    }
    parts
}

fn retry_poll_error(
    store: &mut foundry_ledger::ProductStore,
    cfg: &ProductGithubConfig,
    fingerprint: &str,
    server_retry_at: Option<chrono::DateTime<chrono::Utc>>,
    service: Option<&foundry_ledger::ProductServiceContext>,
    clock: &dyn ProductClock,
) -> ProductPollError {
    let now = clock.now();
    let result = if let Some(service) = service {
        store.record_intake_poll_failure_for_service(service, fingerprint, now, server_retry_at)
    } else {
        store.record_intake_poll_failure(&cfg.connector_id, fingerprint, now, server_retry_at)
    };
    match result {
        Ok(_) => ProductPollError::Retryable,
        Err(error) => ProductPollError::Store(error),
    }
}

fn github_retry_at(
    headers: &reqwest::header::HeaderMap,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    if let Some(value) = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
    {
        if let Ok(seconds) = value.parse::<i64>() {
            return Some(now + chrono::Duration::seconds(seconds.max(0).min(86_400)));
        }
        if let Ok(time) = httpdate::parse_http_date(value) {
            return Some(chrono::DateTime::<chrono::Utc>::from(time));
        }
    }
    headers
        .get("x-ratelimit-reset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
        .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
}

fn validate_github_next_link(
    link: &str,
    origin: &reqwest::Url,
    owner: &str,
    repo: &str,
    expected_page: u32,
) -> Result<(), ProductPollError> {
    let parts = split_link_header(link);
    if parts.is_empty() || parts.iter().any(|part| part.is_empty()) {
        return Err(ProductPollError::Permanent);
    }
    let mut next_count = 0;
    for part in parts {
        let Some((target, rel)) = part.trim().split_once(';') else {
            return Err(ProductPollError::Permanent);
        };
        let mut relation = None;
        for param in rel.split(';') {
            let Some((name, value)) = param.trim().split_once('=') else {
                return Err(ProductPollError::Permanent);
            };
            if name.trim().eq_ignore_ascii_case("rel") {
                if relation.is_some() {
                    return Err(ProductPollError::Permanent);
                }
                relation = Some(value.trim().trim_matches('"').to_string());
            }
        }
        let relation = relation
            .filter(|value| !value.trim().is_empty())
            .ok_or(ProductPollError::Permanent)?;
        let is_next = relation
            .split_ascii_whitespace()
            .any(|value| value == "next");
        if !is_next {
            continue;
        }
        next_count += 1;
        if next_count > 1 {
            return Err(ProductPollError::Permanent);
        }
        let target = target
            .trim()
            .strip_prefix('<')
            .and_then(|s| s.strip_suffix('>'))
            .ok_or(ProductPollError::Permanent)?;
        let parsed = reqwest::Url::parse(target).map_err(|_| ProductPollError::Permanent)?;
        let expected_path = format!("/repos/{owner}/{repo}/issues");
        let query = parsed.query_pairs().collect::<Vec<_>>();
        let value = |name: &str| {
            query
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_ref())
        };
        if parsed.username() != ""
            || parsed.password().is_some()
            || parsed.fragment().is_some()
            || parsed.scheme() != origin.scheme()
            || parsed.host_str() != origin.host_str()
            || parsed.port() != origin.port()
            || parsed.path() != expected_path
            || query.len() != 4
            || query
                .iter()
                .any(|(k, _)| !["labels", "state", "per_page", "page"].contains(&k.as_ref()))
            || value("labels") != Some(LABEL)
            || value("state") != Some("open")
            || value("per_page") != Some("100")
            || value("page").and_then(|p| p.parse::<u32>().ok()) != Some(expected_page)
        {
            return Err(ProductPollError::Permanent);
        }
        return Ok(());
    }
    Ok(())
}

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
    updated_at: String,
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
        Ok(Self {
            cfg,
            client,
            seen: HashSet::new(),
        })
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
                    format!(
                        "{}/{}#{}:{}",
                        self.cfg.owner, self.cfg.repo, issue.number, issue.updated_at
                    ),
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
