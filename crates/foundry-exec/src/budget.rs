use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub reserved_nano: u128,
    pub settled_nano: u128,
    pub carried_nano: u128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReservationOutcome {
    Granted { remaining_nano: u128 },
    Exhausted,
}

impl Budget {
    /// Carries all unspent value forward, including prior carried value.
    pub fn carry_over(&self) -> Budget {
        let remaining_reserved = self.reserved_nano.saturating_sub(self.settled_nano);
        Budget {
            reserved_nano: 0,
            settled_nano: 0,
            carried_nano: self.carried_nano.saturating_add(remaining_reserved),
        }
    }
}

/// Reserves from carried plus currently reserved funds, less settled spend.
pub fn reserve(budget: &Budget, nano: u128) -> ReservationOutcome {
    let available = budget
        .carried_nano
        .saturating_add(budget.reserved_nano)
        .saturating_sub(budget.settled_nano);
    if nano > available {
        ReservationOutcome::Exhausted
    } else {
        ReservationOutcome::Granted {
            remaining_nano: available.saturating_sub(nano),
        }
    }
}
