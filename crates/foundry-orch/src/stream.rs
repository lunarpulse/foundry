//! T58 — 스트림 런타임 + 회복 코디네이터 (M7 플랜 v9 E′/E절).
//!
//! astra r1 재작 계약:
//! - **고유 불변 identity**: 스트림 id 는 런타임 내 유일(중복 등록 거부)·
//!   저널 파일 경로는 스트림 id 에서만 파생(경로 별칭 불가).
//! - **디스패치-코디네이터 결합**: dispatch 는 입장 차단 플래그를 검사
//!   (차단 중 거부 → RefusalRecord), 코디네이터는 스트림을 실제 rebuild
//!   (RecoveryRunner 재사용 — 원장 reopen·저널 verify·신 엔진).
//! - **checked-id 일치**: dispatch 의 order_id 파라미터 == order.id.
//! - **거부 레코드**: 인간 대면 거부는 오케스트레이션 로그에 3요소 기록.
//! - 스트림 활성화는 런타임 수명과 1:1 — leaked 저널 핸들은 활성 스트림
//!   수만큼 유한(같은 root 재활성화는 새 런타임).

use crate::refusal::{record_refusal, RefusalCode};
use crate::{OrchestrationKind, OrchestrationLog};
use foundry_domain::order::{ExternalRef, Order};
use foundry_domain::ports::Ledger as _;
use foundry_domain::{OrderEvent, OrderState};
use foundry_workflow::recovery::RecoveryRunner;
use foundry_workflow::{Outcome, WorkflowEngine};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 스트림 핸들 — 저널 파일과 엔진을 1:1로 소유.
pub struct Stream {
    /// (astra r8) 불변 identity — **비공개**, 생성자에서만 설정. 접근은
    /// id()/journal() 접근자로만(재지정·경로 스왑 우회 원천 차단).
    stream_id: String,
    /// (astra r8) 불변 저널 경로 — 비공개, id 에서만 파생.
    journal_path: PathBuf,
    engine: Mutex<WorkflowEngine>,
    /// 이 스트림이 디스패치한 주문 (이중 디스패치 방지).
    dispatched: Mutex<BTreeSet<String>>,
    /// (astra r3) 펜스 — 회복 실패 스트림의 enroll 차단(fail-closed).
    fenced: std::sync::atomic::AtomicBool,
}

impl Stream {
    /// 신규 스트림 — journal_dir/{stream_id}.db 소유.
    pub fn new(stream_id: &str, journal_dir: &Path) -> Result<Self, String> {
        // (astra r2 identity) id 검증 — 경로 세포 문자/별칭 차단.
        if stream_id.is_empty()
            || stream_id.len() > 128
            || !stream_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(format!(
                "invalid stream id '{stream_id}' — [A-Za-z0-9_-] up to 128 chars only"
            ));
        }
        let journal_path = journal_dir
            .canonicalize()
            .unwrap_or_else(|_| journal_dir.to_path_buf())
            .join(format!("{stream_id}.db"));
        let ledger: &'static mut foundry_ledger::SqliteLedger = Box::leak(Box::new(
            foundry_ledger::SqliteLedger::open(&journal_path).map_err(|e| format!("{e:?}"))?,
        ));
        let mut engine = WorkflowEngine::embedded_default();
        engine.set_ledger(ledger, foundry_domain::ActorId {
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
            fenced: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// 불변 identity 접근자.
    pub fn id(&self) -> &str {
        &self.stream_id
    }

    /// 불변 저널 경로 접근자.
    pub fn journal(&self) -> &Path {
        &self.journal_path
    }

    /// 펜스 — 회복 실패한 스트림은 디스패치를 거부한다(다음 회복 성공
    /// 시 해제).
    pub fn fence(&self) {
        self.fenced.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// (astra r6) 해제는 **회복 코디네이터 전용** — pub(crate). 외부가
    /// 펜스를 임의 해제해 구 엔진을 되살리는 우회를 차단.
    pub(crate) fn unfence(&self) {
        self.fenced.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn is_fenced(&self) -> bool {
        self.fenced.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// 주문 등록 — **order.id 본체**로 keyed (astra r1: checked-id 우회
    /// 제거). 이중 디스패치 거부.
    pub fn enroll(&self, order: Order) -> Result<(), String> {
        if self.is_fenced() {
            return Err(format!(
                "stream '{}' is fenced — recovery failed; dispatch refused until next recovery succeeds",
                self.stream_id
            ));
        }
        let oid = order.id.clone();
        let mut disp = self.dispatched.lock().unwrap();
        if disp.contains(&oid) {
            return Err(format!("order '{oid}' already dispatched on stream {}", self.stream_id));
        }
        self.engine.lock().unwrap().enroll(order);
        disp.insert(oid);
        Ok(())
    }

    /// (astra r5+r10) 펜스 검사 포함 실행 진입 — **크레이트 내부 전용**.
    /// 가드의 &mut 역참조는 스트림 간 엔진 스왑(mem::swap)을 허용하므로,
    /// 실행 주도권은 런타임/코디네이터(신뢰 경계 내)만 가진다. 외부
    /// 공개면은 enroll/관찰(fence 상태·저널)뿐 — 실행 경로는 T61 종단
    /// 배선에서 런타임 진입점으로 제공된다.
    pub(crate) fn engine_if_unfenced(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, WorkflowEngine>, String> {
        if self.is_fenced() {
            return Err(format!(
                "stream '{}' is fenced — execution refused until recovery succeeds",
                self.stream_id
            ));
        }
        Ok(self.engine.lock().unwrap())
    }

    /// 저널 읽기 전용 스냅샷.
    pub fn journal_rows(&self) -> Result<Vec<foundry_ledger::LedgerRow>, String> {
        foundry_ledger::SqliteLedger::open(&self.journal_path)
            .map_err(|e| format!("{e:?}"))?
            .read_all()
            .map_err(|e| format!("{e:?}"))
    }

    /// (astra r3 BLOCKER) 재구축 엔진 **재배선** — 같은 저널/액터/시계로
    /// 새 엔진을 만들어 저널 결속을 회복한다. rebuild 가 복원하는 것은
    /// 상태·attempt 뿐이므로 설치 전에 이 재배선이 필수다.
    /// (astra r9) **크레이트 내부 전용** — 재배선 엔진의 생성은 회복
    /// 코디네이터만 수행한다. 외부가 foreign 저널 결합 엔진을 만들어
    /// engine_if_unfenced 가드로 스왑 설치하는 경로를 차단.
    pub(crate) fn rebuilt_engine(&self) -> Result<WorkflowEngine, String> {
        let ledger: &'static mut foundry_ledger::SqliteLedger = Box::leak(Box::new(
            foundry_ledger::SqliteLedger::open(&self.journal_path)
                .map_err(|e| format!("{e:?}"))?,
        ));
        let mut engine = WorkflowEngine::embedded_default();
        engine.set_ledger(ledger, foundry_domain::ActorId {
            kind: foundry_domain::ActorKind::Agent,
            source: "stream".into(),
            id: self.stream_id.clone(),
        });
        engine.set_clock(chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap());
        Ok(engine)
    }

    /// 주문의 Shipped 종단 여부 (저널 증거).
    pub fn order_shipped(&self, order_id: &str) -> Result<bool, String> {
        Ok(self
            .journal_rows()?
            .iter()
            .any(|r| r.order_id == order_id && r.to_state == OrderState::Shipped.to_string()))
    }
}

/// 런타임 — 스트림 레지스트리 + 오케스트레이션 로그 + 입장 게이트.
pub struct StreamRuntime {
    pub orch: Arc<OrchestrationLog>,
    /// (astra r9 P1) 사이즈 바인딩 검증용 승인 원장 — dispatch가
    /// Chainer 게이트를 실제 호출한다(unsized-spawn 원천 차단).
    approval: Mutex<Option<&'static foundry_approval::ApprovalLedger>>,
    streams: Mutex<Vec<Arc<Stream>>>,
    /// (astra r1 결합) 입장 차단 — 코디네이터가 소유.
    admission_blocked: std::sync::atomic::AtomicBool,
}

impl StreamRuntime {
    pub fn new(orch: OrchestrationLog) -> Self {
        Self {
            orch: Arc::new(orch),
            approval: Mutex::new(None),
            streams: Mutex::new(Vec::new()),
            admission_blocked: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// (astra r9 P1) 승인 원장 결속 — 프로세스 수명 원장을 leak 해
    /// 등록한다(전역 단일 원장 계약, T58과 동일 패턴).
    pub fn bind_approval_ledger(
        &self,
        ledger: foundry_approval::ApprovalLedger,
    ) -> Result<(), String> {
        let leaked: &'static foundry_approval::ApprovalLedger = Box::leak(Box::new(ledger));
        *self.approval.lock().unwrap() = Some(leaked);
        Ok(())
    }

    /// 스트림 등록 — **id 유일 ∧ 저널 경로 유일** 강제. (astra r7 b):
    /// 생성 후 id 를 바꿔 같은 저널 파일을 공유하는 우회는 파일 경로
    /// 단위 검사가 차단한다.
    pub fn add_stream(&self, stream: Stream) -> Result<(), String> {
        let mut g = self.streams.lock().unwrap();
        if g.iter().any(|s| s.stream_id == stream.stream_id) {
            return Err(format!(
                "stream '{}' already registered — ids are unique",
                stream.stream_id
            ));
        }
        if g.iter().any(|s| s.journal_path == stream.journal_path) {
            return Err(format!(
                "journal '{}' already owned by another stream — journals are 1:1",
                stream.journal_path.display()
            ));
        }
        g.push(Arc::new(stream));
        Ok(())
    }

    pub fn stream(&self, id: &str) -> Option<Arc<Stream>> {
        self.streams.lock().unwrap().iter().find(|s| s.stream_id == id).cloned()
    }

    /// 입장 차단 (코디네이터 전용).
    pub(crate) fn block_admission(&self) {
        self.admission_blocked.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn resume_admission(&self) {
        self.admission_blocked.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn is_admission_blocked(&self) -> bool {
        self.admission_blocked.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// (astra r9) plan 의 preset_bound 역산 — spawn_plan 이 이 plan 인
    /// 결속의 sizing_order.
    fn sizing_of_plan(&self, plan: &str) -> Option<String> {
        let rows = self.orch.read_all().ok()?;
        rows.iter().find_map(|r| match &r.kind {
            OrchestrationKind::PresetBound { sizing_order, spawn_plan, .. }
                if spawn_plan == plan =>
            {
                Some(sizing_order.clone())
            }
            _ => None,
        })
    }

    fn digest_of_sizing(&self, sizing: &str) -> Option<String> {
        let rows = self.orch.read_all().ok()?;
        rows.iter().find_map(|r| match &r.kind {
            OrchestrationKind::PresetBound { sizing_order, approved_digest, .. }
                if sizing_order == sizing =>
            {
                Some(approved_digest.clone())
            }
            _ => None,
        })
    }

    fn root_of_sizing(&self, sizing: &str) -> Option<String> {
        let rows = self.orch.read_all().ok()?;
        rows.iter().find_map(|r| match &r.kind {
            OrchestrationKind::PresetBound { sizing_order, selected_root, .. }
                if sizing_order == sizing =>
            {
                Some(selected_root.clone())
            }
            _ => None,
        })
    }

    /// 의존성 간선 조회.
    fn edges_of(&self, order_id: &str) -> Result<Vec<String>, String> {
        let rows = self.orch.read_all().map_err(|e| e.to_string())?;
        for r in rows {
            if let OrchestrationKind::PlanDeclared { edges, .. } = &r.kind {
                if let Some((_, preds)) = edges.iter().find(|(o, _)| o == order_id) {
                    return Ok(preds.clone());
                }
            }
        }
        Err(format!("undeclared: {order_id}"))
    }

    /// 주문의 (plan, stream) 배정 조회.
    fn assignment_of(&self, order_id: &str) -> Result<Option<(String, String)>, String> {
        let rows = self.orch.read_all().map_err(|e| e.to_string())?;
        for r in rows {
            if let OrchestrationKind::PlanDeclared { plan_id, edges, streams } = &r.kind {
                if edges.iter().any(|(o, _)| o == order_id) {
                    if let Some((_, s)) = streams.iter().find(|(o, _)| o == order_id) {
                        return Ok(Some((plan_id.clone(), s.clone())));
                    }
                }
            }
        }
        Ok(None)
    }

    /// ready_to_start — 로그 간선 ∧ 선행 저널 Shipped 교차.
    pub fn ready_to_start(&self, order_id: &str) -> Result<bool, String> {
        let preds = self.edges_of(order_id)?;
        for p in &preds {
            let Some((_, s)) = self.assignment_of(p)? else {
                return Err(format!("pred {p} has no stream"));
            };
            let stream = self.stream(&s).ok_or(format!("stream {s} not registered"))?;
            match stream.order_shipped(p) {
                Ok(true) => {}
                Ok(false) => return Ok(false),
                Err(e) => return Err(e),
            }
        }
        Ok(true)
    }

    /// 디스패치 — (astra r1 결합) 입장 차단 검사·checked-id 일치·선언·
    /// ready 검사·거부 레코드·발행 기록.
    pub fn dispatch(&self, order_id: &str, order: Order) -> Result<(), String> {
        if self.is_admission_blocked() {
            let _ = record_refusal(&self.orch, RefusalCode::QuiescenceBlocked, order_id, None);
            return Err(format!(
                "admission blocked — order '{order_id}' refused during recovery quiescence"
            ));
        }
        if order_id != order.id {
            let _ = record_refusal(&self.orch, RefusalCode::CheckedIdMismatch, order_id, None);
            return Err(format!(
                "dispatch id '{order_id}' != order.id '{}' — checked-id must match",
                order.id
            ));
        }
        let Some((plan, s)) = self.assignment_of(order_id)? else {
            let _ = record_refusal(&self.orch, RefusalCode::UnsizedSpawn, order_id, None);
            return Err(format!(
                "order '{order_id}' has no declaration — unsized/undeclared spawn"
            ));
        };
        // (astra r9 P1) **사이즈 바인딩 게이트** — 발행 전 Chainer 검사.
        // 원장 미결합이면 fail-closed(바인딩 검증 없는 발행 불가).
        let ap_guard = self.approval.lock().unwrap();
        match *ap_guard {
            None => {
                let _ = record_refusal(&self.orch, RefusalCode::UnsizedSpawn, order_id, None);
                return Err(format!(
                    "order '{order_id}' refused — approval ledger not bound (fail-closed)"
                ));
            }
            Some(ap) => {
                let ch = crate::chainer::Chainer::new(&self.orch, ap);
                // 주문 소스 — 인테이크 부트스트랩 판별. 소스가 없으면
                // 비부트스트랩(보수).
                // (astra r10 P1-b) Cron 을 무조건 부트스트랩으로 보지
                // 않는다 — 부트스트랩은 external_id 가 "intake:" 접두로
                // 시작할 때만(인테이크 전용 경로). 그 외 전부 외부.
                let source = format!("ext:{}", order.external.external_id);
                let source = if order.external.external_id.starts_with("intake:") {
                    order.external.external_id.clone()
                } else {
                    source
                };
                let r = ch.check_spawn_binding(
                    order_id,
                    &source,
                    None, // sizing_order 후보: 로그 preset_bound 에서 역산
                    None,
                    &plan,
                    "preset",
                );
                // sizing_order 역산 — 이 plan 의 preset_bound 찾기.
                let sizing = self.sizing_of_plan(&plan);
                let r = match sizing {
                    Some(so) => ch.check_spawn_binding(
                        order_id,
                        &source,
                        Some(&so),
                        self.digest_of_sizing(&so).as_deref(),
                        &plan,
                        self.root_of_sizing(&so).as_deref().unwrap_or("preset"),
                    ),
                    None => r,
                };
                if let Err(e) = r {
                    let code = match &e {
                        crate::chainer::ChainerError::Unsized { .. }
                        | crate::chainer::ChainerError::BootstrapViolation { .. } => {
                            RefusalCode::UnsizedSpawn
                        }
                        crate::chainer::ChainerError::SizingRejected { .. } => {
                            RefusalCode::UnsizedSpawn
                        }
                        _ => RefusalCode::UnsizedSpawn,
                    };
                    let _ = record_refusal(&self.orch, code, order_id, None);
                    return Err(format!("order '{order_id}' failed sizing binding: {e:?}"));
                }
            }
        }
        drop(ap_guard);
        if !self.ready_to_start(order_id)? {
            let _ = record_refusal(&self.orch, RefusalCode::NotReady, order_id, None);
            return Err(format!("order '{order_id}' not ready — predecessors unshipped"));
        }
        let stream = self.stream(&s).ok_or(format!("stream {s} not registered"))?;
        stream.enroll(order)?;
        self.orch.spawn_order(order_id, &plan, &s).map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// 회복 코디네이터 (E′) — 런타임에 실제 결합.
///
/// 1. 입장 차단(런타임 플래그 — dispatch 가 즉시 거부+레코드).
/// 2. 드레인 — 진행 중 디스패치의 완료 승인(ack) 폴링, 유한 대기.
/// 3. **타임아웃 시 스캔 없이 중단** — 부분 정지 스캔 경로 원천 제거.
/// 4. 전역 원장 스캔 1회(의도적 예약 무효화) → **스트림별 실제 rebuild**
///    (RecoveryRunner: 원장·저널 verify·신 엔진 재구축).
/// 5. 입장 재개.
pub struct RecoveryCoordinator<'r> {
    runtime: &'r StreamRuntime,
    /// 유한 타임아웃 (초).
    pub timeout_secs: u64,
}

#[derive(Debug, PartialEq)]
pub enum RecoveryOutcome {
    /// 회복 완료 — 재구축 엔진 설치됨·무효화된 예약 수.
    Completed { streams_rebuilt: usize, invalidated_reservations: usize },
    /// 타임아웃 — **스캔 없이** 중단 (부분 정지 스캔 경로 제거).
    AbortedWithoutScan { missing_acks: Vec<String> },
    /// (astra r2) 스캔/rebuild 실패 — 부분 결과 동반(성공 스트림은
    /// 이미 교체됨). 입장은 재개됐다.
    Failed { stage: String, streams_rebuilt: usize, invalidated_reservations: usize },
}

impl<'r> RecoveryCoordinator<'r> {
    pub fn new(runtime: &'r StreamRuntime, timeout_secs: u64) -> Self {
        Self { runtime, timeout_secs }
    }

    /// 회복 — ack 폴링은 호출자(in-flight 소유자)가 제공.
    pub fn recover<F>(
        &self,
        inflight: &[String],
        mut drain_poll: F,
        approval_db: &Path,
    ) -> RecoveryOutcome
    where
        F: FnMut() -> BTreeSet<String>,
    {
        self.runtime.block_admission();
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(self.timeout_secs);
        let want: BTreeSet<String> = inflight.iter().cloned().collect();
        loop {
            let done = drain_poll();
            if want.iter().all(|k| done.contains(k)) {
                break;
            }
            if std::time::Instant::now() >= deadline {
                let missing: Vec<String> =
                    want.iter().filter(|k| !done.contains(*k)).cloned().collect();
                self.runtime.resume_admission();
                return RecoveryOutcome::AbortedWithoutScan { missing_acks: missing };
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // (4) 전역 원장 스캔 — **1회만**(astra r2: 스트림마다 반복 금지),
        // 의도적 예약 무효화(정지 하).
        let mut invalidated = 0usize;
        match foundry_approval::ApprovalLedger::open(approval_db) {
            Ok(mut ap) => match foundry_approval::ApprovalLedger::recovery_scan(&mut ap) {
                Ok(released) => invalidated = released.len(),
                Err(e) => {
                    // 스캔 실패 — 안전한 상태로 재개하고 실패 보고.
                    self.runtime.resume_admission();
                    return RecoveryOutcome::Failed {
                        stage: format!("scan: {e:?}"),
                        streams_rebuilt: 0,
                        invalidated_reservations: 0,
                    };
                }
            },
            Err(e) => {
                self.runtime.resume_admission();
                return RecoveryOutcome::Failed {
                    stage: format!("ledger open: {e:?}"),
                    streams_rebuilt: 0,
                    invalidated_reservations: 0,
                };
            }
        }
        // 스트림별 rebuild — RecoveryRunner 로 **재구축 엔진을 스트림에
        // 설치**(astra r2 BLOCKER: 검사만 하고 버리는 게 아니라 재획득).
        // 하나라도 실패하면: 실패 스트림까지의 결과를 들고 실패 보고 —
        // 입장은 재개하되 해당 스트림은 기존 엔진 유지(다음 회복이 재시도).
        // (astra r3) 두번째 원장 open 실패도 전파 — 묵칙 Completed 금지.
        let mut ap2 = match foundry_approval::ApprovalLedger::open(approval_db) {
            Ok(ap) => ap,
            Err(e) => {
                // 전역 실패 — 전 스트림 펜스 후 재개 보고.
                let streams = self.runtime.streams.lock().unwrap().clone();
                for s in &streams {
                    s.fence();
                }
                self.runtime.resume_admission();
                return RecoveryOutcome::Failed {
                    stage: format!("ledger reopen: {e:?}"),
                    streams_rebuilt: 0,
                    invalidated_reservations: invalidated,
                };
            }
        };
        let streams = self.runtime.streams.lock().unwrap().clone();
        let mut rebuilt = 0usize;
        let mut first_failure: Option<String> = None;
        for s in &streams {
            let Ok(jr) = foundry_ledger::SqliteLedger::open(&s.journal_path) else {
                first_failure = Some(format!("journal open: {}", s.journal_path.display()));
                s.fence(); // (astra r3) 실패 스트림 펜스 — 디스패치 차단.
                continue;
            };
            // 재배선 엔진 (BLOCKER: 저널 결속 회복 후 rebuild).
            let Ok(mut fresh) = s.rebuilt_engine() else {
                first_failure =
                    Some(format!("journal rewire: {}", s.journal_path.display()));
                s.fence();
                continue;
            };
            match RecoveryRunner::run_after_scan(&jr, &mut fresh) {
                Ok(rep) if rep.journal_verified => {
                    let mut guard = s.engine.lock().unwrap();
                    *guard = fresh;
                    drop(guard);
                    s.unfence();
                    rebuilt += 1;
                }
                Ok(_) => {
                    first_failure =
                        Some(format!("journal verify failed: {}", s.journal_path.display()));
                    s.fence();
                }
                Err(e) => {
                    first_failure = Some(format!("rebuild: {e:?}"));
                    s.fence();
                }
            }
        }
        self.runtime.resume_admission();
        if let Some(stage) = first_failure {
            return RecoveryOutcome::Failed {
                stage,
                streams_rebuilt: rebuilt,
                invalidated_reservations: invalidated,
            };
        }
        RecoveryOutcome::Completed { streams_rebuilt: rebuilt, invalidated_reservations: invalidated }
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

    fn mkorder(id: &str) -> Order {
        // (astra r10) 인테이크 부트스트랩 소스 — external_id "intake:" 접두.
        let mut ext = ExternalRef::cron("s", "r");
        ext.external_id = format!("intake:{id}");
        let mut o = Order::new(id.into(), ext, chrono::Utc::now());
        o.force_state_for_test(OrderState::Received, 1);
        o
    }

    /// 주문을 자율 구간+직접 승인으로 Shipped 까지.
    fn run_to_shipped(
        rt: &StreamRuntime,
        stream_id: &str,
        oid: &str,
        grant_id: &str,
        ap_db: &Path,
    ) {
        let mut ap = foundry_approval::ApprovalLedger::open(ap_db).unwrap();
        let e = rt.stream(stream_id).unwrap();
        {
            let mut eng = e.engine_if_unfenced().unwrap();
            eng.acquire(oid, "ex").unwrap();
            for ev in [
                OrderEvent::Queued,
                OrderEvent::DieStarted,
                OrderEvent::DieSucceeded { attempt: 1 },
                OrderEvent::InspectionPassed,
            ] {
                let att = eng.order(oid).unwrap().attempt().max(1);
                let rows = foundry_ledger::SqliteLedger::open(&e.journal_path)
                    .unwrap()
                    .read_all()
                    .unwrap();
                let ctx = crate::test_support::replay_ctx(oid, &rows);
                let ro: &'static mut foundry_ledger::SqliteLedger = Box::leak(Box::new(
                    foundry_ledger::SqliteLedger::open(&e.journal_path).unwrap(),
                ));
                eng.dispatch_replay_ctx(oid, "ex", &ev, att, &ctx, ro).unwrap();
            }
        }
        ap.issue_grant(grant_id, &format!("tok-{grant_id}"), &serde_json::json!({
            "order_id": oid, "intent": "ship", "artifact_digest": format!("dig-{oid}"), "root_scope": "",
        }))
        .unwrap();
        let mut hook = foundry_workflow::ApprovalLedgerAdapter(ap);
        {
            let e2 = rt.stream(stream_id).unwrap();
            let mut eng = e2.engine_if_unfenced().unwrap();
            let grant = foundry_workflow::GrantRef { grant_id: grant_id.into() };
            let out = eng
                .dispatch_live(
                    oid,
                    "ex",
                    &OrderEvent::Approved {
                        intent: "ship".into(),
                        artifact_digest: format!("dig-{oid}"),
                    },
                    1,
                    Some(&grant),
                    &mut hook,
                )
                .unwrap();
            assert_eq!(out, Outcome::Applied);
            let rows = foundry_ledger::SqliteLedger::open(&e2.journal_path)
                .unwrap()
                .read_all()
                .unwrap();
            let ctx = crate::test_support::replay_ctx(oid, &rows);
            let ro: &'static mut foundry_ledger::SqliteLedger = Box::leak(Box::new(
                foundry_ledger::SqliteLedger::open(&e2.journal_path).unwrap(),
            ));
            eng.dispatch_replay_ctx(
                oid,
                "ex",
                &OrderEvent::PublishConfirmed { pr_url: "https://pr/1".into() },
                1,
                &ctx,
                ro,
            )
            .unwrap();
        }
    }

    /// (게이트 1+2+7) 다주문 단일 스트림 — 두 주문 모두 실주행·의존성 준수·
    /// 직접 승인만·발행 기록·거부 레코드 증거 배제.
    #[test]
    fn stream_hosts_two_orders_full_cycle_with_dependency() {
        let d = tmpdir("multi");
        let orch = OrchestrationLog::open(&d.join("orch.db")).unwrap();
        orch.declare_plan(
            "p1",
            vec![("o-a".into(), vec![]), ("o-b".into(), vec!["o-a".into()])],
            vec![("o-a".into(), "s-1".into()), ("o-b".into(), "s-1".into())],
        )
        .unwrap();
        let rt = StreamRuntime::new(orch);
        rt.add_stream(Stream::new("s-1", &d).unwrap()).unwrap();
        rt.bind_approval_ledger(
            foundry_approval::ApprovalLedger::open(&d.join("ap.db")).unwrap(),
        )
        .unwrap();
        assert!(rt.ready_to_start("o-a").unwrap());
        assert!(!rt.ready_to_start("o-b").unwrap());
        rt.dispatch("o-a", mkorder("o-a")).unwrap();
        // 이중 디스패치 거부.
        assert!(rt.dispatch("o-a", mkorder("o-a")).is_err());
        // checked-id 불일치 거부 (astra r1).
        assert!(rt.dispatch("o-a", mkorder("o-b")).is_err());

        run_to_shipped(&rt, "s-1", "o-a", "g-a", &d.join("ap.db"));
        assert!(rt.stream("s-1").unwrap().order_shipped("o-a").unwrap());
        assert!(rt.ready_to_start("o-b").unwrap());

        // o-b 실주행 — 두 주문 모두 Shipped.
        rt.dispatch("o-b", mkorder("o-b")).unwrap();
        run_to_shipped(&rt, "s-1", "o-b", "g-b", &d.join("ap.db"));
        assert!(rt.stream("s-1").unwrap().order_shipped("o-b").unwrap());

        let rows = rt.stream("s-1").unwrap().journal_rows().unwrap();
        assert!(rows.iter().any(|r| r.order_id == "o-a" && r.to_state == "shipped"));
        assert!(rows.iter().any(|r| r.order_id == "o-b" && r.to_state == "shipped"));
        // 발행 기록 2건.
        let prows = rt.orch.read_all().unwrap();
        let spawned: Vec<_> = prows
            .iter()
            .filter(|r| matches!(&r.kind, OrchestrationKind::OrderSpawned { .. }))
            .collect();
        assert_eq!(spawned.len(), 2);
        // 거부 레코드는 증거에서 배제 — 선언+발행만.
        let decl = rt.orch.declaration_rows().unwrap();
        assert!(decl.len() >= 3);
    }

    /// (BLOCKER 폐쇄) 코디네이터 결합 — 차단 중 dispatch 거부(레코드),
    /// ack 완료 시 실제 rebuild, 타임아웃 시 스캔 없이 중단.
    #[test]
    fn coordinator_blocks_dispatch_and_rebuilds() {
        let d = tmpdir("coord");
        let orch = OrchestrationLog::open(&d.join("orch.db")).unwrap();
        orch.declare_plan(
            "p1",
            vec![("o-a".into(), vec![])],
            vec![("o-a".into(), "s-c".into())],
        )
        .unwrap();
        let rt = StreamRuntime::new(orch);
        rt.add_stream(Stream::new("s-c", &d).unwrap()).unwrap();
        rt.bind_approval_ledger(
            foundry_approval::ApprovalLedger::open(&d.join("ap.db")).unwrap(),
        )
        .unwrap();

        // o-a Shipped 까지 — 저널에 rebuild 증거 심기.
        rt.dispatch("o-a", mkorder("o-a")).unwrap();
        run_to_shipped(&rt, "s-c", "o-a", "g-c", &d.join("ap.db"));

        // 회복(즉시 ack) — 스트림 rebuild 실측.
        let c = RecoveryCoordinator::new(&rt, 3);
        let out = c.recover(
            &["d-1".into()],
            || BTreeSet::from(["d-1".to_string()]),
            &d.join("ap.db"),
        );
        assert_eq!(
            out,
            RecoveryOutcome::Completed { streams_rebuilt: 1, invalidated_reservations: 0 }
        );
        assert!(!rt.is_admission_blocked(), "admission resumed");

        // 타임아웃+차단 중 dispatch 거부 — 별도 런타임 (엔진이 non-Send
        // 라 단일 스레드 시뮬레이션: 짧은 타임아웃 + 즉시 폴링).
        let orch2 = OrchestrationLog::open(&d.join("orch2.db")).unwrap();
        orch2.declare_plan(
            "q1",
            vec![("o-z".into(), vec![])],
            vec![("o-z".into(), "s-t".into())],
        )
        .unwrap();
        let rt2 = StreamRuntime::new(orch2);
        rt2.add_stream(Stream::new("s-t", &d).unwrap()).unwrap();

        let c2 = RecoveryCoordinator::new(&rt2, 0); // 즉시 타임아웃
        // (2) 차단 시작을 관측한 뒤 dispatch 시도 — recover 는 폴링 사이
        // 짧은 sleep 을 주므로, 차단 플래그 점검 후 폴링 클로저 안에서
        // dispatch 거부를 관측한다 (단일 스레드 interleaving).
        let rt2_ref = &rt2;
        let observed = std::cell::RefCell::new(false);
        let out2 = c2.recover(
            &["never".into()],
            || {
                // 차단 중 dispatch 시도 — 거부되어야.
                if !*observed.borrow() {
                    *observed.borrow_mut() = true;
                    let r = rt2_ref.dispatch("o-z", mkorder("o-z"));
                    assert!(r.is_err(), "dispatch during quiescence must be refused");
                }
                BTreeSet::new() // 절대 ack 없음 → 타임아웃
            },
            &d.join("ap.db"),
        );
        assert_eq!(
            out2,
            RecoveryOutcome::AbortedWithoutScan { missing_acks: vec!["never".into()] }
        );
        assert!(!rt2.is_admission_blocked());
        // 거부 레코드 3요소로 남음.
        let rows = rt2.orch.read_all().unwrap();
        let refusals: Vec<_> = rows
            .iter()
            .filter(|r| matches!(&r.kind, OrchestrationKind::Refusal { .. }))
            .collect();
        assert_eq!(refusals.len(), 1, "quiescence refusal recorded");
    }

    /// (게이트 6) 교차 스트림 — 두 스트림 모두 실주행, 저널 분리 단정.
    #[test]
    fn cross_stream_both_orders_run() {
        let d = tmpdir("cross");
        let orch = OrchestrationLog::open(&d.join("orch.db")).unwrap();
        orch.declare_plan(
            "p1",
            vec![("o-x".into(), vec![]), ("o-y".into(), vec![])],
            vec![("o-x".into(), "sx".into()), ("o-y".into(), "sy".into())],
        )
        .unwrap();
        let rt = StreamRuntime::new(orch);
        rt.add_stream(Stream::new("sx", &d).unwrap()).unwrap();
        rt.add_stream(Stream::new("sy", &d).unwrap()).unwrap();
        rt.bind_approval_ledger(
            foundry_approval::ApprovalLedger::open(&d.join("ap.db")).unwrap(),
        )
        .unwrap();
        rt.dispatch("o-x", mkorder("o-x")).unwrap();
        rt.dispatch("o-y", mkorder("o-y")).unwrap();
        run_to_shipped(&rt, "sx", "o-x", "g-x", &d.join("ap.db"));
        run_to_shipped(&rt, "sy", "o-y", "g-y", &d.join("ap.db"));
        let sx = rt.stream("sx").unwrap().journal_rows().unwrap();
        let sy = rt.stream("sy").unwrap().journal_rows().unwrap();
        assert!(sx.iter().all(|r| r.order_id == "o-x"), "sx owns only o-x rows");
        assert!(sy.iter().all(|r| r.order_id == "o-y"), "sy owns only o-y rows");
        assert!(sx.iter().any(|r| r.to_state == "shipped"));
        assert!(sy.iter().any(|r| r.to_state == "shipped"));
        assert_ne!(rt.stream("sx").unwrap().journal(), rt.stream("sy").unwrap().journal());
    }

    /// (astra r4) 펜스 — 실패 스트림의 실행 경로 차단·해제.
    #[test]
    fn fenced_stream_blocks_execution_until_recovery() {
        let d = tmpdir("fence");
        let orch = OrchestrationLog::open(&d.join("orch.db")).unwrap();
        orch.declare_plan(
            "p1",
            vec![("o-f".into(), vec![])],
            vec![("o-f".into(), "s-f".into())],
        )
        .unwrap();
        let rt = StreamRuntime::new(orch);
        let st = Stream::new("s-f", &d).unwrap();
        rt.add_stream(st).unwrap();
        let s = rt.stream("s-f").unwrap();
        assert!(s.engine_if_unfenced().is_ok());
        s.fence();
        assert!(s.engine_if_unfenced().is_err(), "fenced stream refuses execution");
        match s.engine_if_unfenced() {
            Err(msg) => assert!(msg.contains("fenced")),
            Ok(_) => panic!("must be fenced"),
        }
        s.unfence();
        assert!(s.engine_if_unfenced().is_ok());
    }

    /// (astra r7 b + r8) 저널 1:1 — identity/경로 필드는 비공개 불변이므로
    /// 생성 후 리네임·스왑 자체가 불가능하다. 런타임 회귀: 같은 디렉토리에서
    /// 같은 id 로 재생성한 스트림 재등록은 id·경로 유일 검사가 모두 차단.
    #[test]
    fn renamed_stream_sharing_journal_rejected() {
        let d = tmpdir("alias");
        let orch = OrchestrationLog::open(&d.join("orch.db")).unwrap();
        let rt = StreamRuntime::new(orch);
        rt.add_stream(Stream::new("a", &d).unwrap()).unwrap();
        // 같은 id 재생성 — id 유일 검사가 차단.
        let dup_id = rt.add_stream(Stream::new("a", &d).unwrap());
        assert!(dup_id.is_err());
        // journal() 접근자는 불변 참조 — 경로 스왑 불가(컴파일러 보장).
        let a = rt.stream("a").unwrap();
        assert!(a.journal().ends_with("a.db"));
        assert_eq!(a.id(), "a");
    }

    /// 스트림 id 유일 강제 (astra r1).
    #[test]
    fn duplicate_stream_id_rejected() {
        let d = tmpdir("dupid");
        let orch = OrchestrationLog::open(&d.join("orch.db")).unwrap();
        let rt = StreamRuntime::new(orch);
        rt.add_stream(Stream::new("s-dup", &d).unwrap()).unwrap();
        assert!(rt.add_stream(Stream::new("s-dup", &d).unwrap()).is_err());
    }
}
