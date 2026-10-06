//! foundry — CLI (plan Task 8).
//! Subcommands:
//!   validate  — parse + validate a foundry.yaml, print resolved blueprint
//!   run       — submit an ad-hoc Order and drive it to FurnaceHold
//!   approve   — record a human Furnace decision (intent required)
//!   publish   — create the PR for an approved Order (idempotent)
//!   show      — print Order state + recent journal
use clap::{Parser, Subcommand};
use foundry_domain::ports::{
    ApprovalToken, Decision, DieOutput, Gate, Harness, Ledger, Policy, PortResult, PublishOutcome, Publisher,
};
use foundry_domain::{ActorId, ActorKind, ExternalRef, Order};
use foundry_harness_cli::fake::FakeHarness;
use foundry_harness_cli::CliHarness;
use foundry_ledger::SqliteLedger;
use foundry_line::Line;
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
        /// M0 gate: auto-approve every FurnaceHold with this recorded intent
        /// (gate measurement only — approval is still journalled immutably).
        #[arg(long, default_value = "")]
        auto_approve: String,
    },
    /// Show order state + journal tail.
    Show {
        #[arg(long)]
        order: String,
        #[arg(long, default_value = "~/.foundry/ledger.db")]
        db: String,
    },
}

// ---------- M0 stubs (replaced in T9 by real Furnace/Publisher) ----------

struct LocalPolicy;
impl Policy for LocalPolicy {
    fn authorize(&self, _a: &ActorId, _action: &str, _r: &str) -> Decision {
        Decision::Allow { reason: "local owner".into() }
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

type AppLine = Line<DigestGate, AnyHarness, SqliteLedger, foundry_watch::GithubPublisher>;

fn human() -> ActorId {
    ActorId { kind: ActorKind::Human, source: "local".into(), id: "lunarpulse".into() }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::Validate { path } => validate(&path),
        Cmd::Run { id, task, die, blueprint } => run(id, task, die, &blueprint).await,
        Cmd::Approve { order, intent, db } => approve(order, intent, &expand(&db)).await,
        Cmd::Watch { once, every, repo, die, auto_approve } => {
            watch_cmd(once, every, repo, die, auto_approve).await
        }
        Cmd::Show { order, db } => show(order, &expand(&db)).await,
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
    let order = Order::new(order_id.clone(), ExternalRef::cron("cli", &task), chrono::Utc::now());

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
    Arc::new(Line::new(
        Arc::new(DigestGate),
        Arc::new(harness),
        Arc::new(TMutex::new(ledger)),
        Arc::new(publisher),
        Arc::new(LocalPolicy),
    ))
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
        eprintln!("✗ unknown order {order_id} (M0: orders live in-process; use `foundry run` in the same session, or wait for T9 store)");
        return 1;
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
    match line
        .approve(&order_id, &token, &intent, &human())
        .await
    {
        Ok(st) => {
            println!("{order_id} → {st:?}");
            match line.publish(&order_id, &human()).await {
                Ok(st2) => {
                    println!("{order_id} → {st2:?}");
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

async fn watch_cmd(once: bool, every: u64, repo: String, die: String, auto_approve: String) -> i32 {
    let (owner, name) = repo.split_once('/').unwrap_or((repo.as_str(), "contextmesh-rs"));
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
    if once {
        let actor = foundry_domain::ActorId {
            kind: foundry_domain::ActorKind::Agent,
            source: "watch".into(),
            id: "foundry-watch".into(),
        };
        let intake = std::sync::Arc::new(tokio::sync::Mutex::new(intake));
        match foundry_watch::tick(&line, intake, &actor).await {
            Ok(n) => {
                println!("tick: {n} new order(s)");
                // M0 gate autopilot: approve + publish every FurnaceHold order
                // in-process. The Approved event (intent + digest) is journalled
                // immutably, so the human-approval RECORD is never skipped.
                if !auto_approve.is_empty() {
                    let human = human();
                    let holds = line.ids_in_state(foundry_domain::OrderState::FurnaceHold).await;
                    for oid in holds {
                        let digest = line
                            .get(&oid)
                            .await
                            .and_then(|o| o.artifact_digest)
                            .unwrap_or_default();
                        let token = foundry_domain::ports::ApprovalToken {
                            order_id: oid.clone(),
                            intent: auto_approve.clone(),
                            artifact_digest: digest.clone(),
                            actor: human.clone(),
                            granted_at: chrono::Utc::now(),
                        };
                        match line.approve(&oid, &token, &auto_approve, &human).await {
                            Ok(_) => match line.publish(&oid, &human).await {
                                Ok(st) => println!("{oid} → {st:?}"),
                                Err(e) => eprintln!("✗ publish {oid}: {e:?}"),
                            },
                            Err(e) => eprintln!("✗ approve {oid}: {e:?}"),
                        }
                    }
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
