use serde::{Deserialize, Serialize};

/// Maximum distinct runs admitted for each (order, attempt) pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptBudget {
    pub max_attempts: u32,
}
