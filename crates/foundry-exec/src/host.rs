use crate::{ports::DispatchHost, types::ExecutionIntent};

/// S4 storage contract: durable owner of host acceptance receipts and results.
pub trait HostLedger {
    /// Records that the host accepted work. Implementations must make this atomic,
    /// fail-stop, and idempotent.
    fn accept(&mut self, key: &HostKey) -> Result<(), String>;
    fn is_accepted(&self, key: &HostKey) -> bool;
    /// Saves a result only after acceptance. Existing results are immutable;
    /// repeating the same key is successful without replacing the saved value.
    fn save(&mut self, key: &HostKey, result: &HostResult) -> Result<(), String>;
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
pub struct HostResult {
    pub base_commit: String,
    pub result_commit: String,
    pub log_path: String,
    pub exit_code: i32,
}

/// Synchronous execution boundary for a Die worker.
pub trait DieWorker {
    fn run(&mut self, key: &HostKey, work: &str) -> Result<HostResult, String>;
}

/// Adapts a synchronous Die worker to the execution host port.
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

    fn dispatch_once(&mut self, intent: &ExecutionIntent) -> HostResult {
        let key = host_key(intent);
        if let Some(result) = self.ledger.load(&key) {
            return result;
        }

        self.ledger.accept(&key).unwrap_or_else(|error| {
            panic!("host acceptance receipt failed: {error}");
        });
        self.dispatch_count += 1;
        let result = self.worker.run(&key, &intent.external_ref).unwrap_or_else(|error| {
            panic!("Die worker failed: {error}");
        });
        self.ledger.save(&key, &result).unwrap_or_else(|error| {
            panic!("host result persistence failed: {error}");
        });
        result
    }
}

impl<W: DieWorker, L: HostLedger> DispatchHost for DieHostAdapter<'_, W, L> {
    type Result = HostResult;

    fn dispatch(&mut self, intent: &ExecutionIntent) -> Self::Result {
        self.dispatch_once(intent)
    }

    fn dispatch_count(&self) -> u64 {
        self.dispatch_count
    }

    fn accepted(&self, intent: &ExecutionIntent) -> bool {
        DieHostAdapter::accepted(self, intent)
    }

    fn recover_result(&mut self, intent: &ExecutionIntent) -> Option<Self::Result> {
        self.ledger.load(&host_key(intent))
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
