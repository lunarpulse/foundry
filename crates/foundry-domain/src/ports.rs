//! Ports — the seams (plan §3). Everything I/O crosses lives here as a trait.
//! Enterprise adapters land in M4+; M0 ships the smallest honest implementation.
use crate::order::{ExternalRef, Order, OrderEvent, OrderState};
use chrono::{DateTime, Utc};

/// Unified port error contract (plan §3 v3): callers distinguish retryable
/// failures from permanent ones — the outbox executor decides policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortError {
    /// Transient (network, lock contention) — retry with backoff.
    Retryable(String),
    /// Permanent (bad request, authz denial) — do not retry.
    Permanent(String),
}

pub type PortResult<T> = Result<T, PortError>;

/// Where Orders come from. M0: CronIntake (+ GitHub-label polling).
/// M4 enterprise adapters: Jira, Slack, Teams.
pub trait Intake {
    /// Fetch pending drafts. Delivery is at-least-once; `dedup_key` on
    /// ExternalRef gives effectively-once when combined with durable storage.
    fn poll(&mut self) -> PortResult<Vec<OrderDraft>>;
    /// Ack only AFTER the Order is durably persisted (plan §3).
    fn ack(&mut self, draft_id: &str) -> PortResult<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderDraft {
    pub draft_id: String,
    pub external: ExternalRef,
    pub task_kind: String, // "dep-bump" for M0
    pub payload: String,   // opaque to domain; blueprint interprets
}

/// Status flows back to the source (plan §3). M0: noop. M4: GitHub label
/// transitions, Jira transitions.
pub trait StatusSink {
    fn report(&mut self, external: &ExternalRef, state: OrderState) -> PortResult<()>;
}

/// Notifications out (plan §3). M0: log. M1: Discord webhook.
pub trait Notifier {
    fn notify(&mut self, event: &str, order_id: &str) -> PortResult<()>;
}

/// Who acts. M0: single local actor. M4: SSO/OIDC + non-human agent IDs.
/// NOTE (critique): an ActorId is identification, NOT authentication —
/// authentication lives in the M4 Identity adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorId {
    pub kind: ActorKind,
    pub source: String,
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorKind {
    Human,
    Agent,
}

/// Authorization decisions carry a reason (critique: bare bool is dishonest).
/// M0: always Allow for the local owner — never exposed over the network.
pub trait Policy {
    fn authorize(&self, actor: &ActorId, action: &str, resource: &str) -> Decision;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow { reason: String },
    Deny { reason: String },
}

/// Secrets resolve just-in-time. M0: env vars, allowlist env for Die
/// subprocess (plan §11); publish credentials NEVER reach the Die —
/// the Publisher owns them exclusively.
pub trait SecretProvider {
    fn resolve(&self, name: &str) -> PortResult<String>;
}

/// Runs a Die (plan §2): one worker execution. M0: Rustain CLI subprocess.
/// M5: Docker/K8s SandboxRunner — just another Harness impl.
pub trait Harness {
    fn run(
        &self,
        order: &Order,
        attempt: u32,
        workdir: &std::path::Path,
    ) -> impl Future<Output = PortResult<DieOutput>> + Send;
}

/// Structured Die result (critique: not just exit code) — provenance for
/// Inspector and Publisher (plan §1 [MAJOR] finding fix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DieOutput {
    pub attempt: u32,
    pub base_commit: String,   // repo commit the Die started from
    pub result_commit: String, // commit the Die produced ("" on failure)
    pub log_path: String,
    pub exit_code: i32,
}

/// Exclusive owner of publish credentials (plan §11): only this port may
/// create PRs. The Die never sees a token. Approval re-verification
/// (artifact_digest match) happens in the line executor BEFORE calling here.
pub trait Publisher {
    /// operation_id makes this idempotent — same id, same PR (critique #2).
    fn publish(
        &self,
        order: &Order,
        operation_id: &str,
        expected_digest: &str,
    ) -> impl Future<Output = PortResult<PublishOutcome>> + Send;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishOutcome {
    Confirmed { pr_url: String },
    /// Network died mid-call — remote state unknown; reconcile queries GitHub.
    Unknown { operation_id: String },
}

/// The approval gate (plan §2). M0: human-only. Records intent — approval
/// without recorded intent is not an approval (MAOS I4 lineage).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalToken {
    pub order_id: String,
    pub intent: String,
    pub artifact_digest: String,
    pub actor: ActorId,
    pub granted_at: DateTime<Utc>,
}

pub trait Gate {
    /// Verify an approval for THIS order binds THIS artifact.
    /// Returns Err if digest mismatches (plan §11: re-verify pre-publish).
    fn verify(
        &self,
        order: &Order,
        token: &ApprovalToken,
        current_digest: &str,
    ) -> PortResult<()>;
}

/// Append-only audit ledger (plan §11). Every transition writes here in the
/// SAME SQLite transaction as the state change — atomic, or not at all.
pub trait Ledger {
    fn append(
        &mut self,
        order_id: &str,
        event: &OrderEvent,
        from: OrderState,
        to: OrderState,
        actor: &ActorId,
        at: DateTime<Utc>,
    ) -> PortResult<()>;
}
