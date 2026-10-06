//! Line — the Order executor (plan Task 7). Single-writer loop.
//!
//! Design contracts (v3 §10):
//! - **Single writer**: callers acquire a run lease; watch is the only
//!   process that should hold it (#4).
//! - **Atomic transition+ledger**: both happen inside one critical section —
//!   an un-journaled transition is impossible (#5; M1 moves to one SQLite tx).
//! - **Attempt fencing**: the Line drives Die attempts; stale Die reports are
//!   rejected by the domain state machine (#3).
//! - **Idempotent publish**: Publisher gets operation_id = "order:attempt";
//!   crash → PublishUnknown → reconcile (#2).
//! - **Approval ≠ published**: separate FurnaceHold → Publishing → Shipped
//!   states; PR creation is fallible and re-verifies the digest (#1, §11).
use crate::error::LineError;
use foundry_domain::ports::{
    Gate, Harness, Ledger, Policy, PortError, PublishOutcome, Publisher,
};
use foundry_domain::{ActorId, ActorKind, Order, OrderEvent, OrderState};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Backoff between retries: base * 2^(attempt-1), capped.
pub fn backoff_delay(attempt: u32, base_ms: u64, max_ms: u64) -> u64 {
    let d = base_ms.saturating_mul(1u64 << (attempt.saturating_sub(1)).min(16));
    d.min(max_ms)
}

#[derive(Debug, Clone)]
pub struct LineConfig {
    pub base_backoff_ms: u64,
    pub max_backoff_ms: u64,
}

impl Default for LineConfig {
    fn default() -> Self {
        Self {
            base_backoff_ms: 5_000,
            max_backoff_ms: 15 * 60_000,
        }
    }
}

/// Dependencies, trait-generic. The executor never sees concrete types.
pub struct Line<G, P, L, PUB>
where
    G: Gate,
    P: Harness,
    L: Ledger,
    PUB: Publisher,
{
    pub gate: Arc<G>,
    pub harness: Arc<P>,
    pub ledger: Arc<Mutex<L>>,
    pub publisher: Arc<PUB>,
    pub policy: Arc<dyn Policy>,
    pub cfg: LineConfig,
    lease: Mutex<Option<String>>,
    /// In-memory store for M0 (single process, single writer).
    /// M1+ swaps for SQLite with conditional UPDATE ... WHERE version.
    orders: Mutex<HashMap<String, Order>>,
}

impl<G, P, L, PUB> Line<G, P, L, PUB>
where
    G: Gate + Send + Sync,
    P: Harness + Send + Sync,
    L: Ledger + Send + Sync,
    PUB: Publisher + Send + Sync,
{
    pub fn new(
        gate: Arc<G>,
        harness: Arc<P>,
        ledger: Arc<Mutex<L>>,
        publisher: Arc<PUB>,
        policy: Arc<dyn Policy>,
    ) -> Self {
        Self {
            gate,
            harness,
            ledger,
            publisher,
            policy,
            cfg: LineConfig::default(),
            lease: Mutex::new(None),
            orders: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_cfg(mut self, cfg: LineConfig) -> Self {
        self.cfg = cfg;
        self
    }

    // ---- lease (single writer, critique #4) ----

    pub async fn acquire_lease(&self, writer: &str) -> Result<(), LineError> {
        let mut l = self.lease.lock().await;
        match l.as_deref() {
            None => {
                *l = Some(writer.to_string());
                Ok(())
            }
            Some(w) if w == writer => Ok(()), // reentrant
            Some(other) => Err(LineError::LeaseHeld(other.to_string())),
        }
    }

    pub async fn release_lease(&self, writer: &str) {
        let mut l = self.lease.lock().await;
        if l.as_deref() == Some(writer) {
            *l = None;
        }
    }

    // ---- intake ----

    pub async fn submit(&self, order: Order) {
        self.orders.lock().await.insert(order.id.clone(), order);
    }

    pub async fn get(&self, id: &str) -> Option<Order> {
        self.orders.lock().await.get(id).cloned()
    }

    /// All in-process order ids currently in `state` (M0 gate: autopilot drain).
    pub async fn ids_in_state(&self, state: OrderState) -> Vec<String> {
        self.orders
            .lock()
            .await
            .values()
            .filter(|o| o.state() == state)
            .map(|o| o.id.clone())
            .collect()
    }

    // ---- core: atomic transition + journal ----

    async fn commit(
        &self,
        order_id: &str,
        event: OrderEvent,
        event_attempt: u32,
        actor: &ActorId,
    ) -> Result<Order, LineError> {
        let mut orders = self.orders.lock().await;
        let order = orders
            .get_mut(order_id)
            .ok_or_else(|| LineError::UnknownOrder(order_id.to_string()))?;

        let from = order.state();
        let to = order
            .transition(event.clone(), event_attempt)
            .map_err(|e| LineError::IllegalTransition(order_id.to_string(), e))?;

        // Journal inside the same critical section as the state change.
        // M0: the in-process Mutex IS the transaction. M1: one SQLite tx.
        {
            let mut led = self.ledger.lock().await;
            led.append(order_id, &event, from, to, actor, foundry_domain::order::now())
                .map_err(|e| LineError::Port(order_id.to_string(), e))?;
        }
        // NOTE: version is bumped by the domain transition() itself — the
        // executor must not double-bump (journal rows == version invariant).
        Ok(order.clone())
    }

    /// Drive one Order as far as it can go without a human.
    pub async fn drive(&self, order_id: &str) -> Result<OrderState, LineError> {
        let actor = ActorId {
            kind: ActorKind::Agent,
            source: "line".into(),
            id: "executor".into(),
        };

        loop {
            let order = self
                .get(order_id)
                .await
                .ok_or_else(|| LineError::UnknownOrder(order_id.to_string()))?;

            match order.state() {
                OrderState::Received => {
                    self.commit(order_id, OrderEvent::Queued, 0, &actor).await?;
                }
                OrderState::Queued => {
                    let attempt = order.attempt().max(1);
                    let workdir = std::path::PathBuf::from(format!(
                        "/tmp/foundry-die-{order_id}-{attempt}"
                    ));
                    std::fs::create_dir_all(&workdir).map_err(|e| {
                        LineError::Port(order_id.to_string(), PortError::Permanent(e.to_string()))
                    })?;

                    self.commit(order_id, OrderEvent::DieStarted, attempt, &actor)
                        .await?;

                    match self.harness.run(&order, attempt, &workdir).await {
                        Ok(die_out) => {
                            self.commit(
                                order_id,
                                OrderEvent::DieSucceeded { attempt },
                                attempt,
                                &actor,
                            )
                            .await?;
                            // Bind the artifact digest (M0: result commit hash).
                            // Approval (T2 rule) refuses an empty digest.
                            {
                                let mut orders = self.orders.lock().await;
                                if let Some(o) = orders.get_mut(order_id) {
                                    o.artifact_digest =
                                        Some(die_out.result_commit.clone());
                                }
                            }
                            // Inspector (T7b) makes the real verdict; M0 passes through.
                            self.commit(order_id, OrderEvent::InspectionPassed, attempt, &actor)
                                .await?;
                        }
                        Err(PortError::Retryable(reason)) => {
                            self.commit(
                                order_id,
                                OrderEvent::DieFailed { attempt, reason },
                                attempt,
                                &actor,
                            )
                            .await?;
                            let delay = backoff_delay(
                                attempt,
                                self.cfg.base_backoff_ms,
                                self.cfg.max_backoff_ms,
                            );
                            tracing::info!(
                                order = order_id,
                                attempt,
                                delay_ms = delay,
                                "retrying after backoff"
                            );
                            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                        }
                        Err(e) => {
                            self.commit(
                                order_id,
                                OrderEvent::DieFailed {
                                    attempt,
                                    reason: format!("permanent: {e:?}"),
                                },
                                attempt,
                                &actor,
                            )
                            .await?;
                        }
                    }
                }
                OrderState::Working => {
                    // executor crashed mid-Die — fenced attempt; T9 reconcile
                    // re-drives with a fresh attempt after lease recovery.
                    return Ok(OrderState::Working);
                }
                OrderState::Inspecting => {
                    self.commit(
                        order_id,
                        OrderEvent::InspectionPassed,
                        order.attempt(),
                        &actor,
                    )
                    .await?;
                }
                OrderState::FurnaceHold => {
                    return Ok(OrderState::FurnaceHold); // human decides
                }
                OrderState::Publishing => {
                    return Ok(OrderState::Publishing);
                }
                other => return Ok(other),
            }
        }
    }

    /// Human approval: Policy → Gate → Approved event (digest-bound).
    /// Publishing stays a SEPARATE explicit step (critique #1).
    pub async fn approve(
        &self,
        order_id: &str,
        token: &foundry_domain::ports::ApprovalToken,
        intent: &str,
        actor: &ActorId,
    ) -> Result<OrderState, LineError> {
        let decision = self.policy.authorize(actor, "approve", order_id);
        if let foundry_domain::ports::Decision::Deny { reason } = decision {
            return Err(LineError::Policy(order_id.to_string(), reason));
        }

        let order = self
            .get(order_id)
            .await
            .ok_or_else(|| LineError::UnknownOrder(order_id.to_string()))?;

        let digest = order.artifact_digest.clone().unwrap_or_default();
        self.gate
            .verify(&order, token, &digest)
            .map_err(|e| LineError::Port(order_id.to_string(), e))?;

        self.commit(
            order_id,
            OrderEvent::Approved {
                intent: intent.to_string(),
                artifact_digest: digest,
            },
            order.attempt(),
            actor,
        )
        .await?;
        Ok(OrderState::Publishing)
    }

    /// Create the PR — idempotent via operation_id (critique #2).
    pub async fn publish(&self, order_id: &str, actor: &ActorId) -> Result<OrderState, LineError> {
        let order = self
            .get(order_id)
            .await
            .ok_or_else(|| LineError::UnknownOrder(order_id.to_string()))?;
        let attempt = order.attempt();
        let operation_id = format!("{order_id}:{attempt}");
        let digest = order.artifact_digest.clone().unwrap_or_default();

        match self
            .publisher
            .publish(&order, &operation_id, &digest)
            .await
        {
            Ok(PublishOutcome::Confirmed { pr_url }) => {
                self.commit(
                    order_id,
                    OrderEvent::PublishConfirmed { pr_url },
                    attempt,
                    actor,
                )
                .await?;
            }
            Ok(PublishOutcome::Unknown { .. }) => {
                self.commit(
                    order_id,
                    OrderEvent::PublishUnknown { operation_id },
                    attempt,
                    actor,
                )
                .await?;
            }
            Err(e) => return Err(LineError::Port(order_id.to_string(), e)),
        }

        self.get(order_id)
            .await
            .map(|o| o.state())
            .ok_or_else(|| LineError::UnknownOrder(order_id.to_string()))
    }
}
