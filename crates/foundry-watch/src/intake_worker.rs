//! Synchronous durable delivery consumer. Call this whole function from a
//! blocking worker: both the connector acknowledgement and ProductStore use
//! blocking I/O, and the owned client must also be dropped there.

#![allow(clippy::question_mark)] // W6a clippy debt: preserve the explicit early-return path

use chrono::{DateTime, Utc};
use foundry_ledger::{AdmissionReceipt, ProductDelivery, ProductError, ProductStore};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AckError {
    #[error("retryable connector acknowledgement failure")]
    Retryable,
    #[error("permanent connector acknowledgement failure")]
    Permanent,
}

/// Connector-specific acknowledgement. Implementations must acknowledge only
/// the exact stable delivery represented by the argument.
pub trait DeliveryAcknowledger {
    fn acknowledge(&mut self, delivery: &ProductDelivery) -> Result<(), AckError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntakeDrainReport {
    pub examined: usize,
    pub admitted: usize,
    pub acknowledged: usize,
    pub retryable_failures: usize,
    pub permanent_failures: usize,
    pub receipts: Vec<AdmissionReceipt>,
    /// Durable claim outcome. `Unsupported` leaves the backlog untouched;
    /// `Claimed` requires a configured adapter to validate before execution.
    pub work_claim: foundry_ledger::ProductWorkClaimResult,
}

#[derive(Debug, Error)]
pub enum IntakeDrainError {
    #[error(transparent)]
    Product(#[from] ProductError),
}

/// One real ProductAtomicV1 intake tick: bounded page fetch+cursor commit,
/// durable admission, then local acknowledgement of the exact persisted
/// admission receipt. Intake is GET-only; GitHub is never mutated. Invoke the
/// entire function on a blocking worker so all reqwest clients are dropped
/// outside Tokio's runtime.
pub fn product_github_tick(
    store: &mut ProductStore,
    config: &crate::intake::ProductGithubConfig,
    secrets: &dyn foundry_domain::ports::SecretProvider,
    now: DateTime<Utc>,
    admission_limit: usize,
    service: &foundry_ledger::ProductServiceContext,
) -> Result<(crate::intake::ProductPollReport, IntakeDrainReport), ProductGithubTickError> {
    product_github_tick_with_clock(
        store,
        config,
        secrets,
        now,
        admission_limit,
        service,
        &crate::intake::SystemProductClock,
    )
}

pub fn product_github_tick_with_clock(
    store: &mut ProductStore,
    config: &crate::intake::ProductGithubConfig,
    secrets: &dyn foundry_domain::ports::SecretProvider,
    _compatibility_now: DateTime<Utc>,
    admission_limit: usize,
    service: &foundry_ledger::ProductServiceContext,
    clock: &dyn crate::intake::ProductClock,
) -> Result<(crate::intake::ProductPollReport, IntakeDrainReport), ProductGithubTickError> {
    let poll = crate::intake::poll_product_github_page_batch_for_service_with_clock(
        store, config, secrets, service, clock,
    );
    let mut acknowledger = LocalAdmissionAcknowledger;
    let mut drain = drain_admissions_for_service_with_clock(
        store,
        &mut acknowledger,
        service,
        admission_limit,
        clock,
    )?;
    // Claiming is independent of transport success: a prior committed page
    // may have left durable admissions ready while this poll is rate limited.
    drain.work_claim = store.claim_product_work_for_service(
        service,
        &format!("watch-{}", std::process::id()),
        clock.now(),
        chrono::Duration::minutes(5),
    )?;
    Ok((poll?, drain))
}

pub fn drain_admissions_for_service<A: DeliveryAcknowledger>(
    store: &mut ProductStore,
    acknowledger: &mut A,
    service: &foundry_ledger::ProductServiceContext,
    now: DateTime<Utc>,
    limit: usize,
) -> Result<IntakeDrainReport, IntakeDrainError> {
    drain_admissions_for_service_with_clock(
        store,
        acknowledger,
        service,
        limit,
        &FixedDrainClock(now),
    )
}

struct FixedDrainClock(DateTime<Utc>);
impl crate::intake::ProductClock for FixedDrainClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

pub fn drain_admissions_for_service_with_clock<A: DeliveryAcknowledger>(
    store: &mut ProductStore,
    acknowledger: &mut A,
    service: &foundry_ledger::ProductServiceContext,
    limit: usize,
    clock: &dyn crate::intake::ProductClock,
) -> Result<IntakeDrainReport, IntakeDrainError> {
    let due = store.due_intake_deliveries_for_service(service, clock.now(), limit)?;
    let mut report = IntakeDrainReport {
        examined: due.len(),
        admitted: 0,
        acknowledged: 0,
        retryable_failures: 0,
        permanent_failures: 0,
        receipts: Vec::new(),
        work_claim: foundry_ledger::ProductWorkClaimResult::Empty,
    };
    for item in due {
        let delivery = item.delivery;
        let receipt = store.admit_intake_delivery_for_service(
            service,
            &delivery.delivery_id,
            &product_order_id(&delivery),
            clock.now(),
        )?;
        report.admitted += 1;
        report.receipts.push(receipt.clone());
        match acknowledger.acknowledge(&delivery) {
            Ok(()) => {
                store.mark_intake_acked_for_service(
                    service,
                    &delivery.delivery_id,
                    &receipt,
                    clock.now(),
                )?;
                report.acknowledged += 1;
            }
            Err(AckError::Retryable) => {
                let now = clock.now();
                store.defer_intake_delivery_for_service(
                    service,
                    &delivery.delivery_id,
                    &receipt,
                    now + chrono::Duration::seconds(30),
                    now,
                )?;
                report.retryable_failures += 1;
            }
            Err(AckError::Permanent) => {
                store.reject_intake_delivery_for_service(
                    service,
                    &delivery.delivery_id,
                    &receipt,
                    clock.now(),
                )?;
                report.permanent_failures += 1;
            }
        }
    }
    Ok(report)
}

#[derive(Debug, Error)]
pub enum ProductGithubTickError {
    #[error(transparent)]
    Poll(#[from] crate::intake::ProductPollError),
    #[error(transparent)]
    Drain(#[from] IntakeDrainError),
    #[error(transparent)]
    Work(#[from] ProductError),
}

/// ProductAtomicV1 acknowledgement is local. The inbox remains the durable
/// source of redelivery; no remote label removal or other GitHub mutation is
/// part of intake.
struct LocalAdmissionAcknowledger;

impl DeliveryAcknowledger for LocalAdmissionAcknowledger {
    fn acknowledge(&mut self, delivery: &ProductDelivery) -> Result<(), AckError> {
        if delivery.delivery_id.is_empty()
            || delivery.connector_id.is_empty()
            || delivery.repository_id.is_empty()
            || delivery.issue_id.is_empty()
            || delivery.version_digest.is_empty()
        {
            return Err(AckError::Permanent);
        }
        Ok(())
    }
}

/// Admit due deliveries and ack only after the exact S1 receipt is durable.
/// An ack failure leaves the inbox in `admitted`, which is returned by the
/// next due query and maps to the same queued lifecycle and receipt.
pub fn drain_admissions<A: DeliveryAcknowledger>(
    store: &mut ProductStore,
    acknowledger: &mut A,
    now: DateTime<Utc>,
    limit: usize,
) -> Result<IntakeDrainReport, IntakeDrainError> {
    let due = store.due_intake_deliveries(now, limit)?;
    let mut report = IntakeDrainReport {
        examined: due.len(),
        admitted: 0,
        acknowledged: 0,
        retryable_failures: 0,
        permanent_failures: 0,
        receipts: Vec::new(),
        work_claim: foundry_ledger::ProductWorkClaimResult::Empty,
    };
    for due_item in due {
        let delivery = due_item.delivery;
        let order_id = product_order_id(&delivery);
        let receipt = match store.admit_intake_delivery(&delivery.delivery_id, &order_id, now) {
            Ok(receipt) => receipt,
            Err(error) => return Err(error.into()),
        };
        report.admitted += 1;
        report.receipts.push(receipt.clone());
        match acknowledger.acknowledge(&delivery) {
            Ok(()) => {
                store.mark_intake_acked(&delivery.delivery_id, &receipt)?;
                report.acknowledged += 1;
            }
            Err(AckError::Retryable) => {
                store.defer_intake_delivery(
                    &delivery.delivery_id,
                    &receipt,
                    now + chrono::Duration::seconds(30),
                )?;
                report.retryable_failures += 1;
            }
            Err(AckError::Permanent) => {
                store.reject_intake_delivery(&delivery.delivery_id, &receipt)?;
                report.permanent_failures += 1;
            }
        }
    }
    Ok(report)
}

/// Stable opaque order key derived from configured source identity. It does
/// not contain issue titles, body text, or caller-controlled order IDs.
fn product_order_id(delivery: &ProductDelivery) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"foundry-product-order-v1\0");
    for value in [
        &delivery.connector_id,
        &delivery.repository_id,
        &delivery.issue_id,
    ] {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    format!("p-{}", hex::encode(hash.finalize()))
}
