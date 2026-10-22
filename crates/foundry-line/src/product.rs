//! ProductAtomicV1 lifecycle projection. S1 remains authoritative; this
//! projection is replaced only from a verified committed snapshot.
use foundry_domain::ports::{PortError, PortResult, PublishOutcome, Publisher};
use foundry_domain::{Order, OrderState};
use foundry_ledger::{
    Mirror, MirrorState, ProductApprovalReceipt, ProductPublishAuthorization, ProductStore,
};
use std::collections::HashMap;

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
}
