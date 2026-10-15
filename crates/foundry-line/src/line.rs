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
use crate::PublishReceipt;
use foundry_domain::ports::{Gate, Harness, Ledger, Policy, PortError, PublishOutcome, Publisher};
use foundry_domain::{ActorId, ActorKind, Order, OrderEvent, OrderState, workdir_path};
use foundry_exec::{Attempt, Epoch, OrderId, ReviewRunPort, ReviewRunRequest, RunId, TupleStore};
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
    pub policy: Arc<dyn Policy + Send + Sync>,
    review_run: std::sync::Mutex<Option<Box<dyn ReviewRunPort>>>,
    tuple_store: std::sync::Mutex<Option<Arc<dyn TupleStore>>>,
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
        policy: Arc<dyn Policy + Send + Sync>,
    ) -> Self {
        Self {
            gate,
            harness,
            ledger,
            publisher,
            policy,
            review_run: std::sync::Mutex::new(None),
            tuple_store: std::sync::Mutex::new(None),
            cfg: LineConfig::default(),
            lease: Mutex::new(None),
            orders: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_cfg(mut self, cfg: LineConfig) -> Self {
        self.cfg = cfg;
        self
    }

    pub fn with_review_run(self, review_run: impl ReviewRunPort + 'static) -> Self {
        *self.review_run.lock().unwrap() = Some(Box::new(review_run));
        self
    }

    pub fn with_tuple_store(mut self, tuple_store: Arc<dyn TupleStore>) -> Self {
        *self.tuple_store.get_mut().unwrap() = Some(tuple_store);
        self
    }

    pub fn set_tuple_store(&self, tuple_store: Arc<dyn TupleStore>) {
        *self.tuple_store.lock().unwrap() = Some(tuple_store);
    }

    fn has_review_tuple(&self, order: &Order) -> Result<bool, LineError> {
        let store_guard = self.tuple_store.lock().unwrap();
        let Some(store) = store_guard.as_ref() else { return Ok(false); };
        let digest = order.artifact_digest.as_deref().unwrap_or_default();
        if digest.is_empty() { return Ok(false); }
        store.latest_promoted(&OrderId(order.id.clone()), Attempt(order.attempt() as u64))
            .map(|tuple| tuple.is_some_and(|tuple| tuple.immutable_commit.0 == digest))
            .map_err(|e| LineError::Port(order.id.clone(), foundry_domain::ports::PortError::Permanent(format!("tuple store: {e:?}"))))
    }

    /// Test/integration helper: commit a specific event WITHOUT running
    /// the Die — drives the same state machine + ledger append as a real
    /// run (used by serve T27 tests to place an order at FurnaceHold).
    pub async fn commit_for_test(
        &self,
        order_id: &str,
        event: &OrderEvent,
        _from: OrderState,
        _to: OrderState,
        actor: &ActorId,
    ) -> Result<Order, LineError> {
        self.commit(order_id, event.clone(), 1, actor).await
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
            led.append(
                order_id,
                &event,
                from,
                to,
                actor,
                foundry_domain::order::now(),
            )
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
                    let workdir = workdir_path(order_id, attempt);
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
                                    o.artifact_digest = Some(die_out.result_commit.clone());
                                }
                            }
                            let current = self.get(order_id).await
                                .ok_or_else(|| LineError::UnknownOrder(order_id.to_string()))?;
                            // Preserve the no-op taxonomy, but require durable
                            // review evidence before the passing transition.
                            let noop = die_out.result_commit.is_empty()
                                || die_out.result_commit == die_out.base_commit;
                            if noop {
                                let verdict = foundry_domain::inspector::inspect(
                                    &foundry_domain::inspector::DiffStats { files_changed: 0, lines_changed: 0 },
                                    foundry_domain::inspector::change_required(&order),
                                );
                                self.commit(order_id, foundry_domain::inspector::event_for(&verdict), attempt, &actor).await?;
                                match verdict {
                                    foundry_domain::inspector::Verdict::PreflightNoWork { .. }
                                    | foundry_domain::inspector::Verdict::ExpectedChangeButEmptyDiff => {
                                    // terminal — stop driving this order,
                                    // report its now-terminal state
                                    let st = {
                                        let orders = self.orders.lock().await;
                                        orders
                                            .get(order_id)
                                            .map(|o| o.state())
                                            .unwrap_or(OrderState::Failed)
                                    };
                                    return Ok(st);
                                    }
                                    _ => {}
                                }
                            } else if self.has_review_tuple(&current)? {
                                // Reuse durable evidence before invoking the review host.
                                self.commit(order_id, OrderEvent::InspectionPassed, attempt, &actor).await?;
                            } else {
                                let request = ReviewRunRequest {
                                    order: OrderId(order_id.to_string()), attempt: Attempt(attempt as u64),
                                    run: RunId(attempt as u64), base_commit: die_out.base_commit,
                                    result_commit: die_out.result_commit.clone(), artifact_digest: die_out.result_commit,
                                    epoch: Epoch(0), activation: order.version,
                                };
                                let review = match self.review_run.lock().unwrap().as_mut() {
                                    Some(runner) => runner.run(request),
                                    None => Err(foundry_exec::ReviewRunError::MissingRunner),
                                };
                                match review {
                                    Ok(foundry_exec::ReviewRunOutcome::Completed(evidence)) if evidence.verdict == foundry_exec::ReviewVerdict::Passed => {
                                        let tuple={
                                            let store=self.tuple_store.lock().unwrap();
                                            store.as_ref().map(|s|s.latest_promoted(&OrderId(order_id.to_owned()),Attempt(attempt as u64)))
                                                .transpose().map_err(|e|LineError::Port(order_id.to_owned(),foundry_domain::ports::PortError::Permanent(format!("tuple store: {e:?}"))))?.flatten()
                                        };
                                        let bound = evidence.tuple_key.order.0 == order_id
                                            && evidence.tuple_key.attempt.0 == attempt as u64
                                            && evidence.tuple_key.run.0 == attempt as u64
                                            && evidence.emit_receipt.key == evidence.tuple_key
                                            && tuple.as_ref().is_some_and(|t|t.key()==evidence.tuple_key);
                                        if bound {
                                            if let Some(tuple)=tuple && let Some(order)=self.orders.lock().await.get_mut(order_id) {
                                                order.artifact_digest=Some(tuple.immutable_commit.0);
                                            }
                                            self.commit(order_id, OrderEvent::InspectionPassed, attempt, &actor).await?;
                                        } else {
                                            self.commit(order_id, OrderEvent::InspectionFailed { reason: "review evidence tuple mismatch".into() }, attempt, &actor).await?;
                                        }
                                    }
                                    Ok(foundry_exec::ReviewRunOutcome::Held(held)) => {
                                        self.commit(order_id, OrderEvent::ReviewHeld { reason: held.hold.reason }, attempt, &actor).await?;
                                    }
                                    Ok(foundry_exec::ReviewRunOutcome::Completed(_)) => {
                                        self.commit(order_id, OrderEvent::InspectionFailed { reason: "review returned no passing verdict".into() }, attempt, &actor).await?;
                                    }
                                    Err(error) => {
                                        self.commit(order_id, OrderEvent::InspectionFailed { reason: format!("review failed: {error:?}") }, attempt, &actor).await?;
                                    }
                                }
                            }
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
                    if self.has_review_tuple(&order)? {
                        self.commit(order_id, OrderEvent::InspectionPassed, order.attempt(), &actor).await?;
                    } else {
                        return Ok(OrderState::Inspecting);
                    }
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

    /// Restores a verified ledger order using the durable review run as its artifact source.
    /// Review holds have no promoted tuple until human resolution completes.
    pub async fn restore_review_hold(&self, mut order:Order)->Result<(),LineError> {
        let invalid=|message:&str|LineError::Port(order.id.clone(),foundry_domain::ports::PortError::Permanent(message.into()));
        if order.state()!=OrderState::FurnaceHold {return Err(invalid("order is not waiting on review"));}
        let latest={let port=self.review_run.lock().unwrap();
            port.as_ref().ok_or_else(||invalid("review runner unavailable"))?.latest_run(&order.id,order.attempt() as u64)
                .map_err(|e|invalid(&format!("latest review run: {e:?}")))?
        }.ok_or_else(||invalid("review run not found"))?;
        if !matches!(latest.status.as_str(),"held"|"resolved"|"review_aborted") || latest.result_commit.is_empty() {return Err(invalid("review hold is not recoverable"));}
        order.artifact_digest=Some(latest.result_commit);
        self.orders.lock().await.entry(order.id.clone()).or_insert(order);
        Ok(())
    }

    /// Re-enters a resolved review hold through a new durable host run.
    pub async fn resume_review(&self, order_id: &str) -> Result<OrderState, LineError> {
        let order=self.get(order_id).await.ok_or_else(||LineError::UnknownOrder(order_id.to_owned()))?;
        if order.state()!=OrderState::FurnaceHold { return Err(LineError::IllegalTransition(order_id.to_owned(),foundry_domain::order::TransitionError("review resume requires FurnaceHold".into()))); }
        let attempt=order.attempt() as u64;
        let latest={
            let port=self.review_run.lock().unwrap();
            port.as_ref().ok_or_else(||LineError::Port(order_id.to_owned(),foundry_domain::ports::PortError::Permanent("review runner unavailable".into())))?
                .latest_run(order_id,attempt).map_err(|e|LineError::Port(order_id.to_owned(),foundry_domain::ports::PortError::Permanent(format!("latest review run: {e:?}"))))?
        }.ok_or_else(||LineError::Port(order_id.to_owned(),foundry_domain::ports::PortError::Permanent("review run not found".into())))?;
        if latest.status=="held" { return Ok(OrderState::FurnaceHold); }
        if latest.status!="resolved" && latest.status!="review_aborted" {
            return Err(LineError::Port(order_id.to_owned(),foundry_domain::ports::PortError::Permanent("review hold is not resolved".into())));
        }
        let request=ReviewRunRequest{order:OrderId(order_id.to_owned()),attempt:Attempt(attempt),run:RunId(latest.run+1),base_commit:latest.base_commit,result_commit:latest.result_commit,artifact_digest:order.artifact_digest.clone().unwrap_or_default(),epoch:Epoch(order.version),activation:order.version};
        let result={
            let mut port=self.review_run.lock().unwrap();
            port.as_mut().ok_or_else(||LineError::Port(order_id.to_owned(),foundry_domain::ports::PortError::Permanent("review runner unavailable".into())))?
                .run(request)
        };
        let actor=ActorId{kind:ActorKind::Agent,source:"line".into(),id:"review-resume".into()};
        match result {
            Ok(foundry_exec::ReviewRunOutcome::Held(_)) => Ok(OrderState::FurnaceHold),
            Ok(foundry_exec::ReviewRunOutcome::Completed(evidence)) if evidence.verdict==foundry_exec::ReviewVerdict::Passed => {
                let tuple=self.tuple_store.lock().unwrap().as_ref().ok_or_else(||LineError::Port(order_id.to_owned(),foundry_domain::ports::PortError::Permanent("tuple store unavailable".into())))?
                    .latest_promoted(&OrderId(order_id.to_owned()),Attempt(attempt)).map_err(|e|LineError::Port(order_id.to_owned(),foundry_domain::ports::PortError::Permanent(format!("tuple store: {e:?}"))))?
                    .ok_or_else(||LineError::Port(order_id.to_owned(),foundry_domain::ports::PortError::Permanent("promoted review tuple missing".into())))?;
                if evidence.tuple_key!=tuple.key() || evidence.emit_receipt.key!=tuple.key() || tuple.run.0!=latest.run+1 {
                    return Err(LineError::Port(order_id.to_owned(),foundry_domain::ports::PortError::Permanent("review evidence tuple mismatch".into())));
                }
                if let Some(current)=self.orders.lock().await.get_mut(order_id) { current.artifact_digest=Some(tuple.immutable_commit.0); }
                self.commit(order_id,OrderEvent::InspectionPassed,attempt as u32,&actor).await.map(|o|o.state())
            }
            Ok(foundry_exec::ReviewRunOutcome::Completed(_)) => Err(LineError::Port(order_id.to_owned(),foundry_domain::ports::PortError::Permanent("review returned no passing verdict".into()))),
            Err(error) => self.commit(order_id,OrderEvent::InspectionFailed{reason:format!("review failed: {error:?}")},attempt as u32,&actor).await.map(|o|o.state()),
        }
    }

    pub async fn resolve_review_hold(&self,order_id:&str,hold_id:&str,action:foundry_exec::ReviewHoldAction,actor:&str)->Result<OrderState,LineError>{
        if self.get(order_id).await.is_none_or(|o|o.state()!=OrderState::FurnaceHold){return Err(LineError::Port(order_id.to_owned(),foundry_domain::ports::PortError::Permanent("order is not waiting on review".into())));}
        {
            let mut port=self.review_run.lock().unwrap();
            port.as_mut().ok_or_else(||LineError::Port(order_id.to_owned(),foundry_domain::ports::PortError::Permanent("review runner unavailable".into())))?
                .resolve_hold(order_id,hold_id,action,actor).map_err(|e|LineError::Port(order_id.to_owned(),foundry_domain::ports::PortError::Permanent(format!("review hold resolution: {e:?}"))))?;
        }
        self.resume_review(order_id).await
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
        if !self.has_review_tuple(&order)? {
            return Err(LineError::Port(order_id.to_string(), foundry_domain::ports::PortError::Permanent("review evidence required".into())));
        }
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
    pub async fn publish(&self, order_id: &str, actor: &ActorId) -> Result<PublishReceipt, LineError> {
        let order = self
            .get(order_id)
            .await
            .ok_or_else(|| LineError::UnknownOrder(order_id.to_string()))?;
        let attempt = order.attempt();
        let operation_id = format!("{order_id}:{attempt}");
        let digest = order.artifact_digest.clone().unwrap_or_default();
        if !self.has_review_tuple(&order)? {
            return Err(LineError::Port(order_id.to_string(), foundry_domain::ports::PortError::Permanent("review evidence required".into())));
        }

        let mut last_pr_url: Option<String> = None;
        match self.publisher.publish(&order, &operation_id, &digest).await {
            Ok(PublishOutcome::Confirmed { pr_url }) => {
                self.commit(
                    order_id,
                    OrderEvent::PublishConfirmed {
                        pr_url: pr_url.clone(),
                    },
                    attempt,
                    actor,
                )
                .await?;
                last_pr_url = Some(pr_url);
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
            .map(|o| PublishReceipt {
                state: o.state(),
                pr_url: last_pr_url,
            })
            .ok_or_else(|| LineError::UnknownOrder(order_id.to_string()))
    }
}
