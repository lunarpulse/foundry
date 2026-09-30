//! Order — the work unit (plan §2, state machine §9 v3).
use chrono::{DateTime, Utc};
use std::fmt;
use std::str::FromStr;

/// Multi-tenant seam (plan §3). M0 = constant "local"; field exists from day 0.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TenantId(pub String);

impl TenantId {
    pub const LOCAL: &'static str = "local";
}

/// Where an Order came from. dedup_key gives effectively-once intake
/// (at-least-once delivery + dedup), and reverse traceability (plan §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalRef {
    pub source: Source,
    pub external_id: String,
    pub dedup_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Cron,
    GitHub,
    Jira,
}

impl ExternalRef {
    pub fn cron(job: &str, target: &str) -> Self {
        Self {
            source: Source::Cron,
            external_id: format!("{job}/{target}"),
            dedup_key: format!("cron:{job}:{target}:{}", Utc::now().format("%Y-%m-%d")),
        }
    }
}

/// Kanban column == OrderState (1:1, plan §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderState {
    Received,
    Queued,
    Working,
    Inspecting,
    FurnaceHold,
    Publishing,
    Unknown,
    Shipped,
    Failed,
    Rejected,
    Canceling,
    Canceled,
}

impl OrderState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            OrderState::Shipped
                | OrderState::Failed
                | OrderState::Rejected
                | OrderState::Canceled
        )
    }
}

impl fmt::Display for OrderState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            OrderState::Received => "received",
            OrderState::Queued => "queued",
            OrderState::Working => "working",
            OrderState::Inspecting => "review",
            OrderState::FurnaceHold => "waiting",
            OrderState::Publishing => "publishing",
            OrderState::Unknown => "unknown",
            OrderState::Shipped => "shipped",
            OrderState::Failed => "failed",
            OrderState::Rejected => "rejected",
            OrderState::Canceling => "canceling",
            OrderState::Canceled => "canceled",
        };
        f.write_str(s)
    }
}

impl FromStr for OrderState {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "received" => OrderState::Received,
            "queued" => OrderState::Queued,
            "working" => OrderState::Working,
            "review" => OrderState::Inspecting,
            "waiting" => OrderState::FurnaceHold,
            "publishing" => OrderState::Publishing,
            "unknown" => OrderState::Unknown,
            "shipped" => OrderState::Shipped,
            "failed" => OrderState::Failed,
            "rejected" => OrderState::Rejected,
            "canceling" => OrderState::Canceling,
            "canceled" => OrderState::Canceled,
            other => return Err(format!("unknown state: {other}")),
        })
    }
}

/// Events drive transitions (runtime-validated table + store-level conditional
/// UPDATE for atomicity — plan §9 v3: "compile-time enforcement" claim retracted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderEvent {
    Queued,
    DieStarted,
    /// attempt numbers fence stale results (critique #3).
    DieSucceeded { attempt: u32 },
    DieFailed { attempt: u32, reason: String },
    InspectionPassed,
    InspectionFailed { reason: String },
    /// Approval binds intent + artifact digest (plan §11: re-verified pre-publish).
    Approved { intent: String, artifact_digest: String },
    Rejected { intent: String },
    /// Publisher committed the PR — external confirmation.
    PublishConfirmed { pr_url: String },
    /// Publisher crashed before response — outcome unknown, reconcile re-judges.
    PublishUnknown { operation_id: String },
    CancelRequested { reason: String },
    CancelConfirmed,
    /// Direct cancel — legal only from Received/Queued.
    Canceled { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionError(pub String);

impl fmt::Display for TransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "illegal transition: {}", self.0)
    }
}

pub const MAX_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone)]
pub struct Order {
    pub id: String,
    pub tenant: TenantId,
    pub external: ExternalRef,
    state: OrderState,
    attempt: u32,
    pub created_at: DateTime<Utc>,
    /// Bumped on every persisted transition — store does
    /// `UPDATE ... WHERE state=? AND version=?` for atomicity (plan §9).
    pub version: u64,
    /// Bound at approval time; re-checked by Publisher before PR creation (§11).
    pub artifact_digest: Option<String>,
}

impl Order {
    pub fn new(id: String, external: ExternalRef, created_at: DateTime<Utc>) -> Self {
        Self {
            id,
            tenant: TenantId(TenantId::LOCAL.into()),
            external,
            state: OrderState::Received,
            attempt: 0,
            created_at,
            version: 0,
            artifact_digest: None,
        }
    }

    pub fn state(&self) -> OrderState {
        self.state
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// Apply an event. Returns Err and leaves state untouched on any illegal
    /// transition. `event_attempt` is the attempt number the caller believes
    /// this Order is on (0 for lifecycle events before first Die).
    pub fn transition(
        &mut self,
        event: OrderEvent,
        event_attempt: u32,
    ) -> Result<OrderState, TransitionError> {
        let next = match (&event, self.state) {
            // ---- happy path ----
            (OrderEvent::Queued, OrderState::Received) => OrderState::Queued,
            (OrderEvent::DieStarted, OrderState::Queued) => {
                // First run: caller claims attempt 1. Retry: attempt was already
                // advanced by DieFailed, caller re-claims the current number.
                if event_attempt != self.attempt.max(1) {
                    return Err(TransitionError(format!(
                        "DieStarted attempt {event_attempt} != expected {}",
                        self.attempt.max(1)
                    )));
                }
                self.attempt = event_attempt;
                OrderState::Working
            }
            (OrderEvent::DieSucceeded { attempt }, OrderState::Working) => {
                if *attempt != self.attempt {
                    return Err(TransitionError(format!(
                        "stale DieSucceeded attempt {attempt} != current {}",
                        self.attempt
                    )));
                }
                OrderState::Inspecting
            }
            (OrderEvent::DieFailed { attempt, .. }, OrderState::Working) => {
                if *attempt != self.attempt {
                    return Err(TransitionError(format!(
                        "stale DieFailed attempt {attempt} != current {}",
                        self.attempt
                    )));
                }
                if self.attempt >= MAX_ATTEMPTS {
                    OrderState::Failed
                } else {
                    // next DieStarted must claim attempt+1 (checked there)
                    self.attempt += 1;
                    OrderState::Queued
                }
            }
            (OrderEvent::InspectionPassed, OrderState::Inspecting) => OrderState::FurnaceHold,
            (OrderEvent::InspectionFailed { .. }, OrderState::Inspecting) => {
                // inspector rejection is a retryable failure like DieFailed
                if self.attempt >= MAX_ATTEMPTS {
                    OrderState::Failed
                } else {
                    self.attempt += 1;
                    OrderState::Queued
                }
            }
            (OrderEvent::Approved { intent: _, artifact_digest }, OrderState::FurnaceHold) => {
                // approval MUST bind provenance (§11)
                if artifact_digest.is_empty() {
                    return Err(TransitionError(
                        "approval requires non-empty artifact_digest".into(),
                    ));
                }
                self.artifact_digest = Some(artifact_digest.clone());
                OrderState::Publishing
            }
            (OrderEvent::Rejected { intent: _ }, OrderState::FurnaceHold) => OrderState::Rejected,
            // approval → Shipped directly is FORBIDDEN (v3 critique #1)
            (OrderEvent::PublishConfirmed { pr_url: _ }, OrderState::Publishing) => {
                OrderState::Shipped
            }
            (OrderEvent::PublishUnknown { operation_id: _ }, OrderState::Publishing) => {
                OrderState::Unknown
            }
            (OrderEvent::PublishConfirmed { pr_url: _ }, OrderState::Unknown) => OrderState::Shipped,
            // reconcile found nothing — retire the order (operator decides)
            (OrderEvent::Rejected { intent: _ }, OrderState::Unknown) => OrderState::Rejected,
            // ---- cancellation ----
            (OrderEvent::Canceled { reason: _ }, OrderState::Received)
            | (OrderEvent::Canceled { reason: _ }, OrderState::Queued) => OrderState::Canceled,
            (OrderEvent::CancelRequested { reason: _ }, OrderState::Working)
            | (OrderEvent::CancelRequested { reason: _ }, OrderState::Publishing)
            | (OrderEvent::CancelRequested { reason: _ }, OrderState::Unknown) => {
                OrderState::Canceling
            }
            (OrderEvent::CancelConfirmed, OrderState::Canceling) => OrderState::Canceled,
            (ev, s) => {
                return Err(TransitionError(format!(
                    "{s:?} --{ev:?}--> (no legal target)"
                )));
            }
        };
        self.state = next;
        self.version += 1;
        Ok(next)
    }
}
