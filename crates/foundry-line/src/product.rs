//! ProductAtomicV1 lifecycle projection. S1 remains authoritative; this
//! projection is replaced only from a verified committed snapshot.
use foundry_domain::ports::{PortError, PortResult, PublishOutcome, Publisher};
use foundry_domain::{ExternalRef, Order, OrderEvent, OrderState};
use foundry_ledger::{
    Mirror, MirrorState, ProductApprovalReceipt, ProductPublishAuthorization, ProductPublishStart,
    ProductStore,
};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProductPublishReconcileOutcome {
    Confirmed { pr_url: String },
    /// The adapter found the operation's PR, but it is closed without merge.
    /// This is not an accepted external effect and never authorizes a retry.
    Rejected { pr_url: String },
    NotFound,
    Unknown,
}

/// Result of the durable product publish composition. The shared
/// `PublishOutcome` port remains unchanged; rejection is an explicit business
/// result because a closed, unmerged PR requires a new human approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProductPublishResult {
    Published(PublishOutcome),
    Rejected { pr_url: String },
}

impl PartialEq<PublishOutcome> for ProductPublishResult {
    fn eq(&self, other: &PublishOutcome) -> bool {
        matches!(self, Self::Published(outcome) if outcome == other)
    }
}

impl From<PublishOutcome> for ProductPublishResult {
    fn from(outcome: PublishOutcome) -> Self {
        Self::Published(outcome)
    }
}

/// Narrow local adapter seam for looking up an operation after a response
/// loss. It does not change the shared Publisher port contract.
pub trait ProductPublishReconciler {
    fn reconcile(
        &self,
        operation_id: &str,
    ) -> impl std::future::Future<Output = PortResult<ProductPublishReconcileOutcome>> + Send;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductProjectionAnchor {
    pub sequence: i64,
    pub event_hash: String,
}

#[derive(Default)]
pub struct ProductProjection {
    orders: HashMap<String, Order>,
    anchor: Option<ProductProjectionAnchor>,
}

impl ProductProjection {
    pub fn get(&self, order_id: &str) -> Option<&Order> {
        self.orders.get(order_id)
    }

    pub fn anchor(&self) -> Option<&ProductProjectionAnchor> {
        self.anchor.as_ref()
    }

    /// Catch up only from a contiguous, hash-matching mirror prefix. The
    /// snapshot and replay come from ProductStore's verified S1 transaction.
    pub fn catch_up(&mut self, store: &mut ProductStore) -> Result<(), String> {
        let snapshot = store
            .projection_snapshot()
            .map_err(|_| "product snapshot unavailable")?;
        if let Some(anchor) = &self.anchor {
            let row = snapshot
                .rows
                .get((anchor.sequence - 1) as usize)
                .filter(|row| row.seq == anchor.sequence && row.hash == anchor.event_hash)
                .ok_or("existing product projection anchor is absent from S1")?;
            let _ = row;
        }
        let mirror = Mirror::new(store.path());
        let mirror_state = mirror.state(&snapshot.rows);
        let mirror_lines = mirror.lines().map_err(|_| "product mirror unreadable")?;
        let mirrored = mirror_lines
            .last()
            .map(|line| (line.seq, line.hash.as_str()));
        if let Some((seq, hash)) = mirrored {
            let anchor_row = snapshot
                .rows
                .get((seq - 1) as usize)
                .filter(|row| row.seq == seq && row.hash == hash)
                .ok_or("product mirror anchor does not match S1")?;
            let _ = anchor_row;
        }
        match mirror_state {
            MirrorState::Ok { .. } => {}
            MirrorState::Behind { .. } => {
                let from = mirrored.map(|(seq, _)| seq + 1).unwrap_or(1);
                mirror
                    .rebuild(&snapshot.rows, Some(from))
                    .map_err(|_| "product mirror catch-up failed")?;
            }
            _ => return Err("product mirror is corrupt or ahead of S1".into()),
        }
        let mut orders = HashMap::new();
        for lifecycle in snapshot.lifecycles {
            orders.insert(lifecycle.order_id, lifecycle.order);
        }
        self.orders = orders;
        self.anchor = (snapshot.head_sequence > 0).then_some(ProductProjectionAnchor {
            sequence: snapshot.head_sequence,
            event_hash: snapshot.head_hash,
        });
        Ok(())
    }

    /// Apply memory projection only after ProductStore has committed and
    /// returned this receipt. Historical retries are valid even if the live
    /// order has since advanced; catch_up always selects the current lifecycle.
    pub fn apply_committed_approval(
        &mut self,
        store: &mut ProductStore,
        receipt: &ProductApprovalReceipt,
    ) -> Result<(), String> {
        let snapshot = store
            .projection_snapshot()
            .map_err(|_| "product snapshot unavailable")?;
        let row = snapshot
            .rows
            .iter()
            .find(|row| row.seq == receipt.sequence && row.hash == receipt.event_hash)
            .ok_or("approval receipt is not present in committed S1")?;
        if row.order_id != receipt.order_id || row.to_state != OrderState::Publishing.to_string() {
            return Err("approval receipt binding differs from committed S1".into());
        }
        self.catch_up(store)
    }

    /// Gate a separately invoked external publisher. This does not publish or
    /// advance memory state; the caller must retain this private proof and
    /// invoke its Publisher only after the DB revalidation succeeds.
    pub fn authorize_publish(
        &self,
        store: &ProductStore,
        receipt: &ProductApprovalReceipt,
    ) -> Result<ProductPublishAuthorization, String> {
        let current = self
            .orders
            .get(&receipt.order_id)
            .ok_or("product order is absent from committed projection")?;
        if current.state() != OrderState::Publishing {
            return Err("product order is not in Publishing state".into());
        }
        store
            .authorize_product_publish(receipt)
            .map_err(|_| "product approval receipt is not publish-authorized".into())
    }

    /// Invoke the existing Publisher port as a separate network effect after
    /// revalidating the committed ProductAtomicV1 receipt. Approval, retry,
    /// and restore paths never call this method. The Publisher's returned
    /// Confirmed/Unknown result is passed through unchanged; this method does
    /// not claim that a remote effect was atomically committed with SQLite.
    pub async fn publish_approved<P: Publisher>(
        &self,
        store: &ProductStore,
        receipt: &ProductApprovalReceipt,
        publisher: &P,
    ) -> PortResult<PublishOutcome> {
        let authorization = self
            .authorize_publish(store, receipt)
            .map_err(|_| PortError::Permanent("product publish authorization denied".into()))?;
        let order = self
            .orders
            .get(&receipt.order_id)
            .ok_or_else(|| PortError::Permanent("product order unavailable".into()))?;
        let mut bound_order = order.clone();
        bound_order.artifact_digest = Some(authorization.receipt().artifact_digest.clone());
        publisher
            .publish(
                &bound_order,
                &authorization.receipt().operation_id,
                &authorization.receipt().artifact_digest,
            )
            .await
    }

    /// Durable v3 publish composition. A new start is committed by ProductStore
    /// before Publisher is called. Existing pending starts reconcile first;
    /// only a proved-missing external effect may invoke the idempotent publisher
    /// again. Confirmed outcomes are returned from storage without execution.
    pub async fn publish_product_approved<P, R>(
        store: &mut ProductStore,
        session_digest: &str,
        receipt: &ProductApprovalReceipt,
        publisher: &P,
        reconciler: &R,
        now: chrono::DateTime<chrono::Utc>,
    ) -> PortResult<ProductPublishResult>
    where
        P: Publisher,
        R: ProductPublishReconciler,
    {
        // Scoped v3 stores refuse the unscoped projection snapshot, so the
        // durable composition reconstructs its bound Order from the store-
        // verified receipt through the canonical event chain. The journal (S1)
        // remains authoritative; only id/state/attempt/digest are bound here.
        // The reconstruction is deliberately LAZY: it runs only on a Started
        // path, after the durable outcome lookup, so retrieving an already
        // confirmed effect never depends on reconstructing a valid chain
        // (an attempt>=2 receipt whose order advanced must still return its
        // stored URL).
        let start = store
            .begin_product_publish(session_digest, receipt, now)
            .map_err(|_| PortError::Permanent("product publish authorization denied".into()))?;
        match start {
            ProductPublishStart::Confirmed { pr_url } => {
                Ok(ProductPublishResult::Published(PublishOutcome::Confirmed { pr_url }))
            }
            ProductPublishStart::Started => {
                Self::guarded_publish(store, publisher, receipt, now).await
            }
            ProductPublishStart::Reconcile => {
                match reconciler.reconcile(&receipt.operation_id).await? {
                    ProductPublishReconcileOutcome::Confirmed { pr_url } => {
                        store
                            .settle_product_publish(&receipt.operation_id, &pr_url)
                            .map_err(|_| PortError::Permanent("product publish settlement failed".into()))?;
                        Ok(ProductPublishResult::Published(PublishOutcome::Confirmed { pr_url }))
                    }
                    ProductPublishReconcileOutcome::Rejected { pr_url } => {
                        // A closed unmerged PR is neither a confirmed effect
                        // nor proof of absence. Keep the durable start pending
                        // and require a fresh approval before any new effect.
                        Ok(ProductPublishResult::Rejected { pr_url })
                    }
                    ProductPublishReconcileOutcome::Unknown => Ok(PublishOutcome::Unknown {
                        operation_id: receipt.operation_id.clone(),
                    }.into()),
                    ProductPublishReconcileOutcome::NotFound => {
                        // Fence the durable state before any external retry:
                        // a concurrent caller may have confirmed and settled
                        // while this reconciler was in flight, and a retry
                        // without this check would invoke the Publisher after
                        // confirmation (forbidden extra execution).
                        match store
                            .claim_product_publish_retry(receipt)
                            .map_err(|_| {
                                PortError::Permanent(
                                    "product publish retry fence failed".into(),
                                )
                            })? {
                            ProductPublishStart::Confirmed { pr_url } => {
                                Ok(ProductPublishResult::Published(PublishOutcome::Confirmed { pr_url }))
                            }
                            ProductPublishStart::Started => {
                                Self::guarded_publish(store, publisher, receipt, now).await
                            }
                            // claim_product_publish_retry never returns
                            // Reconcile; treat it defensively as an Unknown
                            // (pending is preserved for the next pass).
                            ProductPublishStart::Reconcile => {
                                Ok(ProductPublishResult::Published(PublishOutcome::Unknown {
                                    operation_id: receipt.operation_id.clone(),
                                }))
                            }
                        }
                    }
                }
            }
        }
    }

    /// Durable recheck immediately before ANY Publisher invocation on a
    /// Started path. `claim_product_publish_retry` re-reads the committed
    /// row inside an IMMEDIATE transaction: if a concurrent caller settled
    /// the operation between the fence and this call, the stored Confirmed
    /// URL is returned and the Publisher is NOT contacted (a confirmed
    /// effect is never re-executed). Only a still-pending row with the
    /// exact bound receipt proceeds to the (idempotent, operation-keyed)
    /// Publisher call.
    async fn guarded_publish<P: Publisher>(
        store: &mut ProductStore,
        publisher: &P,
        receipt: &ProductApprovalReceipt,
        now: chrono::DateTime<chrono::Utc>,
    ) -> PortResult<ProductPublishResult> {
        match store
            .claim_product_publish_retry(receipt)
            .map_err(|_| PortError::Permanent("product publish retry fence failed".into()))? {
            ProductPublishStart::Confirmed { pr_url } => Ok(ProductPublishResult::Published(PublishOutcome::Confirmed { pr_url })),
            ProductPublishStart::Started => {
                let bound_order = Self::bound_publishing_order(receipt, now)?;
                Self::call_product_publisher(store, publisher, &bound_order, receipt)
                    .await
                    .map(ProductPublishResult::Published)
            }
            ProductPublishStart::Reconcile => Ok(ProductPublishResult::Published(PublishOutcome::Unknown {
                operation_id: receipt.operation_id.clone(),
            })),
        }
    }

    /// Reconstruct the publisher-bound Order from a store-verified receipt.
    /// Scoped policy makes the unscoped projection snapshot unavailable, and
    /// `begin_product_publish` has already proven the publishing lifecycle and
    /// digest binding in the same transaction that recorded the durable start.
    fn bound_publishing_order(
        receipt: &ProductApprovalReceipt,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Order, PortError> {
        let mut order = Order::new(
            receipt.order_id.clone(),
            ExternalRef::intake(&receipt.order_id, &receipt.order_id),
            now,
        );
        let attempt = u32::try_from(receipt.attempt).unwrap_or(1).max(1);
        // Replay the canonical retry chain: a fresh order claims attempt 1,
        // and each DieFailed advances the attempt before the next DieStarted
        // (DieStarted must claim the CURRENT attempt number — starting a
        // fresh order directly at attempt N is illegal). An attempt>=2
        // approval receipt is therefore reconstructed through its genuine
        // failed-then-retried path.
        order
            .transition(OrderEvent::Queued, 0)
            .and_then(|_| order.transition(OrderEvent::DieStarted, 1))
            .map_err(|_| PortError::Permanent("bound publish order reconstruction failed".into()))?;
        for current in 2..=attempt {
            order
                .transition(
                    OrderEvent::DieFailed {
                        attempt: current - 1,
                        reason: "publish-order-reconstruction-retry".into(),
                    },
                    current - 1,
                )
                .and_then(|_| order.transition(OrderEvent::DieStarted, current))
                .map_err(|_| {
                    PortError::Permanent("bound publish order reconstruction failed".into())
                })?;
        }
        order
            .transition(OrderEvent::DieSucceeded { attempt }, attempt)
            .and_then(|_| order.transition(OrderEvent::InspectionPassed, attempt))
            .and_then(|_| {
                order.transition(
                    OrderEvent::Approved {
                        intent: receipt.operation_id.clone(),
                        artifact_digest: receipt.artifact_digest.clone(),
                    },
                    attempt,
                )
            })
            .map_err(|_| PortError::Permanent("bound publish order reconstruction failed".into()))?;
        Ok(order)
    }

    async fn call_product_publisher<P: Publisher>(
        store: &mut ProductStore,
        publisher: &P,
        order: &Order,
        receipt: &ProductApprovalReceipt,
    ) -> PortResult<PublishOutcome> {
        match publisher
            .publish(order, &receipt.operation_id, &receipt.artifact_digest)
            .await?
        {
            PublishOutcome::Confirmed { pr_url } => {
                store
                    .settle_product_publish(&receipt.operation_id, &pr_url)
                    .map_err(|_| PortError::Permanent("product publish settlement failed".into()))?;
                Ok(PublishOutcome::Confirmed { pr_url })
            }
            unknown @ PublishOutcome::Unknown { .. } => Ok(unknown),
        }
    }
}

#[cfg(test)]
mod publish_reconstruction_tests {
    use super::*;
    use chrono::TimeZone;

    fn receipt_with_attempt(attempt: i64) -> ProductApprovalReceipt {
        ProductApprovalReceipt {
            operation_id: format!("reconstruction-op-{attempt}"),
            actor_id: "reader".into(),
            order_id: format!("reconstruction-order-{attempt}"),
            lifecycle: 2,
            sequence: 6,
            event_hash: "a".repeat(64),
            artifact_digest: "b".repeat(40),
            attempt,
            run_id: 7,
            generation: 0,
            tuple_digest: "c".repeat(64),
            source_revision: 1,
        }
    }

    #[test]
    fn bound_publishing_order_accepts_attempt_1_through_3() {
        let now = chrono::Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        for attempt in 1..=3 {
            let order =
                ProductProjection::bound_publishing_order(&receipt_with_attempt(attempt), now)
                    .unwrap_or_else(|e| panic!("attempt {attempt} must reconstruct: {e:?}"));
            assert_eq!(
                order.state(),
                foundry_domain::OrderState::Publishing,
                "attempt {attempt}"
            );
            assert_eq!(order.attempt(), attempt as u32, "attempt {attempt}");
        }
    }
}
