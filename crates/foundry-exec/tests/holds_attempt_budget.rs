use std::{collections::BTreeMap, fs, path::PathBuf};

use foundry_exec::{
    AttemptBudget, HoldApplied, HoldCommand, HoldInstanceId, HoldOperation, HoldStatus,
    ports::{AttemptLedger, HoldStore, PortError},
    types::{Attempt, OrderId, RunId},
};
use serde::{Deserialize, Serialize};

#[path = "support/hold_store.rs"]
mod hold_store;
use hold_store::DiskHolds;

#[derive(Serialize, Deserialize, Default)]
struct AttemptDb(BTreeMap<String, Vec<(u64, u64)>>);
struct DiskAttempts { path: PathBuf, budget: AttemptBudget }
impl DiskAttempts {
    fn read(&self) -> AttemptDb {
        fs::read(&self.path).map(|bytes| serde_json::from_slice(&bytes).unwrap()).unwrap_or_default()
    }
}
impl AttemptLedger for DiskAttempts {
    fn consume(&self, order: &OrderId, attempt: Attempt, run: RunId) -> Result<(), PortError> {
        let mut db = self.read();
        let attempts = db.0.entry(order.0.clone()).or_default();
        if attempts.contains(&(attempt.0, run.0)) { return Ok(()); }
        if attempts.iter().filter(|(seen, _)| *seen == attempt.0).count() >= self.budget.max_attempts as usize {
            return Err(PortError::AttemptBudgetExhausted { max_attempts: self.budget.max_attempts });
        }
        attempts.push((attempt.0, run.0));
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
    HoldCommand { instance: instance.clone(), op, human_resolution: Some("ticket-17".into()), evidence: evidence.into(), resume_to: None }
}
fn amend(instance: &HoldInstanceId, destination: &str) -> HoldCommand {
    HoldCommand { resume_to: Some(destination.into()), ..command(instance, HoldOperation::Amend, "review amendment") }
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
    let store = DiskHolds::new(path.clone());
    assert!(store.open(&id, "next").unwrap());
    assert!(!store.open(&id, "ignored").unwrap());
    assert!(store.open(&other, "next").unwrap());
    assert_eq!(store.apply(&command(&id, HoldOperation::Info, ""), &other).unwrap_err(),
        PortError::HoldConflict(foundry_exec::HoldCasOutcome::Conflict { observed_instance: id.clone() }));
    assert_eq!(store.apply(&command(&id, HoldOperation::Approve, " \n "), &id).unwrap_err(), PortError::EvidenceRequired);
    assert_eq!(store.apply(&command(&id, HoldOperation::Info, ""), &id).unwrap(), HoldApplied::Info);
    assert_eq!(store.get(&id).unwrap().unwrap().status, HoldStatus::Waiting);
    assert_eq!(store.apply(&amend(&id, "changed"), &id).unwrap(), HoldApplied::Amended { resume_to: "changed".into() });
    assert_eq!(store.apply(&amend(&id, "changed"), &id).unwrap(), HoldApplied::Amended { resume_to: "changed".into() });
    assert_eq!(store.get(&id).unwrap().unwrap().resume_to, "changed");
    let approved = command(&id, HoldOperation::Approve, "evidence");
    assert_eq!(store.apply(&approved, &id).unwrap(), HoldApplied::Approved);
    assert_eq!(store.apply(&approved, &id).unwrap(), HoldApplied::Approved);
    assert_eq!(store.apply(&command(&id, HoldOperation::Approve, "changed evidence"), &id).unwrap_err(), PortError::AlreadyResolved);
    assert_eq!(store.apply(&command(&id, HoldOperation::Reject, "other"), &id).unwrap_err(), PortError::AlreadyResolved);
    assert_eq!(store.apply(&amend(&id, "later"), &id).unwrap_err(), PortError::AlreadyResolved);
    let db = store.read().unwrap();
    assert!(db.commands.iter().any(|cmd| cmd.op == HoldOperation::Approve && cmd.human_resolution.as_deref() == Some("ticket-17")));
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn hold_waiting_and_resolution_survive_store_restart() {
    let path = temp_path("hold-restart");
    let id = instance(0);
    {
        let store = DiskHolds::new(path.clone());
        store.open(&id, "resume-here").unwrap();
    }
    {
        let store = DiskHolds::new(path.clone());
        assert_eq!(store.get(&id).unwrap().unwrap().status, HoldStatus::Waiting);
        store.apply(&command(&id, HoldOperation::Reject, "review record"), &id).unwrap();
    }
    let store = DiskHolds::new(path.clone());
    let restored = store.get(&id).unwrap().unwrap();
    assert_eq!(restored.resume_to, "resume-here");
    assert_eq!(restored.status, HoldStatus::Resolved(HoldApplied::Rejected));
    drop(store);
    fs::remove_file(path).unwrap();
}

#[test]
fn r7_attempt_budget_counts_unique_runs_and_rejects_new_run_after_exhaustion() {
    let path = temp_path("attempts");
    let order = OrderId("o".into());
    let budget = AttemptBudget { max_attempts: 2 };
    {
        let ledger = DiskAttempts { path: path.clone(), budget };
        ledger.consume(&order, Attempt(1), RunId(2)).unwrap();
        ledger.consume(&order, Attempt(1), RunId(2)).unwrap();
        assert_eq!(ledger.consumed(&order).unwrap(), 1);
    }
    {
        let ledger = DiskAttempts { path: path.clone(), budget };
        // Same (order, attempt) with a new run consumes the remaining slot.
        ledger.consume(&order, Attempt(1), RunId(3)).unwrap();
        assert_eq!(ledger.consumed(&order).unwrap(), 2);
        assert_eq!(ledger.consume(&order, Attempt(1), RunId(99)).unwrap_err(), PortError::AttemptBudgetExhausted { max_attempts: 2 });
        assert_eq!(ledger.consumed(&order).unwrap(), 2);
        // Exhaustion cannot block an exact retry, and the pair is the budget scope.
        ledger.consume(&order, Attempt(1), RunId(2)).unwrap();
        ledger.consume(&order, Attempt(2), RunId(99)).unwrap();
        assert_eq!(ledger.consumed(&order).unwrap(), 3);
        let other = OrderId("other".into());
        ledger.consume(&other, Attempt(1), RunId(99)).unwrap();
        assert_eq!(ledger.consumed(&other).unwrap(), 1);
    }
    assert!(matches!(foundry_exec::budget::reserve(&foundry_exec::Budget { reserved_nano: 0, settled_nano: 0, carried_nano: 10 }, 1),
        foundry_exec::ReservationOutcome::Granted { remaining_nano: 9 }));
    fs::remove_file(path).unwrap();
}

#[test]
fn b4_all_operations_compare_instances_before_lookup_or_validation() {
    let path = temp_path("all-hold-cas");
    let store = DiskHolds::new(path.clone());
    let id = instance(0); let missing = instance(1);
    store.open(&id, "next").unwrap();
    for op in [HoldOperation::Approve, HoldOperation::Reject, HoldOperation::Amend, HoldOperation::Info] {
        let cmd = command(&id, op, "");
        assert!(matches!(store.apply(&cmd, &missing), Err(PortError::HoldConflict(_))));
        assert!(matches!(store.apply(&command(&missing, op, ""), &id), Err(PortError::HoldConflict(_))));
    }
    assert_eq!(store.get(&id).unwrap().unwrap().status, HoldStatus::Waiting);
    fs::remove_file(path).unwrap();
}

#[test]
fn b4_amend_destination_and_evidence_are_independently_persisted() {
    let path = temp_path("amend-evidence"); let id = instance(0);
    let store = DiskHolds::new(path.clone());
    store.open(&id, "original").unwrap();
    // Evidence that resembles a node name must never become the destination.
    assert_eq!(store.apply(&command(&id, HoldOperation::Amend, "other-node"), &id),
        Err(PortError::ResumeDestinationRequired));
    let cmd = HoldCommand { evidence: " reviewer record\n".into(), resume_to: Some(" changed ".into()),
        ..amend(&id, "unused") };
    assert_eq!(store.apply(&cmd, &id).unwrap(), HoldApplied::Amended { resume_to: "changed".into() });
    drop(store);
    let store = DiskHolds::new(path.clone());
    let state = store.get(&id).unwrap().unwrap();
    assert_eq!(state.resume_to, "changed");
    assert_eq!(state.status, HoldStatus::Waiting);
    let db = store.read().unwrap();
    assert_eq!(db.commands[0].evidence, " reviewer record\n");
    assert_eq!(db.commands[0].human_resolution, None);
    let count = db.commands.len();
    store.apply(&command(&id, HoldOperation::Info, "info evidence"), &id).unwrap();
    assert_eq!(store.read().unwrap().commands.len(), count);
    fs::remove_file(path).unwrap();
}

#[test]
fn b4_exact_resolution_identity_survives_restart_for_approve_and_reject() {
    for op in [HoldOperation::Approve, HoldOperation::Reject] {
        let path = temp_path("resolution-identity"); let id = instance(0);
        let store = DiskHolds::new(path.clone()); store.open(&id, "next").unwrap();
        assert_eq!(store.apply(&command(&id, op, " \n "), &id), Err(PortError::EvidenceRequired));
        let cmd = command(&id, op, "verbatim evidence");
        let applied = store.apply(&cmd, &id).unwrap(); drop(store);
        let store = DiskHolds::new(path.clone());
        let state = store.get(&id).unwrap().unwrap();
        assert_eq!(state.resolution.as_ref(), Some(&cmd));
        assert_eq!(store.apply(&cmd, &id).unwrap(), applied);
        for changed in [HoldCommand { evidence: "changed".into(), ..cmd.clone() },
            HoldCommand { human_resolution: None, ..cmd.clone() }]
        {
            assert_eq!(foundry_exec::apply_hold(&state, &changed, &id), Err(PortError::AlreadyResolved));
            assert_eq!(store.apply(&changed, &id), Err(PortError::AlreadyResolved));
        }
        assert_eq!(store.apply(&amend(&id, "later"), &id), Err(PortError::AlreadyResolved));
        assert_eq!(store.apply(&command(&id, HoldOperation::Info, ""), &id), Ok(HoldApplied::Info));
        assert_eq!(store.get(&id).unwrap().unwrap(), state);
        fs::remove_file(path).unwrap();
    }
}
