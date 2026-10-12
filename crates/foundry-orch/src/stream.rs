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
    /// (M7.1 리뷰 B2) 이 스트림이 호스팅하는 **프리셋 루트** — 불변.
    /// 부트스트랩 주문은 자신의 preset_bound selected_root 가 이 값과
    /// 일치해야 한다(임의 스트림 부트스트랩 차단).
    preset_root: String,
    /// (astra r3) 펜스 — 회복 실패 스트림의 enroll 차단(fail-closed).
    fenced: std::sync::atomic::AtomicBool,
    /// (M7.1 리뷰 MAJOR-4) 오케스트레이션 로그 — enroll 거부의 영구 기록.
    /// add_stream 시 런타임이 주입(독립 Stream은 None — 기록 없이 거부).
    orch: Mutex<Option<Arc<OrchestrationLog>>>,
}

impl Stream {
    /// 신규 스트림 — journal_dir/{stream_id}.db 소유.
    /// (M7.1 리뷰 B2) 루트 결속 생성자 — 이 스트림의 프리셋 루트를 고정.
    pub fn new_with_root(stream_id: &str, journal_dir: &Path, preset_root: &str) -> Result<Self, String> {
        let mut s = Self::new(stream_id, journal_dir)?;
        s.preset_root = preset_root.to_string();
        Ok(s)
    }

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
        
            preset_root: String::new(),
            orch: Mutex::new(None),
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
    /// 제거). 이중 디스패치 거부. (T61 astra r1) **크레이트 내부 전용** —
    /// 등록은 dispatch 정문(입장 차단·사이즈 게이트 경유)만 가능.
    pub(crate) fn enroll(&self, order: Order) -> Result<(), String> {
        // (M7.1 리뷰 MAJOR-4) 거부는 **영구 기록** — 일시적 에러 반환만으
        // 로 끝나지 않는다(감사·재디에이션 가능).
        let record = |code: RefusalCode, oid: &str| {
            if let Some(orch) = self.orch.lock().unwrap().as_ref() {
                let _ = record_refusal(orch, code, oid, None);
            }
        };
        if self.is_fenced() {
            record(RefusalCode::FencedDispatch, &order.id);
            return Err(format!(
                "stream '{}' is fenced — recovery failed; dispatch refused until next recovery succeeds",
                self.stream_id
            ));
        }
        let oid = order.id.clone();
        let mut disp = self.dispatched.lock().unwrap();
        if disp.contains(&oid) {
            record(RefusalCode::DuplicateDispatch, &oid);
            return Err(format!("order '{oid}' already dispatched on stream {}", self.stream_id));
        }
        // (M7.1 리뷰 MAJOR-1) **내구 이중 디스패치 방지** — dispatched 는
        // 인메모리(크래시 후 소실)이므로 저널 증거로 보강: 이 주문의
        // 행이 저널에 이미 있으면 재구축 후에도 재디스패치 거부.
        let journaled = self
            .journal_rows()
            .map(|rows| rows.iter().any(|r| r.order_id == oid))
            .unwrap_or(false);
        if journaled {
            record(RefusalCode::DuplicateDispatch, &oid);
            return Err(format!(
                "order '{oid}' already dispatched on stream {} (journal evidence)",
                self.stream_id
            ));
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

    /// (M8 데모 어댑터) 주문 스냅샷 읽기 — Die 실행 재료(external ref 등).
    pub fn order_snapshot(&self, order_id: &str) -> Result<foundry_domain::Order, String> {
        self.engine
            .lock()
            .unwrap()
            .order(order_id)
            .cloned()
            .ok_or_else(|| format!("order {order_id} not enrolled"))
    }
}

/// 런타임 — 스트림 레지스트리 + 오케스트레이션 로그 + 입장 게이트.
pub struct StreamRuntime {
    pub orch: Arc<OrchestrationLog>,
    /// (astra r9 P1) 사이즈 바인딩 검증용 승인 원장 — dispatch가
    /// Chainer 게이트를 실제 호출한다(unsized-spawn 원천 차단).
    approval: Mutex<Option<&'static foundry_approval::ApprovalLedger>>,
    /// (M7.1 리뷰 B1) 원장 경로 — 라이브 핸드오프의 소유권 재오픈용.
    approval_path: Mutex<Option<std::path::PathBuf>>,
    streams: Mutex<Vec<Arc<Stream>>>,
    /// (astra r1 결합) 입장 차단 — 코디네이터가 소유.
    admission_blocked: std::sync::atomic::AtomicBool,
}

/// (publish-bridge v1.6 D2) 발행 전 재료 — precheck이 가드 해제 후 반환.
#[derive(Debug, Clone)]
pub struct PublishMaterial {
    pub digest: String,
    pub attempt: u32,
    pub op_id: String,
}

impl StreamRuntime {
    pub fn new(orch: OrchestrationLog) -> Self {
        Self {
            orch: Arc::new(orch),
            approval: Mutex::new(None),
            approval_path: Mutex::new(None),
            streams: Mutex::new(Vec::new()),
            admission_blocked: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// 발행 전 주문 상태와 발행 재료를 확인한다. 엔진 가드는 반환 전에 해제한다.
    pub fn publish_precheck(
        &self,
        stream_id: &str,
        order_id: &str,
    ) -> Result<PublishMaterial, String> {
        let stream = self.stream(stream_id).ok_or(format!("stream {stream_id} not registered"))?;
        let material = {
            let engine = stream.engine_if_unfenced()?;
            let order = engine.order(order_id).ok_or(format!("order {order_id} not enrolled"))?;
            if order.state() != OrderState::Publishing {
                return Err(format!("order {order_id} must be Publishing, was {}", order.state()));
            }
            let digest = order.artifact_digest.clone().unwrap_or_default();
            if digest.is_empty() {
                return Err(format!("order {order_id} has empty artifact digest"));
            }
            let attempt = order.attempt().max(1);
            PublishMaterial { digest, attempt, op_id: format!("{order_id}:{attempt}") }
        };
        Ok(material)
    }

    /// 발행 결과를 저널에 확정한다. 이미 확정된 주문은 같은 URL만 멱등 승인한다.
    pub fn confirm_published(
        &self,
        stream_id: &str,
        order_id: &str,
        exec: &str,
        pr_url: &str,
    ) -> Result<OrderState, String> {
        if pr_url.is_empty() || !pr_url.starts_with("https://") {
            return Err("invalid pr_url".into());
        }
        let stream = self.stream(stream_id).ok_or(format!("stream {stream_id} not registered"))?;
        let mut engine = stream.engine_if_unfenced()?;
        let state = engine.order(order_id).ok_or(format!("order {order_id} not enrolled"))?.state();
        let rows = stream.journal_rows()?;
        let has_url = |rows: &[foundry_ledger::LedgerRow]| rows.iter().any(|row| {
            if row.order_id != order_id || row.to_state != OrderState::Shipped.to_string() {
                return false;
            }
            row.event_json.as_deref().and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                .is_some_and(|v| v.get("type").and_then(|t| t.as_str()) == Some("publish_confirmed")
                    && v.get("pr_url").and_then(|u| u.as_str()) == Some(pr_url))
        });
        if state == OrderState::Shipped {
            return if has_url(&rows) { Ok(OrderState::Shipped) }
                else { Err("pr_url mismatch on shipped order".into()) };
        }
        if state != OrderState::Publishing {
            return Err(format!("order {order_id} must be Publishing or Shipped, was {state}"));
        }
        engine.acquire(order_id, exec).map_err(|e| format!("publish lock: {e:?}"))?;
        let ctx = crate::test_support::replay_ctx(order_id, &rows);
        let ledger: &'static mut foundry_ledger::SqliteLedger = Box::leak(Box::new(
            foundry_ledger::SqliteLedger::open(stream.journal()).map_err(|e| format!("journal: {e:?}"))?,
        ));
        let outcome = engine.dispatch_replay_ctx(
            order_id, exec, &OrderEvent::PublishConfirmed { pr_url: pr_url.into() }, 1, &ctx, ledger,
        ).map_err(|e| format!("publish: {e:?}"))?;
        if outcome != Outcome::Applied {
            return Err(format!("{outcome:?}"));
        }
        let state = engine.order(order_id).ok_or(format!("order {order_id} not enrolled"))?.state();
        if state != OrderState::Shipped {
            return Err(format!("order {order_id} confirmation left state {state}"));
        }
        let rows = stream.journal_rows()?;
        if !has_url(&rows) {
            return Err("published journal pr_url mismatch".into());
        }
        Ok(OrderState::Shipped)
    }

    /// (astra r9 P1) 승인 원장 결속 — 프로세스 수명 원장을 leak 해
    /// 등록한다(전역 단일 원장 계약, T58과 동일 패턴).
    pub fn bind_approval_ledger(
        &self,
        ledger: foundry_approval::ApprovalLedger,
    ) -> Result<(), String> {
        // (M7.1 리뷰 B1) 경로 보존 — 라이브 핸드오프가 소유권 있는
        // 원장 인스턴스로 재오픈할 수 있다.
        *self.approval_path.lock().unwrap() = ledger.db_path().map(|p| p.to_path_buf());
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
        // (M7.1 리뷰 MAJOR-4) 런타임의 로그를 스트림에 주입 — enroll
        // 거부가 영구 기록된다.
        let arc = Arc::new(stream);
        *arc.orch.lock().unwrap() = Some(self.orch.clone());
        g.push(arc);
        Ok(())
    }

    /// (M7 데모) 게이트에서 라이브 승인 — 원장 그랜트(CONSUMED 소비)로
    /// Approved 를 적용한다. advance_to_gate 이후의 프로덕션 경로.
    pub fn approve_at_gate(
        &self,
        stream_id: &str,
        order_id: &str,
        exec: &str,
        grant_id: &str,
        intent: &str,
        artifact_digest: &str,
    ) -> Result<foundry_workflow::Outcome, String> {
        let e = self.stream(stream_id).ok_or(format!("stream {stream_id} not registered"))?;
        let mut eng = e.engine_if_unfenced()?;
        let ap_path = self
            .approval_path
            .lock()
            .unwrap()
            .clone()
            .ok_or("approval ledger not bound (fail-closed)")?;
        let mut ap = foundry_approval::ApprovalLedger::open(&ap_path)
            .map_err(|er| format!("approval reopen: {er:?}"))?;
        ap.issue_grant(
            grant_id,
            &format!("tok-{grant_id}"),
            &serde_json::json!({
                "order_id": order_id, "intent": intent,
                "artifact_digest": artifact_digest, "root_scope": "",
            }),
        )
        .map_err(|er| format!("issue grant: {er:?}"))?;
        let mut hook = foundry_workflow::ApprovalLedgerAdapter(ap);
        let grant = foundry_workflow::GrantRef { grant_id: grant_id.into() };
        eng.dispatch_live(
            order_id,
            exec,
            &foundry_domain::OrderEvent::Approved {
                intent: intent.into(),
                artifact_digest: artifact_digest.into(),
            },
            1,
            Some(&grant),
            &mut hook,
        )
        .map_err(|er| format!("dispatch_live: {er:?}"))
    }

    /// (M7 데모) 출하 확정 — PublishConfirmed 를 재생 컨텍스트로 적용.
    pub fn confirm_publish(
        &self,
        stream_id: &str,
        order_id: &str,
        exec: &str,
        pr_url: &str,
    ) -> Result<(), String> {
        let e = self.stream(stream_id).ok_or(format!("stream {stream_id} not registered"))?;
        let mut eng = e.engine_if_unfenced()?;
        let rows = foundry_ledger::SqliteLedger::open(e.journal())
            .map_err(|er| format!("journal: {er:?}"))?
            .read_all()
            .map_err(|er| format!("journal read: {er:?}"))?;
        let ctx = crate::test_support::replay_ctx(order_id, &rows);
        let ro: &'static mut foundry_ledger::SqliteLedger = Box::leak(Box::new(
            foundry_ledger::SqliteLedger::open(e.journal())
                .map_err(|er| format!("journal: {er:?}"))?,
        ));
        eng.dispatch_replay_ctx(
            order_id,
            exec,
            &foundry_domain::OrderEvent::PublishConfirmed { pr_url: pr_url.into() },
            1,
            &ctx,
            ro,
        )
        .map_err(|er| format!("publish: {er:?}"))?;
        Ok(())
    }

    /// (M7.1 재리뷰 r2) 핸드오프 준비 — 제안 행을 만들고 **매니페스트**를
    /// 반환(그랜트 스코프 발급 재료). 소비는 dispatch_handoff.
    pub fn prepare_handoff(
        &self,
        source_order: &str,
        dest_order: &str,
        artifact_digest: &str,
    ) -> Result<(String, String), String> {
        // (r2 MAJOR-1) 회복 정지 중 입장 차단 — dispatch 와 동일 게이트.
        if self.is_admission_blocked() {
            let _ = record_refusal(
                &self.orch,
                RefusalCode::QuiescenceBlocked,
                source_order,
                None,
            );
            return Err(format!(
                "admission blocked — handoff prepare for '{source_order}' refused during recovery quiescence"
            ));
        }
        let ap_path = self
            .approval_path
            .lock()
            .unwrap()
            .clone()
            .ok_or("approval ledger not bound (fail-closed)")?;
        let ap = foundry_approval::ApprovalLedger::open(&ap_path)
            .map_err(|e| format!("approval reopen: {e:?}"))?;
        let ch = crate::chainer::Chainer::new(&self.orch, &ap);
        let proposal_id = format!("ho-{source_order}-{dest_order}-{artifact_digest}");
        let view = ch
            .propose_handoff(&proposal_id, source_order, dest_order, artifact_digest, "intake")
            .map_err(|e| format!("propose: {e:?}"))?;
        // 반환: 매니페스트 + 그랜트 스코프 발급용 6필드 재료.
        // artifact_digest 는 제안 인코딩(fp1) 문자열 — confirm 결속 기준.
        let encoded = view.artifact_digest.clone();
        Ok((view.manifest, encoded))
    }

    /// (M7.1 재리뷰 r2) 라이브 핸드오프 정문 — 체이너 6필드 스코프
    /// 규약(propose 멱등 → confirm)으로 그랜트를 CONSUMED 소비.
    pub fn dispatch_handoff(
        &self,
        source_order: &str,
        dest_order: &str,
        artifact_digest: &str,
        grant_id: &str,
    ) -> Result<(), String> {
        // (r2 MAJOR-1) 회복 정지 중 입장 차단 — dispatch 와 동일 게이트.
        if self.is_admission_blocked() {
            let _ = record_refusal(
                &self.orch,
                RefusalCode::QuiescenceBlocked,
                source_order,
                None,
            );
            return Err(format!(
                "admission blocked — handoff dispatch for '{source_order}' refused during recovery quiescence"
            ));
        }
        let ap_path = self
            .approval_path
            .lock()
            .unwrap()
            .clone()
            .ok_or("approval ledger not bound (fail-closed)")?;
        let mut ap = foundry_approval::ApprovalLedger::open(&ap_path)
            .map_err(|e| format!("approval reopen: {e:?}"))?;
        let proposal_id = format!("ho-{source_order}-{dest_order}-{artifact_digest}");
        let scope_val: serde_json::Value = {
            let g = ap
                .get_grant(grant_id)
                .map_err(|e| format!("grant lookup: {e:?}"))?;
            serde_json::from_str(&g.scope_json).map_err(|e| format!("scope parse: {e}"))?
        };
        // ① 제안(멱등) + 사전 검증 — 위조 매니페스트/무관 그랜트는
        // 소비 전 거부(그랜트 상태 무관 게이트).
        {
            let ch = crate::chainer::Chainer::new(&self.orch, &ap);
            ch.propose_handoff(&proposal_id, source_order, dest_order, artifact_digest, "intake")
                .map_err(|e| format!("propose: {e:?}"))?;
            ch.verify_handoff(&proposal_id, grant_id)
                .map_err(|e| format!("verify: {e:?}"))?;
        }
        // ② 소비 — 예약→적용 개시→확정(CONSUMED). fence 는 원장 TxGuard.
        // (r3 MAJOR-1) 이미 CONSUMED 면 소비를 건너뛰고 확정만 재수행
        // (멱등 재시도: 정상 소비 후 재호출, finalize↔confirm 사이
        // 크래시 후 재호출 모두 정문으로 회복).
        let already_consumed = ap
            .get_grant(grant_id)
            .map_err(|e| format!("grant lookup: {e:?}"))?
            .state
            == foundry_approval::GrantState::Consumed;
        if !already_consumed {
            let exec = format!("handoff-{source_order}-{dest_order}");
            let sr = ap
                .reserve_grant(grant_id, &scope_val, &exec, 1)
                .map_err(|e| format!("reserve: {e:?}"))?;
            ap.begin_apply(grant_id, &exec, 1, sr.fence_token)
                .map_err(|e| format!("begin_apply: {e:?}"))?;
            ap.finalize_grant(grant_id, &exec, 1, sr.fence_token)
                .map_err(|e| format!("finalize: {e:?}"))?;
        }
        // ③ 확정 — CONSUMED 원장 앵커로 6필드 재검증.
        let ch = crate::chainer::Chainer::new(&self.orch, &ap);
        ch.confirm_handoff(&proposal_id, grant_id)
            .map_err(|e| format!("confirm: {e:?}"))?;
        Ok(())
    }

    /// (M7.1 리뷰 B3) **실행 진입점** — 자율 단계(큐→다이 시작→다이
    /// 성공→검수 통과)를 라이브로 진행해 게이트(승인 대기)에 둔다.
    /// 테스트 전용 헬퍼를 대체하는 프로덕션 경로: 락 획득·저널 replay
    // 문맥·게이트 도달 단정까지 소유.
    pub fn advance_to_gate(
        &self,
        stream_id: &str,
        order_id: &str,
        exec: &str,
    ) -> Result<foundry_domain::OrderState, String> {
        // (r2 MAJOR-1) 회복 정지 중 입장 차단 — dispatch 와 동일 게이트.
        if self.is_admission_blocked() {
            let _ = record_refusal(
                &self.orch,
                RefusalCode::QuiescenceBlocked,
                order_id,
                None,
            );
            return Err(format!(
                "admission blocked — advance for '{order_id}' refused during recovery quiescence"
            ));
        }
        let e = self.stream(stream_id).ok_or(format!("stream {stream_id} not registered"))?;
        let mut eng = e.engine_if_unfenced()?;
        eng.acquire(order_id, exec).map_err(|er| format!("acquire: {er:?}"))?;
        for ev in [
            foundry_domain::OrderEvent::Queued,
            foundry_domain::OrderEvent::DieStarted,
            foundry_domain::OrderEvent::DieSucceeded { attempt: 1 },
            foundry_domain::OrderEvent::InspectionPassed,
        ] {
            let att = eng.order(order_id)
                .ok_or(format!("order {order_id} unknown"))?
                .attempt().max(1);
            let rows = foundry_ledger::SqliteLedger::open(e.journal())
                .map_err(|er| format!("journal: {er:?}"))?
                .read_all().map_err(|er| format!("journal read: {er:?}"))?;
            let ctx = crate::test_support::replay_ctx(order_id, &rows);
            let ro: &'static mut foundry_ledger::SqliteLedger = Box::leak(Box::new(
                foundry_ledger::SqliteLedger::open(e.journal())
                    .map_err(|er| format!("journal: {er:?}"))?,
            ));
            eng.dispatch_replay_ctx(order_id, exec, &ev, att, &ctx, ro)
                .map_err(|er| format!("advance event: {er:?}"))?;
        }
        let st = eng.order(order_id)
            .ok_or(format!("order {order_id} unknown"))?
            .state();
        Ok(st)
    }

    pub fn stream(&self, id: &str) -> Option<Arc<Stream>> {
        self.streams.lock().unwrap().iter().find(|s| s.stream_id == id).cloned()
    }

    /// (M8 데모 어댑터) 실 Die 세그먼트 시작 — Queued→DieStarted까지만
    /// 진행하고 Working 상태에서 반환한다. 호출부가 이 사이에 실제
    /// Harness(예: rustain 경유 foundary-demo PR)를 실행한 뒤
    /// finish_die_segment를 호출한다. 크래시 윈도우(DieStarted 이후
    /// DieSucceeded 이전)는 T61 재입장/재구축 경로가 이미 소유한다.
    pub fn start_die_segment(
        &self,
        stream_id: &str,
        order_id: &str,
        exec: &str,
    ) -> Result<foundry_domain::OrderState, String> {
        if self.is_admission_blocked() {
            let _ = record_refusal(
                &self.orch,
                RefusalCode::QuiescenceBlocked,
                order_id,
                None,
            );
            return Err(format!(
                "admission blocked — die segment for '{order_id}' refused during recovery quiescence"
            ));
        }
        let e = self.stream(stream_id).ok_or(format!("stream {stream_id} not registered"))?;
        let mut eng = e.engine_if_unfenced()?;
        eng.acquire(order_id, exec).map_err(|er| format!("acquire: {er:?}"))?;
        for ev in [
            foundry_domain::OrderEvent::Queued,
            foundry_domain::OrderEvent::DieStarted,
        ] {
            let att = eng
                .order(order_id)
                .ok_or(format!("order {order_id} unknown"))?
                .attempt()
                .max(1);
            let rows = foundry_ledger::SqliteLedger::open(e.journal())
                .map_err(|er| format!("journal: {er:?}"))?
                .read_all()
                .map_err(|er| format!("journal read: {er:?}"))?;
            let ctx = crate::test_support::replay_ctx(order_id, &rows);
            let ro: &'static mut foundry_ledger::SqliteLedger = Box::leak(Box::new(
                foundry_ledger::SqliteLedger::open(e.journal())
                    .map_err(|er| format!("journal: {er:?}"))?,
            ));
            eng.dispatch_replay_ctx(order_id, exec, &ev, att, &ctx, ro)
                .map_err(|er| format!("die segment start: {er:?}"))?;
        }
        let st = eng
            .order(order_id)
            .ok_or(format!("order {order_id} unknown"))?
            .state();
        Ok(st)
    }

    /// (M8 데모 어댑터) 실 Die 세그먼트 종결 — DieOutput를 엔진에 결속
    /// (artifact_digest = result_commit, M0 규약 — line executor와 동일
    /// 결속)하고 DieSucceeded→InspectionPassed를 재생해 게이트에 둔다.
    /// Die 실패는 호출부가 DieFailed를 dispatch하는 별도 경로(라인
    /// executor 백오프 정책 소유)를 쓴다 — 이 정문은 성공 경로만.
    pub fn finish_die_segment(
        &self,
        stream_id: &str,
        order_id: &str,
        exec: &str,
        die: &foundry_domain::ports::DieOutput,
    ) -> Result<foundry_domain::OrderState, String> {
        let e = self.stream(stream_id).ok_or(format!("stream {stream_id} not registered"))?;
        let mut eng = e.engine_if_unfenced()?;
        let att = eng
            .order(order_id)
            .ok_or(format!("order {order_id} unknown"))?
            .attempt()
            .max(1);
        // (no-op taxonomy, T19) result==base 또는 빈 result는 진짜 성공이
        // 아니다 — 거부해 호출부가 DieFailed 경로를 선택하게 한다.
        if die.result_commit.is_empty() || die.result_commit == die.base_commit {
            return Err(format!(
                "die produced no change (result {:?} == base {:?}) — no-op taxonomy refuses",
                die.result_commit,
                die.base_commit
            ));
        }
        let rows = foundry_ledger::SqliteLedger::open(e.journal())
            .map_err(|er| format!("journal: {er:?}"))?
            .read_all()
            .map_err(|er| format!("journal read: {er:?}"))?;
        let ctx = crate::test_support::replay_ctx(order_id, &rows);
        let ro: &'static mut foundry_ledger::SqliteLedger = Box::leak(Box::new(
            foundry_ledger::SqliteLedger::open(e.journal())
                .map_err(|er| format!("journal: {er:?}"))?,
        ));
        eng.bind_die_output(order_id, die)
            .map_err(|er| format!("bind die output: {er:?}"))?;
        for ev in [
            foundry_domain::OrderEvent::DieSucceeded { attempt: att },
            foundry_domain::OrderEvent::InspectionPassed,
        ] {
            eng.dispatch_replay_ctx(order_id, exec, &ev, att, &ctx, ro)
                .map_err(|er| format!("die segment finish: {er:?}"))?;
        }
        let st = eng
            .order(order_id)
            .ok_or(format!("order {order_id} unknown"))?
            .state();
        Ok(st)
    }

    /// 입장 차단 (코디네이터 전용). (T61 astra r2) 오케스트레이션 정지
    /// 래치와 **결합** — 회복 경계에서 preset append 도 함께 멈춘다.
    pub(crate) fn block_admission(&self) {
        self.admission_blocked.store(true, std::sync::atomic::Ordering::SeqCst);
        self.orch.set_quiescent(true);
    }

    pub(crate) fn resume_admission(&self) {
        self.admission_blocked.store(false, std::sync::atomic::Ordering::SeqCst);
        self.orch.set_quiescent(false);
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
                let stream = self.stream(&s).ok_or(format!("stream {s} not registered"))?;
                let ch = crate::chainer::Chainer::new(&self.orch, ap);
                // 주문 소스 — 인테이크 부트스트랩 판별. 소스가 없으면
                // 비부트스트랩(보수).
                // (T61) 부트스트랩은 **타입으로** 판별 — Source::Intake
                // 만 preset_bound 없는 발행 자격(문자열 접두 규약 폐지).
                let source = match order.external.source {
                    foundry_domain::order::Source::Intake => {
                        format!("intake:{}", order.external.external_id)
                    }
                    _ => format!("ext:{}", order.external.external_id),
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
                // (M7.1 리뷰 B2) **스트림 루트 결속** — 부트스트랩
                // (Source::Intake) 주문은 이 스트림의 preset_root 와
                // preset_bound selected_root 가 일치해야 한다. 임의
                // 스트림에서의 부트스트랩 차단(자기비교 폐지).
                let is_bootstrap = matches!(
                    order.external.source,
                    foundry_domain::order::Source::Intake
                );
                if is_bootstrap {
                    let bound_root = sizing
                        .as_deref()
                        .and_then(|so| self.root_of_sizing(so));
                    // (r2 B2) 빈 preset_root 스트림은 부트스트랩 거부 —
                    // 자기비교 폴백 폐지(fail-closed).
                    if stream.preset_root.is_empty() {
                        let _ = record_refusal(
                            &self.orch,
                            RefusalCode::UnsizedSpawn,
                            order_id,
                            None,
                        );
                        return Err(format!(
                            "order '{order_id}' bootstrap refused — stream '{}' has no bound preset root (use new_with_root)",
                            stream.id()
                        ));
                    }
                    let expected = Some(stream.preset_root.clone());
                    // (r2 B2 part3) 미결속(None) 부트스트랩은 루트 결속
                    // 스트림이면 허용 — 사이징 이전 입장(결속 생성 전).
                    // 결속이 존재하면 일치해야 한다.
                    if bound_root.is_some() && bound_root != expected {
                        let _ = record_refusal(
                            &self.orch,
                            RefusalCode::UnsizedSpawn,
                            order_id,
                            None,
                        );
                        return Err(format!(
                            "order '{order_id}' bootstrap root mismatch — stream expects {:?}, bound {:?}",
                            expected, bound_root
                        ));
                    }
                }
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
                // (M7.1 리뷰 MAJOR-2) **drain 증거 무신뢰** — 호출자 폴링
                // 만으로 완료 판정하지 않는다: 원장에서 in-flight 예약이
                // 실제 종결(존재하지 않거나 무효)됐는지 교차 검증.
                let drained_in_ledger = (|| -> Option<bool> {
                    let ap = foundry_approval::ApprovalLedger::open(approval_db).ok()?;
                    // (r2 MAJOR-2) 읽기 실패는 drain 아님(fail-closed).
                    let pending = ap.pending_reservations().ok()?;
                    Some(want.iter().all(|k| !pending.iter().any(|(g, _, _)| g == k)))
                })()
                .unwrap_or(false);
                if drained_in_ledger {
                    break;
                }
                // 원장이 아직 보류 중이면 폴링 주장과 불일치 — 계속 대기
                // (타임아웃이 안전 중단 담당).
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
        // (T61) 인테이크 부트스트랩 — 타입화된 Source::Intake.
        let mut o = Order::new(id.into(), ExternalRef::intake(id, &format!("dedup:{id}")), chrono::Utc::now());
        o.force_state_for_test(OrderState::Received, 1);
        o
    }

    mod publish {
    use super::*;

    fn publish_fixture(name: &str, state: OrderState) -> (PathBuf, StreamRuntime) {
        let d = tmpdir(name);
        let orch = OrchestrationLog::open(&d.join("orch.db")).unwrap();
        let rt = StreamRuntime::new(orch);
        rt.add_stream(Stream::new("publish", &d).unwrap()).unwrap();
        let mut order = mkorder("pub-order");
        order.force_state_for_test(state, 2);
        if state == OrderState::Publishing { order.artifact_digest = Some("digest-1".into()); }
        rt.stream("publish").unwrap().enroll(order).unwrap();
        (d, rt)
    }

    #[test]
    fn precheck_refuses_non_publishing() {
        let (_d, rt) = publish_fixture("precheck-state", OrderState::FurnaceHold);
        assert!(rt.publish_precheck("publish", "pub-order").unwrap_err().contains("Publishing"));
    }

    #[test]
    fn confirm_first_transition_ships() {
        let (_d, rt) = publish_fixture("confirm-first", OrderState::Publishing);
        let url = "https://github.com/x/pr/1";
        assert_eq!(rt.confirm_published("publish", "pub-order", "exec-a", url).unwrap(), OrderState::Shipped);
        assert!(rt.stream("publish").unwrap().journal_rows().unwrap().iter().any(|r|
            r.order_id == "pub-order" && r.to_state == "shipped" &&
            r.event_json.as_deref().is_some_and(|j| j.contains(url))));
    }

    #[test]
    fn confirm_idempotent_on_shipped_matching_url() {
        let (_d, rt) = publish_fixture("confirm-idempotent", OrderState::Publishing);
        let url = "https://github.com/x/pr/1";
        rt.confirm_published("publish", "pub-order", "exec-a", url).unwrap();
        let before = rt.stream("publish").unwrap().journal_rows().unwrap().len();
        assert_eq!(rt.confirm_published("publish", "pub-order", "exec-b", url).unwrap(), OrderState::Shipped);
        assert_eq!(rt.stream("publish").unwrap().journal_rows().unwrap().len(), before);
    }

    #[test]
    fn confirm_rejects_url_mismatch_on_shipped() {
        let (_d, rt) = publish_fixture("confirm-mismatch", OrderState::Publishing);
        rt.confirm_published("publish", "pub-order", "exec-a", "https://github.com/x/pr/1").unwrap();
        assert!(rt.confirm_published("publish", "pub-order", "exec-b", "https://github.com/x/pr/2").is_err());
    }

    #[test]
    fn confirm_rejects_empty_or_invalid_url() {
        let (_d, rt) = publish_fixture("confirm-invalid-url", OrderState::Publishing);
        for url in ["", "github.com/x"] {
            assert_eq!(rt.confirm_published("publish", "pub-order", "exec-a", url).unwrap_err(), "invalid pr_url");
        }
    }
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
        rt.add_stream(Stream::new_with_root("s-1", &d, "bmad-quickloop").unwrap()).unwrap();
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
        rt.add_stream(Stream::new_with_root("s-c", &d, "bmad-quickloop").unwrap()).unwrap();
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
        rt2.add_stream(Stream::new_with_root("s-t", &d, "bmad-quickloop").unwrap()).unwrap();

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
        rt.add_stream(Stream::new_with_root("sx", &d, "bmad-quickloop").unwrap()).unwrap();
        rt.add_stream(Stream::new_with_root("sy", &d, "bmad-quickloop").unwrap()).unwrap();
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
