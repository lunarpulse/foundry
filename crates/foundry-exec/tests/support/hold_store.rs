use std::{collections::BTreeMap, fs::{self, File}, path::PathBuf, sync::Mutex};

use foundry_exec::{
    HoldApplied, HoldCasOutcome, HoldCommand, HoldInstanceId, HoldState, HoldStatus,
    apply_hold, cas_hold, ports::{HoldStore, PortError},
};
use serde::{Deserialize, Serialize};

// Test adapters serialize all handles, including handles reopened after a crash.
static WRITER: Mutex<()> = Mutex::new(());

#[derive(Default, Serialize, Deserialize)]
pub struct HoldDb {
    pub states: BTreeMap<String, HoldState>,
    pub commands: Vec<HoldCommand>,
}

pub struct DiskHolds(PathBuf);
impl DiskHolds {
    pub fn new(path: PathBuf) -> Self { Self(path) }

    pub fn read(&self) -> Result<HoldDb, PortError> {
        match fs::read(&self.0) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| PortError::Store(error.to_string())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(HoldDb::default()),
            Err(error) => Err(PortError::Store(error.to_string())),
        }
    }

    fn write(&self, db: &HoldDb) -> Result<(), PortError> {
        use std::io::Write;
        let pending = self.0.with_extension("pending");
        let persist = || -> Result<(), Box<dyn std::error::Error>> {
            let mut inventory = fs::OpenOptions::new().create(true).append(true)
                .open(std::env::temp_dir().join("runtime-temporaries.txt"))?;
            writeln!(inventory, "{} (hold state)", self.0.display())?;
            writeln!(inventory, "{} (rename temporary)", pending.display())?;
            fs::write(&pending, serde_json::to_vec(db)?)?;
            File::open(&pending)?.sync_all()?;
            fs::rename(&pending, &self.0)?;
            File::open(self.0.parent().unwrap())?.sync_all()?;
            Ok(())
        };
        persist().map_err(|error| PortError::Store(error.to_string()))
    }
}

fn key(instance: &HoldInstanceId) -> String {
    // Structured encoding avoids collisions for ids containing delimiters.
    serde_json::to_string(instance).unwrap()
}

impl HoldStore for DiskHolds {
    fn open(&self, instance: &HoldInstanceId, resume_to: &str) -> Result<bool, PortError> {
        let _writer = WRITER.lock().unwrap();
        let mut db = self.read()?;
        let key = key(instance);
        if db.states.contains_key(&key) { return Ok(false); }
        db.states.insert(key, HoldState { instance: instance.clone(), resume_to: resume_to.into(),
            status: HoldStatus::Waiting, resolution: None });
        self.write(&db)?;
        Ok(true)
    }

    fn apply(&self, cmd: &HoldCommand, expected: &HoldInstanceId) -> Result<HoldApplied, PortError> {
        // CAS mismatch takes precedence over lookup and all operation validation.
        if let conflict @ HoldCasOutcome::Conflict { .. } = cas_hold(expected, cmd) {
            return Err(PortError::HoldConflict(conflict));
        }
        let _writer = WRITER.lock().unwrap();
        let mut db = self.read()?;
        let state = db.states.get_mut(&key(expected)).ok_or(PortError::MissingHold)?;
        let applied = apply_hold(state, cmd, expected)?;
        if matches!(applied, HoldApplied::Info) || matches!(state.status, HoldStatus::Resolved(_)) {
            return Ok(applied);
        }
        let mut bound = cmd.clone();
        match &applied {
            HoldApplied::Amended { resume_to } => {
                state.resume_to = resume_to.clone();
                bound.human_resolution = None;
            }
            HoldApplied::Approved | HoldApplied::Rejected => {
                state.status = HoldStatus::Resolved(applied.clone());
                state.resolution = Some(bound.clone());
            }
            HoldApplied::Info => unreachable!(),
        }
        db.commands.push(bound);
        self.write(&db)?;
        Ok(applied)
    }

    fn get(&self, instance: &HoldInstanceId) -> Result<Option<HoldState>, PortError> {
        let _writer = WRITER.lock().unwrap();
        Ok(self.read()?.states.get(&key(instance)).cloned())
    }
}
