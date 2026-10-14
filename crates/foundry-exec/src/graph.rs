use crate::{
    emit::{EmitLog, EmitOutcome, EmitSink, complete_after_w4, emit_once},
    hold::{HoldCommand, HoldInstanceId, HoldOperation},
    inject::{CrashPoint, FaultInjector},
    lifecycle::{DispatchResult, Executor, LifecycleError, RunStep},
    ports::{DispatchHost, IntentLog, OutcomeStore},
    types::{ArtifactTuple, Attempt, Epoch, ExecutionIntent, Generation, OrderId, RunId},
};
use chrono::Utc;
use foundry_pack::{PackSnapshot, eval_condition, parse_expr};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphStep {
    Advanced { next: String, effects: Vec<String> },
    HoldRequired(HoldCommand),
    Emitted { receipt: crate::types::EmitReceipt },
    Completed,
    Fault(String),
}

pub struct GraphDriver<'a, L, H, S, ES, EL>
where
    L: IntentLog,
    H: DispatchHost<Result = DispatchResult>,
    S: OutcomeStore,
    ES: EmitSink,
    EL: EmitLog,
{
    pack: &'a PackSnapshot,
    intents: &'a mut L,
    host: &'a mut H,
    outcomes: &'a mut S,
    sink: &'a mut ES,
    emit_log: &'a mut EL,
    order: OrderId,
    attempt: Attempt,
    run: RunId,
    generation: Generation,
    epoch: Epoch,
    external_ref: String,
    tuple: Option<ArtifactTuple>,
    activation: u64,
    counters: BTreeMap<String, u64>,
    faults: FaultInjector,
}

impl<'a, L, H, S, ES, EL> GraphDriver<'a, L, H, S, ES, EL>
where
    L: IntentLog,
    H: DispatchHost<Result = DispatchResult>,
    S: OutcomeStore,
    ES: EmitSink,
    EL: EmitLog,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pack: &'a PackSnapshot,
        intents: &'a mut L,
        host: &'a mut H,
        outcomes: &'a mut S,
        sink: &'a mut ES,
        emit_log: &'a mut EL,
        order: OrderId,
        attempt: Attempt,
        run: RunId,
        generation: Generation,
        epoch: Epoch,
        external_ref: impl Into<String>,
        tuple: Option<ArtifactTuple>,
    ) -> Self {
        Self {
            pack,
            intents,
            host,
            outcomes,
            sink,
            emit_log,
            order,
            attempt,
            run,
            generation,
            epoch,
            external_ref: external_ref.into(),
            tuple,
            activation: 0,
            counters: pack.counters().clone(),
            faults: FaultInjector::default(),
        }
    }

    pub fn counters(&self) -> &BTreeMap<String, u64> {
        &self.counters
    }

    pub fn activation(&self) -> u64 {
        self.activation
    }

    pub fn arm_fault(&mut self, point: CrashPoint) {
        self.faults.arm(point);
    }

    pub fn start(&mut self) -> GraphStep {
        let Some(entry) = self.pack.policy().get("entry").and_then(|v| v.as_str()) else {
            return GraphStep::Fault("pack entry is missing".into());
        };
        self.advance(entry)
    }

    pub fn advance(&mut self, cursor: &str) -> GraphStep {
        let Some(node) = self.pack.nodes().iter().find(|n| n.id == cursor).cloned() else {
            return GraphStep::Fault(format!("unknown graph cursor {cursor}"));
        };
        match node.kind.as_str() {
            "dispatch" => self.dispatch_node(&node),
            "route" => self.route_node(&node),
            "hold" => GraphStep::HoldRequired(HoldCommand {
                instance: HoldInstanceId {
                    order: self.order.clone(),
                    attempt: self.attempt,
                    run: self.run,
                    hold_node: node.id,
                },
                op: HoldOperation::Info,
                human_resolution: None,
                evidence: "graph hold node".into(),
            }),
            "redrive" => self.redrive_node(&node),
            "emit" => self.emit_node(&node),
            kind => GraphStep::Fault(format!("unsupported node kind {kind}")),
        }
    }

    fn dispatch_node(&mut self, node: &foundry_pack::Node) -> GraphStep {
        let intent = self.intent(self.activation, self.external_ref.clone());
        let mut executor =
            Executor::with_faults(self.intents, self.host, self.outcomes, self.faults);
        let result = executor.run(intent);
        match result {
            Ok(RunStep::Bound(outcome)) => {
                self.activation = self.activation.saturating_add(1);
                let next = outcome.cursor.or_else(|| {
                    node.value
                        .get("on_done")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned)
                });
                next.map_or(GraphStep::Completed, |next| GraphStep::Advanced {
                    next,
                    effects: vec![format!("dispatch:{}", node.id)],
                })
            }
            Err(error) => {
                if matches!(
                    error,
                    LifecycleError::Crash(CrashPoint::W1 | CrashPoint::W2)
                ) {
                    self.faults = FaultInjector::default();
                }
                lifecycle_fault(error)
            }
        }
    }

    fn route_node(&mut self, node: &foundry_pack::Node) -> GraphStep {
        let Some(branches) = node.value.get("branches").and_then(|v| v.as_array()) else {
            return GraphStep::Fault(format!("route {} has no branches", node.id));
        };
        for branch in branches {
            let Some(condition) = branch.get("when").and_then(|v| v.as_str()) else {
                continue;
            };
            let Ok(expr) = parse_expr(condition) else {
                return GraphStep::Fault(format!("invalid condition: {condition}"));
            };
            if !eval_condition(&expr, &self.counters) {
                continue;
            }
            let mut effects = Vec::new();
            if let Some(actions) = branch.get("do").and_then(|v| v.as_str()) {
                for action in actions.split(';').map(str::trim).filter(|s| !s.is_empty()) {
                    if let Some(counter) = action
                        .strip_prefix("bump(")
                        .and_then(|s| s.strip_suffix(')'))
                    {
                        let Some(value) = self.counters.get_mut(counter.trim()) else {
                            return GraphStep::Fault(format!("unknown counter {counter}"));
                        };
                        let Some(next) = value.checked_add(1) else {
                            return GraphStep::Fault(format!("counter overflow: {counter}"));
                        };
                        *value = next;
                        effects.push(format!("bump:{}={}", counter.trim(), next));
                    } else if action.starts_with("emit(") {
                        let Some(tuple) = self.tuple.as_ref() else {
                            return GraphStep::Fault("emit requires an artifact tuple".into());
                        };
                        if let Err(point) = self.faults.fire(CrashPoint::W3) {
                            return GraphStep::Fault(format!("injected crash: {point:?}"));
                        }
                        match emit_once(self.sink, self.emit_log, tuple, self.epoch) {
                            EmitOutcome::Published(receipt) | EmitOutcome::Existing(receipt) => {
                                if let Err(point) = self.faults.fire(CrashPoint::W4) {
                                    return GraphStep::Fault(format!("injected crash: {point:?}"));
                                }
                                complete_after_w4(self.emit_log, &tuple.key());
                                return GraphStep::Emitted { receipt };
                            }
                            EmitOutcome::StaleRejected => {
                                return GraphStep::Fault("stale emit rejected".into());
                            }
                            EmitOutcome::PromotionRequired => {
                                return GraphStep::Fault(
                                    "emit requires a promotion receipt".into(),
                                );
                            }
                        }
                    }
                }
            }
            return match branch.get("goto").and_then(|v| v.as_str()) {
                Some(next) => GraphStep::Advanced {
                    next: next.into(),
                    effects,
                },
                None => GraphStep::Completed,
            };
        }
        GraphStep::Fault(format!("no route branch matched at {}", node.id))
    }

    fn redrive_node(&mut self, node: &foundry_pack::Node) -> GraphStep {
        if let Some(counter) = node.value.get("bump").and_then(|v| v.as_str()) {
            let Some(value) = self.counters.get_mut(counter) else {
                return GraphStep::Fault(format!("unknown counter {counter}"));
            };
            let Some(next) = value.checked_add(1) else {
                return GraphStep::Fault(format!("counter overflow: {counter}"));
            };
            *value = next;
        }
        self.activation = self.activation.saturating_add(1);
        let payload = node
            .value
            .get("payload")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let intent = self.intent(self.activation, format!("redrive:{}:{payload}", node.id));
        let mut executor =
            Executor::with_faults(self.intents, self.host, self.outcomes, self.faults);
        let result = executor.run(intent);
        match result {
            Ok(RunStep::Bound(outcome)) => {
                self.activation = self.activation.saturating_add(1);
                let next = outcome.cursor.or_else(|| {
                    node.value
                        .get("on_done")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned)
                });
                next.map_or(GraphStep::Completed, |next| GraphStep::Advanced {
                    next,
                    effects: vec![format!("redrive:{}", node.id)],
                })
            }
            Err(error) => {
                if matches!(
                    error,
                    LifecycleError::Crash(CrashPoint::W1 | CrashPoint::W2)
                ) {
                    self.faults = FaultInjector::default();
                }
                lifecycle_fault(error)
            }
        }
    }

    fn emit_node(&mut self, node: &foundry_pack::Node) -> GraphStep {
        let Some(tuple) = self.tuple.as_ref() else {
            return GraphStep::Fault("emit requires an artifact tuple".into());
        };
        if let Err(point) = self.faults.fire(CrashPoint::W3) {
            return GraphStep::Fault(format!("injected crash: {point:?}"));
        }
        match emit_once(self.sink, self.emit_log, tuple, self.epoch) {
            EmitOutcome::Published(receipt) | EmitOutcome::Existing(receipt) => {
                if let Err(point) = self.faults.fire(CrashPoint::W4) {
                    return GraphStep::Fault(format!("injected crash: {point:?}"));
                }
                complete_after_w4(self.emit_log, &tuple.key());
                GraphStep::Emitted { receipt }
            }
            EmitOutcome::StaleRejected => GraphStep::Fault("stale emit rejected".into()),
            EmitOutcome::PromotionRequired => {
                GraphStep::Fault(format!("emit node {} has no promotion receipt", node.id))
            }
        }
    }

    fn intent(&self, activation: u64, external_ref: String) -> ExecutionIntent {
        ExecutionIntent {
            order: self.order.clone(),
            attempt: self.attempt,
            run: self.run,
            activation,
            generation: self.generation,
            epoch: self.epoch,
            external_ref,
            recorded_at: Utc::now(),
        }
    }
}

fn lifecycle_fault(error: LifecycleError) -> GraphStep {
    GraphStep::Fault(format!("execution lifecycle: {error:?}"))
}
