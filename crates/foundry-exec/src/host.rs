use crate::{lifecycle::DispatchResult, ports::DispatchHost, types::{ExecutionIntent, Receipt}};

/// S4 storage contract: durable owner of host acceptance receipts and results.
/// The owner must serialize the complete load/accept/work/save sequence for each
/// key across all handles and processes. `&mut` only excludes sharing one Rust
/// handle; it is not a storage lock. Receipts and results must never be removed
/// while the activation can be recovered. Every loaded result implies acceptance.
pub trait HostLedger {
    /// Records that the host accepted work. Implementations must make this atomic
    /// and idempotent. Ok means durable persistence before any worker effect.
    /// Err may leave a receipt but must never allow this call to start work.
    fn accept(&mut self, key: &HostKey) -> Result<(), String>;
    /// False means proven absence, never an I/O error. Read failures must stop
    /// the owning runtime; catching a panic is not a durable recovery protocol.
    fn is_accepted(&self, key: &HostKey) -> bool;
    /// Saves a result only after acceptance. Existing results are immutable;
    /// repeating the same key is successful without replacing the saved value.
    /// Reject unaccepted keys without writing. Ok means atomic durable publication;
    /// Err may leave a published result. Recover by load, never rerun the worker.
    fn save(&mut self, key: &HostKey, result: &HostResult) -> Result<(), String>;
    /// None means proven absence. Corruption and I/O errors must stop the owning
    /// runtime, never be treated as absent or recovered by catch_unwind + retry.
    fn load(&self, key: &HostKey) -> Option<HostResult>;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct HostKey {
    pub order: String,
    pub attempt: u64,
    pub run: u64,
    pub activation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationCommit {
    pub generation: u64,
    pub commit: String,
    pub fix_log_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoldState {
    pub hold_instance_id: String,
    pub reason: String,
    pub resume_targets: Vec<String>,
    pub deadline_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostResult {
    pub base_commit: String,
    pub result_commit: String,
    pub log_path: String,
    pub exit_code: i32,
    #[serde(default)]
    pub generations: Vec<GenerationCommit>,
    #[serde(default)]
    pub held: Option<HoldState>,
}

// A commit or log path is not a graph continuation. Bind the complete worker
// result as data, leaving the pack to choose the next graph node.
impl From<HostResult> for DispatchResult {
    fn from(result: HostResult) -> Self {
        Self { receipts: vec![Receipt::Host(result)], ..Self::default() }
    }
}

/// Synchronous execution boundary for a Die worker.
pub trait DieWorker {
    fn run(&mut self, key: &HostKey, work: &str) -> Result<HostResult, String>;
}

/// An ordinary host failure, retaining the stage and original diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    Acceptance(String),
    Worker(String),
    Persistence(String),
}

/// Adapts a synchronous Die worker to the execution host port.
/// Use Executor (or its receipt-aware recovery protocol) for retries. A direct
/// dispatch without a saved result explicitly requests work even if accepted;
/// after an ambiguous worker/save failure this can repeat external effects.
/// `dispatch` is the legacy infallible shim and panics on Err; `try_dispatch`
/// returns HostError. Neither a panic nor HostError itself is a durable stop:
/// durable receipts prevent Executor from repeating ambiguous accepted work.
/// Ledger read failures/panics still require stopping the owning runtime.
pub struct DieHostAdapter<'a, W: DieWorker, L: HostLedger> {
    worker: &'a mut W,
    ledger: &'a mut L,
    dispatch_count: u64,
}

impl<'a, W: DieWorker, L: HostLedger> DieHostAdapter<'a, W, L> {
    pub fn new(worker: &'a mut W, ledger: &'a mut L) -> Self {
        Self { worker, ledger, dispatch_count: 0 }
    }

    pub fn accepted(&self, intent: &ExecutionIntent) -> bool {
        self.ledger.is_accepted(&host_key(intent))
    }

    fn dispatch_once(&mut self, intent: &ExecutionIntent) -> Result<HostResult, HostError> {
        let key = host_key(intent);
        if let Some(result) = self.ledger.load(&key) {
            return Ok(result);
        }

        self.ledger.accept(&key).map_err(HostError::Acceptance)?;
        self.dispatch_count += 1;
        let result = self.worker.run(&key, &intent.external_ref).map_err(HostError::Worker)?;
        self.ledger.save(&key, &result).map_err(HostError::Persistence)?;
        Ok(result)
    }
}

impl<W: DieWorker, L: HostLedger> DispatchHost for DieHostAdapter<'_, W, L> {
    type Result = DispatchResult;

    fn dispatch(&mut self, intent: &ExecutionIntent) -> Self::Result {
        self.try_dispatch(intent).unwrap_or_else(|error| panic!("host dispatch failed: {error:?}"))
    }

    fn try_dispatch(&mut self, intent: &ExecutionIntent) -> Result<Self::Result, HostError> {
        self.dispatch_once(intent).map(Into::into)
    }

    fn dispatch_count(&self) -> u64 {
        self.dispatch_count
    }

    fn accepted(&self, intent: &ExecutionIntent) -> bool {
        DieHostAdapter::accepted(self, intent)
    }

    fn recover_result(&mut self, intent: &ExecutionIntent) -> Option<Self::Result> {
        self.ledger.load(&host_key(intent)).map(Into::into)
    }
}

fn host_key(intent: &ExecutionIntent) -> HostKey {
    HostKey {
        order: intent.order.0.clone(),
        attempt: intent.attempt.0,
        run: intent.run.0,
        activation: intent.activation,
    }
}
