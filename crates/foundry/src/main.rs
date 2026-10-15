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
    ApprovalToken, Decision, DieOutput, Gate, Harness, Ledger, Policy, PortResult, PublishOutcome,
    Publisher,
};
use foundry_domain::{ActorId, ActorKind, ExternalRef, Order};
use foundry_harness_cli::CliHarness;
use foundry_harness_cli::fake::FakeHarness;
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
        /// M0 gate: auto-approve every FurnaceHold with this recorded intent
        /// (gate measurement only — approval is still journalled immutably).
        #[arg(long, default_value = "")]
        auto_approve: String,
        /// notify channel: discord | console | all
        #[arg(long, default_value = "discord")]
        notify_mode: String,
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
        Cmd::DeployProfile { check, units, readme } => {
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
        Cmd::Serve { db, bind } => {
            let state = std::sync::Arc::new(match foundry_serve::ServeState::new(&expand(&db)) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("✗ serve: {e}");
                    std::process::exit(1);
                }
            });
            let app = foundry_serve::http::router(state);
            let listener = match tokio::net::TcpListener::bind(&bind).await {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("✗ bind {bind}: {e}");
                    std::process::exit(1);
                }
            };
            println!("foundry-serve on http://{bind} (read-only)");
            match axum::serve(listener, app).await {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("✗ serve: {e}");
                    1
                }
            }
        }
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
    let line = Line::new(
        Arc::new(DigestGate),
        Arc::new(harness),
        Arc::new(TMutex::new(ledger)),
        Arc::new(publisher),
        Arc::new(LocalPolicy),
    );
    if let Ok(tuples) = foundry_exec::SqliteTupleStore::open(tuple_db_path()) {
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
            Err(e) => { eprintln!("✗ tuple store open failed: {e:?}"); return 1; }
        };
        match foundry_line::rehydrate::rehydrate(&ledger_ro, std::path::Path::new("/tmp"), &tuples) {
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

async fn watch_cmd(
    once: bool,
    every: u64,
    repo: String,
    die: String,
    auto_approve: String,
    notify_mode: String,
) -> i32 {
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
                eprintln!("! writer lock unavailable (continuing without): {e}");
                None
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
        use foundry_domain::ports::Notifier as _;
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
                // M0 gate autopilot: approve + publish every FurnaceHold order
                // in-process. The Approved event (intent + digest) is journalled
                // immutably, so the human-approval RECORD is never skipped.
                if !auto_approve.is_empty() {
                    let human = human();
                    let holds = line
                        .ids_in_state(foundry_domain::OrderState::FurnaceHold)
                        .await;
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
                                Ok(st) => {
                                    println!("{oid} → {:?}", st.state);
                                    if st.state == foundry_domain::OrderState::Shipped {
                                        notify(
                                            &mut notifier,
                                            foundry_domain::ports::NotifyEvent::Shipped {
                                                order_id: oid.clone(),
                                                pr_url: st.pr_url.clone().unwrap_or_default(),
                                            },
                                        );
                                    }
                                }
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
        use foundry_watch::casting_wiring::{classify_profile_arg, legacy_profile, profile_from_bundle, ProfileSource};

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
