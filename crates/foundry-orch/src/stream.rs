//! T58 — 스트림 런타임 + 회복 코디네이터 (M7 플랜 v9 E′/E절).
//!
//! 계약:
//! - **스트림 위상**: 스트림 = 프리셋 root의 활성화 인스턴스. 저널=엔진=
//!   스트림 1:1 — 공유 저널 구성은 존재하지 않는다. 한 스트림은 여러
//!   주문을 순차 수용한다(G1 등가).
//! - **ready_to_start**: 오케스트레이션 로그의 의존성 간선 ∧ 각 선행
//!   주문 저널의 Shipped 종단 행 — 로그 단독이 진실이 아니다(교차 검증).
//!   T58 게이트는 **직접 승인만**(핸드오프 없음 — T59 이전 독립성).
//! - **디스패처**: ready 주문만 디스패치. 이중 디스패치 거부(스팬 집합).
//! - **회복 코디네이터** (E′):
//!   1. 입장 차단 — 새 디스패치 거부 시작.
//!   2. 드레인 — 진행 중 디스패치의 완료 승인(ack) 수신, 유한 대기.
//!   3. **타임아웃 시 스캔 없이 중단** — 부분 정지에서 스캔하는 경로
//!      자체를 제거.
//!   4. 정지 완료 → 원장 recovery_scan 1회(의도적으로 대기 중 예약
//!      무효화) → 스트림별 verify+rebuild 순차 → 재개.

use foundry_domain::order::{ExternalRef, Order};
use foundry_domain::ports::Ledger as _;
use foundry_domain::{OrderEvent, OrderState};
use crate::{OrchestrationLog, OrchestrationKind};
use foundry_workflow::recovery::RecoveryRunner;
use foundry_workflow::{Outcome, WorkflowEngine};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 스트림 핸들 — 저널 파일과 엔진을 1:1로 소유.
pub struct Stream {
    pub stream_id: String,
    pub journal_path: PathBuf,
    engine: Mutex<WorkflowEngine>,
    /// 이 스트림이 디스패치한 주문 (이중 디스패치 방지).
    dispatched: Mutex<BTreeSet<String>>,
}

impl Stream {
    pub fn new(stream_id: &str, journal_dir: &Path) -> Result<Self, String> {
        let journal_path = journal_dir.join(format!("{stream_id}.db"));
        let ledger: &'static mut foundry_ledger::SqliteLedger =
            Box::leak(Box::new(
                foundry_ledger::SqliteLedger::open(&journal_path)
                    .map_err(|e| format!("{e:?}"))?,
            ));
        let mut engine = WorkflowEngine::embedded_default();
        engine.set_ledger(ledger, foundry_domain::ports::ActorId {
            kind: foundry_domain::ActorKind::Agent,
            source: "stream".into(),
            id: stream_id.into(),
        });
        engine.set_clock(chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap());
        Ok(Self {
            stream_id: stream_id.into(),
            journal_path,
            engine: Mutex::new(engine),
            dispatched: Mutex::new(BTreeSet::new()),
        })
    }

    /// 주문 등록+디스패치 준비 — 이미 디스패치된 주문이면 거부(이중 방지).
    pub fn enroll(&self, order: Order) -> Result<(), String> {
        let oid = order.id.clone();
        let mut disp = self.dispatched.lock().unwrap();
        if disp.contains(&oid) {
            return Err(format!("order '{oid}' already dispatched on stream {}", self.stream_id));
        }
        let mut e = self.engine.lock().unwrap();
        e.enroll(order);
        disp.insert(oid);
        Ok(())
    }

    pub fn engine(&self) -> std::sync::MutexGuard<'_, WorkflowEngine> {
        self.engine.lock().unwrap()
    }

    /// 저널 읽기 전용 스냅샷 (readiness 교차 검증용).
    pub fn journal_rows(&self) -> Result<Vec<foundry_ledger::LedgerRow>, String> {
        foundry_ledger::SqliteLedger::open(&self.journal_path)
            .map_err(|e| format!("{e:?}"))?
            .read_all()
            .map_err(|e| format!("{e:?}"))
    }

    /// 주문의 Shipped 종단 여부 (저널 증거).
    pub fn order_shipped(&self, order_id: &str) -> Result<bool, String> {
        Ok(self
            .journal_rows()?
            .iter()
            .any(|r| r.order_id == order_id && r.to_state == OrderState::Shipped.to_string()))
    }
}

/// 런타임 — 스트림 레지스트리 + 오케스트레이션 로그 소유.
pub struct StreamRuntime {
    pub orch: Arc<OrchestrationLog>,
    streams: Mutex<Vec<Arc<Stream>>>,
}

#[derive(Debug)]
pub enum ReadyError {
    Undeclared(String),
    DependencyNotShipped { order: String, pred: String },
    Io(String),
}

impl StreamRuntime {
    pub fn new(orch: OrchestrationLog) -> Self {
        Self { orch: Arc::new(orch), streams: Mutex::new(Vec::new()) }
    }

    pub fn add_stream(&self, stream: Stream) {
        self.streams.lock().unwrap().push(Arc::new(stream));
    }

    pub fn stream(&self, id: &str) -> Option<Arc<Stream>> {
        self.streams.lock().unwrap().iter().find(|s| s.stream_id == id).cloned()
    }

    /// 의존성 간선 조회 (선언에서).
    fn edges_of(&self, order_id: &str) -> Result<Vec<String>, ReadyError> {
        let rows = self.orch.read_all().map_err(|e| ReadyError::Io(e.to_string()))?;
        for r in rows {
            if let OrchestrationKind::PlanDeclared { edges, .. } = &r.kind {
                if let Some((_, preds)) = edges.iter().find(|(o, _)| o == order_id) {
                    return Ok(preds.clone());
                }
            }
        }
        Err(ReadyError::Undeclared(order_id.into()))
    }

    /// ready_to_start — 로그 간선 ∧ 선행 저널 Shipped 교차.
    /// 로그 단독이 진실이 아니다: 선행이 선언만 되고 출하 안 됐으면 대기.
    pub fn ready_to_start(&self, order_id: &str) -> Result<bool, ReadyError> {
        let preds = self.edges_of(order_id)?;
        for p in &preds {
            // 선행 주문의 스트림 배정 조회.
            let rows = self.orch.read_all().map_err(|e| ReadyError::Io(e.to_string()))?;
            let mut pred_stream: Option<String> = None;
            for r in &rows {
                if let OrchestrationKind::PlanDeclared { edges, streams, .. } = &r.kind {
                    if edges.iter().any(|(o, _)| o == p) {
                        if let Some((_, s)) = streams.iter().find(|(o, _)| o == p) {
                            pred_stream = Some(s.clone());
                        }
                    }
                }
            }
            let s = pred_stream.ok_or(ReadyError::Io(format!("pred {p} has no stream")))?;
            let stream = self
                .stream(&s)
                .ok_or(ReadyError::Io(format!("stream {s} not registered")))?;
            match stream.order_shipped(p) {
                Ok(true) => {}
                Ok(false) => return Ok(false),
                Err(e) => return Err(ReadyError::Io(e)),
            }
        }
        Ok(true)
    }

    /// 디스패치 — ready 검사 후 스트림에 enroll. 이중 디스패치는
    /// Stream::enroll 이 거부.
    pub fn dispatch(&self, order_id: &str, order: Order) -> Result<(), String> {
        if !self.ready_to_start(order_id).map_err(|e| format!("{e:?}"))? {
            return Err(format!("order '{order_id}' not ready — predecessors unshipped"));
        }
        // 이 주문의 스트림 배정.
        let rows = self.orch.read_all().map_err(|e| e.to_string())?;
        let mut my_stream = None;
        for r in &rows {
            if let OrchestrationKind::PlanDeclared { edges, streams, .. } = &r.kind {
                if edges.iter().any(|(o, _)| o == order_id) {
                    if let Some((_, s)) = streams.iter().find(|(o, _)| o == order_id) {
                        my_stream = Some(s.clone());
                    }
                }
            }
        }
        let s = my_stream.ok_or("no stream assignment")?;
        let stream = self.stream(&s).ok_or("stream not registered")?;
        stream.enroll(order)
    }
}

/// 회복 코디네이터 (E′) — 입장 차단→드레인 ack→타임아웃(무스캔 중단)→
/// 전역 스캔(의도적 무효화)→스트림별 rebuild→재개.
pub struct RecoveryCoordinator {
    /// 입장 차단 플래그.
    paused: std::sync::atomic::AtomicBool,
    /// 드레인 ack 대기 큐.
    acks: Mutex<BTreeSet<String>>,
    pending: Mutex<BTreeSet<String>>,
    /// 유한 타임아웃 (초).
    pub timeout_secs: u64,
}

#[derive(Debug, PartialEq)]
pub enum RecoveryOutcome {
    /// 회복 완료 — 스캔·rebuild 수행됨.
    Completed { streams_rebuilt: usize, invalidated_reservations: usize },
    /// 타임아웃 — **스캔 없이** 중단 (부분 정지 스캔 경로 제거).
    AbortedWithoutScan { missing_acks: Vec<String> },
}

impl RecoveryCoordinator {
    pub fn new(timeout_secs: u64) -> Self {
        Self {
            paused: std::sync::atomic::AtomicBool::new(false),
            acks: Mutex::new(BTreeSet::new()),
            pending: Mutex::new(BTreeSet::new()),
            timeout_secs,
        }
    }

    /// 입장 차단 중인가 — 디스패처는 이것을 보고 새 디스패치를 거부.
    pub fn admission_blocked(&self) -> bool {
        self.paused.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// 진행 중 디스패치 등록 (드레인 대상).
    pub fn register_inflight(&self, key: &str) {
        self.pending.lock().unwrap().insert(key.into());
    }

    /// 진행 중 디스패치 완료 ack.
    pub fn ack(&self, key: &str) {
        self.acks.lock().unwrap().insert(key.into());
    }

    /// 회복 시도 — 성공이든 타임아웃이든 종료 후 입장 재개.
    pub fn recover<F, G>(
        &self,
        inflight: &[String],
        mut drain_poll: F,
        scan: G,
    ) -> RecoveryOutcome
    where
        F: FnMut() -> BTreeSet<String>,
        G: FnOnce() -> (usize, usize), // (streams_rebuilt, invalidated_reservations)
    {
        // (1) 입장 차단.
        self.paused.store(true, std::sync::atomic::Ordering::SeqCst);
        // (2) 드레인 — 유한 폴링.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(self.timeout_secs);
        let want: BTreeSet<String> = inflight.iter().cloned().collect();
        loop {
            let done = drain_poll();
            if want.iter().all(|k| done.contains(k)) {
                break;
            }
            if std::time::Instant::now() >= deadline {
                // (3) 타임아웃 — 스캔 없이 중단.
                let missing: Vec<String> =
                    want.iter().filter(|k| !done.contains(*k)).cloned().collect();
                self.paused.store(false, std::sync::atomic::Ordering::SeqCst);
                return RecoveryOutcome::AbortedWithoutScan { missing_acks: missing };
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // (4) 전역 스캔 — 의도적으로 대기 중 예약 무효화 (정지 하 안전).
        let (streams_rebuilt, invalidated_reservations) = scan();
        // (5) 재개.
        self.paused.store(false, std::sync::atomic::Ordering::SeqCst);
        RecoveryOutcome::Completed { streams_rebuilt, invalidated_reservations }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("foundry-t58-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn orch_path(d: &PathBuf) -> PathBuf {
        d.join("orch.db")
    }

    fn mkorder(id: &str) -> Order {
        let mut o = Order::new(id.into(), ExternalRef::cron("s", "r"), chrono::Utc::now());
        o.force_state_for_test(OrderState::Received, 1);
        o
    }

    /// (게이트 1) 1:1 위상 + 다주문 단일 스트림 — 한 스트림에서 의존성
    /// 있는 두 주문을 순차 진행.
    #[test]
    fn stream_hosts_multiple_orders_sequentially() {
        let d = tmpdir("multi");
        let orch = OrchestrationLog::open(&orch_path(&d)).unwrap();
        orch.declare_plan(
            "p1",
            vec![("o-a".into(), vec![]), ("o-b".into(), vec!["o-a".into()])],
            vec![("o-a".into(), "s-1".into()), ("o-b".into(), "s-1".into())],
        )
        .unwrap();
        let rt = StreamRuntime::new(orch);
        rt.add_stream(Stream::new("s-1", &d).unwrap());
        // o-a 는 선행 없음 — ready.
        assert!(rt.ready_to_start("o-a").unwrap());
        rt.dispatch("o-a", mkorder("o-a")).unwrap();
        // o-b 는 o-a 가 Shipped 전까지 not ready (교차 검증).
        assert!(!rt.ready_to_start("o-b").unwrap());
        // o-a 를 Shipped 까지 진행 (자율+게이트+출하 — 직접 승인만).
        {
            let e = rt.stream("s-1").unwrap();
            let mut eng = e.engine();
            eng.acquire("o-a", "ex").unwrap();
            for ev in [
                OrderEvent::Queued,
                OrderEvent::DieStarted,
                OrderEvent::DieSucceeded { attempt: 1 },
                OrderEvent::InspectionPassed,
            ] {
                let att = eng.order("o-a").unwrap().attempt().max(1);
                let rows = foundry_ledger::SqliteLedger::open(&e.journal_path).unwrap().read_all().unwrap();
                let ctx = crate::test_support::replay_ctx("o-a", &rows);
                let ro: &'static mut foundry_ledger::SqliteLedger = Box::leak(Box::new(
                    foundry_ledger::SqliteLedger::open(&e.journal_path).unwrap(),
                ));
                eng.dispatch_replay_ctx("o-a", "ex", &ev, att, &ctx, ro).unwrap();
            }
        }
        // 아직 FurnaceHold — o-b 여전히 대기.
        assert!(!rt.ready_to_start("o-b").unwrap());
        // 직접 승인 (T58 게이트: 핸드오프 없음).
        {
            let e = rt.stream("s-1").unwrap();
            let mut ap = foundry_approval::ApprovalLedger::open(&d.join("ap.db")).unwrap();
            ap.issue_grant("g-a", "tok-a", &serde_json::json!({
                "order_id": "o-a", "intent": "ship", "artifact_digest": "dig-a", "root_scope": "",
            })).unwrap();
            let mut hook = foundry_workflow::ApprovalLedgerAdapter(ap);
            eng_guard_dispatch_live(&e, "o-a", "ex", &mut hook);
        }
        // Shipped 확인 — o-b ready.
        assert!(rt.stream("s-1").unwrap().order_shipped("o-a").unwrap());
        assert!(rt.ready_to_start("o-b").unwrap());
    }

    fn eng_guard_dispatch_live(
        e: &Arc<Stream>,
        oid: &str,
        exec: &str,
        hook: &mut foundry_workflow::ApprovalLedgerAdapter,
    ) {
        let mut eng = e.engine();
        let grant = foundry_workflow::GrantRef { grant_id: "g-a".into() };
        let out = eng
            .dispatch_live(
                oid,
                exec,
                &OrderEvent::Approved { intent: "ship".into(), artifact_digest: "dig-a".into() },
                1,
                Some(&grant),
                hook,
            )
            .unwrap();
        assert_eq!(out, Outcome::Applied);
        let rows = foundry_ledger::SqliteLedger::open(&e.journal_path).unwrap().read_all().unwrap();
        let ctx = crate::test_support::replay_ctx(oid, &rows);
        let ro: &'static mut foundry_ledger::SqliteLedger = Box::leak(Box::new(
            foundry_ledger::SqliteLedger::open(&e.journal_path).unwrap(),
        ));
        eng.dispatch_replay_ctx(
            oid,
            exec,
            &OrderEvent::PublishConfirmed { pr_url: "https://pr/1".into() },
            1,
            &ctx,
            ro,
        )
        .unwrap();
    }

    /// (게이트 2) 이중 디스패치 거부.
    #[test]
    fn duplicate_dispatch_refused() {
        let d = tmpdir("dup");
        let orch = OrchestrationLog::open(&orch_path(&d)).unwrap();
        orch.declare_plan(
            "p1",
            vec![("o-x".into(), vec![])],
            vec![("o-x".into(), "s-d".into())],
        )
        .unwrap();
        let rt = StreamRuntime::new(orch);
        rt.add_stream(Stream::new("s-d", &d).unwrap());
        rt.dispatch("o-x", mkorder("o-x")).unwrap();
        let err = rt.dispatch("o-x", mkorder("o-x")).unwrap_err();
        assert!(err.contains("already dispatched"), "{err}");
    }

    /// (게이트 3) 회복 — 전 ack 시 완주 (스캔 1회·rebuild).
    #[test]
    fn coordinator_completes_when_acked() {
        let c = RecoveryCoordinator::new(2);
        assert!(!c.admission_blocked());
        c.register_inflight("d-1");
        c.register_inflight("d-2");
        let done = std::sync::Arc::new(Mutex::new(BTreeSet::from(["d-1".to_string()])));
        let done2 = std::sync::Arc::clone(&done);
        let out = c.recover(
            &["d-1".into(), "d-2".into()],
            move || {
                // 두번째 폴링부터 d-2 도 완료.
                let mut g = done2.lock().unwrap();
                g.insert("d-2".into());
                g.clone()
            },
            || (2, 3), // streams_rebuilt, invalidated_reservations
        );
        assert_eq!(
            out,
            RecoveryOutcome::Completed { streams_rebuilt: 2, invalidated_reservations: 3 }
        );
        assert!(!c.admission_blocked(), "admission resumes");
    }

    /// (게이트 4) 타임아웃 — **스캔 없이** 중단.
    #[test]
    fn coordinator_timeout_aborts_without_scan() {
        let c = RecoveryCoordinator::new(1);
        let mut scanned = false;
        let out = c.recover(
            &["never".into()],
            || BTreeSet::new(), // 절대 ack 안 옴
            || {
                scanned = true;
                (0, 0)
            },
        );
        assert_eq!(
            out,
            RecoveryOutcome::AbortedWithoutScan { missing_acks: vec!["never".into()] }
        );
        assert!(!scanned, "timeout must NOT scan — partial-pause scanning removed by construction");
        assert!(!c.admission_blocked());
    }

    /// (게이트 5) 교차 스트림 비간섭 — 스트림 X 의 저널에 스트림 Y 주문 없음.
    #[test]
    fn cross_stream_journals_disjoint() {
        let d = tmpdir("cross");
        let orch = OrchestrationLog::open(&orch_path(&d)).unwrap();
        orch.declare_plan(
            "p1",
            vec![("o-x".into(), vec![]), ("o-y".into(), vec![])],
            vec![("o-x".into(), "sx".into()), ("o-y".into(), "sy".into())],
        )
        .unwrap();
        let rt = StreamRuntime::new(orch);
        rt.add_stream(Stream::new("sx", &d).unwrap());
        rt.add_stream(Stream::new("sy", &d).unwrap());
        rt.dispatch("o-x", mkorder("o-x")).unwrap();
        rt.dispatch("o-y", mkorder("o-y")).unwrap();
        let sx_rows = rt.stream("sx").unwrap().journal_rows().unwrap();
        let sy_rows = rt.stream("sy").unwrap().journal_rows().unwrap();
        // 아직 배선 이벤트 없으면 빈 저널 — 각각 자기 소유 파일.
        assert!(sx_rows.iter().all(|r| r.order_id == "o-x"));
        assert!(sy_rows.iter().all(|r| r.order_id == "o-y"));
        assert_ne!(rt.stream("sx").unwrap().journal_path, rt.stream("sy").unwrap().journal_path);
    }
}
