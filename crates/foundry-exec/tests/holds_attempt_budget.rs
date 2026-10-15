use std::{collections::BTreeMap, fs, path::PathBuf};

use foundry_exec::{
    AttemptBudget, HoldApplied, HoldCommand, HoldInstanceId, HoldOperation, HoldState, HoldStatus,
    apply_hold,
    ports::{AttemptLedger, HoldStore, PortError},
    types::{Attempt, OrderId, RunId},
};
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
struct HoldDb {
    states: BTreeMap<String, HoldState>,
    commands: Vec<HoldCommand>,
}

fn key(instance: &HoldInstanceId) -> String {
    format!("{}:{}:{}:{}:{}", instance.order.0, instance.attempt.0, instance.run.0, instance.hold_node, instance.entry)
}

struct DiskHolds(PathBuf);
impl DiskHolds {
    fn read(&self) -> HoldDb {
        fs::read(&self.0).map(|bytes| serde_json::from_slice(&bytes).unwrap()).unwrap_or_default()
    }
    fn write(&self, db: &HoldDb) -> Result<(), PortError> {
        fs::write(&self.0, serde_json::to_vec(db).map_err(|e| PortError::Store(e.to_string()))?)
            .map_err(|e| PortError::Store(e.to_string()))
    }
}
impl HoldStore for DiskHolds {
    fn open(&self, instance: &HoldInstanceId, resume_to: &str) -> Result<bool, PortError> {
        let mut db = self.read();
        let k = key(instance);
        if db.states.contains_key(&k) { return Ok(false); }
        db.states.insert(k, HoldState { instance: instance.clone(), resume_to: resume_to.into(), status: HoldStatus::Waiting });
        self.write(&db)?;
        Ok(true)
    }
    fn apply(&self, cmd: &HoldCommand, expected: &HoldInstanceId) -> Result<HoldApplied, PortError> {
        let mut db = self.read();
        let k = key(expected);
        let state = db.states.get(&k).cloned().ok_or(PortError::MissingHold)?;
        let applied = apply_hold(&state, cmd, expected)?;
        if cmd.op == HoldOperation::Info { return Ok(HoldApplied::Info); }
        if matches!(state.status, HoldStatus::Resolved(_)) {
            if db.commands.iter().rev().find(|last| key(&last.instance) == k).is_some_and(|last| last == cmd)
                && let HoldStatus::Resolved(applied) = state.status
            {
                return Ok(applied);
            }
            return Err(PortError::AlreadyResolved);
        }
        if matches!(applied, HoldApplied::Amended { .. }) {
            if let HoldApplied::Amended { resume_to } = &applied { db.states.get_mut(&k).unwrap().resume_to = resume_to.clone(); }
        } else if !matches!(applied, HoldApplied::Info) {
            db.states.get_mut(&k).unwrap().status = HoldStatus::Resolved(applied.clone());
        }
        let mut bound = cmd.clone();
        if !matches!(cmd.op, HoldOperation::Approve | HoldOperation::Reject) { bound.human_resolution = None; }
        db.commands.push(bound);
        self.write(&db)?;
        Ok(applied)
    }
    fn get(&self, instance: &HoldInstanceId) -> Result<Option<HoldState>, PortError> {
        Ok(self.read().states.get(&key(instance)).cloned())
    }
}

#[derive(Serialize, Deserialize, Default)]
struct AttemptDb(BTreeMap<String, Vec<u64>>);
struct DiskAttempts { path: PathBuf, budget: AttemptBudget }
impl DiskAttempts {
    fn read(&self) -> AttemptDb {
        fs::read(&self.path).map(|bytes| serde_json::from_slice(&bytes).unwrap()).unwrap_or_default()
    }
}
impl AttemptLedger for DiskAttempts {
    fn consume(&self, order: &OrderId, attempt: Attempt) -> Result<(), PortError> {
        let mut db = self.read();
        let attempts = db.0.entry(order.0.clone()).or_default();
        if attempts.contains(&attempt.0) { return Ok(()); }
        if attempts.len() >= self.budget.max_attempts as usize {
            return Err(PortError::AttemptBudgetExhausted { max_attempts: self.budget.max_attempts });
        }
        attempts.push(attempt.0);
        fs::write(&self.path, serde_json::to_vec(&db).unwrap()).map_err(|e| PortError::Store(e.to_string()))
    }
    fn consumed(&self, order: &OrderId) -> Result<u32, PortError> {
        Ok(self.read().0.get(&order.0).map_or(0, |attempts| attempts.len() as u32))
    }
}

fn instance(entry: u64) -> HoldInstanceId {
    HoldInstanceId { order: OrderId("o".into()), attempt: Attempt(1), run: RunId(2), hold_node: "approval".into(), entry }
}
fn command(instance: &HoldInstanceId, op: HoldOperation, evidence: &str) -> HoldCommand {
    HoldCommand { instance: instance.clone(), op, human_resolution: Some("ticket-17".into()), evidence: evidence.into() }
}
fn temp_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!("foundry-{label}-{}-{}", std::process::id(), rand_suffix()))
}
fn rand_suffix() -> u128 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() }

#[test]
fn hold_cas_transitions_evidence_reentry_info_and_human_resolution() {
    let id = instance(0);
    let other = instance(1);
    let path = temp_path("hold-cas");
    let store = DiskHolds(path.clone());
    assert!(store.open(&id, "next").unwrap());
    assert!(!store.open(&id, "ignored").unwrap());
    assert!(store.open(&other, "next").unwrap());
    assert_eq!(store.apply(&command(&id, HoldOperation::Info, ""), &other).unwrap_err(),
        PortError::HoldConflict(foundry_exec::HoldCasOutcome::Conflict { observed_instance: id.clone() }));
    assert_eq!(store.apply(&command(&id, HoldOperation::Approve, " \n "), &id).unwrap_err(), PortError::EvidenceRequired);
    assert_eq!(store.apply(&command(&id, HoldOperation::Info, ""), &id).unwrap(), HoldApplied::Info);
    assert_eq!(store.get(&id).unwrap().unwrap().status, HoldStatus::Waiting);
    assert_eq!(store.apply(&command(&id, HoldOperation::Amend, "changed"), &id).unwrap(), HoldApplied::Amended { resume_to: "changed".into() });
    assert_eq!(store.apply(&command(&id, HoldOperation::Amend, "changed"), &id).unwrap(), HoldApplied::Amended { resume_to: "changed".into() });
    assert_eq!(store.get(&id).unwrap().unwrap().resume_to, "changed");
    let approved = command(&id, HoldOperation::Approve, "evidence");
    assert_eq!(store.apply(&approved, &id).unwrap(), HoldApplied::Approved);
    assert_eq!(store.apply(&approved, &id).unwrap(), HoldApplied::Approved);
    assert_eq!(store.apply(&command(&id, HoldOperation::Approve, "changed evidence"), &id).unwrap_err(), PortError::AlreadyResolved);
    assert_eq!(store.apply(&command(&id, HoldOperation::Reject, "other"), &id).unwrap_err(), PortError::AlreadyResolved);
    assert_eq!(store.apply(&command(&id, HoldOperation::Amend, "later"), &id).unwrap_err(), PortError::AlreadyResolved);
    let db = store.read();
    assert!(db.commands.iter().any(|cmd| cmd.op == HoldOperation::Approve && cmd.human_resolution.as_deref() == Some("ticket-17")));
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn hold_waiting_and_resolution_survive_store_restart() {
    let path = temp_path("hold-restart");
    let id = instance(0);
    {
        let store = DiskHolds(path.clone());
        store.open(&id, "resume-here").unwrap();
    }
    {
        let store = DiskHolds(path.clone());
        assert_eq!(store.get(&id).unwrap().unwrap().status, HoldStatus::Waiting);
        store.apply(&command(&id, HoldOperation::Reject, "review record"), &id).unwrap();
    }
    let store = DiskHolds(path.clone());
    let restored = store.get(&id).unwrap().unwrap();
    assert_eq!(restored.resume_to, "resume-here");
    assert_eq!(restored.status, HoldStatus::Resolved(HoldApplied::Rejected));
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn attempt_budget_is_non_refundable_idempotent_and_independent_of_nano_budget() {
    let path = temp_path("attempts");
    let order = OrderId("o".into());
    let budget = AttemptBudget { max_attempts: 2 };
    {
        let ledger = DiskAttempts { path: path.clone(), budget };
        ledger.consume(&order, Attempt(1)).unwrap();
        ledger.consume(&order, Attempt(1)).unwrap();
        assert_eq!(ledger.consumed(&order).unwrap(), 1);
    }
    {
        let ledger = DiskAttempts { path: path.clone(), budget };
        ledger.consume(&order, Attempt(2)).unwrap();
        assert_eq!(ledger.consumed(&order).unwrap(), 2);
        assert_eq!(ledger.consume(&order, Attempt(3)).unwrap_err(), PortError::AttemptBudgetExhausted { max_attempts: 2 });
        assert_eq!(ledger.consumed(&order).unwrap(), 2);
    }
    assert!(matches!(foundry_exec::budget::reserve(&foundry_exec::Budget { reserved_nano: 0, settled_nano: 0, carried_nano: 10 }, 1),
        foundry_exec::ReservationOutcome::Granted { remaining_nano: 9 }));
    fs::remove_file(path).unwrap();
}
