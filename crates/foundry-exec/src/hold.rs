use crate::types::{Attempt, OrderId, RunId};
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

pub fn cas_hold(expected: &HoldInstanceId, presented: &HoldCommand) -> HoldCasOutcome {
    if expected == &presented.instance {
        HoldCasOutcome::Applied
    } else {
        HoldCasOutcome::Conflict {
            observed_instance: presented.instance.clone(),
        }
    }
}
