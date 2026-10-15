use serde::{Deserialize, Serialize};

/// Maximum distinct attempts admitted for an order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptBudget {
    pub max_attempts: u32,
}
