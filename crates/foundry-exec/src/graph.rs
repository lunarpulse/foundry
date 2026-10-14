use crate::{
    emit::{EmitLog, EmitOutcome, EmitSink, complete_after_w4, emit_named},
    hold::{HoldCommand, HoldInstanceId, HoldOperation},
    inject::{CrashPoint, FaultInjector},
    lifecycle::{DispatchResult, Executor, LifecycleError, RunStep},
    ports::{CounterLog, CounterRecord, DispatchHost, IntentLog, OutcomeStore},
    types::{ArtifactTuple, Attempt, Epoch, ExecutionIntent, Generation, OrderId, RunId},
};
use chrono::Utc;
use foundry_pack::{Expr, PackSnapshot, eval_condition_checked, parse_expr};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphStep {
    Advanced { next: String, effects: Vec<String> },
    HoldRequired(HoldCommand),
    Emitted { receipt: crate::types::EmitReceipt },
    Completed,
    Fault(String),
}

pub struct GraphDriver<'a, L, H, S, C, ES, EL>
where
    L: IntentLog,
    H: DispatchHost<Result = DispatchResult>,
    S: OutcomeStore,
    C: CounterLog,
    ES: EmitSink,
    EL: EmitLog,
{
    pack: &'a PackSnapshot,
    intents: &'a mut L,
    host: &'a mut H,
    outcomes: &'a mut S,
    counter_log: &'a mut C,
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
    cursor: Option<String>,
    completed: bool,
    counters: BTreeMap<String, u64>,
    faults: FaultInjector,
}

impl<'a, L, H, S, C, ES, EL> GraphDriver<'a, L, H, S, C, ES, EL>
where
    L: IntentLog,
    H: DispatchHost<Result = DispatchResult>,
    S: OutcomeStore,
    C: CounterLog,
    ES: EmitSink,
    EL: EmitLog,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pack: &'a PackSnapshot,
        intents: &'a mut L,
        host: &'a mut H,
        outcomes: &'a mut S,
        counter_log: &'a mut C,
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
        // Recover the execution identity for a pending W1/W2 activation. Full
        // graph counter/cursor restoration still requires a graph-state port.
        let key = (order.clone(), attempt, run);
        let activation = intents.latest(&key).map_or(0, |intent| {
            if outcomes.get(&key, intent.activation).is_some() {
                intent.activation.saturating_add(1)
            } else {
                intent.activation
            }
        });
        Self {
            pack,
            intents,
            host,
            outcomes,
            counter_log,
            sink,
            emit_log,
            order,
            attempt,
            run,
            generation,
            epoch,
            external_ref: external_ref.into(),
            tuple,
            activation,
            cursor: None,
            completed: false,
            counters: pack.counters().clone(),
            faults: FaultInjector::default(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn restore(
        pack: &'a PackSnapshot,
        intents: &'a mut L,
        host: &'a mut H,
        outcomes: &'a mut S,
        counter_log: &'a mut C,
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
        let mut driver = Self::new(pack, intents, host, outcomes, counter_log, sink, emit_log,
            order.clone(), attempt, run, generation, epoch, external_ref, tuple);
        let records = driver.counter_log.load(&order.0, run.0);
        for record in &records {
            if let CounterRecord::Bump { counter, value, .. } = record {
                driver.counters.insert(counter.clone(), *value);
            }
        }
        if let Some(outcome) = driver.outcomes.latest(&(order, attempt, run)) {
            driver.cursor = outcome.cursor.or_else(|| outcome.next_activation.map(|next| next.node));
            driver.activation = outcome.activation.saturating_add(1);
        }
        let mut cursor_is_next_from_log = false;
        if let Some((bump_idx, pending_node)) = records.iter().enumerate().rev().find_map(|(idx, record)| match record {
            CounterRecord::Bump { node_id, .. } => Some((idx, node_id.clone())),
            CounterRecord::Completed { .. } => None,
        }) {
            let completed_idx = records.iter().rposition(|record| matches!(record,
                CounterRecord::Completed { node_id, .. } if node_id == &pending_node));
            if completed_idx.is_none_or(|idx| bump_idx > idx) {
                driver.cursor = Some(pending_node);
                driver.completed = false;
                cursor_is_next_from_log = true;
            }
        }
        if driver.cursor.is_none()
            && let Some(CounterRecord::Completed { goto, .. }) = records.iter().rev().find(|record|
                matches!(record, CounterRecord::Completed { .. }))
        {
            driver.cursor = goto.clone();
            driver.completed = goto.is_none();
            cursor_is_next_from_log = true;
        }
        let mut completion_chain = BTreeSet::new();
        while !cursor_is_next_from_log && let Some(current) = driver.cursor.clone() {
            if !completion_chain.insert(current.clone()) {
                break;
            }
            let completion = records.iter().rev().find_map(|record| match record {
                CounterRecord::Completed { node_id, goto } if node_id == &current => Some(goto.clone()),
                _ => None,
            });
            match completion {
                Some(Some(next)) if next != current => {
                    let repeated = completion_chain.contains(&next);
                    driver.cursor = Some(next);
                    if repeated { break; }
                }
                Some(Some(_)) | None => break,
                Some(None) => { driver.cursor = None; driver.completed = true; break; }
            }
        }
        driver
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
        if self.completed {
            return GraphStep::Completed;
        }
        if let Some(cursor) = self.cursor.clone() {
            return self.advance(&cursor);
        }
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
        let Some(next_activation) = self.activation.checked_add(1) else {
            return GraphStep::Fault("activation overflow".into());
        };
        let intent = self.intent(self.activation, self.external_ref.clone());
        let mut executor =
            Executor::with_faults(self.intents, self.host, self.outcomes, self.faults);
        let result = executor.run(intent);
        match result {
            Ok(RunStep::Bound(outcome)) => {
                self.activation = next_activation;
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
                return GraphStep::Fault("route condition is missing".into());
            };
            let Ok(expr) = parse_expr(condition) else {
                return GraphStep::Fault(format!("invalid condition: {condition}"));
            };
            match eval_condition_checked(&expr, &self.counters) {
                Some(false) => continue,
                Some(true) => {}
                None => return GraphStep::Fault(format!("route condition needs runtime context: {condition}")),
            }
            // A failed/crashed action must leave branch-selection counters intact.
            let mut counters = self.counters.clone();
            let mut effects = Vec::new();
            if let Some(actions) = branch.get("do").and_then(|v| v.as_str()) {
                for action in actions.split(';') {
                    let Ok(Expr::Call(name, args)) = parse_expr(action.trim()) else {
                        return GraphStep::Fault(format!("invalid route action: {action}"));
                    };
                    match (name.as_str(), args.as_slice()) {
                        ("bump", [Expr::Ident(counter)]) => {
                            let Some(value) = counters.get_mut(counter) else {
                                return GraphStep::Fault(format!("unknown counter {counter}"));
                            };
                            if let Err(error) = self.bump_with_log(&node.id, counter, value) {
                                return GraphStep::Fault(error);
                            }
                            let next = *value;
                            effects.push(format!("bump:{counter}={next}"));
                        }
                        ("emit", [Expr::Ident(event), Expr::Ident(payload)]) => {
                            let result = self.emit_declared(event, payload);
                            if matches!(result, GraphStep::Emitted { .. }) {
                                self.counters = counters;
                            }
                            if matches!(result, GraphStep::Emitted { .. }) {
                                let goto = branch.get("goto").and_then(|v| v.as_str()).map(str::to_owned);
                                if let Err(error) = self.append_completed(&node.id, goto) {
                                    return GraphStep::Fault(error);
                                }
                            }
                            return result;
                        }
                        ("revert", _) => {
                            return GraphStep::Fault("revert requires a host action adapter".into());
                        }
                        _ => return GraphStep::Fault(format!("unsupported route action: {action}")),
                    }
                }
            }
            self.counters = counters;
            let goto = branch.get("goto").and_then(|v| v.as_str()).map(str::to_owned);
            if let Err(error) = self.append_completed(&node.id, goto.clone()) {
                return GraphStep::Fault(error);
            }
            return match goto {
                Some(next) => GraphStep::Advanced { next, effects },
                None => { self.completed = true; GraphStep::Completed },
            };
        }
        GraphStep::Fault(format!("no route branch matched at {}", node.id))
    }

    fn redrive_node(&mut self, node: &foundry_pack::Node) -> GraphStep {
        // activation is the next unused identity, not the last used one. A
        // standalone redrive retains the original activation-1 contract.
        let activation = self.activation.max(1);
        let Some(next_activation) = activation.checked_add(1) else {
            return GraphStep::Fault("activation overflow".into());
        };
        let mut counters = self.counters.clone();
        if let Some(counter) = node.value.get("bump").and_then(|v| v.as_str()) {
            let Some(value) = counters.get_mut(counter) else {
                return GraphStep::Fault(format!("unknown counter {counter}"));
            };
            if let Err(error) = self.bump_with_log(&node.id, counter, value) {
                return GraphStep::Fault(error);
            }
        }
        let payload = node.value.get("payload").and_then(|v| v.as_str()).unwrap_or("");
        let intent = self.intent(activation, format!("redrive:{}:{payload}", node.id));
        let mut executor = Executor::with_faults(self.intents, self.host, self.outcomes, self.faults);
        match executor.run(intent) {
            Ok(RunStep::Bound(outcome)) => {
                self.counters = counters;
                self.activation = next_activation;
                let next = outcome.cursor.or_else(|| {
                    node.value.get("on_done").and_then(|v| v.as_str()).map(str::to_owned)
                });
                if let Err(error) = self.append_completed(&node.id, next.clone()) {
                    return GraphStep::Fault(error);
                }
                next.map_or_else(|| { self.completed = true; GraphStep::Completed }, |next| GraphStep::Advanced {
                    next,
                    effects: vec![format!("redrive:{}", node.id)],
                })
            }
            Err(error) => {
                if matches!(error, LifecycleError::Crash(CrashPoint::W1 | CrashPoint::W2)) {
                    self.faults = FaultInjector::default();
                }
                lifecycle_fault(error)
            }
        }
    }

    fn emit_node(&mut self, node: &foundry_pack::Node) -> GraphStep {
        let Some(event) = node.value.get("event").and_then(|v| v.as_str()) else {
            return GraphStep::Fault("emit event is missing".into());
        };
        let Some(payload) = node.value.get("payload_from").and_then(|v| v.as_str()) else {
            return GraphStep::Fault("emit payload reference is missing".into());
        };
        self.emit_declared(event, payload)
    }

    fn bump_with_log(&mut self, node_id: &str, counter: &str, value: &mut u64) -> Result<(), String> {
        let records = self.counter_log.load(&self.order.0, self.run.0);
        let last_completed_idx = records.iter().rposition(|record| matches!(record,
            CounterRecord::Completed { node_id: completed, .. } if completed == node_id));
        let last_bump = records.iter().enumerate().filter_map(|(idx, record)| match record {
            CounterRecord::Bump { node_id: bumped, counter: name, value } if bumped == node_id && name == counter => Some((idx, *value)),
            _ => None,
            }).next_back();
        match last_bump {
            Some((idx, persisted)) if last_completed_idx.is_none_or(|completed| idx > completed) => {
                // Restore loads all bumps, including the pending one. Validate it
                // against the preceding durable counter value, not the already-restored value.
                let previous = records[..idx].iter().rev().find_map(|record| match record {
                    CounterRecord::Bump { counter: name, value, .. } if name == counter => Some(*value),
                    _ => None,
                }).or_else(|| self.pack.counters().get(counter).copied()).unwrap_or(0);
                let expected = previous.checked_add(1).ok_or_else(|| format!("counter overflow: {counter}"))?;
                if persisted != expected {
                    return Err(format!("counter log invariant violated for {node_id}/{counter}: expected {expected}, found {persisted}"));
                }
                *value = persisted;
            }
            _ => {
                let next = value.checked_add(1).ok_or_else(|| format!("counter overflow: {counter}"))?;
                self.counter_log.append(&self.order.0, self.run.0, CounterRecord::Bump {
                    node_id: node_id.to_owned(), counter: counter.to_owned(), value: next,
                }).map_err(|error| format!("counter log append failed: {error}"))?;
                *value = next;
            }
        }
        Ok(())
    }

    fn append_completed(&mut self, node_id: &str, goto: Option<String>) -> Result<(), String> {
        self.counter_log.append(&self.order.0, self.run.0,
            CounterRecord::Completed { node_id: node_id.to_owned(), goto })
            .map_err(|error| format!("counter log append failed: {error}"))
    }

    fn emit_declared(&mut self, event: &str, payload: &str) -> GraphStep {
        let Some(tuple) = self.tuple.as_ref() else {
            return GraphStep::Fault("emit requires an artifact tuple".into());
        };
        if tuple.order != self.order || tuple.attempt != self.attempt
            || tuple.run != self.run || tuple.generation != self.generation
        {
            return GraphStep::Fault("artifact tuple does not belong to this graph run".into());
        }
        let key = tuple.key();
        // W4 recovery touches orchestration only. It must not replay W3 or ask
        // the external sink to republish a committed tuple, even at an old epoch.
        if let Some(receipt) = self.emit_log.emit_of(&key) {
            complete_after_w4(self.emit_log, &key);
            return GraphStep::Emitted { receipt };
        }
        let existing = self.sink.receipt_of(&key).is_some();
        if !existing && self.epoch >= self.emit_log.current_epoch()
            && self.emit_log.promotion_of(&key).is_some()
            && let Err(point) = self.faults.fire(CrashPoint::W3)
        {
            return GraphStep::Fault(format!("injected crash: {point:?}"));
        }
        match emit_named(self.sink, self.emit_log, tuple, self.epoch, event, payload) {
            EmitOutcome::Published(receipt) => {
                if let Err(point) = self.faults.fire(CrashPoint::W4) {
                    return GraphStep::Fault(format!("injected crash: {point:?}"));
                }
                complete_after_w4(self.emit_log, &key);
                GraphStep::Emitted { receipt }
            }
            EmitOutcome::Existing(receipt) => {
                complete_after_w4(self.emit_log, &key);
                GraphStep::Emitted { receipt }
            }
            EmitOutcome::StaleRejected => GraphStep::Fault("stale emit rejected".into()),
            EmitOutcome::PromotionRequired => GraphStep::Fault("emit has no promotion receipt".into()),
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
