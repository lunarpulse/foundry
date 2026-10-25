use foundry_domain::ports::{PublishOutcome, Publisher};
use foundry_domain::{ExternalRef, Order};
use foundry_line::product::{ProductPublishReconcileOutcome, ProductPublishReconciler};
use foundry_watch::{
    GithubApiError, GithubApiErrorKind, GithubHttpResponse, GithubHttpTransport,
    GithubProductPublisher, GithubProductPublisherConfig, GithubRestClient, GithubRestConfig,
};
use reqwest::{Method, Url};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const FAKE_TOKEN: &str = "w7-offline-token-must-never-leak";

struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(label: &str) -> Self {
        let root = PathBuf::from("/home/cosmo/.hermes/cache/scratch/w7-github-adapter-tests");
        std::fs::create_dir_all(&root).expect("create scratch test root");
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = root.join(format!("{label}-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&path).expect("create unique scratch directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone)]
struct ResponseSpec {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl ResponseSpec {
    fn json(status: u16, value: Value) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: value.to_string(),
        }
    }
}

#[derive(Clone, Debug)]
struct SeenRequest {
    method: String,
    url: String,
    authorization: String,
    body: String,
}

struct MockServer {
    origin: String,
    transport: Arc<MockTransport>,
}

struct MockTransport {
    responses: Mutex<VecDeque<ResponseSpec>>,
    seen: Mutex<Vec<SeenRequest>>,
}

impl GithubHttpTransport for MockTransport {
    fn send(
        &self,
        method: &Method,
        url: &Url,
        authorization_header: &str,
        body: Option<&Value>,
    ) -> Result<GithubHttpResponse, GithubApiError> {
        self.seen.lock().unwrap().push(SeenRequest {
            method: method.to_string(),
            url: url.to_string(),
            authorization: authorization_header.to_owned(),
            body: body.map(Value::to_string).unwrap_or_default(),
        });
        let spec = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("mock response");
        Ok(GithubHttpResponse {
            status: spec.status,
            headers: spec.headers,
            body: spec.body.into_bytes(),
        })
    }
}

impl MockServer {
    fn start(specs: Vec<ResponseSpec>) -> Self {
        Self {
            origin: "https://api.github.test".into(),
            transport: Arc::new(MockTransport {
                responses: Mutex::new(specs.into()),
                seen: Mutex::new(Vec::new()),
            }),
        }
    }

    fn transport(&self) -> Arc<dyn GithubHttpTransport> {
        Arc::clone(&self.transport) as Arc<dyn GithubHttpTransport>
    }

    fn requests(&self) -> Vec<SeenRequest> {
        self.transport.seen.lock().unwrap().clone()
    }
}

fn token_file(root: &Path) -> PathBuf {
    let path = root.join("fake-token");
    std::fs::write(&path, FAKE_TOKEN).expect("write fake credential");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("restrict fake credential");
    }
    path
}

fn rest_client(root: &Path, server: &MockServer) -> GithubRestClient {
    GithubRestClient::with_transport(
        GithubRestConfig {
            owner: "test-owner".into(),
            repository: "test-repo".into(),
            token_file: token_file(root),
            api_origin: server.origin.clone(),
        },
        server.transport(),
    )
    .expect("REST client config")
}

fn pull(number: u64, state: &str, merged: bool, branch: &str, sha: &str) -> Value {
    json!({
        "number": number,
        "html_url": format!("https://github.example/pr/{number}"),
        "state": state,
        "merged": merged,
        "head": { "ref": branch, "sha": sha },
    })
}

fn assert_auth_and_no_secret(requests: &[SeenRequest]) {
    for request in requests {
        assert_eq!(request.authorization, format!("Bearer {FAKE_TOKEN}"));
        assert!(!request.url.contains(FAKE_TOKEN));
        assert!(!request.body.contains(FAKE_TOKEN));
    }
}

#[test]
fn rest_client_covers_issue_and_pr_operations_with_header_auth() {
    let scratch = ScratchDir::new("rest-operations");
    let responses = vec![
        ResponseSpec::json(
            200,
            json!([
                {"number": 4, "title": "make", "body": "task", "state": "open", "html_url": "https://github.example/issues/4"},
                {"number": 5, "title": "pull", "body": null, "state": "open", "html_url": "https://github.example/pulls/5", "pull_request": {}}
            ]),
        ),
        ResponseSpec::json(
            201,
            json!({"number": 6, "title": "new", "body": "body", "state": "open", "html_url": "https://github.example/issues/6"}),
        ),
        ResponseSpec::json(
            200,
            json!({"number": 6, "title": "new", "body": "body", "state": "open", "html_url": "https://github.example/issues/6"}),
        ),
        ResponseSpec::json(
            200,
            json!({"number": 6, "title": "new", "body": "body", "state": "closed", "html_url": "https://github.example/issues/6"}),
        ),
        ResponseSpec::json(
            201,
            pull(9, "open", false, "w7-verify/run", "a".repeat(40).as_str()),
        ),
        ResponseSpec::json(
            200,
            pull(9, "open", false, "w7-verify/run", "a".repeat(40).as_str()),
        ),
        ResponseSpec::json(
            200,
            pull(9, "closed", true, "w7-verify/run", "a".repeat(40).as_str()),
        ),
        ResponseSpec::json(
            200,
            json!([pull(
                9,
                "closed",
                true,
                "w7-verify/run",
                "a".repeat(40).as_str()
            )]),
        ),
    ];
    let server = MockServer::start(responses);
    let client = rest_client(scratch.path(), &server);

    let issues = client.list_issues_by_label().expect("list issues");
    assert_eq!(issues.len(), 1, "pull request rows are excluded");
    assert_eq!(issues[0].number, 4);
    assert!(!issues[0].is_pull_request);
    assert_eq!(client.create_issue("new", "body").unwrap().number, 6);
    assert_eq!(client.query_issue(6).unwrap().state, "open");
    assert_eq!(client.close_issue(6).unwrap().state, "closed");
    assert_eq!(
        client
            .create_pull_request("title", "head", "main", "body")
            .unwrap()
            .number,
        9
    );
    assert_eq!(client.query_pull_request(9).unwrap().number, 9);
    assert!(client.close_pull_request(9).unwrap().merged);
    assert_eq!(
        client
            .find_pull_request_by_head("w7-verify/run")
            .unwrap()
            .unwrap()
            .number,
        9
    );

    let requests = server.requests();
    assert_eq!(requests.len(), 8);
    assert!(requests[0].url.contains("labels=foundry%3Amake"));
    assert_eq!(requests[1].method, "POST");
    assert!(
        requests[1]
            .url
            .ends_with("/repos/test-owner/test-repo/issues")
    );
    assert!(requests[3].body.contains("\"state\":\"closed\""));
    assert!(requests[4].body.contains("\"head\":\"head\""));
    assert_eq!(requests[6].method, "PATCH");
    assert!(
        requests[6]
            .url
            .ends_with("/repos/test-owner/test-repo/pulls/9")
    );
    assert_auth_and_no_secret(&requests);
}

#[test]
fn rejected_auth_and_rate_limit_diagnostics_never_include_token() {
    for (status, headers, expected_kind) in [
        (401, vec![], GithubApiErrorKind::Unauthorized),
        (403, vec![], GithubApiErrorKind::Forbidden),
        (
            403,
            vec![("Retry-After".into(), "17".into())],
            GithubApiErrorKind::Retryable,
        ),
        (429, vec![], GithubApiErrorKind::Retryable),
    ] {
        let scratch = ScratchDir::new("status-mapping");
        let server = MockServer::start(vec![ResponseSpec {
            status,
            headers,
            body: format!("{{\"message\":\"{FAKE_TOKEN}\"}}"),
        }]);
        let client = rest_client(scratch.path(), &server);
        let error = client
            .query_issue(1)
            .expect_err("status must map to an error");
        assert_eq!(error.kind, expected_kind);
        if status == 403 && expected_kind == GithubApiErrorKind::Retryable {
            assert_eq!(error.retry_after.as_deref(), Some("17"));
        }
        let diagnostic = format!("{error:?} {error}");
        assert!(!diagnostic.contains(FAKE_TOKEN));
        let requests = server.requests();
        assert_auth_and_no_secret(&requests);
    }
}

#[test]
fn reconciler_reports_confirmed_rejected_and_pending_states() {
    let scratch = ScratchDir::new("reconcile-states");
    let branch = "w7-verify/reconcile";
    let server = MockServer::start(vec![
        ResponseSpec::json(
            200,
            json!([pull(31, "open", false, branch, &"a".repeat(40))]),
        ),
        ResponseSpec::json(
            200,
            json!([pull(32, "closed", true, branch, &"b".repeat(40))]),
        ),
        ResponseSpec::json(
            200,
            json!([pull(33, "closed", false, branch, &"c".repeat(40))]),
        ),
        ResponseSpec::json(200, json!([])),
    ]);
    let publisher = GithubProductPublisher::with_rest_transport(
        GithubProductPublisherConfig {
            owner: "test-owner".into(),
            repository: "test-repo".into(),
            token_file: token_file(scratch.path()),
            api_origin: server.origin.clone(),
            base_branch: "main".into(),
            branch_namespace: "w7-verify".into(),
            workdir_root: scratch.path().to_path_buf(),
        },
        Some(server.transport()),
    )
    .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    assert!(matches!(
        runtime
            .block_on(publisher.reconcile("open-operation"))
            .unwrap(),
        ProductPublishReconcileOutcome::Confirmed { .. }
    ));
    assert!(matches!(
        runtime
            .block_on(publisher.reconcile("merged-operation"))
            .unwrap(),
        ProductPublishReconcileOutcome::Confirmed { .. }
    ));
    assert!(matches!(
        runtime
            .block_on(publisher.reconcile("closed-operation"))
            .unwrap(),
        ProductPublishReconcileOutcome::Rejected { .. }
    ));
    assert_eq!(
        runtime
            .block_on(publisher.reconcile("pending-operation"))
            .unwrap(),
        ProductPublishReconcileOutcome::NotFound
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 4);
    assert_auth_and_no_secret(&requests);
}

#[test]
fn publisher_uses_ssh_local_bare_remote_and_replays_one_operation_pr() {
    let scratch = ScratchDir::new("ssh-publish");
    let remote = scratch.path().join("remote.git");
    let root = scratch.path().join("workdirs");
    let repo = root.join("foundry-die-w7-order-0/repo");
    std::fs::create_dir_all(repo.parent().unwrap()).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    git(&scratch.0, &["init", "--bare", remote.to_str().unwrap()]);
    git(&repo, &["init"]);
    git(&repo, &["config", "user.name", "W7 test"]);
    git(&repo, &["config", "user.email", "w7@example.invalid"]);
    std::fs::write(repo.join("result.txt"), "approved result").unwrap();
    git(&repo, &["add", "result.txt"]);
    git(&repo, &["commit", "-m", "approved result"]);
    let sha = git(&repo, &["rev-parse", "HEAD"]);

    let global_config = scratch.path().join("gitconfig");
    std::fs::write(
        &global_config,
        format!(
            "[url \"file://{}\"]\n\tinsteadOf = git@github.com:test-owner/test-repo.git\n",
            remote.display()
        ),
    )
    .unwrap();
    let previous = std::env::var_os("GIT_CONFIG_GLOBAL");
    // This test runs in the prescribed single-threaded crate gate.
    unsafe { std::env::set_var("GIT_CONFIG_GLOBAL", &global_config) };

    let operation_id = "operation-id-105";
    use sha2::Digest;
    let branch_digest = sha2::Sha256::digest(operation_id.as_bytes());
    let expected_branch = format!("w7-verify/{}", hex::encode(branch_digest));
    let pr_value = pull(23, "open", false, &expected_branch, &sha);
    let server = MockServer::start(vec![
        ResponseSpec::json(200, json!([])),
        ResponseSpec::json(201, pr_value.clone()),
        ResponseSpec::json(200, json!([pr_value.clone()])),
        ResponseSpec::json(200, json!([pr_value])),
    ]);
    let token_path = token_file(scratch.path());
    let publisher = GithubProductPublisher::with_rest_transport(
        GithubProductPublisherConfig {
            owner: "test-owner".into(),
            repository: "test-repo".into(),
            token_file: token_path,
            api_origin: server.origin.clone(),
            base_branch: "main".into(),
            branch_namespace: "w7-verify".into(),
            workdir_root: root,
        },
        Some(server.transport()),
    )
    .unwrap();
    let order = Order::new(
        "w7-order".into(),
        ExternalRef::github(
            "test-owner/test-repo",
            "4",
            "test-owner/test-repo#4:v1".into(),
        ),
        chrono::Utc::now(),
    );
    let computed_branch = publisher.branch_for_operation(operation_id);
    assert!(computed_branch.starts_with("w7-verify/"));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let first = runtime
        .block_on(publisher.publish(&order, operation_id, &sha))
        .expect("first publish");
    assert!(matches!(first, PublishOutcome::Confirmed { .. }));
    let second = runtime
        .block_on(publisher.publish(&order, operation_id, &sha))
        .expect("idempotent publish");
    assert_eq!(first, second);
    let reconciled = runtime
        .block_on(publisher.reconcile(operation_id))
        .expect("reconcile");
    assert!(matches!(
        reconciled,
        ProductPublishReconcileOutcome::Confirmed { .. }
    ));
    let branch_ref = format!("refs/heads/{computed_branch}");
    assert_eq!(
        git(&remote, &["show-ref", &branch_ref])
            .split_whitespace()
            .next(),
        Some(sha.as_str())
    );

    let requests = server.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "POST")
            .count(),
        1
    );
    assert_auth_and_no_secret(&requests);
    assert!(requests.iter().all(|request| !request.url.contains(&sha)));

    match previous {
        Some(value) => unsafe { std::env::set_var("GIT_CONFIG_GLOBAL", value) },
        None => unsafe { std::env::remove_var("GIT_CONFIG_GLOBAL") },
    }
}

fn git(directory: &Path, args: &[&str]) -> String {
    if args.first() == Some(&"init") && !directory.exists() && args.len() == 1 {
        std::fs::create_dir_all(directory).unwrap();
    }
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(directory)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[tokio::test]
async fn real_adapter_construction_inside_tokio_runtime_does_not_panic_or_make_network_calls() {
    let scratch = ScratchDir::new("tokio-real-construction");
    let token_path = token_file(scratch.path());
    let config = GithubProductPublisherConfig {
        owner: "test-owner".into(),
        repository: "test-repo".into(),
        token_file: token_path,
        api_origin: "https://api.github.com".into(),
        base_branch: "main".into(),
        branch_namespace: "w7-verify".into(),
        workdir_root: scratch.path().to_path_buf(),
    };
    let adapter = GithubProductPublisher::new(config).expect("real adapter construction");
    assert!(
        adapter
            .branch_for_operation("offline")
            .starts_with("w7-verify/")
    );
}

#[test]
fn bound_reconciliation_rejects_wrong_sha_and_wrong_ref_for_open_or_merged_prs() {
    let scratch = ScratchDir::new("reconcile-binding");
    let operation_id = "bound-operation";
    use sha2::Digest;
    let branch = format!(
        "w7-verify/{}",
        hex::encode(sha2::Sha256::digest(operation_id))
    );
    let expected_sha = "a".repeat(40);
    let server = MockServer::start(vec![
        ResponseSpec::json(
            200,
            json!([pull(41, "open", false, &branch, &"b".repeat(40))]),
        ),
        ResponseSpec::json(
            200,
            json!([pull(42, "closed", true, &branch, &"b".repeat(40))]),
        ),
        ResponseSpec::json(
            200,
            json!([pull(43, "open", false, "wrong/ref", &expected_sha)]),
        ),
        ResponseSpec::json(
            200,
            json!([pull(44, "closed", true, "wrong/ref", &expected_sha)]),
        ),
    ]);
    let publisher = GithubProductPublisher::with_rest_transport(
        GithubProductPublisherConfig {
            owner: "test-owner".into(),
            repository: "test-repo".into(),
            token_file: token_file(scratch.path()),
            api_origin: server.origin.clone(),
            base_branch: "main".into(),
            branch_namespace: "w7-verify".into(),
            workdir_root: scratch.path().to_path_buf(),
        },
        Some(server.transport()),
    )
    .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    for _ in 0..4 {
        assert!(matches!(
            runtime
                .block_on(publisher.reconcile_bound(operation_id, &expected_sha))
                .unwrap(),
            ProductPublishReconcileOutcome::Rejected { .. }
        ));
    }
}

#[test]
fn changed_head_aborts_publish_without_creating_remote_branch() {
    let scratch = ScratchDir::new("head-mutation");
    let remote = scratch.path().join("remote.git");
    let root = scratch.path().join("workdirs");
    let repo = root.join("foundry-die-w7-mutated-0/repo");
    std::fs::create_dir_all(repo.parent().unwrap()).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    git(
        scratch.path(),
        &["init", "--bare", remote.to_str().unwrap()],
    );
    git(&repo, &["init"]);
    git(&repo, &["config", "user.name", "W7 test"]);
    git(&repo, &["config", "user.email", "w7@example.invalid"]);
    std::fs::write(repo.join("result.txt"), "approved").unwrap();
    git(&repo, &["add", "result.txt"]);
    git(&repo, &["commit", "-m", "approved"]);
    let approved_sha = git(&repo, &["rev-parse", "HEAD"]);
    std::fs::write(repo.join("result.txt"), "mutated").unwrap();
    git(&repo, &["commit", "-am", "mutation"]);
    let global_config = scratch.path().join("gitconfig");
    std::fs::write(
        &global_config,
        format!(
            "[url \"file://{}\"]\n\tinsteadOf = git@github.com:test-owner/test-repo.git\n",
            remote.display()
        ),
    )
    .unwrap();
    let previous = std::env::var_os("GIT_CONFIG_GLOBAL");
    unsafe { std::env::set_var("GIT_CONFIG_GLOBAL", &global_config) };
    let server = MockServer::start(vec![]);
    let publisher = GithubProductPublisher::with_rest_transport(
        GithubProductPublisherConfig {
            owner: "test-owner".into(),
            repository: "test-repo".into(),
            token_file: token_file(scratch.path()),
            api_origin: server.origin.clone(),
            base_branch: "main".into(),
            branch_namespace: "w7-verify".into(),
            workdir_root: root,
        },
        Some(server.transport()),
    )
    .unwrap();
    let order = Order::new(
        "w7-mutated".into(),
        ExternalRef::github(
            "test-owner/test-repo",
            "9",
            "test-owner/test-repo#9:v1".into(),
        ),
        chrono::Utc::now(),
    );
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(publisher.publish(&order, "mutated-operation", &approved_sha));
    assert!(result.is_err());
    let branch = publisher.branch_for_operation("mutated-operation");
    let remote_ref = std::process::Command::new("git")
        .args(["for-each-ref", "--format=%(refname)", "refs/heads"])
        .current_dir(&remote)
        .output()
        .unwrap();
    assert!(remote_ref.status.success());
    assert!(
        !String::from_utf8_lossy(&remote_ref.stdout).contains(&branch),
        "mutation must leave no remote branch"
    );
    match previous {
        Some(value) => unsafe { std::env::set_var("GIT_CONFIG_GLOBAL", value) },
        None => unsafe { std::env::remove_var("GIT_CONFIG_GLOBAL") },
    }
}

struct TraceCapture(std::sync::Arc<Mutex<String>>);

impl tracing::Subscriber for TraceCapture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Fields(String);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                use std::fmt::Write;
                let _ = write!(self.0, "{}={value:?} ", field.name());
            }
        }
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        self.0.lock().unwrap().push_str(&fields.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[test]
fn failing_publish_trace_contains_no_token() {
    let scratch = ScratchDir::new("trace-secret-check");
    let publisher = GithubProductPublisher::new(GithubProductPublisherConfig {
        owner: "test-owner".into(),
        repository: "test-repo".into(),
        token_file: token_file(scratch.path()),
        api_origin: "https://api.github.com".into(),
        base_branch: "main".into(),
        branch_namespace: "w7-verify".into(),
        workdir_root: scratch.path().join("no-worktree"),
    })
    .unwrap();
    let order = Order::new(
        "trace-order".into(),
        ExternalRef::github(
            "test-owner/test-repo",
            "7",
            "test-owner/test-repo#7:v1".into(),
        ),
        chrono::Utc::now(),
    );
    let output = std::sync::Arc::new(Mutex::new(String::new()));
    let dispatch = tracing::Dispatch::new(TraceCapture(output.clone()));
    tracing::dispatcher::with_default(&dispatch, || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert!(
            runtime
                .block_on(publisher.publish(&order, "trace-operation", &"a".repeat(40)))
                .is_err()
        );
    });
    let output = output.lock().unwrap().clone();
    assert!(output.contains("GitHub publish failed"));
    assert!(!output.contains(FAKE_TOKEN));
}

#[test]
fn successful_local_publish_retained_git_configs_contain_no_token() {
    let scratch = ScratchDir::new("git-config-secret-check");
    let remote = scratch.path().join("remote.git");
    let root = scratch.path().join("workdirs");
    let repo = root.join("foundry-die-config-order-0/repo");
    std::fs::create_dir_all(repo.parent().unwrap()).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    git(
        scratch.path(),
        &["init", "--bare", remote.to_str().unwrap()],
    );
    git(&repo, &["init"]);
    git(&repo, &["config", "user.name", "W7 test"]);
    git(&repo, &["config", "user.email", "w7@example.invalid"]);
    std::fs::write(repo.join("result.txt"), "approved").unwrap();
    git(&repo, &["add", "result.txt"]);
    git(&repo, &["commit", "-m", "approved"]);
    let sha = git(&repo, &["rev-parse", "HEAD"]);
    let global_config = scratch.path().join("global-gitconfig");
    std::fs::write(
        &global_config,
        format!(
            "[url \"file://{}\"]\n\tinsteadOf = git@github.com:test-owner/test-repo.git\n",
            remote.display()
        ),
    )
    .unwrap();
    let previous = std::env::var_os("GIT_CONFIG_GLOBAL");
    unsafe { std::env::set_var("GIT_CONFIG_GLOBAL", &global_config) };
    use sha2::Digest;
    let operation = "config-scan-operation";
    let branch = format!("w7-verify/{}", hex::encode(sha2::Sha256::digest(operation)));
    let server = MockServer::start(vec![
        ResponseSpec::json(200, json!([])),
        ResponseSpec::json(201, pull(57, "open", false, &branch, &sha)),
    ]);
    let publisher = GithubProductPublisher::with_rest_transport(
        GithubProductPublisherConfig {
            owner: "test-owner".into(),
            repository: "test-repo".into(),
            token_file: token_file(scratch.path()),
            api_origin: server.origin.clone(),
            base_branch: "main".into(),
            branch_namespace: "w7-verify".into(),
            workdir_root: root,
        },
        Some(server.transport()),
    )
    .unwrap();
    let order = Order::new(
        "config-order".into(),
        ExternalRef::github(
            "test-owner/test-repo",
            "57",
            "test-owner/test-repo#57:v1".into(),
        ),
        chrono::Utc::now(),
    );
    let published = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(publisher.publish(&order, operation, &sha))
        .unwrap();
    assert!(matches!(published, PublishOutcome::Confirmed { .. }));
    let local_config = repo.join(".git/config");
    for path in [local_config, global_config] {
        let contents = std::fs::read_to_string(path).unwrap_or_default();
        assert!(!contents.contains(FAKE_TOKEN));
    }
    match previous {
        Some(value) => unsafe { std::env::set_var("GIT_CONFIG_GLOBAL", value) },
        None => unsafe { std::env::remove_var("GIT_CONFIG_GLOBAL") },
    }
}
