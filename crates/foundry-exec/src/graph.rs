use crate::{
    emit::{EmitLog, EmitOutcome, EmitSink, complete_after_w4, emit_named},
    hold::{HoldCommand, HoldInstanceId, HoldOperation},
    inject::{CrashPoint, FaultInjector},
    lifecycle::{DispatchResult, Executor, LifecycleError, RunStep},
    ports::{AttemptLedger, CounterLog, CounterRecord, DispatchHost, HoldStore, IntentLog, OutcomeStore, TupleStore},
    types::{ArtifactTuple, Attempt, Epoch, ExecutionIntent, Generation, OrderId, RunId},
};
use chrono::Utc;
use foundry_pack::{Expr, PackSnapshot, eval_condition_checked, parse_expr};
use std::collections::{BTreeMap, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphStep {
    Advanced { next: String, effects: Vec<String> },
    HoldRequired(HoldCommand),
    HoldWaiting { instance: HoldInstanceId },
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
    recovery_error: Option<String>,
    hold_store: Option<&'a dyn HoldStore>,
    attempt_ledger: Option<&'a dyn AttemptLedger>,
    tuple_store: Option<&'a dyn TupleStore>,
}

struct Visit {
    counters: BTreeMap<String, u64>,
    pending: VecDeque<(String, u64)>,
    started: bool,
    activation: Option<u64>,
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
            recovery_error: None,
            hold_store: None,
            attempt_ledger: None,
            tuple_store: None,
        }
    }

    /// Enables durable hold handling while retaining the legacy constructor API.
    pub fn with_hold_store(mut self, hold_store: &'a dyn HoldStore) -> Self {
        self.hold_store = Some(hold_store);
        self
    }

    pub fn with_attempt_ledger(mut self, attempt_ledger: &'a dyn AttemptLedger) -> Self {
        self.attempt_ledger = Some(attempt_ledger);
        self
    }

    /// Enables durable tuple recording after a successful emit.
    pub fn with_tuple_store(mut self, tuple_store: &'a dyn TupleStore) -> Self {
        self.tuple_store = Some(tuple_store);
        self
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
        match driver.replay_counters(&records) {
            Ok(counters) => driver.counters = counters,
            Err(error) => driver.recovery_error = Some(error),
        }
        let latest_outcome = driver.outcomes.latest(&(order, attempt, run));
        let outcome_without_cursor = latest_outcome.as_ref().is_some_and(|outcome|
            outcome.cursor.is_none() && outcome.next_activation.is_none());
        if let Some(outcome) = &latest_outcome {
            driver.cursor = outcome.cursor.clone().or_else(|| outcome.next_activation.as_ref().map(|next| next.node.clone()));
            // Self::new has already retained any newer, still-pending intent.
        }
        // Consume facts once in append order. A node id is not a visit identity:
        // looking up its last Completed can incorrectly reuse an older visit.
        // This finite pass handles cycles without a node-based BTreeSet cutoff.
        let mut pending_activation = None;
        for record in &records {
            match record {
                CounterRecord::Started { node_id, activation } => {
                    driver.cursor = Some(node_id.clone());
                    driver.completed = false;
                    // Original B4 logs placed hold entry in this field. Only
                    // host visits can restore the host activation counter.
                    pending_activation = if driver.pack.nodes().iter()
                        .any(|node| &node.id == node_id && node.kind == "hold")
                    { None } else { *activation };
                }
                CounterRecord::Bump { node_id, .. } => {
                    driver.cursor = Some(node_id.clone());
                    driver.completed = false;
                }
                CounterRecord::Completed { goto, .. } => {
                    driver.cursor = goto.clone();
                    driver.completed = goto.is_none();
                    pending_activation = None;
                }
            }
        }
        if let Some(activation) = pending_activation {
            driver.activation = activation;
        }
        if !records.iter().any(|record| matches!(record, CounterRecord::Started { .. }))
            && latest_outcome.is_some()
            && ((records.is_empty() && outcome_without_cursor)
                || driver.cursor.as_ref().is_some_and(|cursor| driver.pack.nodes().iter()
                    .any(|node| &node.id == cursor && matches!(node.kind.as_str(), "dispatch" | "redrive"))))
        {
            // Old logs cannot order a host outcome relative to a counter-only
            // continuation. Guessing could repeat bound external work.
            driver.recovery_error = Some("legacy counter log has no host visit activation; recovery is ambiguous".into());
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
        if let Some(error) = &self.recovery_error {
            return GraphStep::Fault(error.clone());
        }
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
        if let Some(error) = &self.recovery_error {
            return GraphStep::Fault(error.clone());
        }
        let Some(node) = self.pack.nodes().iter().find(|n| n.id == cursor).cloned() else {
            return GraphStep::Fault(format!("unknown graph cursor {cursor}"));
        };
        match node.kind.as_str() {
            "dispatch" => self.dispatch_node(&node),
            "route" => self.route_node(&node),
            "hold" => self.hold_node(&node),
            "redrive" => self.redrive_node(&node),
            "emit" => self.emit_node(&node),
            kind => GraphStep::Fault(format!("unsupported node kind {kind}")),
        }
    }

    fn hold_node(&mut self, node: &foundry_pack::Node) -> GraphStep {
        let Some(store) = self.hold_store else {
            return GraphStep::HoldRequired(HoldCommand {
                instance: HoldInstanceId {
                    order: self.order.clone(), attempt: self.attempt, run: self.run,
                    hold_node: node.id.clone(), entry: self.hold_entry(&node.id),
                },
                op: HoldOperation::Info, human_resolution: None, evidence: "graph hold node".into(), resume_to: None,
            });
        };
        let resume_to = node.value.get("resume_to").and_then(|value| value.as_str())
            .or_else(|| node.value.get("resume_targets").and_then(|value| value.as_array())
                .and_then(|targets| targets.first()).and_then(|target| target.as_str()));
        let Some(resume_to) = resume_to else {
            return GraphStep::Fault(format!("hold node {} has no declared resume destination", node.id));
        };
        let entry = self.hold_entry(&node.id);
        let instance = HoldInstanceId {
            order: self.order.clone(), attempt: self.attempt, run: self.run,
            hold_node: node.id.clone(), entry,
        };
        let visit = match self.prepare_visit(&node.id) {
            Ok(visit) => visit,
            Err(error) => return GraphStep::Fault(error),
        };
        // The per-hold ordinal comes from Started/Completed visit history. It
        // must not occupy the host activation slot used by restore().
        if let Err(error) = self.start_visit(&node.id, &visit, None) {
            return GraphStep::Fault(error);
        }
        if let Err(error) = store.open(&instance, resume_to) {
            return GraphStep::Fault(format!("hold store open failed: {error:?}"));
        }
        let state = match store.get(&instance) {
            Ok(Some(state)) => state,
            Ok(None) => return GraphStep::Fault("hold store lost opened instance".into()),
            Err(error) => return GraphStep::Fault(format!("hold store read failed: {error:?}")),
        };
        match state.status {
            crate::hold::HoldStatus::Waiting => GraphStep::HoldWaiting { instance },
            crate::hold::HoldStatus::Resolved(_) => {
                if let Err(error) = self.append_completed(&node.id, Some(state.resume_to.clone())) {
                    return GraphStep::Fault(error);
                }
                GraphStep::Advanced { next: state.resume_to, effects: vec![format!("hold:{}", node.id)] }
            }
        }
    }

    fn hold_entry(&self, node_id: &str) -> u64 {
        let records = self.counter_log.load(&self.order.0, self.run.0);
        let mut entries = 0u64;
        let mut active = false;
        for record in records {
            match record {
                CounterRecord::Started { node_id: started, .. } if started == node_id => {
                    entries = entries.saturating_add(1);
                    active = true;
                }
                CounterRecord::Completed { node_id: completed, .. } if completed == node_id => active = false,
                _ => {}
            }
        }
        if active { entries.saturating_sub(1) } else { entries }
    }

    fn dispatch_node(&mut self, node: &foundry_pack::Node) -> GraphStep {
        let visit = match self.prepare_visit(&node.id) {
            Ok(visit) => visit,
            Err(error) => return GraphStep::Fault(error),
        };
        let activation = visit.activation.unwrap_or(self.activation);
        let Some(next_activation) = activation.checked_add(1) else {
            return GraphStep::Fault("activation overflow".into());
        };
        if let Err(error) = self.start_visit(&node.id, &visit, Some(activation)) {
            return GraphStep::Fault(error);
        }
        let intent = self.intent(activation, self.external_ref.clone());
        let mut executor = Executor::with_faults(self.intents, self.host, self.outcomes, self.faults);
        if let Some(ledger) = self.attempt_ledger { executor = executor.with_attempt_ledger(ledger); }
        match executor.run(intent) {
            Ok(RunStep::Bound(outcome)) => {
                let next = outcome.cursor.or_else(|| outcome.next_activation.map(|next| next.node))
                    .or_else(|| node.value.get("on_done").and_then(|v| v.as_str()).map(str::to_owned));
                if let Err(error) = self.append_completed(&node.id, next.clone()) {
                    return GraphStep::Fault(error);
                }
                self.activation = next_activation;
                next.map_or(GraphStep::Completed, |next| GraphStep::Advanced {
                    next, effects: vec![format!("dispatch:{}", node.id)],
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

    fn route_node(&mut self, node: &foundry_pack::Node) -> GraphStep {
        let mut visit = match self.prepare_visit(&node.id) {
            Ok(visit) => visit,
            Err(error) => return GraphStep::Fault(error),
        };
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
            // A pending visit selects its branch using the state before that
            // visit, even though counters() exposes all durable Bumps on restore.
            match eval_condition_checked(&expr, &visit.counters) {
                Some(false) => continue,
                Some(true) => {}
                None => return GraphStep::Fault(format!("route condition needs runtime context: {condition}")),
            }
            let mut actions = Vec::new();
            if let Some(declared) = branch.get("do").and_then(|v| v.as_str()) {
                for action in declared.split(';') {
                    let Ok(Expr::Call(name, args)) = parse_expr(action.trim()) else {
                        return GraphStep::Fault(format!("invalid route action: {action}"));
                    };
                    actions.push((name, args));
                }
            }
            let bumps: Vec<_> = actions.iter().filter_map(|(name, args)| match (name.as_str(), args.as_slice()) {
                ("bump", [Expr::Ident(counter)]) => Some(counter.as_str()), _ => None,
            }).collect();
            if visit.pending.len() > bumps.len() || visit.pending.iter().zip(&bumps).any(|((name, _), expected)| name != expected) {
                return GraphStep::Fault(format!("counter log action sequence violated at {}", node.id));
            }
            if let Err(error) = self.start_visit(&node.id, &visit, None) {
                return GraphStep::Fault(error);
            }
            let mut effects = Vec::new();
            let mut emitted = None;
            for (name, args) in actions {
                match (name.as_str(), args.as_slice()) {
                    ("bump", [Expr::Ident(counter)]) => {
                        let Some(value) = visit.counters.get_mut(counter) else {
                            return GraphStep::Fault(format!("unknown counter {counter}"));
                        };
                        if let Err(error) = self.bump_with_log(&node.id, counter, value, &mut visit.pending) {
                            return GraphStep::Fault(error);
                        }
                        effects.push(format!("bump:{counter}={value}"));
                    }
                    ("emit", [Expr::Ident(event), Expr::Ident(payload)]) => {
                        let result = self.emit_declared(event, payload);
                        if !matches!(result, GraphStep::Emitted { .. }) { return result; }
                        emitted = Some(result);
                    }
                    ("revert", _) => return GraphStep::Fault("revert requires a host action adapter".into()),
                    _ => return GraphStep::Fault(format!("unsupported route action: {name}")),
                }
            }
            let goto = branch.get("goto").and_then(|v| v.as_str()).map(str::to_owned);
            if let Err(error) = self.append_completed(&node.id, goto.clone()) {
                return GraphStep::Fault(error);
            }
            self.counters = visit.counters;
            return match (emitted, goto) {
                (Some(step), _) => step,
                (None, Some(next)) => GraphStep::Advanced { next, effects },
                (None, None) => GraphStep::Completed,
            };
        }
        GraphStep::Fault(format!("no route branch matched at {}", node.id))
    }

    fn redrive_node(&mut self, node: &foundry_pack::Node) -> GraphStep {
        let mut visit = match self.prepare_visit(&node.id) {
            Ok(visit) => visit,
            Err(error) => return GraphStep::Fault(error),
        };
        let activation = visit.activation.unwrap_or(self.activation.max(1));
        let Some(next_activation) = activation.checked_add(1) else {
            return GraphStep::Fault("activation overflow".into());
        };
        let bump = node.value.get("bump").and_then(|v| v.as_str());
        if visit.pending.len() > usize::from(bump.is_some()) || visit.pending.front().is_some_and(|(name, _)| Some(name.as_str()) != bump) {
            return GraphStep::Fault(format!("counter log action sequence violated at {}", node.id));
        }
        if let Err(error) = self.start_visit(&node.id, &visit, Some(activation)) {
            return GraphStep::Fault(error);
        }
        if let Some(counter) = bump {
            let Some(value) = visit.counters.get_mut(counter) else {
                return GraphStep::Fault(format!("unknown counter {counter}"));
            };
            if let Err(error) = self.bump_with_log(&node.id, counter, value, &mut visit.pending) {
                return GraphStep::Fault(error);
            }
        }
        let payload = node.value.get("payload").and_then(|v| v.as_str()).unwrap_or("");
        let intent = self.intent(activation, format!("redrive:{}:{payload}", node.id));
        let mut executor = Executor::with_faults(self.intents, self.host, self.outcomes, self.faults);
        if let Some(ledger) = self.attempt_ledger { executor = executor.with_attempt_ledger(ledger); }
        match executor.run(intent) {
            Ok(RunStep::Bound(outcome)) => {
                let next = outcome.cursor.or_else(|| outcome.next_activation.map(|next| next.node))
                    .or_else(|| node.value.get("on_done").and_then(|v| v.as_str()).map(str::to_owned));
                if let Err(error) = self.append_completed(&node.id, next.clone()) {
                    return GraphStep::Fault(error);
                }
                self.counters = visit.counters;
                self.activation = next_activation;
                next.map_or(GraphStep::Completed, |next| GraphStep::Advanced {
                    next, effects: vec![format!("redrive:{}", node.id)],
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
        let visit = match self.prepare_visit(&node.id) {
            Ok(visit) if visit.pending.is_empty() => visit,
            Ok(_) => return GraphStep::Fault(format!("unexpected pending bump at {}", node.id)),
            Err(error) => return GraphStep::Fault(error),
        };
        if let Err(error) = self.start_visit(&node.id, &visit, None) {
            return GraphStep::Fault(error);
        }
        let result = self.emit_declared(event, payload);
        if matches!(result, GraphStep::Emitted { .. })
            && let Err(error) = self.append_completed(&node.id, None)
        {
            return GraphStep::Fault(error);
        }
        result
    }

    fn replay_counters(&self, records: &[CounterRecord]) -> Result<BTreeMap<String, u64>, String> {
        let mut counters = self.pack.counters().clone();
        for record in records {
            if let CounterRecord::Bump { node_id, counter, value } = record {
                let previous = counters.get_mut(counter).ok_or_else(|| format!("unknown counter {counter}"))?;
                let expected = previous.checked_add(1).ok_or_else(|| format!("counter overflow: {counter}"))?;
                if *value != expected {
                    return Err(format!("counter log invariant violated for {node_id}/{counter}: expected {expected}, found {value}"));
                }
                *previous = *value;
            }
        }
        Ok(counters)
    }

    fn prepare_visit(&self, node_id: &str) -> Result<Visit, String> {
        let records = self.counter_log.load(&self.order.0, self.run.0);
        let durable = self.replay_counters(&records)?;
        let boundary = records.iter().rposition(|record| matches!(record,
            CounterRecord::Completed { node_id: completed, .. } if completed == node_id))
            .map_or(0, |idx| idx + 1);
        let started = records[boundary..].iter().position(|record| matches!(record,
            CounterRecord::Started { node_id: started, .. } if started == node_id))
            .map(|idx| boundary + idx);
        let first_bump = records[boundary..].iter().position(|record| matches!(record,
            CounterRecord::Bump { node_id: bumped, .. } if bumped == node_id))
            .map(|idx| boundary + idx);
        let begin = match (started, first_bump) {
            (Some(started), Some(bump)) => Some(started.min(bump)),
            (started, bump) => started.or(bump),
        };
        let counters = match begin {
            Some(idx) => self.replay_counters(&records[..idx])?,
            None if records.is_empty() => self.counters.clone(),
            None => durable,
        };
        let pending = records[begin.unwrap_or(records.len())..].iter().filter_map(|record| match record {
            CounterRecord::Bump { node_id: bumped, counter, value } if bumped == node_id => Some((counter.clone(), *value)),
            _ => None,
        }).collect();
        let activation = started.and_then(|idx| match &records[idx] {
            CounterRecord::Started { activation, .. } => *activation, _ => None,
        });
        Ok(Visit { counters, pending, started: started.is_some(), activation })
    }

    fn start_visit(&mut self, node_id: &str, visit: &Visit, activation: Option<u64>) -> Result<(), String> {
        if !visit.started {
            let key = (self.order.clone(), self.attempt, self.run);
            if activation.is_some() && !visit.pending.is_empty()
                && self.intents.latest(&key).is_some_and(|intent| self.outcomes.get(&key, intent.activation).is_some())
            {
                return Err("legacy pending host visit has no activation identity; recovery is ambiguous".into());
            }
            self.counter_log.append(&self.order.0, self.run.0, CounterRecord::Started {
                node_id: node_id.to_owned(), activation,
            }).map_err(|error| format!("counter log append failed: {error}"))?;
        }
        Ok(())
    }

    fn bump_with_log(&mut self, node_id: &str, counter: &str, value: &mut u64,
        pending: &mut VecDeque<(String, u64)>) -> Result<(), String>
    {
        let next = value.checked_add(1).ok_or_else(|| format!("counter overflow: {counter}"))?;
        if let Some((name, persisted)) = pending.pop_front() {
            if name != counter || persisted != next {
                return Err(format!("counter log invariant violated for {node_id}/{counter}: expected {next}, found {persisted}"));
            }
        } else {
            self.counter_log.append(&self.order.0, self.run.0, CounterRecord::Bump {
                node_id: node_id.to_owned(), counter: counter.to_owned(), value: next,
            }).map_err(|error| format!("counter log append failed: {error}"))?;
        }
        *value = next;
        Ok(())
    }

    fn append_completed(&mut self, node_id: &str, goto: Option<String>) -> Result<(), String> {
        self.counter_log.append(&self.order.0, self.run.0,
            CounterRecord::Completed { node_id: node_id.to_owned(), goto: goto.clone() })
            .map_err(|error| format!("counter log append failed: {error}"))?;
        self.completed = goto.is_none();
        self.cursor = goto;
        Ok(())
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
                if let Some(store) = self.tuple_store {
                    if let Err(error) = store.record(tuple) {
                        return GraphStep::Fault(format!("tuple record failed: {error:?}"));
                    }
                    if let Err(point) = self.faults.fire(CrashPoint::W5) {
                        return GraphStep::Fault(format!("injected crash: {point:?}"));
                    }
                }
                if let Err(point) = self.faults.fire(CrashPoint::W4) {
                    return GraphStep::Fault(format!("injected crash: {point:?}"));
                }
                complete_after_w4(self.emit_log, &key);
                GraphStep::Emitted { receipt }
            }
            EmitOutcome::Existing(receipt) => {
                if let Some(store) = self.tuple_store
                    && let Err(error) = store.record(tuple)
                {
                    return GraphStep::Fault(format!("tuple record failed: {error:?}"));
                }
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

#[cfg(test)]
mod review_tests {
    use super::*;
    use crate::types::{ArtifactTupleKey, EmitReceipt, ExecutionOutcome, PromotionReceipt};

    type Key = (OrderId, Attempt, RunId);
    #[derive(Default)]
    struct Log(Vec<CounterRecord>);
    impl CounterLog for Log {
        fn append(&mut self, order: &str, run: u64, record: CounterRecord) -> Result<(), String> {
            assert_eq!((order, run), ("cycle", 1));
            self.0.push(record);
            Ok(())
        }
        fn load(&self, order: &str, run: u64) -> Vec<CounterRecord> {
            assert_eq!((order, run), ("cycle", 1));
            self.0.clone()
        }
    }
    struct Unused;
    impl IntentLog for Unused {
        fn append(&mut self, _: &ExecutionIntent) { panic!("route must not dispatch") }
        fn mark_started(&mut self, _: &Key, _: u64) { panic!("route must not dispatch") }
        fn latest(&self, _: &Key) -> Option<ExecutionIntent> { None }
        fn get(&self, _: &Key, _: u64) -> Option<ExecutionIntent> { None }
        fn is_started(&self, _: &Key, _: u64) -> bool { false }
    }
    impl OutcomeStore for Unused {
        fn commit(&mut self, _: &ExecutionOutcome) { panic!("route has no outcome") }
        fn latest(&self, _: &Key) -> Option<ExecutionOutcome> { None }
        fn get(&self, _: &Key, _: u64) -> Option<ExecutionOutcome> { None }
    }
    impl DispatchHost for Unused {
        type Result = DispatchResult;
        fn dispatch(&mut self, _: &ExecutionIntent) -> DispatchResult { panic!("route must not dispatch") }
        fn dispatch_count(&self) -> u64 { 0 }
        fn recover_result(&mut self, _: &ExecutionIntent) -> Option<DispatchResult> { None }
    }
    impl EmitSink for Unused {
        fn publish(&mut self, _: &ArtifactTuple) -> EmitReceipt { panic!("route must not emit") }
        fn receipt_of(&self, _: &ArtifactTupleKey) -> Option<EmitReceipt> { None }
    }
    impl EmitLog for Unused {
        fn current_epoch(&self) -> Epoch { Epoch(0) }
        fn promotion_of(&self, _: &ArtifactTupleKey) -> Option<PromotionReceipt> { None }
        fn emit_of(&self, _: &ArtifactTupleKey) -> Option<EmitReceipt> { None }
        fn record_emit(&mut self, _: &EmitReceipt) { panic!("route must not emit") }
        fn mark_complete(&mut self, _: &ArtifactTupleKey) { panic!("route must not emit") }
        fn is_complete(&self, _: &ArtifactTupleKey) -> bool { false }
    }

    fn driver<'a>(pack: &'a PackSnapshot, log: &'a mut Log, intents: &'a mut Unused,
        host: &'a mut Unused, outcomes: &'a mut Unused, sink: &'a mut Unused, emits: &'a mut Unused)
        -> GraphDriver<'a, Unused, Unused, Unused, Log, Unused, Unused>
    {
        GraphDriver::restore(pack, intents, host, outcomes, log, sink, emits,
            OrderId("cycle".into()), Attempt(1), RunId(1), Generation(1), Epoch(0), "cycle", None)
    }

    fn node(pack: &PackSnapshot, id: &str, goto: &str) -> foundry_pack::Node {
        let mut node = pack.nodes().iter().find(|node| node.id == "route1").unwrap().clone();
        node.id = id.into();
        let branches = node.value.get_mut("branches").unwrap().as_array_mut().unwrap();
        branches.truncate(1);
        let branch = branches[0].as_table_mut().unwrap();
        branch.insert("when".into(), "true".into());
        branch.insert("do".into(), "bump(fix_rounds)".into());
        branch.insert("goto".into(), goto.into());
        node
    }

    // PackSnapshot is private and the loader forbids route cycles. Exercise the
    // real driver with directly constructed Nodes; assert cursor before invoking
    // the node, so explicit advance calls cannot mask a restore mistake.
    #[test]
    fn b2c_direct_self_and_two_node_cycles_restore_every_visit() {
        let pack = foundry_pack::load_pack(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../skills/bmad-review-full/v1")).unwrap();
        for two_nodes in [false, true] {
            let n = node(&pack, "n", if two_nodes { "x" } else { "n" });
            let x = node(&pack, "x", "n");
            let mut log = Log::default();
            let mut expected = "n";
            for visit in 0..6 {
                let (mut intents, mut host, mut outcomes, mut sink, mut emits) = (Unused, Unused, Unused, Unused, Unused);
                let mut driver = driver(&pack, &mut log, &mut intents, &mut host, &mut outcomes, &mut sink, &mut emits);
                if visit > 0 { assert_eq!(driver.cursor.as_deref(), Some(expected)); }
                assert!(!driver.completed);
                assert_eq!(driver.counters()["fix_rounds"], visit);
                let current = if expected == "n" { &n } else { &x };
                let GraphStep::Advanced { next, .. } = driver.route_node(current) else { panic!("cycle must execute") };
                assert_eq!(driver.counters()["fix_rounds"], visit + 1);
                expected = if next == "n" { "n" } else { "x" };
            }
            assert_eq!(log.0.len(), 18);
            for records in log.0.chunks_exact(3) {
                assert!(matches!(records[0], CounterRecord::Started { .. }));
                assert!(matches!(records[1], CounterRecord::Bump { .. }));
                assert!(matches!(records[2], CounterRecord::Completed { .. }));
            }
        }
    }

    #[test]
    fn b2c_restore_empty_pending_completed_chain_and_self_loop() {
        let pack = foundry_pack::load_pack(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../skills/bmad-review-full/v1")).unwrap();
        let cases = [
            (vec![], None, false, 0),
            (vec![CounterRecord::Bump { node_id: "n".into(), counter: "fix_rounds".into(), value: 1 }], Some("n"), false, 1),
            (vec![CounterRecord::Completed { node_id: "n".into(), goto: Some("x".into()) },
                CounterRecord::Completed { node_id: "x".into(), goto: None }], None, true, 0),
            (vec![CounterRecord::Completed { node_id: "n".into(), goto: Some("n".into()) }], Some("n"), false, 0),
        ];
        for (records, cursor, completed, count) in cases {
            let mut log = Log(records);
            let (mut intents, mut host, mut outcomes, mut sink, mut emits) = (Unused, Unused, Unused, Unused, Unused);
            let driver = driver(&pack, &mut log, &mut intents, &mut host, &mut outcomes, &mut sink, &mut emits);
            assert_eq!(driver.cursor.as_deref(), cursor);
            assert_eq!(driver.completed, completed);
            assert_eq!(driver.counters()["fix_rounds"], count);
        }
    }
}
