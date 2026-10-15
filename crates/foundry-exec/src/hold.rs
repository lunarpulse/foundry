use crate::types::{Attempt, OrderId, RunId};
use crate::ports::PortError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HoldOperation {
    Approve,
    Reject,
    Amend,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoldInstanceId {
    pub order: OrderId,
    pub attempt: Attempt,
    pub run: RunId,
    pub hold_node: String,
    /// Durable visit ordinal for re-entry into the same hold node.
    pub entry: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoldCommand {
    pub instance: HoldInstanceId,
    pub op: HoldOperation,
    pub human_resolution: Option<String>,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HoldCasOutcome {
    Applied,
    Conflict { observed_instance: HoldInstanceId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HoldApplied {
    Approved,
    Rejected,
    Amended { resume_to: String },
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoldState {
    pub instance: HoldInstanceId,
    pub resume_to: String,
    pub status: HoldStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HoldStatus {
    Waiting,
    Resolved(HoldApplied),
}

pub fn cas_hold(expected: &HoldInstanceId, presented: &HoldCommand) -> HoldCasOutcome {
    if expected == &presented.instance {
        HoldCasOutcome::Applied
    } else {
        HoldCasOutcome::Conflict {
            observed_instance: presented.instance.clone(),
        }
    }
}

/// Validates and computes the transition a HoldStore must atomically persist.
/// `Amend.evidence` carries the replacement resume destination.
pub fn apply_hold(state: &HoldState, cmd: &HoldCommand, expected: &HoldInstanceId) -> Result<HoldApplied, PortError> {
    if cas_hold(expected, cmd) != HoldCasOutcome::Applied || state.instance != *expected {
        return Err(PortError::HoldConflict(HoldCasOutcome::Conflict {
            observed_instance: cmd.instance.clone(),
        }));
    }
    if cmd.op == HoldOperation::Info {
        return Ok(HoldApplied::Info);
    }
    let desired = match cmd.op {
        HoldOperation::Approve | HoldOperation::Reject if cmd.evidence.trim().is_empty() => {
            return Err(PortError::EvidenceRequired);
        }
        HoldOperation::Approve => HoldApplied::Approved,
        HoldOperation::Reject => HoldApplied::Rejected,
        HoldOperation::Amend => {
            let resume_to = cmd.evidence.trim();
            if resume_to.is_empty() { return Err(PortError::EvidenceRequired); }
            HoldApplied::Amended { resume_to: resume_to.to_owned() }
        }
        HoldOperation::Info => unreachable!(),
    };
    match (&state.status, &desired) {
        (HoldStatus::Waiting, HoldApplied::Amended { resume_to }) if state.resume_to == *resume_to => Ok(desired),
        (HoldStatus::Waiting, _) => Ok(desired),
        (HoldStatus::Resolved(_), HoldApplied::Amended { .. }) => Err(PortError::AlreadyResolved),
        (HoldStatus::Resolved(applied), _) if applied == &desired => Ok(applied.clone()),
        (HoldStatus::Resolved(_), _) => Err(PortError::AlreadyResolved),
    }
}
