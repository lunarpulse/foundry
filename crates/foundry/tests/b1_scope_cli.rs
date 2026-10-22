use foundry_ledger::{ProductConfig, ProductStore, RepositoryIdentity, ScopeSourceManifest};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream, ToSocketAddrs},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const INSTALL: &str = "scope-cli-install";
const DATABASE: &str = "scope-cli-db";

fn scratch() -> std::path::PathBuf {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "foundry-b1-scope-cli-{}-{n}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn config() -> ProductConfig {
    ProductConfig {
        installation_id: INSTALL.into(),
        database_id: DATABASE.into(),
    }
}

fn run_cli(root: &std::path::Path, args: &[&str], stdin: &str) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_foundry"))
        .args(args)
        .env("HOME", root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn cli_scope(root: &std::path::Path, args: &[&str]) -> std::process::Output {
    run_cli(root, args, "")
}

fn check_success(out: &std::process::Output) {
    assert!(
        out.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

struct ServerGuard {
    child: Option<Child>,
    stderr: Arc<Mutex<BoundedStderr>>,
    secrets: Arc<Mutex<Vec<String>>>,
    stderr_reader: Option<thread::JoinHandle<()>>,
}

#[derive(Default)]
struct BoundedStderr {
    captured: Vec<u8>,
    total_bytes: usize,
}

impl ServerGuard {
    fn new(mut child: Child) -> Self {
        let stderr_pipe = child.stderr.take().expect("server stderr is piped");
        let stderr = Arc::new(Mutex::new(BoundedStderr::default()));
        let captured = Arc::clone(&stderr);
        let reader = thread::spawn(move || {
            let mut pipe = stderr_pipe;
            let mut chunk = [0u8; 1024];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(size) => {
                        if let Ok(mut log) = captured.lock() {
                            log.total_bytes = log.total_bytes.saturating_add(size);
                            let remaining = 64 * 1024usize.saturating_sub(log.captured.len());
                            log.captured
                                .extend_from_slice(&chunk[..size.min(remaining)]);
                        }
                    }
                }
            }
        });
        Self {
            child: Some(child),
            stderr,
            secrets: Arc::new(Mutex::new(Vec::new())),
            stderr_reader: Some(reader),
        }
    }

    fn redact_secret(&self, secret: &str) {
        if let Ok(mut secrets) = self.secrets.lock() {
            secrets.push(secret.to_owned());
        }
    }

    fn diagnostics(&self) -> String {
        let Ok(log) = self.stderr.lock() else {
            return "server stderr unavailable".into();
        };
        let text = String::from_utf8_lossy(&log.captured);
        let mut redacted = text.into_owned();
        if let Ok(secrets) = self.secrets.lock() {
            for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
                redacted = redacted.replace(secret, "[redacted]");
            }
        }
        let safe_lines = redacted
            .lines()
            .map(sanitize_stderr_line)
            .collect::<Vec<_>>()
            .join("\\n");
        format!(
            "stderr_bytes={} stderr_tail={}",
            log.total_bytes,
            safe_lines
                .chars()
                .rev()
                .take(2048)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>()
        )
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
        if thread::panicking() {
            eprintln!("b1_scope_cli child diagnostics: {}", self.diagnostics());
        }
    }
}

fn sanitize_stderr_line(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    if [
        "password",
        "access_token",
        "authorization",
        "bearer",
        "session_digest",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        "[redacted server diagnostic line]".to_owned()
    } else {
        line.to_owned()
    }
}

const HTTP_REQUEST_BUDGET: Duration = Duration::from_secs(15);
const HTTP_HEADER_BUDGET: usize = 16 * 1024;
const HTTP_BODY_BUDGET: usize = 1024 * 1024;
type HttpParseResult = Result<Option<(u16, Vec<u8>)>, (&'static str, &'static str)>;
thread_local! {
    static HTTP_SCENARIO_DEADLINE: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

#[derive(Debug)]
struct HttpFailure {
    operation: &'static str,
    stage: &'static str,
    method: String,
    path: String,
    elapsed: Duration,
    bytes_received: usize,
    detail: &'static str,
}

impl std::fmt::Display for HttpFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "HTTP {} {} failed at {}/{} after {}ms ({} response bytes): {}",
            self.method,
            self.path,
            self.operation,
            self.stage,
            self.elapsed.as_millis(),
            self.bytes_received,
            self.detail
        )
    }
}

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

fn http(
    address: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: &str,
) -> Result<HttpResponse, HttpFailure> {
    http_with_budget(address, method, path, token, body, HTTP_REQUEST_BUDGET)
}

fn http_with_budget(
    address: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: &str,
    budget: Duration,
) -> Result<HttpResponse, HttpFailure> {
    let started = Instant::now();
    let request_deadline = started + budget;
    let deadline = HTTP_SCENARIO_DEADLINE.with(|slot| {
        slot.get()
            .map_or(request_deadline, |overall| overall.min(request_deadline))
    });
    let operation = if method == "POST" && path == "/api/product/login" {
        "login_kdf"
    } else if method == "GET" && path == "/api/product/export" && token.is_none() {
        "readiness"
    } else {
        "request"
    };
    let failure = |stage, bytes_received, detail| HttpFailure {
        operation,
        stage,
        method: method.to_owned(),
        path: path.split('?').next().unwrap_or(path).to_owned(),
        elapsed: started.elapsed(),
        bytes_received,
        detail,
    };
    let socket = address
        .to_socket_addrs()
        .map_err(|_| failure("connect", 0, "address resolution failed"))?
        .next()
        .ok_or_else(|| failure("connect", 0, "no loopback address"))?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    let mut stream = TcpStream::connect_timeout(&socket, remaining)
        .map_err(|_| failure("connect", 0, "connection failed or deadline elapsed"))?;
    stream
        .set_write_timeout(Some(deadline.saturating_duration_since(Instant::now())))
        .map_err(|_| failure("request_write", 0, "could not set finite write timeout"))?;
    stream
        .set_read_timeout(Some(deadline.saturating_duration_since(Instant::now())))
        .map_err(|_| failure("response_headers", 0, "could not set finite read timeout"))?;
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        body.len()
    );
    if let Some(token) = token {
        request.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream
        .write_all(request.as_bytes())
        .map_err(|_| failure("request_write", 0, "request write failed"))?;

    let mut response = Vec::new();
    let mut chunk = [0u8; 2048];
    loop {
        if response.len() > HTTP_HEADER_BUDGET + HTTP_BODY_BUDGET + 32 * 1024 {
            return Err(failure(
                "response_body",
                response.len(),
                "response exceeded byte budget",
            ));
        }
        if let Some(parsed) = parse_http_response(&response)
            .map_err(|(stage, detail)| failure(stage, response.len(), detail))?
        {
            eprintln!(
                "b1_scope_cli request method={} path={} elapsed_ms={} bytes_received={}",
                method,
                path.split('?').next().unwrap_or(path),
                started.elapsed().as_millis(),
                response.len()
            );
            return Ok(HttpResponse {
                status: parsed.0,
                body: parsed.1,
            });
        }
        if Instant::now() >= deadline {
            let stage = response_wait_stage(&response);
            return Err(failure(
                stage,
                response.len(),
                "overall request deadline elapsed",
            ));
        }
        stream
            .set_read_timeout(Some(deadline.saturating_duration_since(Instant::now())))
            .map_err(|_| {
                failure(
                    "response_body",
                    response.len(),
                    "could not update read timeout",
                )
            })?;
        match stream.read(&mut chunk) {
            Ok(0) => {
                if let Some(parsed) = parse_http_response_eof(&response)
                    .map_err(|(stage, detail)| failure(stage, response.len(), detail))?
                {
                    eprintln!(
                        "b1_scope_cli request method={} path={} elapsed_ms={} bytes_received={}",
                        method,
                        path.split('?').next().unwrap_or(path),
                        started.elapsed().as_millis(),
                        response.len()
                    );
                    return Ok(HttpResponse {
                        status: parsed.0,
                        body: parsed.1,
                    });
                }
                return Err(failure(
                    "eof_wait",
                    response.len(),
                    "connection closed before HTTP framing completed",
                ));
            }
            Ok(size) => response.extend_from_slice(&chunk[..size]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                let stage = response_wait_stage(&response);
                return Err(failure(
                    stage,
                    response.len(),
                    "read timed out before HTTP framing completed",
                ));
            }
            Err(_) => {
                return Err(failure(
                    "response_body",
                    response.len(),
                    "response read failed",
                ));
            }
        }
    }
}

fn response_parts(
    address: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: &str,
) -> (u16, Vec<u8>) {
    let response =
        http(address, method, path, token, body).unwrap_or_else(|error| panic!("{error}"));
    (response.status, response.body)
}

fn response_status(
    address: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: &str,
) -> u16 {
    response_parts(address, method, path, token, body).0
}

fn wait_for_http_readiness(address: &str, deadline: Instant) {
    let mut last_failure = "no HTTP readiness response".to_owned();
    loop {
        if Instant::now() >= deadline {
            panic!(
                "product HTTP readiness deadline elapsed: GET /api/product/export; {last_failure}"
            );
        }
        match http_with_budget(
            address,
            "GET",
            "/api/product/export",
            None,
            "",
            Duration::from_millis(500),
        ) {
            Ok(response) if response.status == 401 || response.status == 403 => return,
            Ok(response) => {
                last_failure = format!("unexpected readiness status {}", response.status)
            }
            Err(error) => last_failure = error.to_string(),
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

fn response_wait_stage(bytes: &[u8]) -> &'static str {
    let Some(header_end) = find_header_end(bytes) else {
        return "response_headers";
    };
    let Ok(headers) = std::str::from_utf8(&bytes[..header_end]) else {
        return "response_headers";
    };
    if headers.lines().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("content-length:") || lower.starts_with("transfer-encoding:")
    }) {
        "response_body"
    } else {
        "eof_wait"
    }
}

fn parse_http_response(bytes: &[u8]) -> HttpParseResult {
    let Some(header_end) = find_header_end(bytes) else {
        if bytes.len() > HTTP_HEADER_BUDGET {
            return Err(("response_headers", "header byte budget exceeded"));
        }
        return Ok(None);
    };
    if header_end > HTTP_HEADER_BUDGET {
        return Err(("response_headers", "header byte budget exceeded"));
    }
    let header = std::str::from_utf8(&bytes[..header_end])
        .map_err(|_| ("response_headers", "headers are not UTF-8"))?;
    let mut lines = header.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or(("response_headers", "malformed status line"))?;
    let mut content_length = None;
    let mut chunked = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(("response_headers", "malformed response header"));
        };
        if name.eq_ignore_ascii_case("content-length") {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| ("response_headers", "invalid content length"))?,
            );
        } else if name.eq_ignore_ascii_case("transfer-encoding")
            && value.trim().eq_ignore_ascii_case("chunked")
        {
            chunked = true;
        }
    }
    if let Some(length) = content_length {
        if length > HTTP_BODY_BUDGET {
            return Err(("response_body", "body byte budget exceeded"));
        }
        let body_start = header_end + 4;
        if bytes.len() < body_start + length {
            return Ok(None);
        }
        return Ok(Some((
            status,
            bytes[body_start..body_start + length].to_vec(),
        )));
    }
    if chunked {
        return parse_chunked_body(&bytes[header_end + 4..])
            .map(|body| body.map(|body| (status, body)));
    }
    Ok(None)
}

fn parse_chunked_body(mut bytes: &[u8]) -> Result<Option<Vec<u8>>, (&'static str, &'static str)> {
    let mut body = Vec::new();
    loop {
        let Some(line_end) = bytes.windows(2).position(|window| window == b"\r\n") else {
            return Ok(None);
        };
        let line = std::str::from_utf8(&bytes[..line_end])
            .map_err(|_| ("response_body", "invalid chunk size"))?;
        let size_text = line.split(';').next().unwrap_or("");
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| ("response_body", "invalid chunk size"))?;
        bytes = &bytes[line_end + 2..];
        if size == 0 {
            if bytes.starts_with(b"\r\n") {
                return Ok(Some(body));
            }
            if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                return Ok(Some(body));
            }
            return Ok(None);
        }
        if body.len().saturating_add(size) > HTTP_BODY_BUDGET {
            return Err(("response_body", "body byte budget exceeded"));
        }
        if bytes.len() < size + 2 {
            return Ok(None);
        }
        if &bytes[size..size + 2] != b"\r\n" {
            return Err(("response_body", "malformed chunk terminator"));
        }
        body.extend_from_slice(&bytes[..size]);
        bytes = &bytes[size + 2..];
    }
}

fn parse_http_response_eof(bytes: &[u8]) -> HttpParseResult {
    let Some(header_end) = find_header_end(bytes) else {
        return Ok(None);
    };
    let header = std::str::from_utf8(&bytes[..header_end])
        .map_err(|_| ("response_headers", "headers are not UTF-8"))?;
    if header.lines().any(|line| {
        line.to_ascii_lowercase()
            .starts_with("transfer-encoding: chunked")
    }) {
        return parse_http_response(bytes);
    }
    if header
        .lines()
        .any(|line| line.to_ascii_lowercase().starts_with("content-length:"))
    {
        return parse_http_response(bytes);
    }
    let status = header
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or(("response_headers", "malformed status line"))?;
    Ok(Some((status, bytes[header_end + 4..].to_vec())))
}

#[test]
fn cli_http_framing_parser_handles_partial_input_and_rejects_bad_or_oversize_frames() {
    let partial = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nh";
    assert_eq!(parse_http_response(partial).unwrap(), None);
    let complete = [partial.as_slice(), b"e\r\n3\r\nllo\r\n0\r\n\r\n".as_slice()].concat();
    assert_eq!(
        parse_http_response(&complete).unwrap().unwrap(),
        (200, b"hello".to_vec())
    );
    assert!(parse_http_response(b"not a status line\r\n\r\n").is_err());
    let oversize = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
        HTTP_BODY_BUDGET + 1
    );
    assert_eq!(
        parse_http_response(oversize.as_bytes()).unwrap_err().0,
        "response_body"
    );
}

fn login(address: &str, actor: &str, password: &str) -> String {
    let body = serde_json::json!({"actor_id":actor,"password":password}).to_string();
    let response = http(address, "POST", "/api/product/login", None, &body).unwrap();
    assert_eq!(response.status, 200);
    serde_json::from_slice::<serde_json::Value>(&response.body).unwrap()["access_token"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn real_scope_cli_and_loopback_product_server_enforce_two_user_source_isolation() {
    let root = scratch();
    let db = root.join("product.sqlite");
    let manifest_path = root.join("bootstrap.json");
    let owner_session = root.join("owner.session");
    let source_a = ScopeSourceManifest {
        connector_id: "github-a".into(),
        source_instance_id: "source-a".into(),
        generation: 1,
        identity: RepositoryIdentity::canonicalized(
            "acme",
            "github",
            "https://api.github.com",
            "42",
        )
        .unwrap(),
        api_owner: Some("acme".into()),
        api_repo: Some("repo-a".into()),
        legacy_repository_locator: "42".into(),
        scope_query: "open+foundry:make".into(),
        enabled: true,
    };
    let source_b = ScopeSourceManifest {
        connector_id: "github-b".into(),
        source_instance_id: "source-b".into(),
        generation: 1,
        identity: RepositoryIdentity::canonicalized(
            "acme",
            "github",
            "https://github.enterprise.example",
            "42",
        )
        .unwrap(),
        api_owner: Some("acme".into()),
        api_repo: Some("repo-b".into()),
        legacy_repository_locator: "42".into(),
        scope_query: "open+foundry:make".into(),
        enabled: true,
    };
    let bootstrap = serde_json::json!({
        "sources":[source_a,source_b],
        "grants":[{"actor_id":"owner","source_instance_id":"source-a","capabilities":["read_order","export_evidence","read_operational_audit"]}]
    });
    std::fs::write(&manifest_path, serde_json::to_vec(&bootstrap).unwrap()).unwrap();

    let bootstrap_out = run_cli(
        &root,
        &[
            "product-bootstrap",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            INSTALL,
            "--database-id",
            DATABASE,
            "--manifest",
            manifest_path.to_str().unwrap(),
        ],
        "owner\nowner-password-strong\n",
    );
    check_success(&bootstrap_out);
    assert!(!String::from_utf8_lossy(&bootstrap_out.stdout).contains("owner-password-strong"));

    let login_out = run_cli(
        &root,
        &[
            "product-login",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            INSTALL,
            "--database-id",
            DATABASE,
            "--session-file",
            owner_session.to_str().unwrap(),
        ],
        "owner\nowner-password-strong\n",
    );
    check_success(&login_out);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&owner_session)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    let account = run_cli(
        &root,
        &[
            "product-account",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            INSTALL,
            "--database-id",
            DATABASE,
            "--session-file",
            owner_session.to_str().unwrap(),
            "--actor",
            "viewer",
            "--role",
            "approver",
        ],
        "viewer-password-strong\n",
    );
    check_success(&account);
    assert!(!String::from_utf8_lossy(&account.stdout).contains("viewer-password-strong"));

    let config_source = cli_scope(
        &root,
        &[
            "product-scope",
            "--action",
            "configure-source",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            INSTALL,
            "--database-id",
            DATABASE,
            "--session-file",
            owner_session.to_str().unwrap(),
            "--connector",
            "github-c",
            "--organization",
            "acme",
            "--provider",
            "github",
            "--api-origin",
            "https://api.github.com",
            "--repository-id",
            "99",
            "--repository-locator",
            "acme/repo",
            "--scope-query",
            "open+foundry:make",
            "--enabled",
            "true",
            "--expected-revision",
            "0",
            "--request-key",
            "configure-c",
        ],
    );
    check_success(&config_source);
    assert!(String::from_utf8_lossy(&config_source.stdout).contains("source_revision=1"));
    let source_query = cli_scope(
        &root,
        &[
            "product-scope",
            "--action",
            "sources",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            INSTALL,
            "--database-id",
            DATABASE,
            "--session-file",
            owner_session.to_str().unwrap(),
        ],
    );
    check_success(&source_query);
    assert!(String::from_utf8_lossy(&source_query.stdout).contains("source-a"));
    assert!(String::from_utf8_lossy(&source_query.stdout).contains("revision=1"));

    let grant = cli_scope(
        &root,
        &[
            "product-scope",
            "--action",
            "grant",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            INSTALL,
            "--database-id",
            DATABASE,
            "--session-file",
            owner_session.to_str().unwrap(),
            "--subject",
            "viewer",
            "--source-instance",
            "source-a",
            "--capability",
            "read_order",
            "--expected-scope-revision",
            "0",
            "--request-key",
            "grant-viewer-a",
        ],
    );
    check_success(&grant);
    let grants = cli_scope(
        &root,
        &[
            "product-scope",
            "--action",
            "grants",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            INSTALL,
            "--database-id",
            DATABASE,
            "--session-file",
            owner_session.to_str().unwrap(),
            "--subject",
            "viewer",
            "--source-instance",
            "source-a",
        ],
    );
    check_success(&grants);
    assert!(String::from_utf8_lossy(&grants.stdout).contains("capability=read_order enabled=true"));

    let mut store = ProductStore::open(&db, config()).unwrap();
    store
        .admit_order(
            "order-a",
            "github-a",
            "42",
            "issue:1",
            "dep-bump",
            "payload-a",
            chrono::Utc::now(),
        )
        .unwrap();
    store
        .admit_order(
            "order-b",
            "github-b",
            "42",
            "issue:1",
            "dep-bump",
            "payload-b",
            chrono::Utc::now(),
        )
        .unwrap();
    drop(store);

    let socket = match TcpListener::bind("127.0.0.1:0") {
        Ok(socket) => socket,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            panic!(
                "sandbox denied loopback bind ({error}); rerun with `CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --locked --offline -p foundry --test b1_scope_cli`"
            );
        }
        Err(error) => panic!("loopback test bind failed: {error}"),
    };
    let address = socket.local_addr().unwrap();
    drop(socket);
    let address = address.to_string();
    let child = Command::new(env!("CARGO_BIN_EXE_foundry"))
        .args([
            "serve",
            "--product",
            "--db",
            db.to_str().unwrap(),
            "--bind",
            &address,
        ])
        .env("HOME", &root)
        .env("FOUNDRY_INSTALLATION_ID", INSTALL)
        .env("FOUNDRY_DATABASE_ID", DATABASE)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let server = ServerGuard::new(child);
    let scenario_started = Instant::now();
    HTTP_SCENARIO_DEADLINE.with(|slot| slot.set(Some(scenario_started + Duration::from_secs(120))));
    wait_for_http_readiness(&address, scenario_started + Duration::from_secs(5));
    let owner_token = login(&address, "owner", "owner-password-strong");
    let viewer_token = login(&address, "viewer", "viewer-password-strong");
    for secret in [
        "owner-password-strong",
        "viewer-password-strong",
        owner_token.as_str(),
        viewer_token.as_str(),
    ] {
        server.redact_secret(secret);
    }
    for endpoint in ["/api/product/export", "/api/product/audit"] {
        let (status, body) = response_parts(&address, "GET", endpoint, Some(&owner_token), "");
        assert_eq!(status, 200, "Owner has explicit capability for source-a");
        let body = String::from_utf8_lossy(&body);
        assert!(
            !body.contains("order-b"),
            "foreign repository must not appear in product projection"
        );
        assert!(!body.contains("raw_prefix_head") && !body.contains("head_hash"));
    }
    assert_eq!(
        response_status(
            &address,
            "GET",
            "/api/product/export",
            Some(&viewer_token),
            ""
        ),
        403,
        "ReadOrder alone does not authorize export"
    );
    assert_eq!(
        response_status(
            &address,
            "GET",
            "/api/product/audit",
            Some(&viewer_token),
            ""
        ),
        403,
        "ReadOrder alone does not authorize operational audit"
    );
    let (status, body) = response_parts(
        &address,
        "GET",
        "/api/product/orders",
        Some(&owner_token),
        "",
    );
    assert_eq!(status, 200);
    let body = String::from_utf8_lossy(&body);
    assert!(body.contains("order-a"));
    assert!(!body.contains("order-b"));
    assert_eq!(
        response_parts(
            &address,
            "GET",
            "/api/product/orders/order-b",
            Some(&owner_token),
            ""
        )
        .0,
        404
    );
    let (status, body) = response_parts(
        &address,
        "GET",
        "/api/product/orders",
        Some(&viewer_token),
        "",
    );
    assert_eq!(status, 200);
    assert!(String::from_utf8_lossy(&body).contains("order-a"));
    assert!(!String::from_utf8_lossy(&body).contains("order-b"));

    let scope_change = |action: &str, capability: &str, revision: &str, request_key: &str| {
        cli_scope(
            &root,
            &[
                "product-scope",
                "--action",
                action,
                "--db",
                db.to_str().unwrap(),
                "--installation-id",
                INSTALL,
                "--database-id",
                DATABASE,
                "--session-file",
                owner_session.to_str().unwrap(),
                "--subject",
                "viewer",
                "--source-instance",
                "source-a",
                "--capability",
                capability,
                "--expected-scope-revision",
                revision,
                "--request-key",
                request_key,
            ],
        )
    };
    check_success(&scope_change(
        "grant",
        "export_evidence",
        "1",
        "grant-viewer-export",
    ));
    assert_eq!(
        response_status(
            &address,
            "GET",
            "/api/product/export",
            Some(&viewer_token),
            ""
        ),
        200
    );
    check_success(&scope_change(
        "revoke",
        "export_evidence",
        "2",
        "revoke-viewer-export",
    ));
    assert_eq!(
        response_status(
            &address,
            "GET",
            "/api/product/export",
            Some(&viewer_token),
            ""
        ),
        403,
        "committed export revocation applies to the still-valid session"
    );
    check_success(&scope_change(
        "grant",
        "read_operational_audit",
        "3",
        "grant-viewer-audit",
    ));
    assert_eq!(
        response_status(
            &address,
            "GET",
            "/api/product/audit",
            Some(&viewer_token),
            ""
        ),
        200
    );
    check_success(&scope_change(
        "revoke",
        "read_operational_audit",
        "4",
        "revoke-viewer-audit",
    ));
    assert_eq!(
        response_status(
            &address,
            "GET",
            "/api/product/audit",
            Some(&viewer_token),
            ""
        ),
        403,
        "committed audit revocation applies to the still-valid session"
    );

    let revoke = cli_scope(
        &root,
        &[
            "product-scope",
            "--action",
            "revoke",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            INSTALL,
            "--database-id",
            DATABASE,
            "--session-file",
            owner_session.to_str().unwrap(),
            "--subject",
            "viewer",
            "--source-instance",
            "source-a",
            "--capability",
            "read_order",
            "--expected-scope-revision",
            "5",
            "--request-key",
            "revoke-viewer-a",
        ],
    );
    check_success(&revoke);
    let (status, body) = response_parts(
        &address,
        "GET",
        "/api/product/orders",
        Some(&viewer_token),
        "",
    );
    assert_eq!(status, 200);
    assert!(String::from_utf8_lossy(&body).contains("\"orders\":[]"));
    assert_eq!(
        response_parts(
            &address,
            "GET",
            "/api/product/orders/order-a",
            Some(&viewer_token),
            ""
        )
        .0,
        404
    );

    let regrant = cli_scope(
        &root,
        &[
            "product-scope",
            "--action",
            "grant",
            "--db",
            db.to_str().unwrap(),
            "--installation-id",
            INSTALL,
            "--database-id",
            DATABASE,
            "--session-file",
            owner_session.to_str().unwrap(),
            "--subject",
            "viewer",
            "--source-instance",
            "source-a",
            "--capability",
            "read_order",
            "--expected-scope-revision",
            "6",
            "--request-key",
            "regrant-viewer-a",
        ],
    );
    check_success(&regrant);
    assert_eq!(
        response_parts(
            &address,
            "GET",
            "/api/product/orders/order-a",
            Some(&viewer_token),
            ""
        )
        .0,
        200,
        "same still-valid bearer observes current scope after regrant"
    );

    eprintln!(
        "b1_scope_cli trial elapsed_ms={}",
        scenario_started.elapsed().as_millis()
    );
    HTTP_SCENARIO_DEADLINE.with(|slot| slot.set(None));
    drop(server);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
    }
    let _ = std::fs::remove_dir_all(root);
}

struct SyntheticServerGuard(Option<thread::JoinHandle<()>>);

impl SyntheticServerGuard {
    fn join(mut self) {
        self.0
            .take()
            .expect("synthetic server thread")
            .join()
            .unwrap();
    }
}

impl Drop for SyntheticServerGuard {
    fn drop(&mut self) {
        if let Some(thread) = self.0.take() {
            let _ = thread.join();
        }
    }
}

fn synthetic_response_server(parts: Vec<(Vec<u8>, Duration)>) -> (String, SyntheticServerGuard) {
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => panic!(
            "sandbox denied synthetic loopback bind ({error}); rerun with `CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --locked --offline -p foundry --test b1_scope_cli cli_http_helper`"
        ),
        Err(error) => panic!("synthetic loopback bind failed: {error}"),
    };
    let address = listener.local_addr().unwrap().to_string();
    listener.set_nonblocking(true).unwrap();
    let thread = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(2);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(_) => return,
            }
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
        let mut request = [0u8; 1024];
        let _ = stream.read(&mut request);
        for (part, delay) in parts {
            if !delay.is_zero() {
                thread::sleep(delay);
            }
            if stream.write_all(&part).is_err() {
                break;
            }
        }
    });
    (address, SyntheticServerGuard(Some(thread)))
}

#[test]
fn cli_http_helper_parses_partial_chunked_body_with_framing() {
    let (address, server) = synthetic_response_server(vec![
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nh".to_vec(),
            Duration::ZERO,
        ),
        (
            b"e\r\n3\r\nllo\r\n0\r\n\r\n".to_vec(),
            Duration::from_millis(20),
        ),
    ]);
    let response = http_with_budget(
        &address,
        "GET",
        "/synthetic",
        None,
        "",
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"hello");
    server.join();
}

#[test]
fn cli_http_helper_rejects_malformed_and_oversize_responses_without_echoing_secrets() {
    let (address, server) = synthetic_response_server(vec![(
        b"not a status line\r\npassword=synthetic-secret\r\n\r\n".to_vec(),
        Duration::ZERO,
    )]);
    let malformed = http_with_budget(
        &address,
        "POST",
        "/synthetic",
        Some("synthetic-bearer"),
        "synthetic-password",
        Duration::from_secs(1),
    )
    .unwrap_err()
    .to_string();
    assert!(malformed.contains("response_headers"));
    assert!(
        !malformed.contains("synthetic-secret")
            && !malformed.contains("synthetic-bearer")
            && !malformed.contains("synthetic-password")
    );
    server.join();

    let (address, server) = synthetic_response_server(vec![(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            HTTP_BODY_BUDGET + 1
        )
        .into_bytes(),
        Duration::ZERO,
    )]);
    let oversized = http_with_budget(
        &address,
        "GET",
        "/oversize",
        None,
        "",
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert_eq!(oversized.stage, "response_body");
    assert_eq!(oversized.path, "/oversize");
    server.join();
}

#[test]
fn cli_http_helper_reports_stalled_body_phase_and_cleans_up() {
    let (address, server) = synthetic_response_server(vec![
        (b"HTTP/1.1 200 OK\r\n\r\nhe".to_vec(), Duration::ZERO),
        (Vec::new(), Duration::from_millis(180)),
    ]);
    let error = http_with_budget(
        &address,
        "GET",
        "/stalled",
        Some("secret-bearer"),
        "secret-password",
        Duration::from_millis(40),
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("GET /stalled failed at request/eof_wait"),
        "{error}"
    );
    assert!(error.contains("response bytes"));
    assert!(!error.contains("secret-bearer") && !error.contains("secret-password"));
    server.join();
}
