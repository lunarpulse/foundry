//! foundry — CLI (plan Task 8).
//! Subcommands:
//!   validate  — parse + validate a foundry.yaml, print resolved blueprint
//!   run       — submit an ad-hoc Order and drive it to FurnaceHold
//!   approve   — record a human Furnace decision (intent required)
//!   publish   — create the PR for an approved Order (idempotent)
//!   show      — print Order state + recent journal
use clap::{Parser, Subcommand};

mod deploy_profile;
use foundry_domain::ports::{
    ApprovalToken, Decision, DieOutput, Gate, Harness, Policy, PortResult, PublishOutcome,
    Publisher,
};
use foundry_domain::{ActorId, ActorKind, ExternalRef, Order};
use foundry_harness_cli::CliHarness;
use foundry_harness_cli::fake::FakeHarness;
use foundry_ledger::SqliteLedger;
use foundry_line::{
    DieStageExecutor, InspectingReviewRun, Line, PackReviewHost, UnavailableStageExecutor,
    durable_review_run,
};
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Mutex as TMutex;

#[derive(Parser)]
#[command(name = "foundry", version, about = "personal software factory")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// M4 T34: deployment profile — check boundary, emit systemd units/readme
    DeployProfile {
        /// run the on-host boundary + permission check
        #[arg(long)]
        check: bool,
        /// print systemd units to stdout
        #[arg(long)]
        units: bool,
        /// print the deployment readme to stdout
        #[arg(long)]
        readme: bool,
    },
    /// Validate a blueprint file.
    Validate {
        /// Path to foundry.yaml
        #[arg(default_value = "foundry.yaml")]
        path: String,
    },
    /// Submit an ad-hoc order and drive it to FurnaceHold.
    Run {
        /// Order id (defaults to generated)
        #[arg(long)]
        id: Option<String>,
        /// Dedup key (e.g. dep:serde)
        #[arg(long)]
        task: String,
        /// Die binary (default: fake harness, no side effects)
        #[arg(long, default_value = "")]
        die: String,
        /// Blueprint file
        #[arg(long, default_value = "foundry.yaml")]
        blueprint: String,
    },
    /// Approve an order in FurnaceHold.
    Approve {
        #[arg(long)]
        order: String,
        /// Why — recorded in the immutable journal (MAOS I4 lineage).
        #[arg(long)]
        intent: String,
        #[arg(long, default_value = "~/.foundry/ledger.db")]
        db: String,
    },
    /// Run the watch daemon (single-writer lease holder).
    Watch {
        /// One tick then exit (for cron-driven mode)
        #[arg(long, default_value_t = false)]
        once: bool,
        /// Tick interval seconds
        #[arg(long, default_value_t = 300)]
        every: u64,
        /// GitHub repo "owner/name" to poll for foundry:make issues
        #[arg(long, default_value = "lunarpulse/contextmesh-rs")]
        repo: String,
        /// Die binary path (empty = fake harness)
        #[arg(long, default_value = "")]
        die: String,
        /// Deprecated synthetic approval mode. Non-empty values are rejected.
        #[arg(long, default_value = "")]
        auto_approve: String,
        /// notify channel: discord | console | all
        #[arg(long, default_value = "discord")]
        notify_mode: String,
    },
    /// ProductAtomicV1 durable GitHub intake (requires a preconfigured source binding).
    ProductWatch {
        #[arg(long)]
        db: String,
        #[arg(long)]
        installation_id: String,
        #[arg(long)]
        database_id: String,
        /// Private Foundry service credential file (independent of GitHub token).
        #[arg(long)]
        service_file: String,
        #[arg(long, default_value = "lunarpulse/contextmesh-rs")]
        repo: String,
        #[arg(long)]
        repository_id: String,
        #[arg(long, default_value = "https://api.github.com")]
        api_origin: String,
        #[arg(long, default_value_t = false)]
        private: bool,
        /// Environment variable name holding the connector token; never a token value.
        #[arg(long)]
        credential_ref: Option<String>,
        #[arg(long, default_value_t = false)]
        once: bool,
        #[arg(long, default_value_t = 300)]
        every: u64,
    },
    /// Show order state + journal tail.
    Show {
        #[arg(long)]
        order: String,
        #[arg(long, default_value = "~/.foundry/ledger.db")]
        db: String,
    },
    /// M1 T15: operational status (read-only).
    Status {
        #[arg(long, default_value = "~/.foundry/ledger.db")]
        db: String,
    },
    /// M1 T15: health checks — chain, mirror, credentials, disk.
    Doctor {
        #[arg(long, default_value = "~/.foundry/ledger.db")]
        db: String,
    },
    /// M1 T15: mirror catch-up. DRY-RUN by default; --apply requires the
    /// single-writer lease (review #5: reconcile must not be a 2nd writer).
    /// T26: casting A/B cycle — run/judge/promote.
    Casting {
        #[arg(long)]
        run_profile: Option<String>,
        #[arg(long)]
        judge: Option<String>,
        #[arg(long)]
        promote: Option<String>,
        #[arg(long, default_value = "~/.foundry/corpus")]
        corpus: String,
        #[arg(long, default_value = "~/.foundry/casting")]
        casting_dir: String,
        #[arg(long, default_value = "42")]
        seed: u64,
    },
    /// T20: benchmark over corpus — baseline snapshot / compare vs baseline.
    Benchmark {
        #[arg(long)]
        baseline: Option<String>,
        #[arg(long)]
        compare: Option<String>,
        #[arg(long, default_value = "~/.foundry/corpus")]
        corpus: String,
    },
    /// T18: derive corpus manifests from the journal (idempotent).
    Corpus {
        #[arg(long, default_value = "~/.foundry/ledger.db")]
        db: String,
        #[arg(long, default_value = "~/.foundry/corpus")]
        out: String,
    },
    Reconcile {
        #[arg(long, default_value = "~/.foundry/ledger.db")]
        db: String,
        #[arg(long)]
        apply: bool,
    },
    /// M1 T11: read-model HTTP API (127.0.0.1 by default).
    Serve {
        #[arg(long, default_value = "~/.foundry/ledger.db")]
        db: String,
        #[arg(long, default_value = "127.0.0.1:7700")]
        bind: String,
        /// Serve the isolated ProductAtomicV1 authentication and approval API.
        #[arg(long, default_value_t = false)]
        product: bool,
    },
    /// Initialize the first ProductAtomicV1 Owner from two bounded stdin lines:
    /// actor id, then password. Passwords are never accepted as argv values.
    ProductBootstrap {
        #[arg(long)]
        db: String,
        #[arg(long)]
        installation_id: String,
        #[arg(long)]
        database_id: String,
        /// Optional reviewed finite JSON source/grant manifest. Omission is
        /// an explicit zero-source, zero-grant bootstrap.
        #[arg(long)]
        manifest: Option<String>,
    },
    /// Verify product credentials using bounded two-line stdin and save the
    /// opaque bearer to a mode-0600 file.
    ProductLogin {
        #[arg(long)]
        db: String,
        #[arg(long)]
        installation_id: String,
        #[arg(long)]
        database_id: String,
        #[arg(long)]
        session_file: String,
    },
    /// Local password recovery; deliberately unavailable over HTTP.
    ProductReset {
        #[arg(long)]
        db: String,
        #[arg(long)]
        installation_id: String,
        #[arg(long)]
        database_id: String,
    },
    /// Create a non-Owner product account under the current Owner session.
    /// The new account password is read from one bounded stdin line.
    ProductAccount {
        #[arg(long)]
        db: String,
        #[arg(long)]
        installation_id: String,
        #[arg(long)]
        database_id: String,
        #[arg(long)]
        session_file: String,
        #[arg(long)]
        actor: String,
        #[arg(long, value_parser = ["viewer", "operator", "approver"])]
        role: String,
    },
    /// Issue a single-use approval grant; its token is written to a private file.
    ProductGrant {
        #[arg(long)]
        db: String,
        #[arg(long)]
        installation_id: String,
        #[arg(long)]
        database_id: String,
        #[arg(long)]
        session_file: String,
        #[arg(long)]
        order: String,
        #[arg(long)]
        token_file: String,
    },
    /// Approve through ProductAtomicV1 using a private session and grant file.
    ProductApprove {
        #[arg(long)]
        db: String,
        #[arg(long)]
        installation_id: String,
        #[arg(long)]
        database_id: String,
        #[arg(long)]
        session_file: String,
        #[arg(long)]
        order: String,
        #[arg(long)]
        grant_id: String,
        #[arg(long)]
        grant_file: String,
        #[arg(long)]
        expected_source_revision: i64,
        #[arg(long)]
        expected_attempt: i64,
        #[arg(long)]
        run_id: i64,
        #[arg(long)]
        generation: i64,
        #[arg(long)]
        artifact_digest: String,
        #[arg(long)]
        tuple_digest: String,
        #[arg(long)]
        intent: String,
        #[arg(long)]
        idempotency_key: String,
        /// Write the canonical approval receipt to a new mode-0600 file.
        #[arg(long)]
        receipt_out: Option<String>,
    },
    /// ProductAtomicV1 operations.
    Product {
        #[command(subcommand)]
        command: ProductCmd,
    },
    /// Query product holds/progress or accept an authenticated hold decision.
    /// Resume requires the configured ProductAtomicV1 frozen-pack runtime.
    ProductReview {
        #[arg(long)]
        db: String,
        #[arg(long)]
        installation_id: String,
        #[arg(long)]
        database_id: String,
        #[arg(long)]
        session_file: String,
        #[arg(long, value_parser = ["holds", "progress", "resume", "commit-pass", "commit-fail", "abort"])]
        action: String,
        #[arg(long)]
        order: Option<String>,
        #[arg(long)]
        hold: Option<String>,
        #[arg(long)]
        operation_id: Option<String>,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        idempotency_key: Option<String>,
    },
    /// Manage ProductAtomicV1 repository sources and scoped capabilities.
    ProductScope {
        #[arg(long, value_parser = ["sources", "configure-source", "grants", "grant", "revoke"])]
        action: String,
        #[arg(long)]
        db: String,
        #[arg(long)]
        installation_id: String,
        #[arg(long)]
        database_id: String,
        #[arg(long)]
        session_file: String,
        #[arg(long)]
        connector: Option<String>,
        #[arg(long)]
        organization: Option<String>,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        api_origin: Option<String>,
        #[arg(long)]
        repository_id: Option<String>,
        #[arg(long)]
        repository_locator: Option<String>,
        #[arg(long)]
        scope_query: Option<String>,
        #[arg(long)]
        enabled: Option<bool>,
        #[arg(long)]
        expected_revision: Option<i64>,
        #[arg(long)]
        subject: Option<String>,
        #[arg(long)]
        source_instance: Option<String>,
        #[arg(long)]
        capability: Option<String>,
        #[arg(long)]
        expected_scope_revision: Option<i64>,
        #[arg(long)]
        request_key: Option<String>,
    },
    /// Explicit stopped-process ProductAtomicV1 scope migration from a JSON manifest.
    ProductMigrateScope {
        #[arg(long)]
        db: String,
        #[arg(long)]
        installation_id: String,
        #[arg(long)]
        database_id: String,
        #[arg(long)]
        manifest: String,
    },
    /// Owner-managed source/action scoped service authority.
    ProductService {
        #[arg(long, value_parser = ["configure", "revoke", "query", "issue", "rotate"])]
        action: String,
        #[arg(long)]
        db: String,
        #[arg(long)]
        installation_id: String,
        #[arg(long)]
        database_id: String,
        #[arg(long)]
        session_file: String,
        #[arg(long)]
        service_id: String,
        #[arg(long, default_value_t = 0)]
        expected_revision: i64,
        #[arg(long)]
        request_key: String,
        #[arg(long)]
        lease_until: Option<String>,
        #[arg(long)]
        expires_at: Option<String>,
        #[arg(long)]
        source_instance: Option<String>,
        #[arg(long)]
        actions: Option<String>,
        #[arg(long)]
        credential_file: Option<String>,
    },
}

#[derive(Subcommand)]
enum ProductCmd {
    /// Publish a previously approved receipt through an explicitly configured adapter.
    Publish {
        #[arg(long)]
        db: String,
        #[arg(long)]
        installation_id: String,
        #[arg(long)]
        database_id: String,
        #[arg(long)]
        session_file: String,
        #[arg(long)]
        receipt_file: String,
    },
}

// ---------- M0 stubs (replaced in T9 by real Furnace/Publisher) ----------

struct LocalPolicy;
impl Policy for LocalPolicy {
    fn authorize(&self, _a: &ActorId, _action: &str, _r: &str) -> Decision {
        Decision::Allow {
            reason: "local owner".into(),
        }
    }
}

struct DigestGate;
impl Gate for DigestGate {
    fn verify(&self, _o: &Order, _t: &ApprovalToken, _d: &str) -> PortResult<()> {
        Ok(())
    }
}

struct DryRunPublisher;
impl Publisher for DryRunPublisher {
    fn publish(
        &self,
        _order: &Order,
        operation_id: &str,
        _digest: &str,
    ) -> impl Future<Output = PortResult<PublishOutcome>> + Send {
        async move {
            Ok(PublishOutcome::Confirmed {
                pr_url: format!("dryrun://{operation_id}"),
            })
        }
    }
}

fn expand(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    p.to_string()
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductBootstrapManifest {
    sources: Vec<foundry_ledger::ScopeSourceManifest>,
    grants: Vec<foundry_ledger::ScopeGrantManifest>,
}

fn read_product_json<T: serde::de::DeserializeOwned>(path: &str) -> Result<T, ()> {
    use std::io::Read;
    const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
    let file = std::fs::File::open(path).map_err(|_| ())?;
    if file.metadata().map_err(|_| ())?.len() > MAX_MANIFEST_BYTES {
        return Err(());
    }
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(());
    }
    serde_json::from_slice(&bytes).map_err(|_| ())
}

fn product_bootstrap_cmd(
    db: &str,
    installation_id: &str,
    database_id: &str,
    manifest_path: Option<&str>,
) -> i32 {
    use std::io::{BufRead, IsTerminal};
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        eprintln!(
            "product bootstrap requires the documented two-line stdin protocol; interactive TTY input is disabled"
        );
        return 2;
    }
    let mut input = stdin.lock();
    let mut actor = Vec::new();
    let mut password = Vec::new();
    let mut extra = Vec::new();
    if input.read_until(b'\n', &mut actor).is_err()
        || input.read_until(b'\n', &mut password).is_err()
        || input.read_until(b'\n', &mut extra).is_err()
        || actor.len() > 257
        || password.len() > 1025
        || !extra.is_empty()
    {
        eprintln!("product bootstrap input rejected");
        return 2;
    }
    trim_line(&mut actor);
    trim_line(&mut password);
    let actor = match std::str::from_utf8(&actor) {
        Ok(v) if !v.trim().is_empty() => v.to_owned(),
        _ => {
            eprintln!("product bootstrap input rejected");
            return 2;
        }
    };
    if password.is_empty() || password.len() > 1024 {
        eprintln!("product bootstrap input rejected");
        return 2;
    }
    let password = match String::from_utf8(password) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("product bootstrap input rejected");
            return 2;
        }
    };
    let phc = match foundry_ledger::product_identity::ProductPasswordKdf::hash(&password) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("product bootstrap credential processing failed");
            return 1;
        }
    };
    drop(password);
    let manifest = match manifest_path {
        Some(path) => match read_product_json::<ProductBootstrapManifest>(path) {
            Ok(manifest) => manifest,
            Err(()) => {
                eprintln!("product bootstrap manifest rejected");
                return 2;
            }
        },
        None => ProductBootstrapManifest {
            sources: Vec::new(),
            grants: Vec::new(),
        },
    };
    let mut store = match foundry_ledger::ProductStore::open(
        std::path::Path::new(db),
        foundry_ledger::ProductConfig {
            installation_id: installation_id.into(),
            database_id: database_id.into(),
        },
    ) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("product database contract/configuration rejected");
            return 1;
        }
    };
    match store.bootstrap_owner_scoped(&actor, &phc, &manifest.sources, &manifest.grants) {
        Ok(()) => {
            println!("product Owner initialized: {actor}");
            0
        }
        Err(_) => {
            eprintln!("product Owner initialization rejected");
            1
        }
    }
}

fn read_secret_line(label: &str) -> Result<String, ()> {
    use std::io::{BufRead, IsTerminal, Read};
    if std::io::stdin().is_terminal() {
        eprintln!("{label} requires bounded stdin input");
        return Err(());
    }
    let mut bytes = Vec::new();
    if std::io::stdin()
        .lock()
        .take(1026)
        .read_until(b'\n', &mut bytes)
        .is_err()
        || bytes.len() > 1024
        || bytes.is_empty()
    {
        eprintln!("{label} input rejected");
        return Err(());
    }
    trim_line(&mut bytes);
    String::from_utf8(bytes).map_err(|_| {
        eprintln!("{label} input rejected");
        ()
    })
}

fn private_file(path: &str, secret: &str) -> Result<(), ()> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path).map_err(|_| ())?;
    file.write_all(secret.as_bytes())
        .and_then(|_| file.write_all(b"\n"))
        .map_err(|_| ())
}

fn read_private(path: &str) -> Result<String, ()> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| ())?;
    if metadata.len() > 1024 || !metadata.file_type().is_file() {
        return Err(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(());
        }
    }
    let value = std::fs::read_to_string(path).map_err(|_| ())?;
    let value = value.trim_end_matches(['\r', '\n']).to_owned();
    if value.is_empty() || value.len() > 1024 {
        return Err(());
    }
    Ok(value)
}

fn product_config(installation_id: &str, database_id: &str) -> foundry_ledger::ProductConfig {
    foundry_ledger::ProductConfig {
        installation_id: installation_id.into(),
        database_id: database_id.into(),
    }
}

fn product_review_host(
    db: &str,
    installation_id: &str,
    database_id: &str,
) -> Result<Option<PackReviewHost>, String> {
    let Ok(pack) = std::env::var("FOUNDRY_REVIEW_PACK") else {
        return Ok(None);
    };
    let get_root = |name: &str| -> Result<std::path::PathBuf, String> {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(std::path::PathBuf::from)
            .ok_or_else(|| format!("{name} is required for product review continuation"))
    };
    let artifact_root = get_root("FOUNDRY_PRODUCT_ARTIFACT_ROOT")?;
    let session_root = get_root("FOUNDRY_PRODUCT_SESSION_ROOT")?;
    let claim_root = get_root("FOUNDRY_PRODUCT_CLAIM_ROOT")?;
    let executor: Box<dyn foundry_line::StageExecutor> = match (
        std::env::var("FOUNDRY_DIE_BINARY"),
        std::env::var("FOUNDRY_REPO_PATH"),
    ) {
        (Ok(binary), Ok(repo)) if !binary.is_empty() && !repo.is_empty() => {
            let profile = std::env::var("FOUNDRY_DIE_PROFILE").unwrap_or_else(|_| "coding".into());
            let timeout = std::env::var("FOUNDRY_DIE_TIMEOUT")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(600);
            Box::new(DieStageExecutor::new(
                std::path::PathBuf::from(binary),
                profile,
                timeout,
                std::path::PathBuf::from(repo),
                session_root.clone(),
            ))
        }
        _ => Box::new(UnavailableStageExecutor),
    };
    let store = foundry_ledger::ProductStore::open(
        std::path::Path::new(db),
        product_config(installation_id, database_id),
    )
    .map_err(|_| "product review database rejected".to_owned())?;
    PackReviewHost::with_product_store(
        &pack,
        executor,
        store,
        artifact_root,
        session_root,
        claim_root,
    )
    .map(Some)
    .map_err(|_| "product review runtime configuration rejected".to_owned())
}

fn product_login_cmd(
    db: &str,
    installation_id: &str,
    database_id: &str,
    session_file: &str,
) -> i32 {
    let actor = match read_secret_line("product login") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => return 2,
    };
    let password = match read_secret_line("product login") {
        Ok(v) => v,
        _ => return 2,
    };
    let mut store = match foundry_ledger::ProductStore::open(
        std::path::Path::new(db),
        product_config(installation_id, database_id),
    ) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("product database rejected");
            return 1;
        }
    };
    let session = match store.login(&actor, &password, chrono::Utc::now()) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("product authentication failed");
            return 1;
        }
    };
    drop(password);
    match private_file(session_file, session.bearer()) {
        Ok(()) => {
            println!("product session saved");
            0
        }
        Err(()) => {
            eprintln!("could not create private session file");
            1
        }
    }
}

fn product_reset_cmd(db: &str, installation_id: &str, database_id: &str) -> i32 {
    let actor = match read_secret_line("product reset") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => return 2,
    };
    let password = match read_secret_line("product reset") {
        Ok(v) if !v.is_empty() => v,
        _ => return 2,
    };
    let phc = match foundry_ledger::product_identity::ProductPasswordKdf::hash(&password) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("product credential processing failed");
            return 1;
        }
    };
    drop(password);
    let mut store = match foundry_ledger::ProductStore::open(
        std::path::Path::new(db),
        product_config(installation_id, database_id),
    ) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("product database rejected");
            return 1;
        }
    };
    match store.reset_credentials_local(&actor, &phc) {
        Ok(()) => {
            println!("product credential reset");
            0
        }
        Err(_) => {
            eprintln!("product credential reset rejected");
            1
        }
    }
}

fn product_account_cmd(
    db: &str,
    installation_id: &str,
    database_id: &str,
    session_file: &str,
    actor: &str,
    role: &str,
) -> i32 {
    let bearer = match read_private(session_file) {
        Ok(value) if value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()) => value,
        _ => {
            eprintln!("private session file rejected");
            return 1;
        }
    };
    let password = match read_secret_line("product account") {
        Ok(value) => value,
        Err(()) => return 2,
    };
    if actor.trim().is_empty() || actor.len() > 256 || password.len() < 12 {
        eprintln!("product account input rejected");
        return 2;
    }
    let phc = match foundry_ledger::product_identity::ProductPasswordKdf::hash(&password) {
        Ok(phc) => phc,
        Err(_) => {
            eprintln!("product account credential processing failed");
            return 1;
        }
    };
    drop(password);
    let mut store = match foundry_ledger::ProductStore::open(
        std::path::Path::new(db),
        product_config(installation_id, database_id),
    ) {
        Ok(store) => store,
        Err(_) => {
            eprintln!("product database rejected");
            return 1;
        }
    };
    match store.create_account(
        &sha256_hex(bearer.as_bytes()),
        actor,
        role,
        &phc,
        chrono::Utc::now(),
    ) {
        Ok(()) => {
            println!("product account created: {actor}");
            0
        }
        Err(_) => {
            eprintln!("product account creation unavailable");
            1
        }
    }
}

fn product_grant_cmd(
    db: &str,
    installation_id: &str,
    database_id: &str,
    session_file: &str,
    order: &str,
    token_file: &str,
) -> i32 {
    use rand_core::RngCore as _;
    let bearer = match read_private(session_file) {
        Ok(v) => v,
        Err(()) => {
            eprintln!("private session file rejected");
            return 1;
        }
    };
    let digest = sha256_hex(bearer.as_bytes());
    let mut token = [0u8; 32];
    let mut id = [0u8; 16];
    if rand_core::OsRng.try_fill_bytes(&mut token).is_err()
        || rand_core::OsRng.try_fill_bytes(&mut id).is_err()
    {
        eprintln!("secure random source unavailable");
        return 1;
    }
    let token = hex::encode(token);
    let grant_id = hex::encode(id);
    let token_digest = sha256_hex(token.as_bytes());
    let mut store = match foundry_ledger::ProductStore::open(
        std::path::Path::new(db),
        product_config(installation_id, database_id),
    ) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("product database rejected");
            return 1;
        }
    };
    if !matches!(store.scope_policy_enforced(), Ok(true)) {
        eprintln!("product scope setup or migration required");
        return 1;
    }
    match store
        .issue_approval_grant(&digest, &grant_id, &token_digest, order, chrono::Utc::now())
        .and_then(|_| {
            private_file(token_file, &token)
                .map_err(|_| foundry_ledger::ProductError::Invariant("private token file rejected"))
        }) {
        Ok(()) => {
            println!("approval grant: {grant_id}");
            0
        }
        Err(_) => {
            eprintln!("approval grant unavailable");
            1
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn product_approve_cmd(
    db: &str,
    installation_id: &str,
    database_id: &str,
    session_file: &str,
    order: String,
    grant_id: String,
    grant_file: &str,
    expected_source_revision: i64,
    expected_attempt: i64,
    run_id: i64,
    generation: i64,
    artifact_digest: String,
    tuple_digest: String,
    intent_text: String,
    request_key: String,
    receipt_out: Option<&str>,
) -> i32 {
    let bearer = match read_private(session_file) {
        Ok(v) if v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()) => v,
        _ => {
            eprintln!("private session file rejected");
            return 1;
        }
    };
    let grant = match read_private(grant_file) {
        Ok(v) if v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()) => v,
        _ => {
            eprintln!("private grant file rejected");
            return 1;
        }
    };
    let digest = sha256_hex(bearer.as_bytes());
    let approval = foundry_ledger::ProductApprovalIntent {
        request_key,
        grant_id,
        token_digest: sha256_hex(grant.as_bytes()),
        order_id: order,
        expected_source_revision,
        expected_attempt,
        run_id,
        generation,
        artifact_digest,
        tuple_digest,
        intent: intent_text,
        now: chrono::Utc::now(),
    };
    let mut store = match foundry_ledger::ProductStore::open(
        std::path::Path::new(db),
        product_config(installation_id, database_id),
    ) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("product database rejected");
            return 1;
        }
    };
    if !matches!(store.scope_policy_enforced(), Ok(true)) {
        eprintln!("product scope setup or migration required");
        return 1;
    }
    let result = store
        .prepare_approval(&digest, &approval)
        .and_then(|proof| store.approve_product(&digest, &approval, &proof));
    match result {
        Ok(receipt) => {
            if let Some(path) = receipt_out {
                let encoded = match serde_json::to_string(&receipt) {
                    Ok(value) => value,
                    Err(_) => {
                        eprintln!("approval receipt unavailable");
                        return 1;
                    }
                };
                if private_file(path, &encoded).is_err() {
                    eprintln!("approval receipt file unavailable");
                    return 1;
                }
            }
            println!("approval recorded: {}", receipt.event_hash);
            0
        }
        Err(_) => {
            eprintln!("product approval unavailable");
            1
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictProductApprovalReceipt {
    operation_id: String,
    actor_id: String,
    order_id: String,
    lifecycle: i64,
    sequence: i64,
    event_hash: String,
    artifact_digest: String,
    attempt: i64,
    run_id: i64,
    generation: i64,
    tuple_digest: String,
    source_revision: i64,
}

impl From<StrictProductApprovalReceipt> for foundry_ledger::ProductApprovalReceipt {
    fn from(receipt: StrictProductApprovalReceipt) -> Self {
        Self {
            operation_id: receipt.operation_id,
            actor_id: receipt.actor_id,
            order_id: receipt.order_id,
            lifecycle: receipt.lifecycle,
            sequence: receipt.sequence,
            event_hash: receipt.event_hash,
            artifact_digest: receipt.artifact_digest,
            attempt: receipt.attempt,
            run_id: receipt.run_id,
            generation: receipt.generation,
            tuple_digest: receipt.tuple_digest,
            source_revision: receipt.source_revision,
        }
    }
}

async fn product_publish_cmd(
    db: &str,
    installation_id: &str,
    database_id: &str,
    session_file: &str,
    receipt_file: &str,
) -> i32 {
    let bearer = match read_private(session_file) {
        Ok(value) if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) => {
            value
        }
        _ => {
            eprintln!("private session file rejected");
            return 1;
        }
    };
    let receipt_bytes = match std::fs::read(receipt_file) {
        Ok(bytes) if !bytes.is_empty() && bytes.len() <= 64 * 1024 => bytes,
        _ => {
            eprintln!("approval receipt file rejected");
            return 1;
        }
    };
    let strict: StrictProductApprovalReceipt = match serde_json::from_slice(&receipt_bytes) {
        Ok(receipt) => receipt,
        Err(_) => {
            eprintln!("approval receipt file rejected");
            return 1;
        }
    };
    let receipt = foundry_ledger::ProductApprovalReceipt::from(strict);
    #[allow(unused_mut)]
    let mut store = match foundry_ledger::ProductStore::open(
        std::path::Path::new(db),
        product_config(installation_id, database_id),
    ) {
        Ok(store) => store,
        Err(_) => {
            eprintln!("product database rejected");
            return 1;
        }
    };
    if !matches!(store.scope_policy_enforced(), Ok(true)) {
        eprintln!("product scope setup or migration required");
        return 1;
    }

    #[cfg(feature = "p2-test-support")]
    if let Some(adapter) = FileRegistryPublishAdapter::from_env() {
        let session_digest = sha256_hex(bearer.as_bytes());
        let now = chrono::Utc::now();
        return match foundry_line::product::ProductProjection::publish_product_approved(
            &mut store,
            &session_digest,
            &receipt,
            &adapter,
            &adapter,
            now,
        )
        .await
        {
            Ok(foundry_line::product::ProductPublishResult::Published(
                foundry_domain::ports::PublishOutcome::Confirmed { pr_url },
            )) => {
                println!("{pr_url}");
                0
            }
            Ok(foundry_line::product::ProductPublishResult::Published(
                foundry_domain::ports::PublishOutcome::Unknown { operation_id },
            )) => {
                println!("publish outcome unknown; retry operation {operation_id}");
                0
            }
            Ok(foundry_line::product::ProductPublishResult::Rejected { pr_url }) => {
                eprintln!("external PR closed without merge: {pr_url}; stored pending; a new approval is required to retry");
                1
            }
            Err(_) => {
                eprintln!("product publish unavailable");
                1
            }
        };
    }

    let _ = (bearer, receipt, store);
    eprintln!("no publish adapter configured — external publishing arrives in W6");
    1
}

#[cfg(feature = "p2-test-support")]
struct FileRegistryPublishAdapter {
    root: std::path::PathBuf,
}

#[cfg(feature = "p2-test-support")]
#[derive(Clone, Copy)]
enum FileRegistryPrState {
    Open,
    ClosedMerged,
    ClosedUnmerged,
}

#[cfg(feature = "p2-test-support")]
struct FileRegistryPr {
    url: String,
    state: FileRegistryPrState,
}

#[cfg(feature = "p2-test-support")]
impl FileRegistryPublishAdapter {
    fn from_env() -> Option<Self> {
        let root = std::env::var_os("B1_PUBLISH_TEST_DIR")?;
        let root = std::path::PathBuf::from(root);
        if root.as_os_str().is_empty() || std::fs::create_dir_all(&root).is_err() {
            return None;
        }
        Some(Self { root })
    }

    fn registry_path(&self, operation_id: &str) -> std::path::PathBuf {
        self.root
            .join(format!("{}.pr", sha256_hex(operation_id.as_bytes())))
    }

    fn read_pr(&self, operation_id: &str) -> Option<FileRegistryPr> {
        let value = std::fs::read_to_string(self.registry_path(operation_id)).ok()?;
        let mut lines = value.lines();
        let url = lines.next()?.to_owned();
        if !url.starts_with("https://example.invalid/pr/") {
            return None;
        }
        // One-line entries from earlier local fixtures represent an open PR.
        let state = match lines.next().unwrap_or("open") {
            "open" => FileRegistryPrState::Open,
            "closed-merged" => FileRegistryPrState::ClosedMerged,
            "closed-unmerged" => FileRegistryPrState::ClosedUnmerged,
            _ => return None,
        };
        if lines.next().is_some() {
            return None;
        }
        Some(FileRegistryPr { url, state })
    }

    fn record_invocation(&self, operation_id: &str) -> std::io::Result<()> {
        use std::io::Write;
        let path = self.root.join("publisher-invocations.log");
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        writeln!(file, "{operation_id}")
    }
}

#[cfg(feature = "p2-test-support")]
impl Publisher for FileRegistryPublishAdapter {
    fn publish(
        &self,
        _order: &Order,
        operation_id: &str,
        _expected_digest: &str,
    ) -> impl std::future::Future<Output = PortResult<PublishOutcome>> + Send {
        let path = self.registry_path(operation_id);
        let result = (|| {
            self.record_invocation(operation_id)?;
            if let Some(pr) = self.read_pr(operation_id) {
                return Ok(PublishOutcome::Confirmed { pr_url: pr.url });
            }
            let url = format!("https://example.invalid/pr/{operation_id}");
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(mut file) => {
                    use std::io::Write;
                    file.write_all(url.as_bytes())?;
                    file.write_all(b"\nopen\n")?;
                    Ok(PublishOutcome::Confirmed { pr_url: url })
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => self
                    .read_pr(operation_id)
                    .map(|pr| PublishOutcome::Confirmed { pr_url: pr.url })
                    .ok_or_else(|| std::io::Error::other("test PR registry entry is invalid")),
                Err(error) => Err(error),
            }
        })()
        .map_err(|_| {
            foundry_domain::ports::PortError::Permanent("test publish adapter unavailable".into())
        });
        async move { result }
    }
}

#[cfg(feature = "p2-test-support")]
impl foundry_line::product::ProductPublishReconciler for FileRegistryPublishAdapter {
    fn reconcile(
        &self,
        operation_id: &str,
    ) -> impl std::future::Future<
        Output = PortResult<foundry_line::product::ProductPublishReconcileOutcome>,
    > + Send {
        let outcome = match self.read_pr(operation_id) {
            Some(FileRegistryPr {
                url: pr_url,
                state: FileRegistryPrState::Open | FileRegistryPrState::ClosedMerged,
            }) => foundry_line::product::ProductPublishReconcileOutcome::Confirmed { pr_url },
            Some(FileRegistryPr {
                url: pr_url,
                state: FileRegistryPrState::ClosedUnmerged,
            }) => foundry_line::product::ProductPublishReconcileOutcome::Rejected { pr_url },
            None => foundry_line::product::ProductPublishReconcileOutcome::NotFound,
        };
        async move { Ok(outcome) }
    }
}

fn product_review_cmd(
    db: &str,
    installation_id: &str,
    database_id: &str,
    session_file: &str,
    action: &str,
    order: Option<&str>,
    hold_id: Option<&str>,
    operation_id: Option<&str>,
    target: Option<&str>,
    request_key: Option<&str>,
) -> i32 {
    let bearer = match read_private(session_file) {
        Ok(value) if value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()) => value,
        _ => {
            eprintln!("private session file rejected");
            return 1;
        }
    };
    let session = sha256_hex(bearer.as_bytes());
    let mut store = match foundry_ledger::ProductStore::open(
        std::path::Path::new(db),
        product_config(installation_id, database_id),
    ) {
        Ok(store) => store,
        Err(_) => {
            eprintln!("product database rejected");
            return 1;
        }
    };
    if !matches!(store.scope_policy_enforced(), Ok(true)) {
        eprintln!("product scope setup or migration required");
        return 1;
    }
    match action {
        "holds" => match store.list_product_review_holds(&session, chrono::Utc::now()) {
            Ok(holds) => {
                for hold in holds {
                    println!(
                        "order={} hold={} lifecycle={} attempt={} run={} activation={} revision={} targets={:?} deadline={}",
                        hold.order_id,
                        hold.hold_instance_id,
                        hold.expectation.lifecycle,
                        hold.expectation.attempt,
                        hold.expectation.originating_run,
                        hold.expectation.activation,
                        hold.expectation.hold_revision,
                        hold.allowed_targets,
                        hold.deadline
                    );
                }
                0
            }
            Err(_) => {
                eprintln!("product holds unavailable");
                1
            }
        },
        "progress" => {
            let Some(operation_id) = operation_id else {
                eprintln!("--operation-id is required");
                return 2;
            };
            match store.product_review_progress(&session, operation_id, chrono::Utc::now()) {
                Ok(progress) => {
                    println!(
                        "operation={} phase={} revision={}",
                        progress.receipt.operation_id, progress.phase, progress.progress_revision
                    );
                    0
                }
                Err(_) => {
                    eprintln!("product review progress unavailable");
                    1
                }
            }
        }
        "resume" | "commit-pass" | "commit-fail" | "abort" => {
            let (Some(order), Some(hold_id), Some(request_key)) = (order, hold_id, request_key)
            else {
                eprintln!("--order, --hold and --idempotency-key are required");
                return 2;
            };
            let decision = match action {
                "resume" => match target
                    .filter(|value| !value.trim().is_empty() && value.len() <= 256)
                {
                    Some(value) => foundry_ledger::ProductHoldDecision::Resume(value.to_owned()),
                    None => {
                        eprintln!("--target is required for resume");
                        return 2;
                    }
                },
                "commit-pass" => foundry_ledger::ProductHoldDecision::CommitPass,
                "commit-fail" => foundry_ledger::ProductHoldDecision::CommitFail,
                _ => foundry_ledger::ProductHoldDecision::Abort,
            };
            let holds = match store.list_product_review_holds(&session, chrono::Utc::now()) {
                Ok(holds) => holds,
                Err(_) => {
                    eprintln!("product holds unavailable");
                    return 1;
                }
            };
            let Some(hold) = holds
                .into_iter()
                .find(|view| view.order_id == order && view.hold_instance_id == hold_id)
            else {
                eprintln!("product hold unavailable");
                return 1;
            };
            let mut host = match product_review_host(db, installation_id, database_id) {
                Ok(Some(host)) => host,
                _ => {
                    eprintln!("product review runtime unavailable");
                    return 1;
                }
            };
            if matches!(&decision, foundry_ledger::ProductHoldDecision::Resume(_))
                && host.ensure_product_executor_available().is_err()
            {
                eprintln!("product review runtime unavailable");
                return 1;
            }
            let receipt = match store.accept_product_review_hold(
                &session,
                order,
                hold_id,
                &hold.expectation,
                request_key,
                decision,
                chrono::Utc::now(),
            ) {
                Ok(receipt) => receipt,
                Err(_) => {
                    eprintln!("product review decision unavailable");
                    return 1;
                }
            };
            let continuation = host.continue_accepted_product_hold(
                &session,
                &receipt.operation_id,
                chrono::Utc::now(),
            );
            match store.product_review_progress(&session, &receipt.operation_id, chrono::Utc::now())
            {
                Ok(progress) => {
                    println!(
                        "decision accepted: operation={} phase={}",
                        receipt.operation_id, progress.phase
                    );
                    if continuation.is_ok() { 0 } else { 1 }
                }
                Err(_) => {
                    println!("decision accepted: operation={}", receipt.operation_id);
                    1
                }
            }
        }
        _ => 2,
    }
}

#[allow(clippy::too_many_arguments)]
fn product_scope_cmd(
    action: &str,
    db: &str,
    installation_id: &str,
    database_id: &str,
    session_file: &str,
    connector: Option<&str>,
    organization: Option<&str>,
    provider: Option<&str>,
    api_origin: Option<&str>,
    repository_id: Option<&str>,
    repository_locator: Option<&str>,
    scope_query: Option<&str>,
    enabled: Option<bool>,
    expected_revision: Option<i64>,
    subject: Option<&str>,
    source_instance: Option<&str>,
    capability: Option<&str>,
    expected_scope_revision: Option<i64>,
    request_key: Option<&str>,
) -> i32 {
    let bearer = match read_private(session_file) {
        Ok(value) if value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()) => value,
        _ => {
            eprintln!("private session file rejected");
            return 1;
        }
    };
    let session = sha256_hex(bearer.as_bytes());
    let mut store = match foundry_ledger::ProductStore::open(
        std::path::Path::new(db),
        product_config(installation_id, database_id),
    ) {
        Ok(store) => store,
        Err(_) => {
            eprintln!("product database rejected");
            return 1;
        }
    };
    match action {
        "sources" => match store.list_scoped_sources(&session, chrono::Utc::now()) {
            Ok(sources) => {
                for view in sources {
                    println!(
                        "connector={} source={} generation={} current={} enabled={} connector_revision={} source_instance_revision={} provider={} origin={} stable_repository_id={} api_owner={} api_repo={} scope={}",
                        view.source.connector_id,
                        view.source.source_instance_id,
                        view.source.generation,
                        view.current,
                        view.source.enabled,
                        view.connector_revision,
                        view.source_instance_revision,
                        view.source.identity.provider,
                        view.source.identity.api_origin,
                        view.source.identity.stable_repository_id,
                        view.source.api_owner.as_deref().unwrap_or("unmapped"),
                        view.source.api_repo.as_deref().unwrap_or("unmapped"),
                        view.source.scope_query
                    );
                }
                0
            }
            Err(_) => {
                eprintln!("product source query unavailable");
                1
            }
        },
        "configure-source" => {
            let required = (
                connector,
                organization,
                provider,
                api_origin,
                repository_id,
                repository_locator,
                scope_query,
                enabled,
                expected_revision,
                request_key,
            );
            let (
                Some(connector),
                Some(organization),
                Some(provider),
                Some(api_origin),
                Some(repository_id),
                Some(repository_locator),
                Some(scope_query),
                Some(enabled),
                Some(expected_revision),
                Some(request_key),
            ) = required
            else {
                eprintln!("source configuration fields are required");
                return 2;
            };
            let identity = match foundry_ledger::RepositoryIdentity::canonicalized(
                organization,
                provider,
                api_origin,
                repository_id,
            ) {
                Ok(identity) => identity,
                Err(_) => {
                    eprintln!("source identity rejected");
                    return 2;
                }
            };
            let Some((api_owner, api_repo)) = repository_locator.split_once('/') else {
                eprintln!("repository locator must be OWNER/REPOSITORY");
                return 2;
            };
            if api_owner.is_empty() || api_repo.is_empty() || api_repo.contains('/') {
                eprintln!("repository locator rejected");
                return 2;
            }
            match store.configure_scoped_source(
                &session,
                connector,
                identity,
                api_owner,
                api_repo,
                repository_locator,
                scope_query,
                enabled,
                expected_revision,
                request_key,
                chrono::Utc::now(),
            ) {
                Ok(source) => match store.list_scoped_sources(&session, chrono::Utc::now()) {
                    Ok(views) => {
                        let revision = views
                            .iter()
                            .find(|view| {
                                view.source.source_instance_id == source.source_instance_id
                            })
                            .map(|view| view.source_revision)
                            .unwrap_or_default();
                        println!(
                            "source configured: {} generation={} source_revision={revision}",
                            source.source_instance_id, source.generation
                        );
                        0
                    }
                    Err(_) => {
                        eprintln!("source configured but revision query unavailable");
                        1
                    }
                },
                Err(_) => {
                    eprintln!("source configuration unavailable");
                    1
                }
            }
        }
        "grants" => {
            let (Some(subject), Some(source_instance)) = (subject, source_instance) else {
                eprintln!("--subject and --source-instance are required");
                return 2;
            };
            match store.list_product_scope_grants(
                &session,
                subject,
                source_instance,
                chrono::Utc::now(),
            ) {
                Ok(grants) => {
                    for grant in grants {
                        println!(
                            "subject={} source={} capability={} enabled={} grant_revision={} scope_revision={}",
                            grant.actor_id,
                            grant.source_instance_id,
                            grant.capability.as_str(),
                            grant.enabled,
                            grant.grant_revision,
                            grant.scope_revision
                        );
                    }
                    0
                }
                Err(_) => {
                    eprintln!("product grant query unavailable");
                    1
                }
            }
        }
        "grant" | "revoke" => {
            let (
                Some(subject),
                Some(source_instance),
                Some(capability),
                Some(expected_scope_revision),
                Some(request_key),
            ) = (
                subject,
                source_instance,
                capability,
                expected_scope_revision,
                request_key,
            )
            else {
                eprintln!(
                    "--subject, --source-instance, --capability, --expected-scope-revision and --request-key are required"
                );
                return 2;
            };
            let capability = match foundry_ledger::ProductCapability::parse(capability) {
                Ok(capability) => capability,
                Err(_) => {
                    eprintln!("product capability rejected");
                    return 2;
                }
            };
            let mutation = if action == "grant" {
                foundry_ledger::ScopeMutation::Grant
            } else {
                foundry_ledger::ScopeMutation::Revoke
            };
            match store.change_product_scope(
                &session,
                subject,
                source_instance,
                capability,
                mutation,
                expected_scope_revision,
                request_key,
                chrono::Utc::now(),
            ) {
                Ok(receipt) => {
                    println!(
                        "scope {}: capability={} grant_revision={} scope_revision={}",
                        action,
                        receipt.capability.as_str(),
                        receipt.grant_revision,
                        receipt.scope_revision
                    );
                    0
                }
                Err(_) => {
                    eprintln!("product scope change unavailable");
                    1
                }
            }
        }
        _ => 2,
    }
}

fn product_scope_migrate_cmd(
    db: &str,
    installation_id: &str,
    database_id: &str,
    manifest_path: &str,
) -> i32 {
    let manifest = match read_product_json::<foundry_ledger::ScopeMigrationManifest>(manifest_path)
    {
        Ok(manifest) => manifest,
        Err(()) => {
            eprintln!("product scope migration manifest rejected");
            return 2;
        }
    };
    match foundry_ledger::ProductStore::migrate_scope_v1(
        std::path::Path::new(db),
        &product_config(installation_id, database_id),
        &manifest,
        chrono::Utc::now(),
    ) {
        Ok(receipt) => {
            println!(
                "scope migration complete: verified={} quarantined={} sources={} grants={}",
                receipt.verified_orders,
                receipt.quarantined_orders,
                receipt.source_instances,
                receipt.grants
            );
            0
        }
        Err(_) => {
            eprintln!(
                "scope migration rejected; ensure all old product processes are stopped and the reviewed manifest is valid"
            );
            1
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn product_service_cmd(
    action: &str,
    db: &str,
    installation: &str,
    database: &str,
    session_file: &str,
    service_id: &str,
    expected_revision: i64,
    request_key: &str,
    lease_raw: Option<&str>,
    expires_raw: Option<&str>,
    source_instance: Option<&str>,
    actions_raw: Option<&str>,
    credential_file: Option<&str>,
) -> i32 {
    let bearer = match read_private(session_file) {
        Ok(v) if v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()) => v,
        _ => {
            eprintln!("private session file rejected");
            return 1;
        }
    };
    let session = sha256_hex(bearer.as_bytes());
    let mut store = match foundry_ledger::ProductStore::open(
        std::path::Path::new(db),
        product_config(installation, database),
    ) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("product database rejected");
            return 1;
        }
    };
    let now = chrono::Utc::now();
    match action {
        "query" => match store.list_product_services(&session, now) {
            Ok(services) => {
                for item in services.into_iter().filter(|s| s.service_id == service_id) {
                    println!(
                        "service {} revision={} credential_revision={} enabled={} lease_until={} grants={}",
                        item.service_id,
                        item.revision,
                        item.credential_revision,
                        item.enabled,
                        item.lease_until,
                        item.grants
                            .iter()
                            .map(|g| format!("{}:{}", g.source_instance_id, g.action.as_str()))
                            .collect::<Vec<_>>()
                            .join(",")
                    );
                }
                0
            }
            Err(_) => {
                eprintln!("service query unavailable");
                1
            }
        },
        "configure" | "revoke" => {
            let grants = if action == "configure" {
                let source = match source_instance {
                    Some(v) => v,
                    None => {
                        eprintln!("service source instance required");
                        return 2;
                    }
                };
                let actions = match actions_raw {
                    Some(v) => v,
                    None => {
                        eprintln!("service actions required");
                        return 2;
                    }
                };
                let mut grants = Vec::new();
                for raw in actions.split(',') {
                    let action = match raw.trim() {
                        "intake_read" => foundry_ledger::ProductServiceAction::IntakeRead,
                        "admit_delivery" => foundry_ledger::ProductServiceAction::AdmitDelivery,
                        "ack_delivery" => foundry_ledger::ProductServiceAction::AckDelivery,
                        "drive_review" => foundry_ledger::ProductServiceAction::DriveReview,
                        _ => {
                            eprintln!("service action rejected");
                            return 2;
                        }
                    };
                    grants.push(foundry_ledger::ProductServiceGrant {
                        source_instance_id: source.to_owned(),
                        action,
                    });
                }
                grants
            } else {
                Vec::new()
            };
            let lease = if let Some(raw) = lease_raw {
                match chrono::DateTime::parse_from_rfc3339(raw) {
                    Ok(v) => v.with_timezone(&chrono::Utc),
                    Err(_) => {
                        eprintln!("service lease rejected");
                        return 2;
                    }
                }
            } else if action == "revoke" {
                match store
                    .list_product_services(&session, now)
                    .ok()
                    .and_then(|all| all.into_iter().find(|s| s.service_id == service_id))
                {
                    Some(s) => match chrono::DateTime::parse_from_rfc3339(&s.lease_until) {
                        Ok(v) => v.with_timezone(&chrono::Utc),
                        Err(_) => return 1,
                    },
                    None => {
                        eprintln!("service unavailable");
                        return 1;
                    }
                }
            } else {
                eprintln!("explicit service lease required");
                return 2;
            };
            match store.manage_product_service(
                &session,
                service_id,
                &grants,
                action == "configure",
                lease,
                expected_revision,
                request_key,
                now,
            ) {
                Ok(r) => {
                    println!(
                        "service {} revision={} enabled={} lease_until={}",
                        r.service_id, r.revision, r.enabled, r.lease_until
                    );
                    0
                }
                Err(_) => {
                    eprintln!("service management rejected");
                    1
                }
            }
        }
        "issue" | "rotate" => {
            let path = match credential_file {
                Some(p) => p,
                None => {
                    eprintln!("private service credential file required");
                    return 2;
                }
            };
            let expires =
                match expires_raw.and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok()) {
                    Some(v) => v.with_timezone(&chrono::Utc),
                    None => {
                        eprintln!("explicit service credential expiry required");
                        return 2;
                    }
                };
            let service = match store
                .list_product_services(&session, now)
                .ok()
                .and_then(|all| all.into_iter().find(|s| s.service_id == service_id))
            {
                Some(s) => s,
                None => {
                    eprintln!("service unavailable");
                    return 1;
                }
            };
            if service.revision != expected_revision
                || ((action == "issue") != (service.credential_revision == 0))
            {
                eprintln!("service credential operation conflicts with current revision");
                return 1;
            }
            match store.issue_product_service_credential(
                &session,
                service_id,
                expected_revision,
                expires,
                request_key,
                now,
            ) {
                Ok(token) => match private_file(path, token.expose_once_to_file_writer()) {
                    Ok(()) => {
                        println!(
                            "service credential saved privately; plaintext is not recoverable by request retry"
                        );
                        0
                    }
                    Err(()) => {
                        eprintln!(
                            "credential was issued but private file creation failed; rotate with a new request key and a new file path"
                        );
                        1
                    }
                },
                Err(_) => {
                    eprintln!("service credential issuance rejected");
                    1
                }
            }
        }
        _ => 2,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn trim_line(line: &mut Vec<u8>) {
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
}

type AppLine = Line<DigestGate, AnyHarness, SqliteLedger, foundry_watch::GithubPublisher>;

fn human() -> ActorId {
    ActorId {
        kind: ActorKind::Human,
        source: "local".into(),
        id: "lunarpulse".into(),
    }
}

/// (Notifier v2.7 temporary) Extract the order_id from a NotifyEvent.
fn notify_event_order_id(e: &foundry_domain::ports::NotifyEvent) -> &str {
    match e {
        foundry_domain::ports::NotifyEvent::FurnaceHold { order_id, .. }
        | foundry_domain::ports::NotifyEvent::Failed { order_id, .. }
        | foundry_domain::ports::NotifyEvent::Shipped { order_id, .. } => order_id,
        _ => "",
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::DeployProfile {
            check,
            units,
            readme,
        } => {
            if check {
                match deploy_profile::check_profile() {
                    deploy_profile::CheckOutcome::Ok => {
                        println!("deploy profile: OK — boundary table consistent, credentials 600");
                        0
                    }
                    deploy_profile::CheckOutcome::Violations(v) => {
                        for x in v {
                            eprintln!("VIOLATION: {x}");
                        }
                        1
                    }
                }
            } else if units {
                print!("{}", deploy_profile::systemd_units());
                0
            } else if readme {
                print!("{}", deploy_profile::readme());
                0
            } else {
                eprintln!("nothing to do: pass --check, --units, or --readme");
                2
            }
        }
        Cmd::Validate { path } => validate(&path),
        Cmd::Run {
            id,
            task,
            die,
            blueprint,
        } => run(id, task, die, &blueprint).await,
        Cmd::Approve { order, intent, db } => approve(order, intent, &expand(&db)).await,
        Cmd::Watch {
            once,
            every,
            repo,
            die,
            auto_approve,
            notify_mode,
        } => watch_cmd(once, every, repo, die, auto_approve, notify_mode).await,
        Cmd::ProductWatch {
            db,
            installation_id,
            database_id,
            service_file,
            repo,
            repository_id,
            api_origin,
            private,
            credential_ref,
            once,
            every,
        } => {
            product_watch_cmd(
                &expand(&db),
                installation_id,
                database_id,
                &expand(&service_file),
                repo,
                repository_id,
                api_origin,
                private,
                credential_ref,
                once,
                every,
            )
            .await
        }
        Cmd::Show { order, db } => show(order, &expand(&db)).await,
        Cmd::Status { db } => status_cmd(&expand(&db)),
        Cmd::Doctor { db } => doctor_cmd(&expand(&db)),
        Cmd::Casting {
            run_profile,
            judge,
            promote,
            corpus,
            casting_dir,
            seed,
        } => {
            casting_cmd(
                run_profile,
                judge,
                promote,
                &expand(&corpus),
                &expand(&casting_dir),
                seed,
            )
            .await
        }
        Cmd::Benchmark {
            baseline,
            compare,
            corpus,
        } => benchmark_cmd(baseline, compare, &expand(&corpus)),
        Cmd::Corpus { db, out } => corpus_cmd(&expand(&db), &expand(&out)),
        Cmd::Reconcile { db, apply } => reconcile_cmd(&expand(&db), apply).await,
        Cmd::Serve { db, bind, product } => {
            let app = if product {
                let installation_id = match std::env::var("FOUNDRY_INSTALLATION_ID") {
                    Ok(v) if !v.trim().is_empty() => v,
                    _ => {
                        eprintln!("✗ product serve requires FOUNDRY_INSTALLATION_ID");
                        std::process::exit(1);
                    }
                };
                let database_id = match std::env::var("FOUNDRY_DATABASE_ID") {
                    Ok(v) if !v.trim().is_empty() => v,
                    _ => {
                        eprintln!("✗ product serve requires FOUNDRY_DATABASE_ID");
                        std::process::exit(1);
                    }
                };
                let product_db = expand(&db);
                let mut store = match foundry_ledger::ProductStore::open(
                    std::path::Path::new(&product_db),
                    product_config(&installation_id, &database_id),
                ) {
                    Ok(store) => store,
                    Err(_) => {
                        eprintln!("✗ product database contract/configuration rejected");
                        std::process::exit(1);
                    }
                };
                if !matches!(store.scope_policy_enforced(), Ok(true)) {
                    eprintln!(
                        "✗ product database scope is not active; run the local reviewed scope bootstrap or `product-migrate-scope` before serving"
                    );
                    std::process::exit(1);
                }
                if let Ok(artifact_root) = std::env::var("FOUNDRY_PRODUCT_ARTIFACT_ROOT")
                    && !artifact_root.trim().is_empty()
                    && store
                        .configure_product_artifact_root(expand(&artifact_root))
                        .is_err()
                {
                    eprintln!("✗ product artifact root configuration rejected");
                    std::process::exit(1);
                }
                let state = match product_review_host(&product_db, &installation_id, &database_id) {
                    Ok(Some(host)) => {
                        foundry_serve::auth_http::ProductHttpState::with_review_host(store, host)
                    }
                    Ok(None) => foundry_serve::auth_http::ProductHttpState::new(store),
                    Err(_) => {
                        eprintln!("✗ product review runtime configuration rejected");
                        std::process::exit(1);
                    }
                };
                foundry_serve::http::product_router(state)
            } else {
                let state =
                    std::sync::Arc::new(match foundry_serve::ServeState::new(&expand(&db)) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("✗ serve: {e}");
                            std::process::exit(1);
                        }
                    });
                foundry_serve::http::router(state)
            };
            let listener = match tokio::net::TcpListener::bind(&bind).await {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("✗ bind {bind}: {e}");
                    std::process::exit(1);
                }
            };
            println!("foundry-serve on http://{bind}");
            match axum::serve(listener, app).await {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("✗ serve: {e}");
                    1
                }
            }
        }
        Cmd::ProductBootstrap {
            db,
            installation_id,
            database_id,
            manifest,
        } => {
            let manifest = manifest.map(|path| expand(&path));
            product_bootstrap_cmd(
                &expand(&db),
                &installation_id,
                &database_id,
                manifest.as_deref(),
            )
        }
        Cmd::ProductLogin {
            db,
            installation_id,
            database_id,
            session_file,
        } => product_login_cmd(
            &expand(&db),
            &installation_id,
            &database_id,
            &expand(&session_file),
        ),
        Cmd::ProductReset {
            db,
            installation_id,
            database_id,
        } => product_reset_cmd(&expand(&db), &installation_id, &database_id),
        Cmd::ProductAccount {
            db,
            installation_id,
            database_id,
            session_file,
            actor,
            role,
        } => product_account_cmd(
            &expand(&db),
            &installation_id,
            &database_id,
            &expand(&session_file),
            &actor,
            &role,
        ),
        Cmd::ProductGrant {
            db,
            installation_id,
            database_id,
            session_file,
            order,
            token_file,
        } => product_grant_cmd(
            &expand(&db),
            &installation_id,
            &database_id,
            &expand(&session_file),
            &order,
            &expand(&token_file),
        ),
        Cmd::ProductApprove {
            db,
            installation_id,
            database_id,
            session_file,
            order,
            grant_id,
            grant_file,
            expected_source_revision,
            expected_attempt,
            run_id,
            generation,
            artifact_digest,
            tuple_digest,
            intent,
            idempotency_key,
            receipt_out,
        } => {
            let receipt_out = receipt_out.map(|path| expand(&path));
            product_approve_cmd(
                &expand(&db),
                &installation_id,
                &database_id,
                &expand(&session_file),
                order,
                grant_id,
                &expand(&grant_file),
                expected_source_revision,
                expected_attempt,
                run_id,
                generation,
                artifact_digest,
                tuple_digest,
                intent,
                idempotency_key,
                receipt_out.as_deref(),
            )
        }
        Cmd::Product {
            command:
                ProductCmd::Publish {
                    db,
                    installation_id,
                    database_id,
                    session_file,
                    receipt_file,
                },
        } => {
            product_publish_cmd(
                &expand(&db),
                &installation_id,
                &database_id,
                &expand(&session_file),
                &expand(&receipt_file),
            )
            .await
        }
        Cmd::ProductReview {
            db,
            installation_id,
            database_id,
            session_file,
            action,
            order,
            hold,
            operation_id,
            target,
            idempotency_key,
        } => product_review_cmd(
            &expand(&db),
            &installation_id,
            &database_id,
            &expand(&session_file),
            &action,
            order.as_deref(),
            hold.as_deref(),
            operation_id.as_deref(),
            target.as_deref(),
            idempotency_key.as_deref(),
        ),
        Cmd::ProductScope {
            action,
            db,
            installation_id,
            database_id,
            session_file,
            connector,
            organization,
            provider,
            api_origin,
            repository_id,
            repository_locator,
            scope_query,
            enabled,
            expected_revision,
            subject,
            source_instance,
            capability,
            expected_scope_revision,
            request_key,
        } => product_scope_cmd(
            &action,
            &expand(&db),
            &installation_id,
            &database_id,
            &expand(&session_file),
            connector.as_deref(),
            organization.as_deref(),
            provider.as_deref(),
            api_origin.as_deref(),
            repository_id.as_deref(),
            repository_locator.as_deref(),
            scope_query.as_deref(),
            enabled,
            expected_revision,
            subject.as_deref(),
            source_instance.as_deref(),
            capability.as_deref(),
            expected_scope_revision,
            request_key.as_deref(),
        ),
        Cmd::ProductMigrateScope {
            db,
            installation_id,
            database_id,
            manifest,
        } => product_scope_migrate_cmd(
            &expand(&db),
            &installation_id,
            &database_id,
            &expand(&manifest),
        ),
        Cmd::ProductService {
            action,
            db,
            installation_id,
            database_id,
            session_file,
            service_id,
            expected_revision,
            request_key,
            lease_until,
            expires_at,
            source_instance,
            actions,
            credential_file,
        } => product_service_cmd(
            &action,
            &expand(&db),
            &installation_id,
            &database_id,
            &expand(&session_file),
            &service_id,
            expected_revision,
            &request_key,
            lease_until.as_deref(),
            expires_at.as_deref(),
            source_instance.as_deref(),
            actions.as_deref(),
            credential_file.as_deref().map(|p| expand(p)).as_deref(),
        ),
    };
    std::process::exit(code);
}

fn validate(path: &str) -> i32 {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {path}: {e}");
            return 1;
        }
    };
    match foundry_blueprint::Blueprint::parse(&src) {
        Ok(bp) => {
            println!("✓ blueprint valid: {}", bp.name);
            for r in &bp.repositories {
                println!("  repo: {}/{}", r.owner, r.name);
            }
            0
        }
        Err(e) => {
            eprintln!("✗ invalid blueprint: {e}");
            1
        }
    }
}

async fn run(id: Option<String>, task: String, die: String, blueprint: &str) -> i32 {
    // Blueprint gate: refuse to run against an invalid blueprint.
    if let Ok(src) = std::fs::read_to_string(blueprint) {
        if let Err(e) = foundry_blueprint::Blueprint::parse(&src) {
            eprintln!("✗ blueprint invalid, refusing to run: {e}");
            return 1;
        }
    }

    let db_path = expand("~/.foundry/ledger.db");
    if let Some(dir) = std::path::Path::new(&db_path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let ledger = match SqliteLedger::open(std::path::Path::new(&db_path)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("✗ ledger open failed: {e:?}");
            return 1;
        }
    };

    let order_id = id.unwrap_or_else(|| format!("o-{}", chrono::Utc::now().timestamp_millis()));
    let order = Order::new(
        order_id.clone(),
        ExternalRef::cron("cli", &task),
        chrono::Utc::now(),
    );

    let line = build_line(&die, ledger);
    if let Err(e) = line.acquire_lease("cli").await {
        eprintln!("✗ {e:?}");
        return 1;
    }
    line.submit(order).await;

    match line.drive(&order_id).await {
        Ok(st) => {
            println!("{order_id} → {st:?}");
            if st == foundry_domain::OrderState::FurnaceHold {
                println!("approve with: foundry approve --order {order_id} --intent \"...\"");
            }
            0
        }
        Err(e) => {
            eprintln!("✗ {order_id}: {e:?}");
            1
        }
    }
}

/// Harness is RPITIT (not dyn-compatible) — dispatch via enum.
enum AnyHarness {
    Fake(FakeHarness),
    Cli(CliHarness),
}
impl Harness for AnyHarness {
    fn run(
        &self,
        order: &Order,
        attempt: u32,
        workdir: &std::path::Path,
    ) -> impl Future<Output = PortResult<DieOutput>> + Send {
        let this = self.clone_inner();
        this.run_inner(order.clone(), attempt, workdir.to_path_buf())
    }
}

impl AnyHarness {
    fn clone_inner(&self) -> AnyHarness {
        match self {
            AnyHarness::Fake(h) => AnyHarness::Fake(h.clone()),
            AnyHarness::Cli(h) => AnyHarness::Cli(h.clone()),
        }
    }

    fn run_inner(
        self,
        order: Order,
        attempt: u32,
        workdir: std::path::PathBuf,
    ) -> Pin<Box<dyn Future<Output = PortResult<DieOutput>> + Send>> {
        Box::pin(async move {
            match self {
                AnyHarness::Fake(h) => h.run(&order, attempt, &workdir).await,
                AnyHarness::Cli(h) => h.run(&order, attempt, &workdir).await,
            }
        })
    }
}

fn build_line(die: &str, ledger: SqliteLedger) -> Arc<AppLine> {
    let harness = if die.is_empty() {
        AnyHarness::Fake(FakeHarness::default())
    } else {
        AnyHarness::Cli(CliHarness::new(die, "coding", 900))
    };
    let publisher = foundry_watch::GithubPublisher {
        owner: "lunarpulse".into(),
        repo: "foundary-demo".into(),
        token_path: expand("~/.foundry/gh_token").into(),
        base: "main".into(),
    };
    let mut line = Line::new(
        Arc::new(DigestGate),
        Arc::new(harness),
        Arc::new(TMutex::new(ledger)),
        Arc::new(publisher),
        Arc::new(LocalPolicy),
    );
    if let Ok(pack) = std::env::var("FOUNDRY_REVIEW_PACK") {
        let tuples: Arc<dyn foundry_exec::TupleStore> = Arc::new(
            foundry_exec::SqliteTupleStore::open(tuple_db_path())
                .unwrap_or_else(|e| panic!("tuple store: {e:?}")),
        );
        line.set_tuple_store(tuples.clone());
        let host = PackReviewHost::new(
            &pack,
            UnavailableStageExecutor,
            PackReviewHost::default_db_path(),
        )
        .unwrap_or_else(|e| panic!("review host setup: {e:?}"));
        let runner = durable_review_run(host, PackReviewHost::default_db_path(), tuples)
            .unwrap_or_else(|e| panic!("review runner setup: {e}"));
        line = line.with_review_run(InspectingReviewRun::new(runner));
    } else if let Ok(tuples) = foundry_exec::SqliteTupleStore::open(tuple_db_path()) {
        line.set_tuple_store(Arc::new(tuples));
    }
    Arc::new(line)
}

fn tuple_db_path() -> std::path::PathBuf {
    let raw = std::env::var("FOUNDRY_TUPLES").unwrap_or_else(|_| "~/.foundry/tuples.db".into());
    expand(&raw).into()
}

async fn approve(order_id: String, intent: String, db: &str) -> i32 {
    let ledger = match SqliteLedger::open(std::path::Path::new(db)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("✗ ledger open failed: {e:?}");
            return 1;
        }
    };
    let line = build_line("", ledger);
    if line.get(&order_id).await.is_none() {
        // M1 defect #7 fix: rehydrate orders from the journal so approvals
        // survive watch-process restarts (the E2E caught this).
        let ledger_ro = match open_ledger_ro(&db) {
            Some(l) => l,
            None => {
                eprintln!("✗ cannot open ledger {db}");
                return 1;
            }
        };
        let tuples = match foundry_exec::SqliteTupleStore::open(tuple_db_path()) {
            Ok(store) => store,
            Err(e) => {
                eprintln!("✗ tuple store open failed: {e:?}");
                return 1;
            }
        };
        match foundry_line::rehydrate::rehydrate(&ledger_ro, std::path::Path::new("/tmp"), &tuples)
        {
            Ok(orders) => {
                let n_waiting = orders
                    .iter()
                    .filter(|o| o.state() == foundry_domain::OrderState::FurnaceHold)
                    .count();
                eprintln!(
                    "rehydrated {} order(s) from journal ({n_waiting} waiting)",
                    orders.len()
                );
                for o in orders {
                    line.submit(o).await;
                }
            }
            Err(e) => {
                eprintln!("✗ rehydrate failed: {e:?}");
                return 1;
            }
        }
        if line.get(&order_id).await.is_none() {
            eprintln!("✗ unknown order {order_id} (not in journal either)");
            return 1;
        }
    }
    let token = ApprovalToken {
        order_id: order_id.clone(),
        intent: intent.clone(),
        artifact_digest: line
            .get(&order_id)
            .await
            .and_then(|o| o.artifact_digest)
            .unwrap_or_default(),
        actor: human(),
        granted_at: chrono::Utc::now(),
    };
    match line.approve(&order_id, &token, &intent, &human()).await {
        Ok(st) => {
            println!("{order_id} → {st:?}");
            match line.publish(&order_id, &human()).await {
                Ok(st2) => {
                    println!("{order_id} → {:?}", st2.state);
                    0
                }
                Err(e) => {
                    eprintln!("✗ publish: {e:?}");
                    1
                }
            }
        }
        Err(e) => {
            eprintln!("✗ approve: {e:?}");
            1
        }
    }
}

async fn show(order_id: String, db: &str) -> i32 {
    match SqliteLedger::open(std::path::Path::new(db)) {
        Ok(led) => match led.verify_chain() {
            Ok(true) => {
                println!("{order_id}: ledger chain verified ✓");
                0
            }
            Ok(false) => {
                println!("{order_id}: ledger chain BROKEN ✗");
                1
            }
            Err(e) => {
                eprintln!("✗ {e:?}");
                1
            }
        },
        Err(e) => {
            eprintln!("✗ {e:?}");
            1
        }
    }
}

struct EnvSecretProvider;
impl foundry_domain::ports::SecretProvider for EnvSecretProvider {
    fn resolve(&self, name: &str) -> PortResult<String> {
        if name.is_empty()
            || name.len() > 128
            || !name
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(foundry_domain::ports::PortError::Permanent(
                "credential reference rejected".into(),
            ));
        }
        std::env::var(name).map_err(|_| {
            foundry_domain::ports::PortError::Permanent("credential unavailable".into())
        })
    }
}

async fn product_watch_cmd(
    db: &str,
    installation_id: String,
    database_id: String,
    service_file: &str,
    repo: String,
    repository_id: String,
    api_origin: String,
    private: bool,
    credential_ref: Option<String>,
    once: bool,
    every: u64,
) -> i32 {
    let (owner, name) = repo.split_once('/').unwrap_or((repo.as_str(), ""));
    if owner.is_empty() || name.is_empty() || (private && credential_ref.is_none()) {
        eprintln!("product watch configuration rejected");
        return 2;
    }
    loop {
        let db = db.to_string();
        let installation = installation_id.clone();
        let database = database_id.clone();
        let service_file = service_file.to_owned();
        let cfg = foundry_watch::intake::ProductGithubConfig {
            connector_id: "github-primary".into(),
            api_origin: api_origin.clone(),
            owner: owner.into(),
            repo: name.into(),
            repository_id: repository_id.clone(),
            private,
            credential_ref: credential_ref.clone(),
            allow_loopback_http: false,
            max_pages_per_tick: 5,
            now: chrono::Utc::now(),
        };
        let result = tokio::task::spawn_blocking(move || {
            let mut store = foundry_ledger::ProductStore::open(
                std::path::Path::new(&db),
                foundry_ledger::ProductConfig {
                    installation_id: installation,
                    database_id: database,
                },
            )
            .map_err(|_| "product database rejected")?;
            if !matches!(store.scope_policy_enforced(), Ok(true)) {
                return Err("product scope setup or migration required");
            }
            let token = read_private(&service_file)
                .map_err(|_| "private service credential file rejected")?;
            let service = store
                .authenticate_product_service(
                    &token,
                    &cfg.connector_id,
                    &cfg.api_origin,
                    &cfg.repository_id,
                    chrono::Utc::now(),
                )
                .map_err(|_| "product service authority rejected")?;
            foundry_watch::intake_worker::product_github_tick(
                &mut store,
                &cfg,
                &EnvSecretProvider,
                chrono::Utc::now(),
                1000,
                &service,
            )
            .map_err(|_| "product intake tick failed")
        })
        .await;
        match result {
            Ok(Ok((poll, drain))) => println!(
                "product intake: pages={} fetched={} admitted={} acknowledged={} retryable={} permanent={} work={}",
                poll.pages,
                poll.fetched,
                drain.admitted,
                drain.acknowledged,
                drain.retryable_failures,
                drain.permanent_failures,
                match drain.work_claim {
                    foundry_ledger::ProductWorkClaimResult::Empty => "empty",
                    foundry_ledger::ProductWorkClaimResult::Unsupported { .. } =>
                        "waiting-unsupported",
                    foundry_ledger::ProductWorkClaimResult::Claimed(_) =>
                        "claimed-awaiting-adapter",
                }
            ),
            Ok(Err(error)) => {
                eprintln!("{error}");
                return 1;
            }
            Err(_) => {
                eprintln!("product intake worker failed");
                return 1;
            }
        }
        if once {
            return 0;
        }
        tokio::time::sleep(std::time::Duration::from_secs(every.max(1))).await;
    }
}

async fn watch_cmd(
    once: bool,
    every: u64,
    repo: String,
    die: String,
    auto_approve: String,
    notify_mode: String,
) -> i32 {
    if !auto_approve.is_empty() {
        eprintln!(
            "✗ --auto-approve is disabled; approvals require an authenticated product session"
        );
        return 2;
    }
    let (owner, name) = repo
        .split_once('/')
        .unwrap_or((repo.as_str(), "contextmesh-rs"));
    let cfg = foundry_watch::WatchConfig {
        tick_secs: every,
        owner: owner.into(),
        repo: name.into(),
        die_bin: (!die.is_empty()).then_some(die.clone()),
        ledger_path: expand("~/.foundry/ledger.db").into(),
    };
    let ledger = match foundry_ledger::SqliteLedger::open(std::path::Path::new(&cfg.ledger_path)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("✗ ledger: {e:?}");
            return 1;
        }
    };
    // T15 (review #5): cross-process single-writer lock for the whole watch
    // lifetime — blocks `reconcile --apply` while we write.
    let _writer_lock = {
        let lock_path = format!("{}.writer.lock", cfg.ledger_path.display());
        match std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
        {
            Ok(f) => {
                use std::os::unix::io::AsRawFd;
                let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
                if rc != 0 {
                    eprintln!("✗ another writer holds {lock_path} (watch already running?)");
                    return 1;
                }
                Some(f)
            }
            Err(e) => {
                eprintln!("✗ writer lock unavailable: {e}");
                return 1;
            }
        }
    };
    // The blocking client spins its own runtime — build it OFF the async
    // context or its drop panics inside tokio (T9 실측 버그 #3).
    let icfg = foundry_watch::IntakeConfig {
        owner: cfg.owner.clone(),
        repo: cfg.repo.clone(),
        poll_secs: cfg.tick_secs,
    };
    let intake = match tokio::task::spawn_blocking(move || {
        foundry_watch::GithubLabelIntake::new(icfg)
    })
    .await
    {
        Ok(Ok(i)) => i,
        Ok(Err(e)) => {
            eprintln!("✗ intake: {e:?}");
            return 1;
        }
        Err(e) => {
            eprintln!("✗ intake join: {e}");
            return 1;
        }
    };
    let line = build_line(&die, ledger);
    // T10: workdir GC — sweep stale Die workdirs at tick start, and hold off
    // new Dies when /tmp usage trips the guard (M0 defect #6: measured
    // ENOSPC killed 10 publishes mid-run; better slow than dead).
    let gc_cfg = foundry_watch::gc::GcConfig::default();
    match foundry_watch::gc::sweep(
        &gc_cfg,
        &line.ids_in_state(foundry_domain::OrderState::Failed).await,
        0,
        chrono::Utc::now().timestamp_millis() as u64,
    ) {
        Ok(actions) if !actions.is_empty() => {
            eprintln!("gc: {} action(s): {:?}", actions.len(), actions);
        }
        Ok(_) => {}
        Err(e) => eprintln!("gc: sweep failed (non-fatal): {e}"),
    }
    if foundry_watch::gc::guard_tripped(&gc_cfg) {
        let pct = foundry_watch::gc::tmp_usage_pct().unwrap_or(0);
        eprintln!(
            "gc: /tmp usage {pct}% ≥ guard {}% — holding new Dies this tick",
            gc_cfg.guard_pct
        );
    }
    if once {
        let actor = foundry_domain::ActorId {
            kind: foundry_domain::ActorKind::Agent,
            source: "watch".into(),
            id: "foundry-watch".into(),
        };
        // T13: Discord notifier — observation plane, never blocks the line.
        let mut notifier: Box<dyn foundry_domain::ports::Notifier> = match notify_mode.as_str() {
            "console" => Box::new(foundry_watch::discord_notifier::ConsoleNotifier),
            "all" => Box::new(foundry_watch::discord_notifier::FanOutNotifier(vec![
                Box::new(foundry_watch::discord_notifier::DiscordNotifier::from_env()),
                Box::new(foundry_watch::discord_notifier::ConsoleNotifier),
            ])),
            _ => Box::new(foundry_watch::discord_notifier::DiscordNotifier::from_env()),
        };
        let notify = |n: &mut Box<dyn foundry_domain::ports::Notifier>,
                      event: foundry_domain::ports::NotifyEvent| {
            let oid = notify_event_order_id(&event).to_string();
            if let Err(e) = n.notify(event) {
                eprintln!("notify: {oid} failed (non-fatal): {e:?}");
            }
        };
        let intake = std::sync::Arc::new(tokio::sync::Mutex::new(intake));
        match foundry_watch::tick(&line, intake, &actor).await {
            Ok(n) => {
                println!("tick: {n} new order(s)");
                // T13: every FurnaceHold gets a self-contained approval
                // request (digest included — review #3); Failed orders get
                // the first failure line. Shipped notifications fire on
                // publish confirmation below.
                for oid in line
                    .ids_in_state(foundry_domain::OrderState::FurnaceHold)
                    .await
                {
                    let digest = line
                        .get(&oid)
                        .await
                        .and_then(|o| o.artifact_digest)
                        .unwrap_or_default();
                    let d: String = digest.chars().take(12).collect();
                    notify(
                        &mut notifier,
                        foundry_domain::ports::NotifyEvent::FurnaceHold {
                            order_id: oid.clone(),
                            digest: d.clone(),
                        },
                    );
                }
                for oid in line.ids_in_state(foundry_domain::OrderState::Failed).await {
                    notify(
                        &mut notifier,
                        foundry_domain::ports::NotifyEvent::Failed {
                            order_id: oid.clone(),
                            reason: "see ledger".into(),
                        },
                    );
                }
                0
            }
            Err(e) => {
                eprintln!("✗ tick: {e}");
                1
            }
        }
    } else {
        match foundry_watch::run(&line, intake, cfg).await {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("✗ watch: {e}");
                1
            }
        }
    }
}

// ---------- M1 T15: ops CLI ----------

fn open_ledger_ro(db: &str) -> Option<foundry_ledger::SqliteLedger> {
    foundry_ledger::SqliteLedger::open(std::path::Path::new(db)).ok()
}

fn status_cmd(db: &str) -> i32 {
    let Some(led) = open_ledger_ro(db) else {
        eprintln!("✗ cannot open ledger at {db}");
        return 1;
    };
    let rows = led.read_all().unwrap_or_default();
    let mirror = foundry_ledger::Mirror::new(std::path::Path::new(db));
    let mstate = mirror.state(&rows);
    let counts = foundry_serve::replay_cards(&rows, |rs| {
        rs.iter()
            .zip(rs.iter().skip(1))
            .all(|(a, b)| b.prev_hash == a.hash)
    })
    .unwrap_or_default();
    let mut by_state: std::collections::BTreeMap<String, usize> = Default::default();
    for c in counts.values() {
        *by_state.entry(c.state.clone()).or_default() += 1;
    }
    println!("ledger height: {}", rows.len());
    println!("mirror: {mstate}");
    println!(
        "/tmp usage: {}%",
        foundry_watch::gc::tmp_usage_pct().unwrap_or(0)
    );
    println!("orders by state:");
    for (st, n) in &by_state {
        println!("  {st}: {n}");
    }
    0
}

fn doctor_cmd(db: &str) -> i32 {
    let mut fail = 0;
    // 1. chain
    let Some(led) = open_ledger_ro(db) else {
        eprintln!("✗ cannot open ledger at {db}");
        return 2;
    };
    let rows = led.read_all().unwrap_or_default();
    match led.verify_chain() {
        Ok(true) => println!("✓ chain valid ({} rows)", rows.len()),
        _ => {
            println!("✗ CHAIN BROKEN");
            fail = 1;
        }
    }
    // 2. mirror
    let mirror = foundry_ledger::Mirror::new(std::path::Path::new(db));
    let mstate = mirror.state(&rows);
    if matches!(mstate, foundry_ledger::MirrorState::Ok { .. }) {
        println!("✓ mirror {mstate}");
    } else {
        println!("✗ mirror {mstate} (run: foundry reconcile --db {db} --apply)");
        fail = 1;
    }
    // 3. credentials (existence + owner-only bits — no values, review #5)
    let home = std::env::var("HOME").unwrap_or_default();
    for (name, p) in [
        ("gh_token", format!("{home}/.foundry/gh_token")),
        ("die_env", format!("{home}/.foundry/die.env")),
        ("notifier_env", format!("{home}/.foundry/notifier.env")),
    ] {
        match std::fs::metadata(&p) {
            Ok(m) => {
                #[cfg(unix)]
                let owner_only = {
                    use std::os::unix::fs::PermissionsExt;
                    m.permissions().mode() & 0o077 == 0
                };
                #[cfg(not(unix))]
                let owner_only = true;
                if owner_only {
                    println!("✓ {name} present, owner-only");
                } else {
                    println!("! {name} present but GROUP/WORLD readable — chmod 600");
                    fail = 1;
                }
            }
            Err(_) => println!("- {name} absent (optional)"),
        }
    }
    // 4. disk guard
    let pct = foundry_watch::gc::tmp_usage_pct().unwrap_or(0);
    if pct >= 85 {
        println!("✗ /tmp at {pct}% ≥ 85% guard — Dies are being held");
        fail = 1;
    } else {
        println!("✓ /tmp at {pct}% (< 85% guard)");
    }
    if fail == 0 {
        println!("doctor: ALL OK");
    }
    fail
}

async fn reconcile_cmd(db: &str, apply: bool) -> i32 {
    let Some(led) = open_ledger_ro(db) else {
        eprintln!("✗ cannot open ledger at {db}");
        return 1;
    };
    let rows = led.read_all().unwrap_or_default();
    let mirror = foundry_ledger::Mirror::new(std::path::Path::new(db));
    let mstate = mirror.state(&rows);
    println!("mirror state: {mstate}");
    if matches!(mstate, foundry_ledger::MirrorState::Ok { .. }) {
        println!("nothing to reconcile");
        return 0;
    }
    if !apply {
        println!("dry-run: would rebuild from last-good prefix (pass --apply)");
        return 0;
    }
    // review #5: --apply must be the single writer. The Line lease is
    // in-process; cross-process we take an OS-level exclusive lock on
    // <db>.writer.lock — a running `foundry watch` holds it for its
    // lifetime (wired in watch_cmd), so reconcile --apply refuses while
    // any writer lives.
    let lock_path = format!("{db}.writer.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path);
    match lock_file {
        Ok(f) => {
            use std::os::unix::io::AsRawFd;
            let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc != 0 {
                eprintln!(
                    "✗ writer lock held ({lock_path}) — a writer (watch) is live; refusing reconcile --apply"
                );
                return 1;
            }
            // lock held for the remainder of this function via `f`
        }
        Err(e) => {
            eprintln!("✗ cannot open writer lock {lock_path}: {e}");
            return 1;
        }
    }
    // find last good seq from mirror vs ledger prefix comparison
    let existing = std::fs::read_to_string(mirror.path()).unwrap_or_default();
    let last_good = existing
        .lines()
        .filter_map(|l| serde_json::from_str::<foundry_ledger::MirrorLine>(l).ok())
        .map(|m| m.seq)
        .filter(|seq| {
            rows.iter().any(|r| r.seq == *seq) // only seqs that still exist
        })
        .max()
        .unwrap_or(0);
    match mirror.rebuild(&rows, Some(last_good + 1)) {
        Ok(n) => {
            println!("reconciled: rewrote {n} line(s) from seq {}", last_good + 1);
            println!("mirror now: {}", mirror.state(&rows));
            0
        }
        Err(e) => {
            eprintln!("✗ reconcile failed: {e}");
            1
        }
    }
}

fn corpus_cmd(db: &str, out: &str) -> i32 {
    let led = match open_ledger_ro(db) {
        Some(l) => l,
        None => return 1,
    };
    let rows = led.read_all().unwrap_or_default();
    let replayed = match foundry_ledger::replay::replay(&rows, |rs| {
        rs.iter()
            .zip(rs.iter().skip(1))
            .all(|(a, b)| b.prev_hash == a.hash)
    }) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("✗ replay failed: {e:?}");
            return 1;
        }
    };
    let root = std::path::PathBuf::from(out);
    let mut n = 0;
    for roc in &replayed {
        if foundry_watch::corpus::wants_manifest(roc.order.state()) {
            let m = foundry_watch::corpus_emit::manifest_for(roc, &rows);
            match m.save(&root) {
                Ok(p) => {
                    n += 1;
                    println!(
                        "{} {} [{}..{}]",
                        m.terminal_state,
                        m.order_id,
                        m.ledger_row_range.unwrap().0,
                        m.ledger_row_range.unwrap().1
                    );
                    let _ = p;
                }
                Err(e) => eprintln!("✗ write {}: {e}", m.order_id),
            }
        }
    }
    println!("corpus: {n} manifest(s) -> {}", root.display());
    0
}

fn benchmark_cmd(baseline: Option<String>, compare: Option<String>, corpus_dir: &str) -> i32 {
    use foundry_watch::benchmark;
    use foundry_watch::corpus::CorpusManifest;
    let corpus = match CorpusManifest::load_all(std::path::Path::new(corpus_dir)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("✗ corpus load: {e}");
            return 1;
        }
    };
    if let Some(b) = &baseline {
        let out = std::path::Path::new(b);
        match benchmark::write_baseline(&corpus, out) {
            Ok(()) => {
                let m = benchmark::compute(&corpus);
                println!(
                    "baseline written: {} orders | shipped={} failed={} rejected(no-op)={}",
                    m.cohort_size, m.shipped, m.failed, m.rejected_noop
                );
                println!(
                    "  cost: in={:?} (known {}/{}), attempts={}",
                    m.cost.input_tokens, m.cost.known_input, m.cohort_size, m.attempts_total
                );
                0
            }
            Err(e) => {
                eprintln!("✗ baseline write: {e}");
                1
            }
        }
    } else if let Some(b) = &compare {
        match benchmark::compare(&corpus, std::path::Path::new(b)) {
            Ok(rep) => {
                println!("compare vs {}:", b);
                println!(
                    "  shipped:  {} -> {} (delta {:+})",
                    rep.baseline.shipped, rep.current.shipped, rep.shipped_delta
                );
                println!(
                    "  failed:   {} -> {} (delta {:+})",
                    rep.baseline.failed, rep.current.failed, rep.failed_delta
                );
                println!(
                    "  rejected: {} -> {} (delta {:+})",
                    rep.baseline.rejected_noop, rep.current.rejected_noop, rep.rejected_delta
                );
                0
            }
            Err(benchmark::CompareError::VersionMismatch {
                baseline_mv,
                current_mv,
                baseline_schema,
                current_schema,
            }) => {
                eprintln!(
                    "✗ version mismatch: metric v{baseline_mv} vs v{current_mv}, schema v{baseline_schema} vs v{current_schema} — refusing to compare across definitions"
                );
                1
            }
            Err(e) => {
                eprintln!("✗ compare: {e:?}");
                1
            }
        }
    } else {
        eprintln!("specify --baseline <path> or --compare <path>");
        1
    }
}

async fn casting_cmd(
    run_profile: Option<String>,
    judge: Option<String>,
    promote: Option<String>,
    corpus_dir: &str,
    casting_dir: &str,
    seed: u64,
) -> i32 {
    use foundry_domain::casting::CastingRun;
    use foundry_ledger::casting_journal::CastingLedger;
    use foundry_watch::casting_run;
    use foundry_watch::judging::{self, JudgingPolicy, PrimaryMetric};

    let cdir = std::path::PathBuf::from(casting_dir);
    let journal = match CastingLedger::open(&cdir.join("casting.db")) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("✗ casting journal: {e}");
            return 1;
        }
    };

    if let Some(profile_id) = run_profile {
        // M5.2 T60c: the profile is built through the WIRING layer —
        // bundle-<digest> routes load a verified bundle (projection,
        // consumption-verified); anything else is the legacy string
        // path, journaled as such and non-promotable (astra B1).
        use foundry_watch::casting_wiring::{
            ProfileSource, classify_profile_arg, legacy_profile, profile_from_bundle,
        };

        let (profile, route_record) = match classify_profile_arg(&profile_id) {
            ProfileSource::Bundle { digest } => {
                let store = foundry_watch::bundles::BundleStore::open(&cdir.join("bundles.db"));
                match profile_from_bundle(&store, &digest, &format!("cast-{profile_id}-{seed}")) {
                    Ok((p, rec)) => (p, rec),
                    Err(e) => {
                        eprintln!("✗ bundle route: {e:?} — only verified bundles run");
                        return 1;
                    }
                }
            }
            ProfileSource::Legacy { raw } => {
                let (p, rec) = legacy_profile(&raw, &format!("cast-{profile_id}-{seed}"), seed);
                eprintln!(
                    "⚠ legacy profile route (journaled, non-promotable): run_id={} route={} verified={}",
                    rec.run_id, rec.route, rec.consumption_verified
                );
                (p, rec)
            }
        };
        let _ = &route_record.bundle_digest; // route choice observable on the record
        let policy = JudgingPolicy {
            max_cost_increase_frac: 0.10,
            primary_metrics: vec![
                PrimaryMetric::QualityFailuresDown,
                PrimaryMetric::NoOpRejectsDown,
            ],
            min_coverage_frac: 0.80,
            policy_version: 1,
        };
        // corpus hash: dir content digest
        let corpus_hash = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            let mut names: Vec<_> = std::fs::read_dir(corpus_dir)
                .map(|it| {
                    it.filter_map(|e| e.ok())
                        .map(|e| e.file_name().to_string_lossy().to_string())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            names.sort();
            for n in names {
                h.update(n.as_bytes());
            }
            hex::encode(h.finalize())
        };
        let run = CastingRun {
            id: format!("cast-{profile_id}-{seed}"),
            candidate_profile_id: profile_id.clone(),
            candidate_profile_hash: profile.hash(),
            corpus_ref: corpus_dir.to_string(),
            corpus_hash,
            policy_hash: judging::policy_hash(&policy),
            metric_version: foundry_watch::benchmark::METRIC_VERSION,
            seed,
            base_git_sha: {
                let corpus = foundry_watch::corpus::CorpusManifest::load_all(std::path::Path::new(
                    corpus_dir,
                ))
                .unwrap_or_default();
                corpus
                    .first()
                    .and_then(|c| c.base_git_sha.clone())
                    .unwrap_or_default()
            },
            tool_versions: vec![("die".into(), "dry-1".into())],
            input_digest: String::new(),
            state: foundry_domain::casting::CastingState::Draft,
        };
        let input_digest = CastingRun::compute_input_digest(
            &run.candidate_profile_hash,
            &run.corpus_hash,
            &run.policy_hash,
            run.metric_version,
            run.seed,
            &run.base_git_sha,
            &run.tool_versions,
        );
        let mut run = run;
        run.input_digest = input_digest;
        match casting_run::run_casting(
            run,
            std::path::Path::new(corpus_dir),
            &cdir.join("sandbox"),
            &cdir.join("candidate"),
            &journal,
            profile, // the verified profile IS what executes (astra 5th)
        )
        .await
        {
            Ok(out) => {
                println!(
                    "casting run {}: executed={} skipped-breaker={} non-replayable={}",
                    out.run.id, out.executed, out.skipped_breaker, out.non_replayable
                );
                println!(
                    "  input_digest {:.16}.. bound (profile/corpus/policy/seed/sha)",
                    out.run.input_digest
                );
                0
            }
            Err(e) => {
                eprintln!("✗ {e}");
                1
            }
        }
    } else if let Some(_run_id) = judge {
        let policy = JudgingPolicy {
            max_cost_increase_frac: 0.10,
            primary_metrics: vec![
                PrimaryMetric::QualityFailuresDown,
                PrimaryMetric::NoOpRejectsDown,
            ],
            min_coverage_frac: 0.80,
            policy_version: 1,
        };
        // NOTE: full judge requires a persisted CastingRun record; the
        // M3 gate exercises judge via the integration test (4-path block).
        eprintln!(
            "judge: use the integration path (casting::tests) or persist run state first — direct CLI judge lands with serve (T27)"
        );
        let _ = policy;
        1
    } else if let Some(_run_id) = promote {
        eprintln!(
            "promote: requires human ApprovalToken — provide via serve approve action (T27); CLI cannot mint tokens"
        );
        1
    } else {
        eprintln!("specify --run-profile <id>, --judge <run-id>, or --promote <run-id>");
        1
    }
}
