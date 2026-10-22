//! Transaction participants for ProductAtomicV1. ProductStore owns the
//! transaction; these functions never commit and validate the S1 anchor.
use rusqlite::{OptionalExtension, Transaction, params};

#[derive(Debug, Clone)]
pub struct ProductGrantBinding {
    pub grant_id: String,
    pub token_digest: String,
    pub actor_id: String,
    pub order_id: String,
    pub operation_id: String,
}

#[derive(Debug, Clone)]
pub struct ProductReservation {
    binding: ProductGrantBinding,
    fence: i64,
}
impl ProductReservation {
    pub fn fence(&self) -> i64 {
        self.fence
    }
}

#[derive(Debug, Clone)]
pub struct ProductApprovalAnchor {
    pub sequence: i64,
    pub event_hash: String,
    pub lifecycle: i64,
    pub artifact_digest: String,
}

pub fn issue_product_grant_in(
    tx: &Transaction<'_>,
    binding: &ProductGrantBinding,
) -> Result<(), String> {
    tx.execute("INSERT INTO product_grants_v1(grant_id,token_digest,actor_id,order_id,state,fence) VALUES(?1,?2,?3,?4,'issued',0)",
        params![binding.grant_id,binding.token_digest,binding.actor_id,binding.order_id]).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn prepare_product_in(
    tx: &Transaction<'_>,
    binding: ProductGrantBinding,
) -> Result<ProductReservation, String> {
    let owner: Option<(String,String,String)> = tx.query_row(
        "SELECT token_digest,actor_id,order_id FROM product_grants_v1 WHERE grant_id=?1 AND state='issued'",
        [&binding.grant_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).optional().map_err(|e| e.to_string())?;
    let (token, actor, order) = owner.ok_or_else(|| "product grant unavailable".to_string())?;
    if token != binding.token_digest || actor != binding.actor_id || order != binding.order_id {
        return Err("product grant binding mismatch".into());
    }
    let fence: i64 = tx
        .query_row(
            "UPDATE product_fence_counter_v1 SET next=next+1 WHERE singleton=1 RETURNING next",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let n = tx.execute("UPDATE product_grants_v1 SET state='reserved',fence=?1 WHERE grant_id=?2 AND state='issued'", params![fence,binding.grant_id]).map_err(|e| e.to_string())?;
    if n != 1 {
        return Err("product grant reservation raced".into());
    }
    Ok(ProductReservation { binding, fence })
}

pub fn finalize_product_in(
    tx: &Transaction<'_>,
    reservation: &ProductReservation,
    anchor: &ProductApprovalAnchor,
) -> Result<(), String> {
    validate_approval_anchor_in(tx, &reservation.binding, anchor)?;
    consume_product_grant_fenced_in(tx, &reservation.binding, reservation.fence)?;
    consume_product_token_in(tx, &reservation.binding, anchor)
}

/// Validate the S1 transition and immutable approval artifact before any
/// single-use participant state can be consumed.
pub fn validate_approval_anchor_in(
    tx: &Transaction<'_>,
    binding: &ProductGrantBinding,
    anchor: &ProductApprovalAnchor,
) -> Result<(), String> {
    let actual: Option<(String,String,String,String,String,String)> = tx.query_row(
        "SELECT l.order_id,l.hash,r.operation,l.from_state,l.to_state,l.event_json FROM ledger l JOIN product_transition_receipts_v1 r ON r.seq=l.seq AND r.event_hash=l.hash WHERE l.seq=?1 AND l.hash=?2 AND r.lifecycle=?3",
        params![anchor.sequence,anchor.event_hash,anchor.lifecycle], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
    ).optional().map_err(|e| e.to_string())?;
    match actual {
        Some((order, hash, operation, from_state, to_state, event_json))
            if order == binding.order_id
                && hash == anchor.event_hash
                && operation == "human-approval"
                && from_state == "waiting"
                && to_state == "publishing"
                && serde_json::from_str::<serde_json::Value>(&event_json)
                    .ok()
                    .is_some_and(|event| {
                        event.get("type").and_then(|v| v.as_str()) == Some("approved")
                            && event.get("artifact_digest").and_then(|v| v.as_str())
                                == Some(anchor.artifact_digest.as_str())
                    }) => {}
        _ => return Err("approval S1 anchor is absent or mismatched".into()),
    }
    Ok(())
}

/// Consume the exact fenced grant after its S1 anchor has been validated.
pub fn consume_product_grant_fenced_in(
    tx: &Transaction<'_>,
    binding: &ProductGrantBinding,
    fence: i64,
) -> Result<(), String> {
    let n = tx.execute("UPDATE product_grants_v1 SET state='consumed' WHERE grant_id=?1 AND state='reserved' AND fence=?2 AND actor_id=?3 AND order_id=?4",
        params![binding.grant_id,fence,binding.actor_id,binding.order_id]).map_err(|e| e.to_string())?;
    if n != 1 {
        return Err("product reservation fence is stale".into());
    }
    Ok(())
}

/// Consume the exact one-time token for the already validated transition.
pub fn consume_product_token_in(
    tx: &Transaction<'_>,
    binding: &ProductGrantBinding,
    anchor: &ProductApprovalAnchor,
) -> Result<(), String> {
    tx.execute(
        "INSERT INTO product_token_consumptions_v1(token_digest,operation_id,seq) VALUES(?1,?2,?3)",
        params![binding.token_digest, binding.operation_id, anchor.sequence],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Ledger-side completion by persisted reservation fence. The reservation
/// proof remains constructible only inside this participant module.
pub fn finalize_product_fenced_in(
    tx: &Transaction<'_>,
    binding: ProductGrantBinding,
    fence: i64,
    anchor: &ProductApprovalAnchor,
) -> Result<(), String> {
    finalize_product_in(tx, &ProductReservation { binding, fence }, anchor)
}
