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
    // Subtract spend before adding balances so an overflowing sum cannot
    // discard funds that would have fit after settlement.
    fn available_nano(&self) -> u128 {
        if self.settled_nano <= self.reserved_nano {
            self.carried_nano
                .saturating_add(self.reserved_nano - self.settled_nano)
        } else {
            self.carried_nano
                .saturating_sub(self.settled_nano - self.reserved_nano)
        }
    }

    /// Carries all unspent value forward, including prior carried value.
    pub fn carry_over(&self) -> Budget {
        Budget {
            reserved_nano: 0,
            settled_nano: 0,
            carried_nano: self.available_nano(),
        }
    }
}

/// Reserves from carried plus currently reserved funds, less settled spend.
pub fn reserve(budget: &Budget, nano: u128) -> ReservationOutcome {
    let available = budget.available_nano();
    if nano > available {
        ReservationOutcome::Exhausted
    } else {
        ReservationOutcome::Granted {
            remaining_nano: available.saturating_sub(nano),
        }
    }
}
