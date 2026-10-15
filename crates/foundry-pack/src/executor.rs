use crate::{eval_condition_checked, parse_expr, Expr, Node, PackError, PackSnapshot, Result};
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineAction {
    Bump { counter: String, after: u64 },
    Revert { base: String },
    Emit { event: String, payload: String },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HoldEntry {
    pub node_id: String,
    pub entry_counters: BTreeMap<String, u64>,
    pub reverted_base: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepOutcome {
    pub destinations: Vec<String>,
    pub actions: Vec<EngineAction>,
    pub hold: Option<HoldEntry>,
    pub terminal: bool,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RunInput {
    pub verdicts: Vec<String>,
    pub has_findings: bool,
    pub budget_exhausted: bool,
    pub timeout: bool,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineState {
    pub counters: BTreeMap<String, u64>,
    current: Option<String>,
    hold: Option<HoldEntry>,
    reverted_base: Option<String>,
    last_input: Option<RunInput>,
    active_hold_target: Option<String>,
    verify_evidence: bool,
}

#[derive(Clone, Debug)]
enum Kind {
    Dispatch { on_done: String, hold_target: String, verify: bool, on_invalid: String, on_no_valid: String },
    Route { branches: Vec<Branch> },
    Redrive { on_done: String, hold_target: String, bump: Option<String>, on_error: String },
    Emit { event: String, payload: String },
    Hold { resume_targets: Vec<String> },
}
#[derive(Clone, Debug)]
struct Branch {
    condition: Expr,
    goto: Option<String>,
    actions: Vec<Action>,
}
#[derive(Clone, Debug)]
enum Action { Bump(String), Revert(String), Emit(String, String) }

#[derive(Clone, Debug)]
pub struct GraphEngine {
    nodes: HashMap<String, Kind>,
    entry: String,
    declared_counters: BTreeMap<String, u64>,
}

impl GraphEngine {
    pub fn from_snapshot(snap: &PackSnapshot) -> Result<Self> {
        let entry = snap.policy().get("entry").and_then(toml::Value::as_str)
            .ok_or_else(|| exec_error("E_EXEC_ENTRY", "missing entry"))?.to_owned();
        let mut nodes = HashMap::new();
        for node in snap.nodes() {
            let kind = parse_node(node)?;
            if nodes.insert(node.id.clone(), kind).is_some() {
                return Err(exec_error("E_EXEC_NODE_DUPLICATE", format!("duplicate node {}", node.id)));
            }
        }
        if !matches!(nodes.get(&entry), Some(Kind::Dispatch { .. })) {
            return Err(exec_error("E_EXEC_ENTRY", format!("entry {entry} is not a dispatch node")));
        }
        for (id, kind) in &nodes {
            for target in targets(kind) {
                if !nodes.contains_key(target) {
                    return Err(exec_error("E_EXEC_UNKNOWN_NODE", format!("{id} references unknown node {target}")));
                }
            }
        }
        Ok(Self { nodes, entry, declared_counters: snap.counters().clone() })
    }

    pub fn initial_state(&self, inherited_counters: BTreeMap<String, u64>) -> EngineState {
        let counters = self.declared_counters.keys().map(|name|
            (name.clone(), inherited_counters.get(name).copied().unwrap_or(0))).collect();
        EngineState { counters, current: None, hold: None, reverted_base: None,
            last_input: None, active_hold_target: None, verify_evidence: false }
    }

    pub fn run(&self, state: &mut EngineState, input: &RunInput, max_steps: u32) -> Result<StepOutcome> {
        if state.hold.is_some() { return Err(exec_error("E_EXEC_HELD", "held execution requires explicit resume")); }
        state.last_input = Some(input.clone());
        let current = state.current.clone().unwrap_or_else(|| self.entry.clone());
        self.execute(state, input, current, max_steps)
    }

    /// Run the graph while yielding each dispatch/redrive boundary to a host.
    /// The returned input from a dispatch becomes the input used by subsequent
    /// routes. Redrive callbacks are for pack-declared pre-stages and their
    /// returned input is intentionally ignored.
    pub fn run_staged<F>(&self, state: &mut EngineState, input: &RunInput, max_steps: u32, mut execute: F) -> Result<StepOutcome>
    where F: FnMut(&str, bool, &RunInput) -> Result<RunInput> {
        self.run_staged_checkpointed(state, input, max_steps, |node, redrive, input, _| execute(node, redrive, input))
    }

    /// The host persists the counters before beginning each external boundary.
    pub fn run_staged_checkpointed<F>(&self, state: &mut EngineState, input: &RunInput, max_steps: u32, mut execute: F) -> Result<StepOutcome>
    where F: FnMut(&str, bool, &RunInput, &BTreeMap<String, u64>) -> Result<RunInput> {
        if state.hold.is_some() { return Err(exec_error("E_EXEC_HELD", "held execution requires explicit resume")); }
        let current = state.current.clone().unwrap_or_else(|| self.entry.clone());
        self.execute_staged(state, input, current, max_steps, &mut execute)
    }

    pub fn resume(&self, state: &mut EngineState, target_node: &str) -> Result<StepOutcome> {
        self.resume_staged(state, target_node, |_, _, input, _| Ok(input.clone()))
    }

    pub fn resume_staged<F>(&self, state: &mut EngineState, target_node: &str, mut execute: F) -> Result<StepOutcome>
    where F: FnMut(&str, bool, &RunInput, &BTreeMap<String, u64>) -> Result<RunInput> {
        let held = state.hold.as_ref().ok_or_else(|| exec_error("E_EXEC_NOT_HELD", "resume requires a hold state"))?;
        let allowed = match self.nodes.get(&held.node_id) {
            Some(Kind::Hold { resume_targets }) => resume_targets,
            _ => return Err(exec_error("E_EXEC_STATE", "held node is not a hold node")),
        };
        if !allowed.iter().any(|target| target == target_node) {
            return Err(exec_error("E_EXEC_RESUME_TARGET", format!("{target_node} is not allowed from {}", held.node_id)));
        }
        if !self.nodes.contains_key(target_node) {
            return Err(exec_error("E_EXEC_UNKNOWN_NODE", format!("unknown node {target_node}")));
        }
        if state.last_input.as_ref().is_some_and(|input|input.budget_exhausted) {
            return Err(exec_error("E_EXEC_BUDGET", "execution cannot resume an exhausted budget"));
        }
        let mut input = state.last_input.clone().ok_or_else(|| exec_error("E_EXEC_STATE", "missing held run input"))?;
        input.timeout = false; // A human resume starts a fresh execution attempt.
        state.current = Some(target_node.to_owned());
        state.hold = None;
        let mut outcome = self.execute_staged(state, &input, target_node.to_owned(), 10_000, &mut execute)?;
        outcome.destinations.insert(0, target_node.to_owned());
        Ok(outcome)
    }

    fn execute(&self, state: &mut EngineState, input: &RunInput, current: String, max_steps: u32) -> Result<StepOutcome> {
        self.execute_staged(state, input, current, max_steps, &mut |_, _, input, _| Ok(input.clone()))
    }

    fn execute_staged<F>(&self, state: &mut EngineState, input: &RunInput, mut current: String, max_steps: u32, execute_stage: &mut F) -> Result<StepOutcome>
    where F: FnMut(&str, bool, &RunInput, &BTreeMap<String, u64>) -> Result<RunInput> {
        let mut input = input.clone();
        state.last_input = Some(input.clone());
        let mut outcome = StepOutcome { destinations: Vec::new(), actions: Vec::new(), hold: None, terminal: false };
        for _ in 0..max_steps {
            let kind = self.nodes.get(&current).ok_or_else(|| exec_error("E_EXEC_UNKNOWN_NODE", format!("unknown node {current}")))?;
            match kind {
                Kind::Dispatch { on_done, hold_target, verify, on_invalid, on_no_valid } => {
                    state.active_hold_target = Some(hold_target.clone());
                    if *verify { state.verify_evidence = false; }
                    if input.budget_exhausted || input.timeout { return self.overflow_hold(state, &mut outcome, hold_target); }
                    input = match execute_stage(&current, false, &input, &state.counters) {
                        Ok(output) => output,
                        Err(error) => {
                            let policy = match error.code {
                                "E_EXEC_VERIFY_INVALID" => on_invalid,
                                "E_EXEC_NO_VALID" | "E_EXEC_ALL_ERROR" => on_no_valid,
                                "E_EXEC_BUDGET" => "hold",
                                _ => return Err(error),
                            };
                            if policy == "hold" { return self.overflow_hold(state, &mut outcome, hold_target); }
                            return Err(error);
                        }
                    };
                    state.last_input = Some(input.clone());
                    if *verify {
                        state.verify_evidence = true;
                        state.reverted_base = None;
                    }
                    visit(&mut outcome, &current);
                    current = on_done.clone();
                }
                Kind::Redrive { on_done, hold_target, bump, on_error } => {
                    state.active_hold_target = Some(hold_target.clone());
                    state.verify_evidence = false;
                    if input.budget_exhausted || input.timeout { return self.overflow_hold(state, &mut outcome, hold_target); }
                    if let Err(error) = execute_stage(&current, true, &input, &state.counters) {
                        if error.code == "E_EXEC_BUDGET" || (error.code == "E_EXEC_REDRIVE_ERROR" && on_error == "hold") {
                            return self.overflow_hold(state, &mut outcome, hold_target);
                        }
                        return Err(error);
                    }
                    if let Some(counter) = bump
                        && let Err(error) = bump_counter(state, &mut outcome.actions, counter)
                    {
                        if error.code != "E_EXEC_COUNTER_OVERFLOW" { return Err(error); }
                        return self.overflow_hold(state, &mut outcome, hold_target);
                    }
                    current = on_done.clone();
                }
                Kind::Route { branches } => {
                    let mut selected = None;
                    for branch in branches {
                        let matches = evaluate(&branch.condition, &input, &state.counters)
                            .ok_or_else(|| exec_error("E_EXEC_CONDITION", format!("unbound condition at {current}")))?;
                        if matches { selected = Some(branch); break; }
                    }
                    let branch = selected.ok_or_else(|| exec_error("E_EXEC_NO_BRANCH", format!("no matching branch at {current}")))?;
                    for action in &branch.actions {
                        match action {
                            Action::Bump(counter) => {
                                if let Err(error) = bump_counter(state, &mut outcome.actions, counter) {
                                    if error.code != "E_EXEC_COUNTER_OVERFLOW" { return Err(error); }
                                    let target = state.active_hold_target.clone().ok_or(error)?;
                                    return self.overflow_hold(state, &mut outcome, &target);
                                }
                            }
                            Action::Revert(base) => {
                                state.reverted_base = Some(base.clone());
                                state.verify_evidence = false;
                                outcome.actions.push(EngineAction::Revert { base: base.clone() });
                            }
                            Action::Emit(event, payload) => {
                                if state.reverted_base.is_some() { return Err(exec_error("E_EXEC_REVERT_EMIT", "emit after revert requires revalidation")); }
                                if event == "InspectionPassed" && (input.has_findings || !state.verify_evidence) {
                                    let target = state.active_hold_target.clone().ok_or_else(|| exec_error("E_EXEC_PASS_FINDINGS", "pass has no active hold target"))?;
                                    return self.overflow_hold(state, &mut outcome, &target);
                                }
                                outcome.actions.push(EngineAction::Emit { event: event.clone(), payload: payload.clone() });
                                outcome.terminal = true;
                                state.current = None;
                                return Ok(outcome);
                            }
                        }
                    }
                    let dest = branch.goto.as_ref().ok_or_else(|| exec_error("E_EXEC_EMIT_GOTO", "emit branch must not have goto"))?;
                    let destination = if matches!(self.nodes.get(dest), Some(Kind::Hold { .. })) { format!("hold:{dest}") } else { dest.clone() };
                    visit(&mut outcome, &destination);
                    current = dest.clone();
                    if matches!(self.nodes.get(&current), Some(Kind::Hold { .. })) {
                        let hold = HoldEntry { node_id: current.clone(), entry_counters: state.counters.clone(), reverted_base: state.reverted_base.clone() };
                        outcome.hold = Some(hold.clone());
                        state.hold = Some(hold);
                        state.current = Some(current);
                        return Ok(outcome);
                    }
                    if matches!(self.nodes.get(&current), Some(Kind::Emit { .. })) { /* next step emits */ }
                }
                Kind::Emit { event, payload } => {
                    if state.reverted_base.is_some() { return Err(exec_error("E_EXEC_REVERT_EMIT", "emit after revert requires revalidation")); }
                    if event == "InspectionPassed" && (input.has_findings || !state.verify_evidence) {
                        let target = state.active_hold_target.clone().ok_or_else(|| exec_error("E_EXEC_PASS_FINDINGS", "pass with findings has no active hold target"))?;
                        visit(&mut outcome, &format!("hold:{target}"));
                        let hold = HoldEntry { node_id: target, entry_counters: state.counters.clone(), reverted_base: state.reverted_base.clone() };
                        outcome.hold = Some(hold.clone()); state.hold = Some(hold); state.current = outcome.hold.as_ref().map(|h| h.node_id.clone());
                        return Ok(outcome);
                    }
                    outcome.actions.push(EngineAction::Emit { event: event.clone(), payload: payload.clone() });
                    outcome.terminal = true; state.current = None; return Ok(outcome);
                }
                Kind::Hold { .. } => {
                    visit(&mut outcome, &format!("hold:{current}"));
                    let hold = HoldEntry { node_id: current.clone(), entry_counters: state.counters.clone(), reverted_base: state.reverted_base.clone() };
                    outcome.hold = Some(hold.clone()); state.hold = Some(hold); state.current = Some(current); return Ok(outcome);
                }
            }
        }
        Err(exec_error("E_EXEC_MAX_STEPS", format!("execution exceeded {max_steps} steps")))
    }

    fn overflow_hold(&self, state: &mut EngineState, outcome: &mut StepOutcome, target: &str) -> Result<StepOutcome> {
        if !matches!(self.nodes.get(target), Some(Kind::Hold { .. })) {
            return Err(exec_error("E_EXEC_COUNTER_OVERFLOW", format!("overflow hold target {target} is not a hold node")));
        }
        visit(outcome, &format!("hold:{target}"));
        let hold = HoldEntry { node_id: target.to_owned(), entry_counters: state.counters.clone(), reverted_base: state.reverted_base.clone() };
        outcome.hold = Some(hold.clone());
        state.hold = Some(hold);
        state.current = Some(target.to_owned());
        Ok(outcome.clone())
    }
}

impl EngineState {
    pub fn export(&self) -> String {
        serde_json::to_string(self).expect("EngineState serialization is infallible")
    }

    pub fn import(json: &str) -> std::result::Result<EngineState, String> {
        serde_json::from_str(json).map_err(|error| error.to_string())
    }
}

fn parse_node(n: &Node) -> Result<Kind> {
    let str_field = |name: &str| n.value.get(name).and_then(toml::Value::as_str).map(str::to_owned)
        .ok_or_else(|| exec_error("E_EXEC_SCHEMA", format!("{} missing string field {name}", n.id)));
    let kind = match n.kind.as_str() {
        "dispatch" => {
            let stages = n.value.get("stages").and_then(toml::Value::as_array).filter(|a| !a.is_empty());
            if stages.is_none() { return Err(exec_error("E_EXEC_SCHEMA", format!("{} has no stages", n.id))); }
            let verify = stages.unwrap().iter().any(|stage| stage.get("role").and_then(toml::Value::as_str) == Some("verify"));
            Kind::Dispatch { on_done: str_field("on_done")?, hold_target: str_field("hold_target")?, verify,
                on_invalid: str_field("on_verify_invalid")?, on_no_valid: str_field("on_no_valid")? }
        }
        "route" => {
            let list = n.value.get("branches").and_then(toml::Value::as_array).ok_or_else(|| exec_error("E_EXEC_SCHEMA", format!("{} has no branches", n.id)))?;
            let mut branches = Vec::new();
            for b in list {
                let when = b.get("when").and_then(toml::Value::as_str).ok_or_else(|| exec_error("E_EXEC_SCHEMA", "branch missing when"))?;
                let actions = b.get("do").and_then(toml::Value::as_str).map(parse_actions).transpose()?.unwrap_or_default();
                branches.push(Branch { condition: parse_expr(when).map_err(|e| exec_error("E_EXEC_CONDITION", e.to_string()))?,
                    goto: b.get("goto").and_then(toml::Value::as_str).map(str::to_owned), actions });
            }
            if !branches.last().is_some_and(|b| matches!(b.condition, Expr::Bool(true))) { return Err(exec_error("E_EXEC_FALLBACK", format!("{} lacks true fallback", n.id))); }
            Kind::Route { branches }
        }
        "redrive" => Kind::Redrive { on_done: str_field("on_done")?, hold_target: str_field("hold_target")?, bump: n.value.get("bump").and_then(toml::Value::as_str).map(str::to_owned), on_error: str_field("on_redrive_error")? },
        "emit" => Kind::Emit { event: str_field("event")?, payload: str_field("payload_from")? },
        "hold" => {
            let list = n.value.get("resume_targets").and_then(toml::Value::as_array).ok_or_else(|| exec_error("E_EXEC_SCHEMA", format!("{} has no resume_targets", n.id)))?;
            Kind::Hold { resume_targets: list.iter().map(|v| v.as_str().map(str::to_owned).ok_or_else(|| exec_error("E_EXEC_SCHEMA", "resume target must be string"))).collect::<Result<_>>()? }
        }
        other => return Err(exec_error("E_EXEC_KIND", format!("unsupported node kind {other}"))),
    };
    Ok(kind)
}

fn targets(kind: &Kind) -> Vec<&str> {
    match kind {
        Kind::Dispatch { on_done, hold_target, .. } | Kind::Redrive { on_done, hold_target, .. } => vec![on_done, hold_target],
        Kind::Route { branches } => branches.iter().flat_map(|b| b.goto.iter().map(String::as_str)).collect(),
        Kind::Hold { resume_targets } => resume_targets.iter().map(String::as_str).collect(),
        Kind::Emit { .. } => vec![],
    }
}
fn parse_actions(s: &str) -> Result<Vec<Action>> {
    let mut result = Vec::new();
    for part in s.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        let (name, args) = part.split_once('(').ok_or_else(|| exec_error("E_EXEC_ACTION", format!("invalid action {part}")))?;
        let args = args.strip_suffix(')').ok_or_else(|| exec_error("E_EXEC_ACTION", format!("invalid action {part}")))?;
        let vals = args.split(',').map(|s| s.trim().trim_matches('"').to_owned()).collect::<Vec<_>>();
        let action = match (name.trim(), vals.as_slice()) {
            ("bump", [counter]) => Action::Bump(counter.clone()),
            ("revert", [base]) if base == "origin" || base == "latest" => Action::Revert(base.clone()),
            ("emit", [event, payload]) => Action::Emit(event.clone(), payload.clone()),
            _ => return Err(exec_error("E_EXEC_ACTION", format!("invalid action {part}"))),
        };
        result.push(action);
    }
    if result.iter().filter(|a| matches!(a, Action::Emit(..))).count() > 1 { return Err(exec_error("E_EXEC_MULTI_EMIT", "multiple emit actions")); }
    Ok(result)
}
fn evaluate(expr: &Expr, input: &RunInput, counters: &BTreeMap<String, u64>) -> Option<bool> {
    match expr {
        Expr::Bool(v) => Some(*v), Expr::Not(e) => evaluate(e,input,counters).map(|v| !v),
        Expr::And(a,b) => { if !evaluate(a,input,counters)? { Some(false) } else { evaluate(b,input,counters) } },
        Expr::Or(a,b) => { if evaluate(a,input,counters)? { Some(true) } else { evaluate(b,input,counters) } },
        Expr::Call(name,args) => match name.as_str() {
            "count_lt" => eval_condition_checked(expr,counters),
            "any_in" => if let [Expr::Ident(_), Expr::List(values)] = args.as_slice() { Some(values.iter().any(|v| matches!(v,Expr::String(s) if input.verdicts.contains(s)))) } else { None },
            "has_findings" => Some(input.has_findings), "timeout" => Some(input.timeout), "budget_exhausted" => Some(input.budget_exhausted), _ => None,
        }, _ => None,
    }
}
fn bump_counter(state: &mut EngineState, actions: &mut Vec<EngineAction>, counter: &str) -> Result<()> {
    let value = state.counters.get_mut(counter).ok_or_else(|| exec_error("E_EXEC_COUNTER", format!("undeclared counter {counter}")))?;
    *value = value.checked_add(1).ok_or_else(|| exec_error("E_EXEC_COUNTER_OVERFLOW", format!("counter {counter} overflow")))?;
    actions.push(EngineAction::Bump { counter: counter.to_owned(), after: *value }); Ok(())
}
fn visit(outcome: &mut StepOutcome, node: &str) { outcome.destinations.push(node.to_owned()); }
fn exec_error(code: &'static str, message: impl Into<String>) -> PackError { PackError::new(code, message) }

#[cfg(test)]
mod executor_error_tests {
    use super::*;

    fn state() -> EngineState {
        EngineState { counters: BTreeMap::new(), current: None, hold: None,
            reverted_base: None, last_input: None, active_hold_target: None,
            verify_evidence: false }
    }
    fn input() -> RunInput {
        RunInput { verdicts: Vec::new(), has_findings: false,
            budget_exhausted: false, timeout: false }
    }

    #[test]
    fn rejects_emit_after_revert_action() {
        let engine = GraphEngine {
            nodes: HashMap::from([("route".into(), Kind::Route { branches: vec![Branch {
                condition: Expr::Bool(true), goto: None,
                actions: vec![Action::Revert("origin".into()), Action::Emit("InspectionPassed".into(), "summary".into())],
            }] })]), entry: "route".into(), declared_counters: BTreeMap::new(),
        };
        assert_eq!(engine.run(&mut state(), &input(), 2).unwrap_err().code, "E_EXEC_REVERT_EMIT");
    }

    #[test]
    fn rejects_unknown_transition_destination() {
        let engine = GraphEngine {
            nodes: HashMap::from([("route".into(), Kind::Route { branches: vec![Branch {
                condition: Expr::Bool(true), goto: Some("missing".into()), actions: Vec::new(),
            }] })]), entry: "route".into(), declared_counters: BTreeMap::new(),
        };
        assert_eq!(engine.run(&mut state(), &input(), 2).unwrap_err().code, "E_EXEC_UNKNOWN_NODE");
    }

    #[test]
    fn inline_pass_requires_the_same_evidence_gate_as_an_emit_node() {
        let engine=GraphEngine{
            nodes:HashMap::from([
                ("route".into(),Kind::Route{branches:vec![Branch{condition:Expr::Bool(true),goto:None,
                    actions:vec![Action::Emit("InspectionPassed".into(),"summary".into())]}]}),
                ("human".into(),Kind::Hold{resume_targets:vec![]}),
            ]),entry:"route".into(),declared_counters:BTreeMap::new(),
        };
        for (evidence,findings,passed) in [(false,false,false),(true,true,false),(true,false,true)] {
            let mut state=state();state.verify_evidence=evidence;state.active_hold_target=Some("human".into());
            let input=RunInput{has_findings:findings,..input()};
            let outcome=engine.run(&mut state,&input,4).unwrap();
            assert_eq!(outcome.terminal,passed);
            assert_eq!(outcome.actions.iter().any(|a|matches!(a,EngineAction::Emit{..})),passed);
            assert_eq!(outcome.hold.is_some(),!passed);
        }
    }

    #[test]
    fn initial_counters_reset_then_inherit_only_declared_names() {
        let engine=GraphEngine{nodes:HashMap::new(),entry:String::new(),declared_counters:BTreeMap::from([("count".into(),9)])};
        assert_eq!(engine.initial_state(BTreeMap::new()).counters["count"],0);
        assert_eq!(engine.initial_state(BTreeMap::from([("count".into(),4),("unknown".into(),99)])).counters,
            BTreeMap::from([("count".into(),4)]));
    }

    #[test]
    fn unchanged_artifact_keeps_verify_evidence_through_a_later_lens_dispatch() {
        let engine=GraphEngine{nodes:HashMap::from([
            ("verify".into(),Kind::Dispatch{on_done:"lens".into(),hold_target:"human".into(),verify:true,on_invalid:"hold".into(),on_no_valid:"hold".into()}),
            ("lens".into(),Kind::Dispatch{on_done:"pass".into(),hold_target:"human".into(),verify:false,on_invalid:"hold".into(),on_no_valid:"hold".into()}),
            ("pass".into(),Kind::Emit{event:"InspectionPassed".into(),payload:"summary".into()}),
            ("human".into(),Kind::Hold{resume_targets:vec![]}),
        ]),entry:"verify".into(),declared_counters:BTreeMap::new()};
        let outcome=engine.run_staged(&mut state(),&input(),8,|_,_,input|Ok(input.clone())).unwrap();
        assert!(outcome.terminal);assert!(outcome.hold.is_none());
    }
}
