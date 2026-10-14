use crate::inject::{CrashPoint, FaultInjector};
use crate::ports::{DispatchHost, IntentLog, OutcomeStore};
use crate::types::{ExecutionIntent, ExecutionOutcome, OrderId, Receipt};

pub type IntentKey = (OrderId, crate::types::Attempt, crate::types::RunId);

/// The output returned by a host dispatch and atomically bound as an outcome.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DispatchResult {
    pub cursor: Option<String>,
    pub next_activation: Option<crate::types::ActivationRef>,
    pub receipts: Vec<Receipt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunStep {
    Bound(ExecutionOutcome),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeAction {
    NeedsFreshRun,
    ReDispatch,
    ReBind,
    AlreadyBound(ExecutionOutcome),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleError(pub CrashPoint);

/// Runs only through the storage and host ports; it never owns storage details.
pub struct Executor<'a, L: IntentLog, H: DispatchHost<Result = DispatchResult>, S: OutcomeStore> {
    intents: &'a mut L,
    host: &'a mut H,
    outcomes: &'a mut S,
    faults: FaultInjector,
}

impl<'a, L, H, S> Executor<'a, L, H, S>
where
    L: IntentLog,
    H: DispatchHost<Result = DispatchResult>,
    S: OutcomeStore,
{
    pub fn new(intents: &'a mut L, host: &'a mut H, outcomes: &'a mut S) -> Self {
        Self::with_faults(intents, host, outcomes, FaultInjector::default())
    }

    pub fn with_faults(
        intents: &'a mut L,
        host: &'a mut H,
        outcomes: &'a mut S,
        faults: FaultInjector,
    ) -> Self {
        Self {
            intents,
            host,
            outcomes,
            faults,
        }
    }

    /// Appends the intent before marking it started or calling the external host.
    pub fn run(&mut self, intent: ExecutionIntent) -> Result<RunStep, LifecycleError> {
        let key = (intent.order.clone(), intent.attempt, intent.run);
        self.intents.append(&intent);
        self.faults.fire(CrashPoint::W1).map_err(LifecycleError)?;

        self.intents.mark_started(&key, intent.activation);
        let result = self.host.dispatch(&intent);
        let outcome = outcome_from(&intent, result);
        self.faults.fire(CrashPoint::W2).map_err(LifecycleError)?;

        self.outcomes.commit(&outcome);
        Ok(RunStep::Bound(outcome))
    }

    /// Resolves a previously recorded activation. W1 redispatches; W2 recovers
    /// the host result and binds it without dispatching again.
    pub fn resume(&mut self, key: &IntentKey) -> ResumeAction {
        let Some(intent) = self.intents.latest(key) else {
            return ResumeAction::NeedsFreshRun;
        };
        if let Some(outcome) = self.outcomes.latest(key) {
            return ResumeAction::AlreadyBound(outcome);
        }

        if !self.intents.is_started(key, intent.activation) {
            self.intents.mark_started(key, intent.activation);
            let result = self.host.dispatch(&intent);
            let outcome = outcome_from(&intent, result);
            self.outcomes.commit(&outcome);
            return ResumeAction::ReDispatch;
        }

        if let Some(result) = self.host.recover_result(&intent) {
            let outcome = outcome_from(&intent, result);
            self.outcomes.commit(&outcome);
        }
        ResumeAction::ReBind
    }

    pub fn dispatch_count(&self) -> u64 {
        self.host.dispatch_count()
    }

    pub fn outcome(&self, key: &IntentKey) -> Option<ExecutionOutcome> {
        self.outcomes.latest(key)
    }

    pub fn latest_intent(&self, key: &IntentKey) -> Option<ExecutionIntent> {
        self.intents.latest(key)
    }

    pub fn is_started(&self, key: &IntentKey, activation: u64) -> bool {
        self.intents.is_started(key, activation)
    }
}

fn outcome_from(intent: &ExecutionIntent, result: DispatchResult) -> ExecutionOutcome {
    ExecutionOutcome {
        intent_key: (intent.order.clone(), intent.attempt, intent.run),
        activation: intent.activation,
        cursor: result.cursor,
        next_activation: result.next_activation,
        receipts: result.receipts,
    }
}
