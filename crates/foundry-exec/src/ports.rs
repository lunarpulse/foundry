use crate::types::{Attempt, ExecutionIntent, ExecutionOutcome, OrderId, RunId};

/// Persistence boundary for execution intents. Implementations own all storage writes.
pub trait IntentLog {
    fn append(&mut self, intent: &ExecutionIntent);
    fn mark_started(&mut self, key: &(OrderId, Attempt, RunId), activation: u64);
    fn latest(&self, key: &(OrderId, Attempt, RunId)) -> Option<ExecutionIntent>;

    /// Returns whether this specific activation crossed the external-start boundary.
    fn is_started(&self, key: &(OrderId, Attempt, RunId), activation: u64) -> bool;
}

/// External execution boundary. Implementations may adapt a real host or a test double.
pub trait DispatchHost {
    type Result;

    fn dispatch(&mut self, intent: &ExecutionIntent) -> Self::Result;
    fn dispatch_count(&self) -> u64;

    /// Recovers a result already received by the host adapter after a process restart.
    fn recover_result(&mut self, _intent: &ExecutionIntent) -> Option<Self::Result> {
        None
    }
}

/// Atomic persistence boundary for an outcome and its continuation data.
pub trait OutcomeStore {
    fn commit(&mut self, outcome: &ExecutionOutcome);
    fn latest(&self, key: &(OrderId, Attempt, RunId)) -> Option<ExecutionOutcome>;
}
