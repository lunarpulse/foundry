use crate::inject::{CrashPoint, FaultInjector};
use crate::ports::{AttemptLedger, DispatchHost, IntentLog, OutcomeStore, PortError};
use crate::types::{ExecutionIntent, ExecutionOutcome, OrderId, Receipt};

/// Run-level lookup key. Durable intent identity additionally includes activation.
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
    /// Dispatch was claimed, but its durable result is not available yet.
    AwaitResult,
    /// A fault interrupted recovery before the outcome was committed.
    Interrupted(LifecycleError),
    AlreadyBound(ExecutionOutcome),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleError {
    Host(crate::host::HostError),
    Crash(CrashPoint),
    IntentConflict(u64),
    ActivationPending(u64),
    AwaitResult(u64),
    AttemptBudget(PortError),
}

/// Runs only through the storage and host ports; it never owns storage details.
pub struct Executor<'a, L: IntentLog, H: DispatchHost<Result = DispatchResult>, S: OutcomeStore> {
    intents: &'a mut L,
    host: &'a mut H,
    outcomes: &'a mut S,
    faults: FaultInjector,
    attempt_ledger: Option<&'a dyn AttemptLedger>,
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
            attempt_ledger: None,
        }
    }

    pub fn with_attempt_ledger(mut self, ledger: &'a dyn AttemptLedger) -> Self {
        self.attempt_ledger = Some(ledger);
        self
    }

    /// Records fresh work before dispatch. Retries of a recorded activation reuse
    /// its binding or recover its result; new activations wait for the prior binding.
    pub fn run(&mut self, intent: ExecutionIntent) -> Result<RunStep, LifecycleError> {
        if let Some(ledger) = self.attempt_ledger {
            ledger.consume(&intent.order, intent.attempt).map_err(LifecycleError::AttemptBudget)?;
        }
        let key = (intent.order.clone(), intent.attempt, intent.run);
        if let Some(recorded) = self.intents.get(&key, intent.activation) {
            // A retry may have a new timestamp, but cannot change the work's identity.
            if recorded.generation != intent.generation
                || recorded.epoch != intent.epoch
                || recorded.external_ref != intent.external_ref
            {
                return Err(LifecycleError::IntentConflict(intent.activation));
            }
            if let Some(outcome) = self.outcomes.get(&key, intent.activation) {
                return Ok(RunStep::Bound(outcome));
            }
            if self.intents.is_started(&key, intent.activation) {
                let Some(result) = self.host.recover_result(&recorded) else {
                    if self.host.accepted(&recorded) {
                        return Err(LifecycleError::AwaitResult(intent.activation));
                    }
                    return self.dispatch_recorded(&key, &recorded).map(RunStep::Bound);
                };
                return self.bind_result(&recorded, result).map(RunStep::Bound);
            }
            return self.dispatch_recorded(&key, &recorded).map(RunStep::Bound);
        }
        let pending = self
            .intents
            .latest(&key)
            .filter(|p| self.outcomes.get(&key, p.activation).is_none());
        if let Some(previous) = pending {
            return Err(LifecycleError::ActivationPending(previous.activation));
        }
        self.intents.append(&intent);
        self.dispatch_recorded(&key, &intent).map(RunStep::Bound)
    }

    /// Consumes the order's attempt before any execution work. Ledger
    /// implementations make repeated calls for the same `(order, attempt)` idempotent.
    pub fn run_with_attempt_ledger(
        &mut self,
        intent: ExecutionIntent,
        ledger: &dyn AttemptLedger,
    ) -> Result<RunStep, LifecycleError> {
        ledger.consume(&intent.order, intent.attempt).map_err(LifecycleError::AttemptBudget)?;
        self.run(intent)
    }

    /// Resolves a previously recorded activation. W1 redispatches; W2 recovers
    /// the host result and binds it without dispatching again. Started work without
    /// a durable result waits unless the host proves receipt absence. Legacy hosts
    /// without receipt support conservatively wait.
    pub fn resume(&mut self, key: &IntentKey) -> ResumeAction {
        let Some(intent) = self.intents.latest(key) else {
            return ResumeAction::NeedsFreshRun;
        };
        if let Some(outcome) = self.outcomes.get(key, intent.activation) {
            return ResumeAction::AlreadyBound(outcome);
        }

        if !self.intents.is_started(key, intent.activation) {
            return match self.dispatch_recorded(key, &intent) {
                Ok(_) => ResumeAction::ReDispatch,
                Err(error) => ResumeAction::Interrupted(error),
            };
        }

        if let Some(result) = self.host.recover_result(&intent) {
            return match self.bind_result(&intent, result) {
                Ok(_) => ResumeAction::ReBind,
                Err(error) => ResumeAction::Interrupted(error),
            };
        }
        if self.host.accepted(&intent) {
            ResumeAction::AwaitResult
        } else {
            match self.dispatch_recorded(key, &intent) {
                Ok(_) => ResumeAction::ReDispatch,
                Err(error) => ResumeAction::Interrupted(error),
            }
        }
    }

    pub fn dispatch_count(&self) -> u64 {
        self.host.dispatch_count()
    }

    pub fn outcome(&self, key: &IntentKey) -> Option<ExecutionOutcome> {
        let intent = self.intents.latest(key)?;
        self.outcomes.get(key, intent.activation)
    }

    pub fn latest_intent(&self, key: &IntentKey) -> Option<ExecutionIntent> {
        self.intents.latest(key)
    }

    pub fn is_started(&self, key: &IntentKey, activation: u64) -> bool {
        self.intents.is_started(key, activation)
    }

    // Only called with an appended or recovered intent, never arbitrary host input.
    fn dispatch_recorded(
        &mut self,
        key: &IntentKey,
        intent: &ExecutionIntent,
    ) -> Result<ExecutionOutcome, LifecycleError> {
        self.faults.fire(CrashPoint::W1).map_err(LifecycleError::Crash)?;
        self.intents.mark_started(key, intent.activation);
        let result = self.host.try_dispatch(intent).map_err(LifecycleError::Host)?;
        self.bind_result(intent, result)
    }

    fn bind_result(
        &mut self,
        intent: &ExecutionIntent,
        result: DispatchResult,
    ) -> Result<ExecutionOutcome, LifecycleError> {
        let outcome = outcome_from(intent, result);
        self.faults.fire(CrashPoint::W2).map_err(LifecycleError::Crash)?;
        self.outcomes.commit(&outcome);
        Ok(outcome)
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
