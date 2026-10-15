#![allow(dead_code)] // Shared by graph and tuple durability integration binaries.

use std::{fs::{self, File}, io::Write, path::{Path, PathBuf}, sync::{Arc, Mutex, OnceLock, Weak}};
use foundry_exec::{ArtifactTuple, ArtifactTupleKey, Attempt, EmitReceipt, OrderId, PortError, Recorded, TupleStore};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

fn error(e: impl ToString) -> PortError { PortError::Store(e.to_string()) }

pub fn read<T: DeserializeOwned + Default>(path: &Path) -> Result<T, PortError> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(error),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(error(e)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteStop { PartialTemporary, BeforeRename, AfterRename }

pub fn owner(path: &Path) -> Arc<Mutex<()>> {
    static OWNERS: OnceLock<Mutex<std::collections::BTreeMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    let mut owners = OWNERS.get_or_init(Default::default).lock().unwrap();
    if let Some(owner) = owners.get(path).and_then(Weak::upgrade) { return owner; }
    let owner = Arc::new(Mutex::new(()));
    owners.insert(path.to_owned(), Arc::downgrade(&owner));
    owner
}

fn sync(path: &Path) -> Result<(), PortError> {
    File::open(path).and_then(|f| f.sync_all()).map_err(error)?;
    File::open(path.parent().unwrap()).and_then(|f| f.sync_all()).map_err(error)
}

// The fixture serializes all same-path handles in this process. A production
// adapter must additionally hold the storage owner's cross-process lock.
pub fn write<T: Serialize>(path: &Path, value: &T, stop: Option<WriteStop>) -> Result<(), PortError> {
    let tmp = path.with_extension("tmp");
    let bytes = serde_json::to_vec(value).map_err(error)?;
    let mut file = File::create(&tmp).map_err(error)?;
    if stop == Some(WriteStop::PartialTemporary) {
        file.write_all(&bytes[..bytes.len() / 2]).map_err(error)?;
        return Err(error("crash during temporary write"));
    }
    file.write_all(&bytes).map_err(error)?;
    file.sync_all().map_err(error)?;
    drop(file);
    if stop == Some(WriteStop::BeforeRename) { return Err(error("crash before rename")); }
    fs::rename(&tmp, path).map_err(error)?;
    if stop == Some(WriteStop::AfterRename) { return Err(error("crash after rename")); }
    File::open(path.parent().unwrap()).and_then(|f| f.sync_all()).map_err(error)
}

#[derive(Default, Serialize, Deserialize)]
struct Db {
    tuples: Vec<ArtifactTuple>,
    promotions: Vec<EmitReceipt>,
}
impl Db {
    fn record(&mut self, tuple: &ArtifactTuple) -> Result<Recorded, PortError> {
        if let Some(old) = self.tuples.iter().find(|old| old.key() == tuple.key()) {
            return if old == tuple { Ok(Recorded) }
                else { Err(PortError::Permanent("tuple key collision".into())) };
        }
        self.tuples.push(tuple.clone());
        Ok(Recorded)
    }
    fn latest(&self, order: &OrderId, attempt: Attempt) -> Option<ArtifactTuple> {
        self.tuples.iter().filter(|t| &t.order == order && t.attempt == attempt)
            .max_by_key(|t| (t.run, t.generation)).cloned()
    }
    fn promote(&mut self, key: &ArtifactTupleKey, receipt: &EmitReceipt) -> Result<(), PortError> {
        if &receipt.key != key || !self.tuples.iter().any(|tuple| &tuple.key() == key) {
            return Err(PortError::MissingEmitReceipt);
        }
        if let Some(old) = self.promotions.iter().find(|old| &old.key == key) {
            return if old == receipt { Ok(()) }
                else { Err(PortError::Permanent("promotion receipt conflict".into())) };
        }
        self.promotions.push(receipt.clone());
        Ok(())
    }
}

#[derive(Default)]
pub struct MemoryTuples(Mutex<Db>);
impl TupleStore for MemoryTuples {
    fn record(&self, tuple: &ArtifactTuple) -> Result<Recorded, PortError> { self.0.lock().unwrap().record(tuple) }
    fn latest(&self, order: &OrderId, attempt: Attempt) -> Result<Option<ArtifactTuple>, PortError> {
        Ok(self.0.lock().unwrap().latest(order, attempt))
    }
    fn promote(&self, key: &ArtifactTupleKey, receipt: &EmitReceipt) -> Result<(), PortError> {
        self.0.lock().unwrap().promote(key, receipt)
    }
}

pub struct DiskTuples {
    pub path: PathBuf,
    pub stop: Option<WriteStop>,
    owner: Arc<Mutex<()>>,
}
impl DiskTuples {
    pub fn new(path: PathBuf) -> Self { let owner = owner(&path); Self { path, stop: None, owner } }
    pub fn promoted(&self, key: &ArtifactTupleKey) -> bool {
        read::<Db>(&self.path).unwrap().promotions.iter().any(|r| &r.key == key)
    }
}
impl TupleStore for DiskTuples {
    fn record(&self, tuple: &ArtifactTuple) -> Result<Recorded, PortError> {
        let _owner = self.owner.lock().unwrap();
        let mut db: Db = read(&self.path)?;
        let count = db.tuples.len();
        let recorded = db.record(tuple)?;
        if db.tuples.len() != count { write(&self.path, &db, self.stop)?; }
        else { sync(&self.path)?; } // Also settles an ambiguous post-rename retry.
        Ok(recorded)
    }
    fn latest(&self, order: &OrderId, attempt: Attempt) -> Result<Option<ArtifactTuple>, PortError> {
        let _owner = self.owner.lock().unwrap();
        Ok(read::<Db>(&self.path)?.latest(order, attempt))
    }
    fn promote(&self, key: &ArtifactTupleKey, receipt: &EmitReceipt) -> Result<(), PortError> {
        let _owner = self.owner.lock().unwrap();
        let mut db: Db = read(&self.path)?;
        let count = db.promotions.len();
        db.promote(key, receipt)?;
        if db.promotions.len() != count { write(&self.path, &db, self.stop)?; }
        else { sync(&self.path)?; }
        Ok(())
    }
}

pub struct TestDir(pub PathBuf);
impl TestDir {
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!("b5-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        let mut inventory = fs::OpenOptions::new().create(true).append(true)
            .open(std::env::temp_dir().join("runtime-temporaries.txt")).unwrap();
        for entry in fs::read_dir(&self.0).unwrap() {
            writeln!(inventory, "{} (deleted)", entry.unwrap().path().display()).unwrap();
        }
        fs::remove_dir_all(&self.0).unwrap();
    }
}
