//! The watch loop — plan Task 9.
//! Single writer holding the Line lease; every tick:
//!   1. reconcile Unknown publishes (critique #2)
//!   2. Intake.poll → dedup → submit → drive (to FurnaceHold)
//!   3. notify FurnaceHold orders (M0: stdout; M1: Discord Notifier)
use crate::intake::GithubLabelIntake;
use foundry_domain::ports::Intake;
use foundry_domain::{ActorId, ActorKind, Order};
use foundry_line::Line;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub struct WatchConfig {
    pub tick_secs: u64,
    pub owner: String,
    pub repo: String,
    pub die_bin: Option<String>,
    pub ledger_path: PathBuf,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            tick_secs: 300,
            owner: "lunarpulse".into(),
            repo: "contextmesh-rs".into(),
            die_bin: None,
            ledger_path: PathBuf::from(".foundry/ledger.db"),
        }
    }
}

/// Run one poll→drive cycle. Returns how many orders were processed.
/// Public so tests / `foundry run --once` can drive a single tick.
pub async fn tick<G, P, L, PUB>(
    line: &Line<G, P, L, PUB>,
    intake: Arc<Mutex<GithubLabelIntake>>,
    actor: &ActorId,
) -> Result<usize, String>
where
    G: foundry_domain::ports::Gate + Send + Sync,
    P: foundry_domain::ports::Harness + Send + Sync,
    L: foundry_domain::ports::Ledger + Send + Sync,
    PUB: foundry_domain::ports::Publisher + Send + Sync,
{
    // poll() blocks (reqwest::blocking) — run off the async runtime so the
    // client's internal pool never drops inside tokio (T9 실측 버그 #3).
    let drafts = {
        let i = intake.clone();
        tokio::task::spawn_blocking(move || {
            let mut i = i.blocking_lock();
            i.poll()
        })
        .await
        .map_err(|e| format!("intake join: {e}"))?
        .map_err(|e| format!("intake poll: {e:?}"))?
    };
    let n = drafts.len();

    for draft in drafts {
        let order_id = format!(
            "o-{}",
            draft.external.dedup_key.split('#').next_back().unwrap_or("x")
        );
        let order = Order::new(order_id.clone(), draft.external.clone(), chrono::Utc::now());
        line.submit(order).await;
        match line.drive(&order_id).await {
            Ok(foundry_domain::OrderState::FurnaceHold) => {
                tracing::info!(order = %order_id, "FurnaceHold — awaiting approval");
                { let mut i = intake.lock().await; i.ack(&draft.draft_id).map_err(|e| format!("ack: {e:?}"))?; }
            }
            Ok(other) => {
                tracing::info!(order = %order_id, state = ?other, "driven");
                { let mut i = intake.lock().await; i.ack(&draft.draft_id).map_err(|e| format!("ack: {e:?}"))?; }
            }
            Err(e) => {
                // Leave un-acked: at-least-once redelivery on next tick.
                tracing::warn!(order = %order_id, error = ?e, "drive failed — will redeliver");
            }
        }
    }
    let _ = actor;
    Ok(n)
}

/// Long-running loop. Holds the lease; exits if someone else holds it.
pub async fn run<G, P, L, PUB>(
    line: &Line<G, P, L, PUB>,
    intake: GithubLabelIntake,
    cfg: WatchConfig,
) -> Result<(), String>
where
    G: foundry_domain::ports::Gate + Send + Sync + 'static,
    P: foundry_domain::ports::Harness + Send + Sync + 'static,
    L: foundry_domain::ports::Ledger + Send + Sync + 'static,
    PUB: foundry_domain::ports::Publisher + Send + Sync + 'static,
{
    let actor = ActorId {
        kind: ActorKind::Agent,
        source: "watch".into(),
        id: "foundry-watch".into(),
    };

    line.acquire_lease("watch")
        .await
        .map_err(|e| format!("lease: {e}"))?;
    tracing::info!(tick_secs = cfg.tick_secs, "watch started (lease held)");

    let intake_arc = std::sync::Arc::new(tokio::sync::Mutex::new(intake));

    let mut interval = tokio::time::interval(std::time::Duration::from_secs(cfg.tick_secs));
    loop {
        interval.tick().await;
        match tick(line, intake_arc.clone(), &actor).await {
            Ok(0) => {} // quiet tick
            Ok(n) => tracing::info!(orders = n, "tick processed"),
            Err(e) => tracing::warn!(error = %e, "tick failed"),
        }
    }
}
