//! Small GitHub REST client for the product adapters.
//!
//! Credentials are read only from the configured private file and are attached
//! as an Authorization header. They are never included in request URLs or
//! diagnostic values.

use reqwest::blocking::Client;
use reqwest::header::AUTHORIZATION;
use reqwest::{Method, StatusCode, Url};
use serde_json::{Value, json};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct GithubRestConfig {
    pub owner: String,
    pub repository: String,
    pub token_file: PathBuf,
    pub api_origin: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GithubApiErrorKind {
    Unauthorized,
    Forbidden,
    NotFound,
    Retryable,
    InvalidResponse,
    Configuration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GithubApiError {
    pub kind: GithubApiErrorKind,
    pub retry_after: Option<String>,
}

impl fmt::Display for GithubApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.kind, &self.retry_after) {
            (GithubApiErrorKind::Retryable, Some(delay)) => {
                write!(f, "GitHub request is retryable; Retry-After: {delay}")
            }
            (GithubApiErrorKind::Retryable, None) => f.write_str("GitHub request is retryable"),
            (GithubApiErrorKind::Unauthorized, _) => f.write_str("GitHub authentication rejected"),
            (GithubApiErrorKind::Forbidden, _) => f.write_str("GitHub request forbidden"),
            (GithubApiErrorKind::NotFound, _) => f.write_str("GitHub resource not found"),
            (GithubApiErrorKind::InvalidResponse, _) => f.write_str("GitHub response was invalid"),
            (GithubApiErrorKind::Configuration, _) => {
                f.write_str("GitHub adapter configuration invalid")
            }
        }
    }
}

impl std::error::Error for GithubApiError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GithubIssue {
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub html_url: String,
    pub is_pull_request: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GithubPullRequest {
    pub number: u64,
    pub html_url: String,
    pub state: String,
    pub merged: bool,
    pub head_ref: String,
    pub head_sha: String,
}

#[derive(Clone)]
pub struct GithubRestClient {
    config: GithubRestConfig,
    transport: Arc<dyn GithubHttpTransport>,
}

/// HTTP boundary used by production reqwest and deterministic offline tests.
pub trait GithubHttpTransport: Send + Sync {
    fn send(
        &self,
        method: &Method,
        url: &Url,
        authorization_header: &str,
        body: Option<&Value>,
    ) -> Result<GithubHttpResponse, GithubApiError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GithubHttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

struct ReqwestGithubTransport(std::sync::OnceLock<Result<Client, ()>>);

impl GithubHttpTransport for ReqwestGithubTransport {
    fn send(
        &self,
        method: &Method,
        url: &Url,
        authorization_header: &str,
        body: Option<&Value>,
    ) -> Result<GithubHttpResponse, GithubApiError> {
        // The production adapter invokes transport calls on spawn_blocking
        // workers. Defer blocking Client construction until that boundary so
        // construction from async command setup cannot panic.
        let client = self
            .0
            .get_or_init(|| {
                Client::builder()
                    .user_agent("foundry-product-github")
                    .timeout(Duration::from_secs(30))
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .map_err(|_| ())
            })
            .as_ref()
            .map_err(|_| api_error(GithubApiErrorKind::Configuration, None))?;
        let mut request = client
            .request(method.clone(), url.clone())
            .header(AUTHORIZATION, authorization_header);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .map_err(|_| api_error(GithubApiErrorKind::Retryable, None))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().to_owned(), value.to_owned()))
            })
            .collect();
        let body = response
            .bytes()
            .map_err(|_| api_error(GithubApiErrorKind::Retryable, None))?
            .to_vec();
        Ok(GithubHttpResponse {
            status,
            headers,
            body,
        })
    }
}

impl GithubRestClient {
    pub fn new(config: GithubRestConfig) -> Result<Self, GithubApiError> {
        if !valid_segment(&config.owner)
            || !valid_segment(&config.repository)
            || !valid_production_api_origin(&config.api_origin)
        {
            return Err(api_error(GithubApiErrorKind::Configuration, None));
        }
        Ok(Self {
            config,
            transport: Arc::new(ReqwestGithubTransport(std::sync::OnceLock::new())),
        })
    }

    /// Build the same REST semantics around an injected transport.
    pub fn with_transport(
        config: GithubRestConfig,
        transport: Arc<dyn GithubHttpTransport>,
    ) -> Result<Self, GithubApiError> {
        if !valid_segment(&config.owner)
            || !valid_segment(&config.repository)
            || !valid_api_origin(&config.api_origin)
        {
            return Err(api_error(GithubApiErrorKind::Configuration, None));
        }
        Ok(Self { config, transport })
    }

    pub fn list_issues_by_label(&self) -> Result<Vec<GithubIssue>, GithubApiError> {
        let mut url = self.endpoint("issues")?;
        url.query_pairs_mut()
            .append_pair("labels", "foundry:make")
            .append_pair("state", "open")
            .append_pair("per_page", "100");
        let value = self.send_json(Method::GET, url, None)?;
        let rows = value
            .as_array()
            .ok_or_else(|| api_error(GithubApiErrorKind::InvalidResponse, None))?;
        Ok(rows
            .iter()
            .filter_map(parse_issue)
            .filter(|issue| !issue.is_pull_request)
            .collect())
    }

    pub fn create_issue(&self, title: &str, body: &str) -> Result<GithubIssue, GithubApiError> {
        let value = self.send_json(
            Method::POST,
            self.endpoint("issues")?,
            Some(json!({ "title": title, "body": body })),
        )?;
        parse_issue(&value).ok_or_else(|| api_error(GithubApiErrorKind::InvalidResponse, None))
    }

    pub fn query_issue(&self, number: u64) -> Result<GithubIssue, GithubApiError> {
        let value = self.send_json(
            Method::GET,
            self.endpoint(&format!("issues/{number}"))?,
            None,
        )?;
        parse_issue(&value).ok_or_else(|| api_error(GithubApiErrorKind::InvalidResponse, None))
    }

    pub fn close_issue(&self, number: u64) -> Result<GithubIssue, GithubApiError> {
        let value = self.send_json(
            Method::PATCH,
            self.endpoint(&format!("issues/{number}"))?,
            Some(json!({ "state": "closed" })),
        )?;
        parse_issue(&value).ok_or_else(|| api_error(GithubApiErrorKind::InvalidResponse, None))
    }

    pub fn create_pull_request(
        &self,
        title: &str,
        head: &str,
        base: &str,
        body: &str,
    ) -> Result<GithubPullRequest, GithubApiError> {
        let value = self.send_json(
            Method::POST,
            self.endpoint("pulls")?,
            Some(json!({
                "title": title,
                "head": head,
                "base": base,
                "body": body,
            })),
        )?;
        parse_pull(&value).ok_or_else(|| api_error(GithubApiErrorKind::InvalidResponse, None))
    }

    pub fn query_pull_request(&self, number: u64) -> Result<GithubPullRequest, GithubApiError> {
        let value = self.send_json(
            Method::GET,
            self.endpoint(&format!("pulls/{number}"))?,
            None,
        )?;
        parse_pull(&value).ok_or_else(|| api_error(GithubApiErrorKind::InvalidResponse, None))
    }

    pub fn close_pull_request(&self, number: u64) -> Result<GithubPullRequest, GithubApiError> {
        let value = self.send_json(
            Method::PATCH,
            self.endpoint(&format!("pulls/{number}"))?,
            Some(json!({ "state": "closed" })),
        )?;
        parse_pull(&value).ok_or_else(|| api_error(GithubApiErrorKind::InvalidResponse, None))
    }

    pub fn find_pull_request_by_head(
        &self,
        branch: &str,
    ) -> Result<Option<GithubPullRequest>, GithubApiError> {
        let mut url = self.endpoint("pulls")?;
        url.query_pairs_mut()
            .append_pair("head", &format!("{}:{branch}", self.config.owner))
            .append_pair("state", "all");
        let value = self.send_json(Method::GET, url, None)?;
        let rows = value
            .as_array()
            .ok_or_else(|| api_error(GithubApiErrorKind::InvalidResponse, None))?;
        let matches: Vec<_> = rows.iter().filter_map(parse_pull).collect();
        match matches.as_slice() {
            [] => Ok(None),
            [pull] => Ok(Some(pull.clone())),
            _ => Err(api_error(GithubApiErrorKind::InvalidResponse, None)),
        }
    }

    fn endpoint(&self, path: &str) -> Result<Url, GithubApiError> {
        let base = format!(
            "{}/repos/{}/{}/",
            self.config.api_origin.trim_end_matches('/'),
            self.config.owner,
            self.config.repository
        );
        Url::parse(&base)
            .and_then(|url| url.join(path))
            .map_err(|_| api_error(GithubApiErrorKind::Configuration, None))
    }

    fn send_json(
        &self,
        method: Method,
        url: Url,
        body: Option<Value>,
    ) -> Result<Value, GithubApiError> {
        let token = read_private_token(&self.config.token_file)?;
        let response =
            self.transport
                .send(&method, &url, &format!("Bearer {token}"), body.as_ref())?;
        let status = StatusCode::from_u16(response.status)
            .map_err(|_| api_error(GithubApiErrorKind::InvalidResponse, None))?;
        let retry_after = response
            .headers
            .iter()
            .find_map(|(name, value)| name.eq_ignore_ascii_case("retry-after").then_some(value))
            .filter(|value| {
                !value.is_empty() && value.len() <= 10 && value.bytes().all(|b| b.is_ascii_digit())
            })
            .cloned();
        if !status.is_success() {
            return Err(status_error(status, retry_after));
        }
        serde_json::from_slice(&response.body)
            .map_err(|_| api_error(GithubApiErrorKind::InvalidResponse, None))
    }
}

fn read_private_token(path: &PathBuf) -> Result<String, GithubApiError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(path)
            .map_err(|_| api_error(GithubApiErrorKind::Configuration, None))?;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(api_error(GithubApiErrorKind::Configuration, None));
        }
    }
    let token = std::fs::read_to_string(path)
        .map_err(|_| api_error(GithubApiErrorKind::Configuration, None))?;
    let token = token.trim();
    if token.is_empty() || token.chars().any(char::is_whitespace) {
        return Err(api_error(GithubApiErrorKind::Configuration, None));
    }
    Ok(token.to_owned())
}

fn parse_issue(value: &Value) -> Option<GithubIssue> {
    Some(GithubIssue {
        number: value.get("number")?.as_u64()?,
        title: value.get("title")?.as_str()?.to_owned(),
        body: value.get("body").and_then(Value::as_str).map(str::to_owned),
        state: value.get("state")?.as_str()?.to_owned(),
        html_url: value.get("html_url")?.as_str()?.to_owned(),
        is_pull_request: value.get("pull_request").is_some(),
    })
}

fn parse_pull(value: &Value) -> Option<GithubPullRequest> {
    let merged = value
        .get("merged")
        .and_then(Value::as_bool)
        .or_else(|| value.get("merged_at").map(|time| !time.is_null()))
        .unwrap_or(false);
    Some(GithubPullRequest {
        number: value.get("number")?.as_u64()?,
        html_url: value.get("html_url")?.as_str()?.to_owned(),
        state: value.get("state")?.as_str()?.to_owned(),
        merged,
        head_ref: value.get("head")?.get("ref")?.as_str()?.to_owned(),
        head_sha: value.get("head")?.get("sha")?.as_str()?.to_owned(),
    })
}

fn status_error(status: StatusCode, retry_after: Option<String>) -> GithubApiError {
    let kind = match status {
        StatusCode::UNAUTHORIZED => GithubApiErrorKind::Unauthorized,
        StatusCode::FORBIDDEN if retry_after.is_some() => GithubApiErrorKind::Retryable,
        StatusCode::FORBIDDEN => GithubApiErrorKind::Forbidden,
        StatusCode::TOO_MANY_REQUESTS => GithubApiErrorKind::Retryable,
        StatusCode::NOT_FOUND => GithubApiErrorKind::NotFound,
        status if status.is_server_error() => GithubApiErrorKind::Retryable,
        _ => GithubApiErrorKind::InvalidResponse,
    };
    api_error(kind, retry_after)
}

fn api_error(kind: GithubApiErrorKind, retry_after: Option<String>) -> GithubApiError {
    GithubApiError { kind, retry_after }
}

fn valid_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'.' || byte == b'_'
        })
        && value != "."
        && value != ".."
}

fn valid_api_origin(value: &str) -> bool {
    Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    })
}

fn valid_production_api_origin(value: &str) -> bool {
    valid_api_origin(value)
        && Url::parse(value).is_ok_and(|url| {
            url.host_str() == Some("api.github.com") && (url.path().is_empty() || url.path() == "/")
        })
}
