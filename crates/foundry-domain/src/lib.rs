//! foundry-domain — pure types + traits. No I/O. (plan §6)
pub mod order;

pub use order::{ExternalRef, Order, OrderEvent, OrderState, TransitionError};
