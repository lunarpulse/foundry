//! FakeHarness — deterministic in-process Die for tests and Replay (plan M2).
//! Records invocations; never touches the filesystem or network.
use foundry_domain::Order;
use foundry_domain::ports::{DieOutput, Harness, PortError, PortResult};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct FakeHarness {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    calls: Mutex<Vec<(String, u32)>>,
    fail_first_n: AtomicU32,
    /// Scripted exit codes per attempt (1-indexed). Missing => 0.
    script: Mutex<std::collections::HashMap<u32, i32>>,
    /// When set, Die "succeeds" but produces no new commit (result == base)
    /// — the M1 defect #8 shape for T19 taxonomy tests.
    noop: std::sync::atomic::AtomicBool,
}

impl FakeHarness {
    pub fn fail_first(n: u32) -> Self {
        let h = Self::default();
        h.inner.fail_first_n.store(n, Ordering::SeqCst);
        h
    }

    /// Die exits 0 but result_commit == base_commit (no-op success).
    pub fn noop_success() -> Self {
        let h = Self::default();
        h.inner.noop.store(true, Ordering::SeqCst);
        h
    }

    pub fn calls(&self) -> Vec<(String, u32)> {
        self.inner.calls.lock().unwrap().clone()
    }
}

impl Harness for FakeHarness {
    fn run(
        &self,
        order: &Order,
        attempt: u32,
        _workdir: &Path,
    ) -> impl Future<Output = PortResult<DieOutput>> + Send {
        self.inner
            .calls
            .lock()
            .unwrap()
            .push((order.id.clone(), attempt));
        let n_fail = self.inner.fail_first_n.load(Ordering::SeqCst);
        let scripted = *self
            .inner
            .script
            .lock()
            .unwrap()
            .get(&attempt)
            .unwrap_or(&0);
        let noop = self.inner.noop.load(Ordering::SeqCst);
        async move {
            if attempt <= n_fail {
                return Err(PortError::Retryable(format!("scripted failure {attempt}")));
            }
            Ok(DieOutput {
                attempt,
                base_commit: "base".into(),
                result_commit: if noop {
                    "base".into()
                } else {
                    format!("die-{attempt}")
                },
                log_path: "fake".into(),
                exit_code: scripted,
            })
        }
    }
}
