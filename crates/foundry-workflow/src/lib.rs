//! T51b — 워크플로 실행 엔진 v1 (M6).
//!
//! 계약 준수 설계 (M6 플랜):
//! * **C-W4 이중 계층**: 이 엔진은 전이 가드를 재구현하지 않는다 —
//!   런타임 권위는 도메인의 `Order::transition`이 유일하다. 엔진은
//!   입장허가(부호화 엣지 조회 — 없으면 `NoEdgeForEvent`)로 *거부 근거를
//!   이름 붙이고*, 실제 승인은 도메인에 위임한다. 둘이 disagree하면
//!   `EncodingDrift`(치명 — 증명 E1/E2가 깨졌다는 뜻)로 표면화.
//! * **B1 주문당 활성 실행 1개**: OrderLock — 획득/해제가 저널 이벤트가
//!   아니라 엔진 로컬 관측(ops 카운터). 동시 디스패치는 두 번째
//!   획득 시도에서 **결정론적 거부**(G10: 누가 먼저든 한 쪽은
//!   LockHeld, 순서 무관).
//! * **B2 실행별 예산**: 스텝 상한. 초과 시 `BudgetExhausted` —
//!   정지 + 저널 이벤트 1개(`wf.budget_exhausted` 관측 항목으로
//!   확장 — 주문 상태머신 바깥의 실행 축) + **정의된 주문 상태 유지**
//!   (상태를 발명하지 않는다: 주문은 마지막 합법 상태에 그대로).
//! * **C-W5 단일 스트림**: 워크플로 실행 이벤트는 관측 벡터로
//!   산출된다(`observations()`) — 주문 저널 내 이름공간 항목으로
//!   심을 재료. 엔진 자체는 저널에 쓰지 않는다(테스트 결정성).
//! * **B6 관측 표면**: 어느 워크플로 digest·노드(엣지)·가드·결과를
//!   운영자가 볼 수 있는 `ExecObservation`.
//!
//! G1(바이트 동등성)의 엔진측 근거: `dispatch`는 `Order::transition`과
//! 동일한 승인·동일한 거부를 산출하고, 관측 벡터는 결정적이다 —
//! 같은 이벤트 스트림 → 같은 관측 바이트열.


/// **T56**: 승인 원장의 프로덕션 어댑터 — ApprovalLedgerHook 구현.
/// (게이트·서비스가 사용하는 정상 경로; 단위 테스트용 로컬 정의는 유지.)
pub struct ApprovalLedgerAdapter(pub foundry_approval::ApprovalLedger);

impl ApprovalLedgerHook for ApprovalLedgerAdapter {
    fn reserve(&mut self, grant_id: String, scope: serde_json::Value, exec_id: String, dispatch_seq: u64) -> Result<u64, String> {
        self.0.reserve_grant(&grant_id, &scope, &exec_id, dispatch_seq).map(|r| r.fence_token).map_err(|e| format!("{e:?}"))
    }
    fn begin_apply(&mut self, grant_id: String, exec_id: String, dispatch_seq: u64, fence: u64) -> Result<(), String> {
        self.0.begin_apply(&grant_id, &exec_id, dispatch_seq, fence).map_err(|e| format!("{e:?}"))
    }
    fn release(&mut self, grant_id: String, exec_id: String, dispatch_seq: u64, expect_fence: u64) -> Result<(), String> {
        self.0.release_reservation_fenced(&grant_id, &exec_id, dispatch_seq, expect_fence).map_err(|e| format!("{e:?}"))
    }
    fn finalize(&mut self, grant_id: String, exec_id: String, dispatch_seq: u64, fence: u64) -> Result<(), String> {
        self.0.finalize_grant(&grant_id, &exec_id, dispatch_seq, fence).map_err(|e| format!("{e:?}"))
    }
}

pub mod dsl;
pub mod skill;
pub mod validator;
pub mod router;
pub mod executor_failures;
pub mod replay;
pub mod recovery;

use foundry_domain::order::{Order, OrderEvent, OrderState};
use foundry_domain::ports::Ledger;
use foundry_domain::workflow_encoding::{edge_event_of, EdgeTarget, EmbeddedWorkflow};
use std::collections::HashMap;

/// 실행 식별자 (op_id 관례 승계).
pub type ExecId = String;

#[derive(Debug, Clone, PartialEq)]
pub enum EngineError {
    /// B1: 다른 실행이 주문을 잡고 있다 (G10 — 결정론적).
    LockHeld { order_id: String, by: ExecId },
    /// 부호화에 (이벤트 패밀리, 원천 상태) 엣지가 없다 — 입장 불가.
    /// 도메인이 수용한다면 EncodingDrift가 된다(아래).
    NoEdgeForEvent { event: String, from: OrderState },
    /// 도메인이 수용했는데 엣지가 없거나 그 반대 — E1/E2 파괴. 치명.
    EncodingDrift { detail: String },
    /// B2: 예산 소진 — 정지 + 정의된 상태(발명 없음).
    BudgetExhausted { exec_id: ExecId, steps: u32, cap: u32 },
    /// 도메인 가드 거부 (stale attempt 등) — 상태 불변.
    DomainRefusal { detail: String },
    /// T53a (v6): 라이브 승인 디스패치의 인가 실패 — 도메인 전이 미발생,
    /// 주문 저널 미기록, 실행 래치. 인가는 승인 원장이 소유한다.
    AuthorizationRefused { exec_id: ExecId, grant_id: Option<String>, reason: String },
    /// T55a (v6 B5): 매핑 불능 실행기 실패 — 주문 상태 불변, 실행 래치.
    UnmappableFailure { exec_id: ExecId, node: String, error: String },
    /// T56 P0: 저널 append 실패 — 전이 롤백됨, 관측 미발행, 실행 래치.
    Infrastructure { detail: String },
}

/// 라이브 디스패치가 제시하는 grant 참조 (T53a).
#[derive(Debug, Clone, PartialEq)]
pub struct GrantRef {
    pub grant_id: String,
}

/// 승인 원장 훅 — 엔진이 원장과 대화하는 유일한 경계 (스토어 분리).
/// 구현체(foundry-approval)가 소유: 예약·해제·확정의 실제 원자성은
/// 원장 트랜잭션이 보장한다. Err(String)은 인가 거부 사유.
pub trait ApprovalLedgerHook {
    fn reserve(&mut self, grant_id: String, scope: serde_json::Value, exec_id: String, dispatch_seq: u64) -> Result<u64, String>;
    /// apply 직전 **원자적 적용 시작** (T53a-r2 BLOCKER) — 즉시 쓰기
    /// 트랜잭션에서 소유권 검증 후 state=Applying 전환·커밋.
    fn begin_apply(&mut self, grant_id: String, exec_id: String, dispatch_seq: u64, fence: u64) -> Result<(), String>;
    fn release(&mut self, grant_id: String, exec_id: String, dispatch_seq: u64, expect_fence: u64) -> Result<(), String>;
    fn finalize(&mut self, grant_id: String, exec_id: String, dispatch_seq: u64, fence: u64) -> Result<(), String>;
}

/// B2 실행 예산.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub max_steps: u32,
}

impl Default for Budget {
    fn default() -> Self {
        Budget { max_steps: 64 }
    }
}

/// B6: 단일 노드(엣지) 실행의 관측 — 운영자 가시 표면.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecObservation {
    pub exec_id: ExecId,
    pub seq: u64,
    pub wf_digest: String,
    pub event_family: String,
    pub from: OrderState,
    pub to: Option<OrderState>,
    /// 가드 이름 (부호화에서 온 것 — 도메인 승인의 명목 근거).
    pub guard: String,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// 도메인 승인, 상태 전이 완료.
    Applied,
    /// 도메인 거부 — 상태 불변.
    Refused { detail: String },
    /// 엔진 거부 — 입장 불가 (부호화에 엣지 없음).
    NoEdge,
    /// 예산 소진 — 실행 정지.
    BudgetStopped,
}

/// B1: 주문 잠금 — 활성 실행 1개. 획득/해제는 관측된다.
struct OrderLock {
    held_by: Option<ExecId>,
}

pub struct WorkflowEngine {
    wf: EmbeddedWorkflow,
    wf_digest: String,
    budget: Budget,
    locks: HashMap<String, OrderLock>,
    orders: HashMap<String, Order>,
    observations: Vec<ExecObservation>,
    ops: u64,
    /// G5: 실행별 결속 digest (획득 시점의 문서 Merkle root).
    exec_bindings: HashMap<ExecId, String>,
    /// G5: 엔진이 서비스 중인 활성 문서 root.
    active_root: Option<String>,
    /// **P0 (T56)**: 프로덕션 저널 배선 — 설정 시 모든 Applied 전이는
    /// Ledger::append 성공 후에만 공개된다 (append 실패=롤백+래치).
    pub(crate) ledger: Option<crate::replay::LedgerHandle>,
    /// **P3 (T56)**: 저널 시각 소스 — 기본 Utc::now(), set_clock(t0)으로
    /// 고정 시계 시작(append마다 +1초). 결정론 게이트용.
    pub(crate) clock: Option<chrono::DateTime<chrono::Utc>>,
    /// 고정 시계 스텝 카운터.
    pub(crate) clock_steps: u64,
    /// 정지한 실행 — 값은 정지 사유 (예산 소진 또는 치명 드리프트).
    /// astra r3 MAJOR: every fatal drift path must latch (missing edge,
    /// unsatisfied guard, destination mismatch — a drifted exec must
    /// not be resumable). r3 MINOR: the latched CAUSE is preserved —
    /// a drift-latched exec reports drift, not a fictitious budget
    /// exhaustion.
    halted: std::collections::HashMap<ExecId, HaltCause>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HaltCause {
    BudgetExhausted { steps: u32, cap: u32 },
    EncodingDrift { detail: String },
    /// T53a (v6 E): 승인 계열 이벤트의 라이브 디스패치가 유효 grant
    /// 예약에 실패 — 실행 래치. detail은 바이트 동일 재생.
    AuthorizationRefused { grant_id: Option<String>, reason: String },
    /// T55a (v6 B5/G14): 매핑 불능 실행기 오류 — 합법적 실패 엣지가 없는
    /// 상태. 엔진 래치, 주문 상태 불변 (새 상태 발명 금지).
    UnmappableFailure { node: String, error: String },
    /// T56 P0: 저널 append 실패 — 인프라 계열 정지 (실행기·원장 아님).
    Infrastructure { detail: String },
}

impl WorkflowEngine {
    pub fn new(wf: EmbeddedWorkflow, budget: Budget) -> Self {
        let wf_digest = wf.digest();
        WorkflowEngine {
            wf,
            wf_digest,
            budget,
            locks: HashMap::new(),
            orders: HashMap::new(),
            observations: Vec::new(),
            ops: 0,
            exec_bindings: HashMap::new(),
            active_root: None,
            ledger: None,
            clock: None,
            clock_steps: 0,
            halted: Default::default(),
        }
    }

    /// 내장 워크플로 v1 + 기본 예산.
    pub fn embedded_default() -> Self {
        Self::new(EmbeddedWorkflow::frozen_v1(), Budget::default())
    }

    /// B1 획득 — 이미 잡혀 있으면 결정론적 거부 (G10). 획득은 관측된다
    /// (wf.lock_acquired — astra: "acquisition/release produce neither
    /// observations nor journal records").
    pub fn acquire(&mut self, order_id: &str, exec_id: &str) -> Result<(), EngineError> {
        let l = self.locks.entry(order_id.to_string()).or_insert(OrderLock { held_by: None });
        if let Some(by) = &l.held_by {
            if by == exec_id {
                return Ok(());
            }
            return Err(EngineError::LockHeld { order_id: order_id.into(), by: by.clone() });
        }
        l.held_by = Some(exec_id.to_string());
        self.ops += 1;
        self.observations.push(ExecObservation {
            exec_id: exec_id.into(),
            seq: self.ops,
            wf_digest: self.wf_digest.clone(),
            event_family: "wf.lock_acquired".into(),
            from: OrderState::Unknown,
            to: None,
            guard: "-".into(),
            outcome: Outcome::Applied,
        });
        Ok(())
    }

    /// G5 (재작): 실행을 **문서 Merkle root**에 결속 — 획득 직후 필수.
    /// 재결속 시도(덮어쓰기)는 거부한다 (astra: "optional, overwritable").
    /// 인자는 문서 root여야 한다 — encoding digest가 아니라.
    pub fn bind_root(&mut self, exec_id: &str, doc_root: &str) -> Result<(), EngineError> {
        if let Some(prev) = self.exec_bindings.get(exec_id) {
            if prev != doc_root {
                return Err(EngineError::EncodingDrift {
                    detail: format!(
                        "exec {exec_id} already bound to {prev} — rebinding to {doc_root} refused (G5)"
                    ),
                });
            }
            return Ok(());
        }
        self.exec_bindings.insert(exec_id.into(), doc_root.into());
        Ok(())
    }

    /// G5: 실행의 결속 root.
    pub fn binding_of(&self, exec_id: &str) -> Option<&str> {
        self.exec_bindings.get(exec_id).map(|s| s.as_str())
    }

    /// G5: 엔진이 현재 서비스하는 활성 문서 root (문서 교체 시 갱신).
    pub fn set_active_root(&mut self, root: &str) {
        self.active_root = Some(root.to_string());
    }

    /// B1 해제 — 보유자만. 해제도 관측된다 (wf.lock_released).
    pub fn release(&mut self, order_id: &str, exec_id: &str) -> bool {
        match self.locks.get_mut(order_id) {
            Some(l) if l.held_by.as_deref() == Some(exec_id) => {
                l.held_by = None;
                self.ops += 1;
                self.observations.push(ExecObservation {
                    exec_id: exec_id.into(),
                    seq: self.ops,
                    wf_digest: self.wf_digest.clone(),
                    event_family: "wf.lock_released".into(),
                    from: OrderState::Unknown,
                    to: None,
                    guard: "-".into(),
                    outcome: Outcome::Applied,
                });
                true
            }
            _ => false,
        }
    }

    /// 주문 등록 (아직 없으면).
    pub fn enroll(&mut self, order: Order) {
        self.orders.entry(order.id.clone()).or_insert(order);
    }

    /// **라이브 디스패치 (T53a, v6 B)** — 승인 계열 이벤트
    /// (Approved/Rejected)는 `grant` 제시가 필수다. grant는 (grant_id,
    /// scope) 쌍이며 엔진은 예약(reserve)을 **승인 원장 훅**에 위임한다 —
    /// 훅이 Err면 AuthorizationRefused 래치 (도메인 전이 미발생·주문
    /// 저널 미기록). 성공하면 확정(finalize) 훅이 전이 적용 직후 불린다.
    ///
    /// 승인 원장 자체(foundry-approval)는 엔진에 의존하지 않는다 — 엔진은
    /// 트레잇 훅으로만 대화한다 (스토어 분리 원칙 유지).
    pub fn dispatch_live<L>(
        &mut self,
        order_id: &str,
        exec_id: &str,
        event: &OrderEvent,
        event_attempt: u32,
        grant: Option<&GrantRef>,
        approval: &mut L,
    ) -> Result<Outcome, EngineError>
    where
        L: ApprovalLedgerHook,
    {
        // halted 최선위 (v6 E) — 래치된 실행은 예약조차 시도하지 않고
        // 최초 사유를 재생한다 (astra T53-r1 MAJOR: 사유 보존은 insert만이
        // 아니라 반환 경로 전체에서 성립해야 한다).
        if self.halted.contains_key(exec_id) {
            return Err(self.halted_error(exec_id));
        }
        // 승인 계열 이벤트 판별 — 인가가 필요한 전이만.
        let needs_authority = matches!(event, OrderEvent::Approved { .. } | OrderEvent::Rejected { .. });
        if needs_authority {
            // 연산 정체성 — seq는 이 디스패치 내내 불변 (관측 수와 무관하게
            // 진입 시 1회 계산. astra T53-r1 MAJOR: release가 N-1을 쓰는
            // 불일치 폐쇄).
            let seq = self.next_dispatch_seq(exec_id);
            let scope_json = self.scope_for(order_id, event);
            let reservation = match grant {
                None => Err("no grant presented (live approval dispatch requires a GrantRef)".to_string()),
                Some(g) => approval.reserve(g.grant_id.clone(), scope_json, exec_id.to_string(), seq),
            };
            match reservation {
                Ok(fence) => {
                    // **사전 전수 검증 (astra r3/r4 MAJOR)**: begin_apply를
                    // 내기 전에 실패 가능성 전부를 순수 읽기로 확인한다 —
                    // 잠금·예산·(패밀리,from) 엣지 존재·가드 만족 가능.
                    // 하나라도 결격이면 begin_apply 없이 dispatch_inner로 —
                    // 전이가 안 날 경로는 인가를 소비하지 않는다 (Applying
                    // 좌초 원천 차단).
                    let can_apply = (|| -> bool {
                        // 잠금 — 이 실행이 주문을 잡고 있어야.
                        match self.locks.get(order_id) {
                            Some(l) if l.held_by.as_deref() == Some(exec_id) => {}
                            _ => return false,
                        }
                        // 예산 — 잔여 스텝. dispatch_inner와 **동일 필터**
                        // (wf.lock_* 관측 제외 — astra r5 BLOCKER: 카운트
                        // 불일치가 precheck 오판을 냈다).
                        let steps = self.observations.iter().filter(|o| {
                            o.exec_id == exec_id && !o.event_family.starts_with("wf.lock_")
                        }).count() as u32;
                        if steps >= self.budget.max_steps {
                            return false;
                        }
                        // root 결속 — active root 엔진은 실행 결속 필수
                        // (dispatch_inner의 G5 검사와 동일 — astra r5 MAJOR)
                        // **및** 결속 root가 active root와 일치 (astra r6
                        // 조건: stale-bound 실행의 라이브 승인이 Applying을
                        // 좌초시키던 갭 폐쇄).
                        if let Some(active) = &self.active_root {
                            match self.exec_bindings.get(exec_id) {
                                None => return false,
                                Some(bound) if bound != active => return false,
                                _ => {}
                            }
                        }
                        // 엣지 — 부호화에 (패밀리, from) 존재.
                        let from_state = self.orders.get(order_id).map(|o| o.state()).unwrap_or(OrderState::Received);
                        let fam = foundry_domain::workflow_encoding::edge_event_of(event);
                        if !self.wf.edges.iter().any(|e| e.event == fam && e.from == from_state) {
                            return false;
                        }
                        // 가드 — 현재 시도값에서 만족 가능.
                        let cur_attempt = self.orders.get(order_id).map(|o| o.attempt()).unwrap_or(1);
                        self.wf
                            .edges
                            .iter()
                            .any(|e| e.event == fam && e.from == from_state
                                && foundry_domain::workflow_encoding::guard_satisfied(&e.guard, event, cur_attempt))
                    })();
                    if !can_apply {
                        // 전이가 나지 못한다 — begin_apply 없이 진행·거부.
                        let out = self.dispatch_inner(order_id, exec_id, event, event_attempt);
                        if matches!(&out, Ok(Outcome::Applied)) {
                            // astra r5 BLOCKER: precheck 불능인데 전이가 났다면
                            // precheck와 dispatch_inner의 논리가 갈라진 것 —
                            // 엔진 계약 위반. 래치하고 치명으로 보고 (인가 없는
                            // 적용은 절대 승인하지 않는다).
                            let detail = "precheck judged dispatch inapplicable but dispatch_inner APPLIED — engine invariant broken (precheck/inner logic divergence)".to_string();
                            if !self.halted.contains_key(exec_id) {
                                self.halted.insert(exec_id.into(), HaltCause::EncodingDrift { detail: detail.clone() });
                            }
                            return Err(EngineError::EncodingDrift { detail });
                        }
                        // 실패한 디스패치의 예약은 즉시 해제 (NoEdge 포함).
                        if !matches!(&out, Ok(Outcome::Applied)) {
                            if let Some(g) = grant {
                                // 해제 실패도 무시하지 않는다: 래치+전파.
                                if let Err(cleanup) = approval.release(g.grant_id.clone(), exec_id.to_string(), seq, fence) {
                                    let detail = format!(
                                        "grant {} release FAILED after refusal: {cleanup} — recovery_scan will reconcile; exec halted",
                                        g.grant_id
                                    );
                                    if !self.halted.contains_key(exec_id) {
                                        self.halted.insert(exec_id.into(), HaltCause::EncodingDrift { detail: detail.clone() });
                                    }
                                    return Err(EngineError::EncodingDrift { detail });
                                }
                            }
                        }
                        return out;
                    }

                    // apply 직전 **원자적 적용 시작** (astra r2 BLOCKER):
                    // 즉시 쓰기 트랜잭션에서 소유권 검증+Applying 전환이
                    // 함께 커밋 — 검증↔전이 사이 회복 끼임 불가.
                    if let Some(g) = grant {
                        if let Err(reason) = approval.begin_apply(g.grant_id.clone(), exec_id.to_string(), seq, fence) {
                            let grant_id = grant.map(|g| g.grant_id.clone());
                            if !self.halted.contains_key(exec_id) {
                                self.halted.insert(
                                    exec_id.into(),
                                    HaltCause::AuthorizationRefused { grant_id: grant_id.clone(), reason: reason.clone() },
                                );
                                let _ = self.observe(exec_id, event, from_placeholder(), None, "-", Outcome::Refused { detail: "authorization refused (apply-intent lost)".into() });
                            }
                            return Err(EngineError::AuthorizationRefused { exec_id: exec_id.into(), grant_id, reason });
                        }
                    }
                    let out = self.dispatch_inner(order_id, exec_id, event, event_attempt);
                    let applied = matches!(&out, Ok(Outcome::Applied));
                    if !applied {
                        // 거부/NoEdge/에러 전부 — 예약은 소비되지 않는다.
                        // (astra MAJOR: NoEdge가 grant를 소비하던 결함 폐쇄.)
                        if let Some(g) = grant {
                            if let Err(cleanup) = approval.release(g.grant_id.clone(), exec_id.to_string(), seq, fence) {
                                // astra r3/r4 MAJOR: 클린업 실패도 래치 —
                                // 방치 예약은 회복이 정리하되 실행 정지.
                                let detail = format!(
                                    "grant {} release FAILED after refusal: {cleanup} — recovery_scan will reconcile; exec halted",
                                    g.grant_id
                                );
                                if !self.halted.contains_key(exec_id) {
                                    self.halted.insert(exec_id.into(), HaltCause::EncodingDrift { detail: detail.clone() });
                                }
                                return Err(EngineError::EncodingDrift { detail });
                            }
                        }
                    } else if let Some(g) = grant {
                        // 전이 적용 성공 직후 — 확정. 확정 실패는 삼키지 않는다
                        // (astra BLOCKER): 그 상태로 Applied를 반환하면
                        // grant가 재사용 가능한 채 남는다. 치명 에러로 전파.
                        if let Err(reason) = approval.finalize(g.grant_id.clone(), exec_id.to_string(), seq, fence) {
                            // astra r2 MAJOR: "halting exec" 주장을 실제로 —
                            // 래치. Applying이므로 회복은 CONSUMED로 완결한다.
                            let detail = format!(
                                "grant {} finalized FAILED after transition: {reason} — recovery_scan will complete CONSUMED; exec halted", g.grant_id
                            );
                            if !self.halted.contains_key(exec_id) {
                                self.halted.insert(exec_id.into(), HaltCause::EncodingDrift { detail: detail.clone() });
                            }
                            return Err(EngineError::EncodingDrift { detail });
                        }
                    }
                    out
                }
                Err(reason) => {
                    let grant_id = grant.map(|g| g.grant_id.clone());
                    // 래치 사유 보존 (astra MAJOR): 이미 래치된 실행은 새 실패로
                    // 사유를 덮지 않는다 — halted 조회가 최선위이므로 여기는
                    // 첫 인가 실패만 기록한다.
                    if !self.halted.contains_key(exec_id) {
                        self.halted.insert(
                            exec_id.into(),
                            HaltCause::AuthorizationRefused { grant_id: grant_id.clone(), reason: reason.clone() },
                        );
                        let _ = self.observe(exec_id, event, from_placeholder(), None, "-", Outcome::Refused { detail: "authorization refused (exec latched)".into() });
                    }
                    return Err(EngineError::AuthorizationRefused { exec_id: exec_id.into(), grant_id, reason });
                }
            }
        } else {
            self.dispatch_inner(order_id, exec_id, event, event_attempt)
        }
    }

    /// **T56 P1b**: 무커서 공개 dispatch_replay도 제거되었다 — 재생의
    /// 유일한 입구는 replay::ReplayContext 소유 dispatch_replay_ctx다
    /// (G17① 원천 봉쇄: 커서 없는 재생은 존재하지 않는다).

    /// 실행의 현재 래치 사유 조회 (T55a).
    pub fn halted_of(&self, exec_id: &str) -> Option<&HaltCause> {
        self.halted.get(exec_id)
    }

    /// 래치 사유를 EngineError로 재생 (T55a) — 사유 종류별 동일 규율.
    pub fn halted_error(&self, exec_id: &str) -> EngineError {
        match self.halted.get(exec_id).expect("caller checked halted_of") {
            HaltCause::BudgetExhausted { steps, cap } => EngineError::BudgetExhausted {
                exec_id: exec_id.into(),
                steps: *steps,
                cap: *cap,
            },
            HaltCause::EncodingDrift { detail } => EngineError::EncodingDrift { detail: detail.clone() },
            HaltCause::AuthorizationRefused { grant_id, reason } => EngineError::AuthorizationRefused {
                exec_id: exec_id.into(),
                grant_id: grant_id.clone(),
                reason: reason.clone(),
            },
            HaltCause::Infrastructure { detail } => EngineError::Infrastructure { detail: detail.clone() },
            HaltCause::UnmappableFailure { node, error } => EngineError::UnmappableFailure {
                exec_id: exec_id.into(),
                node: node.clone(),
                error: error.clone(),
            },
        }
    }

    /// 외부 진입점용 래치 — 사유 + 관측 1회 (T55a 규율: 바이트 동일 detail).
    pub fn halt_with(&mut self, exec_id: &str, cause: HaltCause, detail: String) {
        if self.halted.contains_key(exec_id) {
            return; // astra r2 MAJOR: 최초 사유 보존 — 덮지 않는다.
        }
        self.halted.insert(exec_id.into(), cause);
        let _ = self.observe(exec_id, &foundry_domain::OrderEvent::Queued, from_placeholder(), None, "-", Outcome::Refused { detail });
    }

    /// 이 실행의 다음 디스패치 일련번호 (연산 정체성 — 예약 키).
    fn next_dispatch_seq(&self, exec_id: &str) -> u64 {
        self.observations.iter().filter(|o| o.exec_id == exec_id).count() as u64 + 1
    }

    /// 승인 이벤트의 grant 스코프 — 주문 상태 + 활성 root에서 유도.
    /// (astra T53-r1 MAJOR: 엔진 스코프가 root를 누락해 grant_scope의
    /// root_scope와 정합하지 않았다 — 이제 active_root가 설정된 엔진의
    /// 라이브 승인은 root-결속 grant만 매칭된다.)
    fn scope_for(&self, order_id: &str, event: &OrderEvent) -> serde_json::Value {
        let root_scope = self.active_root.clone().unwrap_or_default();
        match event {
            OrderEvent::Approved { intent, artifact_digest } => serde_json::json!({
                "order_id": order_id, "intent": intent, "artifact_digest": artifact_digest,
                "root_scope": root_scope,
            }),
            OrderEvent::Rejected { intent } => serde_json::json!({
                "order_id": order_id, "intent": intent, "artifact_digest": "",
                "root_scope": root_scope,
            }),
            _ => unreachable!("scope_for is only called for approval events"),
        }
    }

    /// **T56 P1b**: 공개 무커서 dispatch는 **제거**되었다 — 재생의 유일한
    /// 입구는 ReplayContext 소유 dispatch_replay_ctx다 (G17① 원천 봉쇄).
    /// 내부 테스트·직접 경로는 dispatch_inner(비공개)를 사용한다.
    pub(crate) fn dispatch_inner_pub(
        &mut self,
        order_id: &str,
        exec_id: &str,
        event: &OrderEvent,
        event_attempt: u32,
    ) -> Result<Outcome, EngineError> {
        self.dispatch_inner(order_id, exec_id, event, event_attempt)
    }

    /// 핵심: 단일 이벤트 디스패치 — G1 동등성의 실행 단위.
    ///
    /// 순서 (C-W4): 입장허가(엣지) → 예산 검사 → 도메인 승인
    /// (`Order::transition`) → 관측 기록. 도메인이 최종 권위다.
    #[allow(clippy::too_many_arguments)]
    fn dispatch_inner(
        &mut self,
        order_id: &str,
        exec_id: &str,
        event: &OrderEvent,
        event_attempt: u32,
    ) -> Result<Outcome, EngineError> {
        // halted 최선위 (v6 E · astra r3/r5 · T56 P1b — 공개 래퍼 제거로
        // 내부로 통합): 래치된 실행은 잠금·커서와 무관하게 최초 사유 재생.
        if self.halted.contains_key(exec_id) {
            return Err(self.halted_error(exec_id));
        }
        // B1: 잠금 확인 — 미획득 디스패치는 계약 위반
        match self.locks.get(order_id) {
            Some(l) if l.held_by.as_deref() == Some(exec_id) => {}
            _ => {
                return Err(EngineError::LockHeld {
                    order_id: order_id.into(),
                    by: "<unheld>".into(),
                })
            }
        }
        // G5: 결속된 실행은 자기 시점의 digest로 완결 — 스킬/정책
        // 교체로 root가 바뀌어도 진행 중 실행은 옛 root를 고수하거나
        // 명시적으로 거부된다 (혼합 없음).
        let active_root_now = self.active_root.clone();
        // astra r2 MAJOR: unbound executions bypass root changes — on an
        // engine with an active root, dispatch REQUIRES a binding.
        if let Some(active) = &active_root_now {
            if !self.exec_bindings.contains_key(exec_id) {
                return Err(EngineError::EncodingDrift {
                    detail: format!(
                        "exec {exec_id} has NO root binding — dispatch refused while active root {active} is set (G5 mandatory)"
                    ),
                });
            }
        }
        if let Some(bound) = self.exec_bindings.get(exec_id).cloned() {
            if let Some(active) = &active_root_now {
                if &bound != active {
                    let _ = self.observe(exec_id, event, from_placeholder(), None, "-", Outcome::Refused {
                        detail: "doc root changed mid-flight (G5: no mixing)".into(),
                    });
                    return Err(EngineError::EncodingDrift {
                        detail: format!(
                            "exec {exec_id} bound to {bound} but active root is {active} — refuse to mix"
                        ),
                    });
                }
            }
        }
        // 상태 조회 (등록 필요)
        let from = self
            .orders
            .get(order_id)
            .map(|o| o.state())
            .ok_or_else(|| EngineError::EncodingDrift {
                detail: format!("order {order_id} not enrolled"),
            })?;
        // B2: 예산 — 노드 실행만 계수 (wf.lock_* 생명주기 관측은 제외:
        // B2: 예산 — 노드 실행만 계수 (wf.lock_* 생명주기 관측은 제외).
        // astra r2 MAJOR: exhaustion LATCHES — repeated dispatches return
        // the same error without appending more stop records (the old
        // code double-counted prior BudgetStopped rows as steps and
        // appended a new stop record each time).
        if let Some(HaltCause::EncodingDrift { detail }) = self.halted.get(exec_id) {
            return Err(EngineError::EncodingDrift { detail: detail.clone() });
        }
        if let Some(HaltCause::BudgetExhausted { steps, cap }) = self.halted.get(exec_id) {
            return Err(EngineError::BudgetExhausted {
                exec_id: exec_id.into(),
                steps: *steps,
                cap: *cap,
            });
        }
        if let Some(HaltCause::AuthorizationRefused { grant_id, reason }) = self.halted.get(exec_id) {
            return Err(EngineError::AuthorizationRefused {
                exec_id: exec_id.into(),
                grant_id: grant_id.clone(),
                reason: reason.clone(),
            });
        }
        if let Some(HaltCause::UnmappableFailure { node, error }) = self.halted.get(exec_id) {
            return Err(EngineError::UnmappableFailure {
                exec_id: exec_id.into(),
                node: node.clone(),
                error: error.clone(),
            });
        }
        let steps = self.observations.iter().filter(|o| {
            &o.exec_id == exec_id && !o.event_family.starts_with("wf.lock_")
        }).count() as u32;
        if steps >= self.budget.max_steps {
            self.halted.insert(exec_id.into(), HaltCause::BudgetExhausted { steps, cap: self.budget.max_steps });
            let _ = self.observe(exec_id, event, from, None, "-", Outcome::BudgetStopped);
            return Err(EngineError::BudgetExhausted {
                exec_id: exec_id.into(),
                steps,
                cap: self.budget.max_steps,
            });
        }
        // 입장허가: 부호화에 엣지가 있는가 — 가드를 만족하는 것으로 선택.
        // (astra MAJOR "first event/from edge wins": DieFailed from Working
        // has TWO edges (MAX / retry back-edge); .find() always took the
        // first. Now the guard picks — guard_satisfied is advisory only;
        // the destination cross-check below remains the authority.)
        let fam = edge_event_of(event);
        let candidates: Vec<&_> = self
            .wf
            .edges
            .iter()
            .filter(|e| e.event == fam && e.from == from)
            .collect();
        let edge = candidates
            .iter()
            .find(|e| foundry_domain::workflow_encoding::guard_satisfied(&e.guard, event, current_attempt_of(self.orders.get(order_id).unwrap())));
        let edge = match edge {
            Some(e) => (*e).clone(),
            None if candidates.is_empty() => {
                // 도메인도 거부해야 정상 (불일치=EncodingDrift)
                let mut probe = self.orders.get(order_id).unwrap().clone();
                let dom = probe.transition(event.clone(), event_attempt);
                match dom {
                    Err(_) => {
                        let _ = self.observe(exec_id, event, from, None, "-", Outcome::NoEdge);
                        return Ok(Outcome::NoEdge);
                    }
                    Ok(_) => {
                        let detail = format!(
                            "domain ACCEPTS {fam:?} from {from:?} but encoding has no edge (E1 broken)"
                        );
                        self.halted.insert(exec_id.into(), HaltCause::EncodingDrift { detail: detail.clone() });
                        let _ = self.observe(exec_id, event, from, None, "-", Outcome::Refused { detail: "encoding drift: no edge (exec latched)".into() });
                        return Err(EngineError::EncodingDrift { detail });
                    }
                }
            }
            None => {
                // 엣지는 있으나 어느 가드도 만족하지 않는다 — 도메인도 거부해야
                // 정상 (E7). 도메인이 수용하면 드리프트.
                let mut probe = self.orders.get(order_id).unwrap().clone();
                match probe.transition(event.clone(), event_attempt) {
                    Err(_) => {
                        let _ = self.observe(exec_id, event, from, None, "-", Outcome::Refused { detail: "no guard satisfied".into() });
                        return Ok(Outcome::Refused { detail: "no guard satisfied (E7-consistent)".into() });
                    }
                    Ok(_) => {
                        let detail = format!(
                            "domain ACCEPTS {fam:?} from {from:?} but NO matching edge's guard is satisfied (E7 broken)"
                        );
                        self.halted.insert(exec_id.into(), HaltCause::EncodingDrift { detail: detail.clone() });
                        let _ = self.observe(exec_id, event, from, None, "-", Outcome::Refused { detail: "encoding drift: no guard satisfied (exec latched)".into() });
                        return Err(EngineError::EncodingDrift { detail });
                    }
                }
            }
        };
        // 도메인 승인 — 유일한 런타임 권위. astra r2 MAJOR(atomicity):
        // 드리프트 거부가 살아있는 주문을 진행시켜선 안 된다 — 전이 전
        // 스냅샷으로 원자적 되돌림.
        let order = self.orders.get_mut(order_id).unwrap();
        let snapshot = order.clone();
        let before = order.attempt();
        match order.transition(event.clone(), event_attempt) {
            Ok(to) => {
                // astra MAJOR fix: the domain's REACHED destination must
                // match the edge's destination (state AND token effect).
                // The old code compared nothing after the first edge
                // match — a tampered destination never drifted.
                let edge_dest_ok = match &edge.to {
                    // DieStarted의 claim 효과(attempt := max(before,1))는
                    // 상태 엣지의 정당한 부수효과다 (도메인이 그렇게 정의).
                    EdgeTarget::State(t) => {
                        *t == to
                            && (order.attempt() == before
                                || (edge.event == foundry_domain::workflow_encoding::EdgeEvent::DieStarted
                                    && order.attempt() == before.max(1)))
                    }
                    EdgeTarget::BackEdge { to: t, .. } => *t == to && order.attempt() == before + 1,
                };
                if !edge_dest_ok {
                    // 원자적 되돌림 + 관측 기록 후 정지 (래치).
                    // astra r4 MINOR: ONE shared detail string — the latch,
                    // the returned error, and redispatch are byte-identical.
                    *order = snapshot;
                    let after = order.attempt();
                    let edge_repr = format!("{:?}", &edge.to);
                    drop(order);
                    let detail = format!(
                        "destination mismatch: edge says {edge_repr} (domain reached {to:?}, attempt {before}->{after}) — E1/E2 violated; order ROLLED BACK, exec latched"
                    );
                    self.halted.insert(exec_id.into(), HaltCause::EncodingDrift { detail: detail.clone() });
                    let _ = self.observe(exec_id, event, from, None, guard_name(&edge.guard).as_str(), Outcome::Refused { detail: "encoding drift: destination mismatch (exec latched)".into() });
                    return Err(EngineError::EncodingDrift { detail });
                }
                // P0 (T56): 저널 배선 — append 성공 후에만 상태 공개·관측.
                // append 실패 = 전이 무효 (롤백·관측 미발행·Infrastructure 래치).
                if let Some(handle) = &self.ledger {
                    let at = match self.clock {
                        Some(t0) => {
                            self.clock_steps += 1;
                            t0 + chrono::Duration::seconds(self.clock_steps as i64)
                        }
                        None => chrono::Utc::now(),
                    };
                    match crate::replay::journal_append(handle, order_id, event, from, to, at) {
                        Ok(()) => {}
                        Err(e) => {
                            *order = snapshot;
                            drop(order);
                            let detail = format!(
                                "journal append FAILED after transition: {e} — transition ROLLED BACK, no observation, exec latched (infrastructure)"
                            );
                            if !self.halted.contains_key(exec_id) {
                                self.halted.insert(exec_id.into(), HaltCause::Infrastructure { detail: detail.clone() });
                            }
                            // astra T56 MAJOR: append 실패는 관측도 내지 않는다
                            // — "no observation" 계약의 문자 그대로 이행.
                            return Err(EngineError::Infrastructure { detail });
                        }
                    }
                }
                drop(order);
                let _ = self.observe(exec_id, event, from, Some(to), guard_name(&edge.guard).as_str(), Outcome::Applied);
                Ok(Outcome::Applied)
            }
            Err(e) => {
                // 도메인 거부는 정상 경로 (가드) — EncodingDrift 아님:
                // 엣지는 존재하지만 가드 조건 미충족일 뿐.
                let _ = self.observe(exec_id, event, from, None, guard_name(&edge.guard).as_str(), Outcome::Refused { detail: e.to_string() });
                Ok(Outcome::Refused { detail: e.to_string() })
            }
        }
    }

    /// B2 예산 초과 시의 저널 심기 재료 — 주문 상태는 발명하지 않는다.
    pub fn budget_stop_journal_events(&self, exec_id: &str) -> Vec<String> {
        self.observations
            .iter()
            .filter(|o| &o.exec_id == exec_id && o.outcome == Outcome::BudgetStopped)
            .map(|o| format!("wf.budget_exhausted exec={} step_cap={}", o.exec_id, self.budget.max_steps))
            .collect()
    }

    /// 관측 벡터 (C-W5 — 주문 저널 내 이름공간 항목 재료).
    pub fn observations(&self) -> &[ExecObservation] {
        &self.observations
    }

    /// G1 동등성 검사 재료: 저널에 실제로 심을 (order_id, event) 스트림을
    /// 엔진 경유와 직접 경유로 각각 실행해 동일한 결과가 나오는지.
    pub fn order(&self, order_id: &str) -> Option<&Order> {
        self.orders.get(order_id)
    }

    fn observe(
        &mut self,
        exec_id: &str,
        event: &OrderEvent,
        from: OrderState,
        to: Option<OrderState>,
        guard: &str,
        outcome: Outcome,
    ) -> ExecObservation {
        self.ops += 1;
        let obs = ExecObservation {
            exec_id: exec_id.into(),
            seq: self.ops,
            wf_digest: self.wf_digest.clone(),
            event_family: format!("{:?}", edge_event_of(event)),
            from,
            to,
            guard: guard.into(),
            outcome,
        };
        self.observations.push(obs.clone());
        obs
    }
}

fn from_placeholder() -> OrderState {
    OrderState::Unknown
}

fn current_attempt_of(o: &Order) -> u32 {
    o.attempt()
}

/// 가드 이름 (관측용).
fn guard_name(g: &foundry_domain::workflow_encoding::Guard) -> String {
    format!("{g:?}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use foundry_domain::order::{ExternalRef, Order};

    fn order(id: &str) -> Order {
        Order::new(id.into(), ExternalRef::cron("a", "b"), Default::default())
    }

    /// G10: 동시 디스패치 → 결정론적 거부. 누가 먼저든 한 쪽만 잡는다.
    #[test]
    fn g10_concurrent_dispatch_is_deterministically_refused() {
        let mut e = WorkflowEngine::embedded_default();
        e.enroll(order("o1"));
        // 두 실행이 같은 주문을 잡으려 한다
        let a = e.acquire("o1", "exec-a");
        let b = e.acquire("o1", "exec-b");
        assert!(a.is_ok());
        assert!(matches!(b, Err(EngineError::LockHeld { by, .. }) if by == "exec-a"));
        // 역순으로도: b가 잡으면 a는 거부 (순서 무관성)
        assert!(e.release("o1", "exec-a"));
        assert!(e.acquire("o1", "exec-b").is_ok());
        assert!(matches!(e.acquire("o1", "exec-a"), Err(EngineError::LockHeld { by, .. }) if by == "exec-b"));
        // 해제는 보유자만
        assert!(!e.release("o1", "exec-a"));
        assert!(e.release("o1", "exec-b"));
    }

    /// B2/G11: 예산 위반 → 정지 + 정의된 상태 (발명 없음).
    #[test]
    fn g11_budget_exhaustion_stops_with_defined_state() {
        let mut e = WorkflowEngine::new(EmbeddedWorkflow::frozen_v1(), Budget { max_steps: 2 });
        e.enroll(order("o1"));
        e.acquire("o1", "ex").unwrap();
        // 합법 2스텝
        assert_eq!(e.dispatch_inner("o1", "ex", &OrderEvent::Queued, 1).unwrap(), Outcome::Applied);
        assert_eq!(
            e.dispatch_inner("o1", "ex", &OrderEvent::DieStarted, 1).unwrap(),
            Outcome::Applied
        );
        // 3스텝째: 예산 초과 — 상태는 Working에 머문다 (새 상태 없음)
        let err = e.dispatch_inner("o1", "ex", &OrderEvent::DieSucceeded { attempt: 1 }, 1).unwrap_err();
        assert!(matches!(err, EngineError::BudgetExhausted { steps: 2, cap: 2, .. }));
        assert_eq!(e.order("o1").unwrap().state(), OrderState::Working);
        // 저널 심기 재료가 산출된다
        assert!(!e.budget_stop_journal_events("ex").is_empty());
    }

    /// C-W4/G3 계: 도메인은 유일 권위 — 엣지가 있어도 가드 미충족이면 거부,
    /// 상태 불변. (stale attempt)
    #[test]
    fn domain_refusal_leaves_state_untouched() {
        let mut e = WorkflowEngine::embedded_default();
        e.enroll(order("o1"));
        e.acquire("o1", "ex").unwrap();
        e.dispatch_inner("o1", "ex", &OrderEvent::Queued, 1).unwrap();
        e.dispatch_inner("o1", "ex", &OrderEvent::DieStarted, 1).unwrap();
        // stale: 현재 attempt 1에 attempt 0 주장
        let r = e.dispatch_inner("o1", "ex", &OrderEvent::DieSucceeded { attempt: 0 }, 1).unwrap();
        assert!(matches!(r, Outcome::Refused { .. }));
        assert_eq!(e.order("o1").unwrap().state(), OrderState::Working);
    }

    /// NoEdge 경로: 부호화에 없는 (이벤트, 상태) — 도메인도 거부하므로
    /// NoEdge (드리프트 아님).
    #[test]
    fn no_edge_no_drift() {
        let mut e = WorkflowEngine::embedded_default();
        e.enroll(order("o1"));
        e.acquire("o1", "ex").unwrap();
        e.dispatch_inner("o1", "ex", &OrderEvent::Queued, 1).unwrap();
        // Working에서 Queued 이벤트는 부호화·도메인 모두에 없음
        e.dispatch_inner("o1", "ex", &OrderEvent::DieStarted, 1).unwrap();
        let r = e.dispatch_inner("o1", "ex", &OrderEvent::Queued, 1).unwrap();
        assert_eq!(r, Outcome::NoEdge);
    }

    /// G1 엔진측: 같은 스트림 → 같은 관측 바이트열 (결정성).
    #[test]
    fn g1_engine_determinism_same_stream_same_observations() {
        let run = || {
            let mut e = WorkflowEngine::embedded_default();
            e.enroll(order("o1"));
            e.acquire("o1", "ex").unwrap();
            for ev in [
                OrderEvent::Queued,
                OrderEvent::DieStarted,
                OrderEvent::DieSucceeded { attempt: 1 },
                OrderEvent::InspectionPassed,
            ] {
                e.dispatch_inner("o1", "ex", &ev, 1).unwrap();
            }
            format!("{:?}", e.observations())
        };
        assert_eq!(run(), run());
    }

    /// B6: 관측은 wf digest·가드·결과를 담는다.
    #[test]
    fn observations_expose_operational_surface() {
        let mut e = WorkflowEngine::embedded_default();
        e.enroll(order("o1"));
        e.acquire("o1", "ex").unwrap();
        e.dispatch_inner("o1", "ex", &OrderEvent::Queued, 1).unwrap();
        let all = e.observations();
        // B1 생명주기 관측(lock_acquired) + 노드 관측 1
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].event_family, "wf.lock_acquired");
        let obs = &all[1];
        assert!(obs.wf_digest.starts_with("wf-emb-"));
        assert_eq!(obs.guard, "None");
        assert_eq!(obs.outcome, Outcome::Applied);
        assert_eq!(obs.to, Some(OrderState::Queued));
    }

    /// G5: 참조 스킬 digest 변경 → 진행 중 실행은 옛 root로 완결하거나
    /// 거부된다 — 혼합 없음. 엔진 인스턴스의 wf_digest가 교체되면
    /// 결속된 실행의 dispatch는 즉시 거부.
    #[test]
    fn g5_rebound_digest_refuses_in_flight_exec() {
        let mut e = WorkflowEngine::embedded_default();
        e.enroll(order("o1"));
        e.acquire("o1", "ex").unwrap();
        let root1 = "wf-root-doc-original".to_string();
        e.bind_root("ex", &root1).unwrap();
        e.set_active_root(&root1);
        // 결속 일치 — 정상 진행
        assert_eq!(e.dispatch_inner("o1", "ex", &OrderEvent::Queued, 1).unwrap(), Outcome::Applied);
        // 스킬 교체 등으로 문서가 바뀌어 새 root가 로드됨 — 결속 재시도는 거부
        assert!(e.bind_root("ex", "wf-root-changed").is_err(), "rebinding must be refused");
        e.set_active_root("wf-root-changed");
        let r = e.dispatch_inner("o1", "ex", &OrderEvent::DieStarted, 1);
        let detail = match &r {
            Err(EngineError::EncodingDrift { detail }) => detail.clone(),
            other => panic!("in-flight exec must be refused on digest change, got {other:?}"),
        };
        assert!(detail.contains("refuse to mix"));
        // 새 실행도 결속 없이는 시작 불가 (r2 MAJOR: unbound bypass 폐쇄)
        let _ = e.acquire("o1", "ex2").is_err();
        assert!(e.release("o1", "ex"));
        e.acquire("o1", "ex2").unwrap();
        assert!(e.dispatch_inner("o1", "ex2", &OrderEvent::DieStarted, 1).is_err(), "unbound dispatch must be refused");
        e.bind_root("ex2", "wf-root-changed").unwrap();
        assert_eq!(e.dispatch_inner("o1", "ex2", &OrderEvent::DieStarted, 1).unwrap(), Outcome::Applied);
    }

    /// r3 MINOR 회귀: 변조 목적지 → 원자적 롤백 + 래치 + 사유 보존.
    /// 엔진의 부호화에서 엣지 목적지를 찢으면 dispatch는 드리프트를
    /// 내고, 주문은 전이 전 상태 그대로, 실행은 재디스패치 불가,
    /// 재시도는 같은 드리프트 사유를 반환한다 (가짜 예산 아님).
    #[test]
    fn destination_drift_rolls_back_and_latches_with_cause() {
        let mut e = WorkflowEngine::embedded_default();
        e.enroll(order("o1"));
        e.acquire("o1", "ex").unwrap();
        e.dispatch_inner("o1", "ex", &OrderEvent::Queued, 1).unwrap();
        // 부호화 변조: Queued/Received 엣지의 목적지를 Failed로 찢는다
        e.wf.edges[0].to = EdgeTarget::State(OrderState::Failed);
        // 미결속 엔진(active_root 없음)에서도 목적지 드리프트는 치명 —
        // 새 등록 주문으로 재현
        e.enroll(order("o2"));
        e.acquire("o2", "ex3").unwrap();
        let r = e.dispatch_inner("o2", "ex3", &OrderEvent::Queued, 1);
        let detail = match &r {
            Err(EngineError::EncodingDrift { detail }) => detail.clone(),
            other => panic!("expected drift, got {other:?}"),
        };
        assert!(detail.contains("destination mismatch"), "{detail}");
        // 주문은 Received 그대로 (롤백)
        assert_eq!(e.order("o2").unwrap().state(), OrderState::Received);
        // 래치: 재디스패치는 **동일한** 사유 (문자열 동등성)
        let r2 = e.dispatch_inner("o2", "ex3", &OrderEvent::Queued, 1);
        assert_eq!(
            r2, r,
            "redispatch must replay the identical drift error (byte identity)"
        );
        // 사유 보존: BudgetExhausted로 위장하지 않는다
        assert!(!matches!(r2, Err(EngineError::BudgetExhausted { .. })));
    }

    /// r4 MINOR 회귀: 엣지 제거 드리프트 — DieStarted 엣지를 부호화에서
    /// 지우면 (도메인은 수용, 부호화에는 없음 = E1 위반) dispatch는
    /// 드리프트로 래치되고, 다른 합법 이벤트(Canceled)도 거부된다.
    #[test]
    fn missing_edge_drift_latches_and_blocks_other_events() {
        let mut e = WorkflowEngine::embedded_default();
        e.enroll(order("o1"));
        e.acquire("o1", "ex").unwrap();
        e.dispatch_inner("o1", "ex", &OrderEvent::Queued, 1).unwrap();
        // 부호화 변조: DieStarted 엣지 제거 (도메인엔 있음 — E1 위반)
        e.wf.edges.retain(|edge| edge.event != foundry_domain::workflow_encoding::EdgeEvent::DieStarted);
        let r = e.dispatch_inner("o1", "ex", &OrderEvent::DieStarted, 1);
        assert!(matches!(&r, Err(EngineError::EncodingDrift { detail }) if detail.contains("no edge")), "{r:?}");
        // 래치 확인: 다른 합법 이벤트조차 같은 드리프트로 거부
        let r2 = e.dispatch_inner("o1", "ex", &OrderEvent::Canceled { reason: "r".into() }, 1);
        assert_eq!(r2, r, "latched exec refuses even otherwise-legal events with the same cause");
    }

    /// r4 MINOR 회귀: 가드 미충족 드리프트 — 엣지는 있으나 어느 가드도
    /// 만족하지 않는데 도메인이 수용하면 (E7 위반) 드리프트로 래치.
    #[test]
    fn unsatisfied_guard_drift_latches() {
        let mut e = WorkflowEngine::embedded_default();
        e.enroll(order("o1"));
        e.acquire("o1", "ex").unwrap();
        e.dispatch_inner("o1", "ex", &OrderEvent::Queued, 1).unwrap();
        // 부호화 변조: DieStarted 엣지의 가드를 절대 만족 불가로 교체
        // (가드 평가기는 거짓, 도메인은 수용 — 가드/도메인 불일치 = E7 위반)
        for edge in e.wf.edges.iter_mut() {
            if edge.event == foundry_domain::workflow_encoding::EdgeEvent::DieStarted {
                edge.guard = foundry_domain::workflow_encoding::Guard::Conj(vec![
                    foundry_domain::workflow_encoding::Guard::MaxAttemptsExhausted,
                    foundry_domain::workflow_encoding::Guard::RetriesRemain,
                ]); // 모순 가드: 항상 거짓
            }
        }
        let r = e.dispatch_inner("o1", "ex", &OrderEvent::DieStarted, 1);
        assert!(matches!(&r, Err(EngineError::EncodingDrift { detail }) if detail.contains("NO matching edge's guard")), "{r:?}");
        let r2 = e.dispatch_inner("o1", "ex", &OrderEvent::DieStarted, 1);
        assert_eq!(r2, r, "redispatch replays the identical cause");
    }

    /// r3 MINOR 회귀: 예산 소진은 정확히 한 번 관측되고 이후 디스패치는
    /// 같은 에러를 반복한다 (stop record 증가 없음).
    #[test]
    fn budget_exhaustion_latches_with_single_stop_record() {
        let mut e = WorkflowEngine::new(EmbeddedWorkflow::frozen_v1(), Budget { max_steps: 1 });
        e.enroll(order("o1"));
        e.acquire("o1", "ex").unwrap();
        assert_eq!(e.dispatch_inner("o1", "ex", &OrderEvent::Queued, 1).unwrap(), Outcome::Applied);
        let stops_before = e.observations().iter()
            .filter(|o| &o.exec_id == "ex" && o.outcome == Outcome::BudgetStopped).count();
        assert_eq!(stops_before, 0);
        // 초과 디스패치 3회 — 모두 같은 에러
        for _ in 0..3 {
            assert!(matches!(
                e.dispatch_inner("o1", "ex", &OrderEvent::DieStarted, 1),
                Err(EngineError::BudgetExhausted { .. })
            ));
        }
        // stop 관측은 정확히 1개 (반복 없음)
        let stops_after = e.observations().iter()
            .filter(|o| &o.exec_id == "ex" && o.outcome == Outcome::BudgetStopped).count();
        assert_eq!(stops_after, 1, "exactly one durable stop record");
    }

    // ------------------------------------------------------------------
    // G3 실행 케이스 (T53a) — 실제 승인 원장으로 dispatch_live 집행.
    // ② 무grant ③ 타스코프 grant ④ 유효 인간 승인 대조군.
    // ------------------------------------------------------------------

    /// 원장 훅 어댑터 — foundry-approval의 ApprovalLedger를 엔진 트레잇에.
    struct LedgerHook(foundry_approval::ApprovalLedger);

    impl ApprovalLedgerHook for LedgerHook {
        fn reserve(&mut self, grant_id: String, scope: serde_json::Value, exec_id: String, dispatch_seq: u64) -> Result<u64, String> {
            self.0.reserve_grant(&grant_id, &scope, &exec_id, dispatch_seq).map(|r| r.fence_token).map_err(|e| e.to_string())
        }
        fn begin_apply(&mut self, grant_id: String, exec_id: String, dispatch_seq: u64, fence: u64) -> Result<(), String> {
            self.0.begin_apply(&grant_id, &exec_id, dispatch_seq, fence).map_err(|e| e.to_string())
        }
        fn release(&mut self, grant_id: String, exec_id: String, dispatch_seq: u64, expect_fence: u64) -> Result<(), String> {
            self.0.release_reservation_fenced(&grant_id, &exec_id, dispatch_seq, expect_fence).map_err(|e| e.to_string())
        }
        fn finalize(&mut self, grant_id: String, exec_id: String, dispatch_seq: u64, fence: u64) -> Result<(), String> {
            self.0.finalize_grant(&grant_id, &exec_id, dispatch_seq, fence).map_err(|e| e.to_string())
        }
    }

    fn g3_engine() -> WorkflowEngine {
        let mut e = WorkflowEngine::embedded_default();
        let mut o = order("g3-o");
        // FurnaceHold까지 직행 — 승인 게이트 앞에 세운다.
        o.force_state_for_test(OrderState::FurnaceHold, 1);
        e.enroll(o);
        e
    }

    fn g3_approval_event() -> OrderEvent {
        OrderEvent::Approved { intent: "ship".into(), artifact_digest: "dig-1".into() }
    }

    /// ② 무grant → AuthorizationRefused: 상태 불변·래치·재디스패치 동일 사유.
    #[test]
    fn g3_live_no_grant_is_authorization_refused() {
        let mut e = g3_engine();
        e.acquire("g3-o", "ex").unwrap();
        let mut hook = LedgerHook(foundry_approval::ApprovalLedger::open_in_memory().unwrap());
        let r = e.dispatch_live("g3-o", "ex", &g3_approval_event(), 1, None, &mut hook);
        let (gid, reason) = match &r {
            Err(EngineError::AuthorizationRefused { grant_id, reason, .. }) => (grant_id.clone(), reason.clone()),
            other => panic!("expected AuthorizationRefused, got {other:?}"),
        };
        assert_eq!(gid, None);
        assert!(reason.contains("no grant"), "{reason}");
        // 상태 불변 (도메인 전이 미발생).
        assert_eq!(e.order("g3-o").unwrap().state(), OrderState::FurnaceHold);
        // 래치 — 재디스패치는 동일 사유 (다른 합법 이벤트도).
        let r2 = e.dispatch_live("g3-o", "ex", &g3_approval_event(), 1, None, &mut hook);
        assert_eq!(r, r2, "redispatch replays the identical authorization failure");
    }

    /// ③ 유효하지만 다른 스코프의 grant → 거부 (전 스코프 매칭).
    #[test]
    fn g3_live_wrong_scope_grant_refused() {
        let mut e = g3_engine();
        e.acquire("g3-o", "ex").unwrap();
        let mut ledger = foundry_approval::ApprovalLedger::open_in_memory().unwrap();
        // 다른 artifact_digest 스코프로 발급된 (진짜 인간) grant.
        let wrong_scope = serde_json::json!({
            "order_id": "g3-o", "intent": "ship", "artifact_digest": "dig-OTHER", "root_scope": "",
        });
        ledger.issue_grant("g-wrong", "tok-1", &wrong_scope).unwrap();
        let mut hook = LedgerHook(ledger);
        let grant = GrantRef { grant_id: "g-wrong".into() };
        let r = e.dispatch_live("g3-o", "ex", &g3_approval_event(), 1, Some(&grant), &mut hook);
        assert!(
            matches!(&r, Err(EngineError::AuthorizationRefused { grant_id: Some(g), reason, .. }) if g == "g-wrong" && reason.contains("scope")),
            "{r:?}"
        );
        assert_eq!(e.order("g3-o").unwrap().state(), OrderState::FurnaceHold, "state unchanged");
    }

    /// ④ 유효 인간 승인 대조군 — grant 소비로 전이 성공.
    #[test]
    fn g3_live_valid_grant_applies_and_consumes() {
        let mut e = g3_engine();
        e.acquire("g3-o", "ex").unwrap();
        let mut ledger = foundry_approval::ApprovalLedger::open_in_memory().unwrap();
        // 엔진에 active_root가 없으면 root_scope=''/ — 정합 스코프.
        let right = serde_json::json!({
            "order_id": "g3-o", "intent": "ship", "artifact_digest": "dig-1", "root_scope": "",
        });
        ledger.issue_grant("g-right", "tok-9", &right).unwrap();
        let mut hook = LedgerHook(ledger);
        let grant = GrantRef { grant_id: "g-right".into() };
        let out = e.dispatch_live("g3-o", "ex", &g3_approval_event(), 1, Some(&grant), &mut hook);
        assert_eq!(out.unwrap(), Outcome::Applied);
        assert_eq!(e.order("g3-o").unwrap().state(), OrderState::Publishing);
        // grant는 CONSUMED — 재사용 불가 (래치된 게 아니라 소비됨).
        assert_eq!(hook.0.get_grant("g-right").unwrap().state, foundry_approval::GrantState::Consumed);
    }

    /// 재생 경로는 grant 불요 — G1 하위호환 (승인 이벤트도 커서 없는
    /// 내부 디스패치로 재생된다: M1-M5 재생 불변).
    #[test]
    fn g3_replay_needs_no_grant() {
        let mut e = g3_engine();
        e.acquire("g3-o", "ex").unwrap();
        let mut hook = LedgerHook(foundry_approval::ApprovalLedger::open_in_memory().unwrap());
        let _ = &mut hook;
        let out = e.dispatch_inner("g3-o", "ex", &g3_approval_event(), 1);
        assert_eq!(out.unwrap(), Outcome::Applied, "replay of a journaled approval needs no grant (envelope owns integrity)");
    }

    /// (astra T53-r1 MAJOR 4) 래치 사유 보존 — 첫 인가 실패 사유가 이후
    /// 실패로 덮이지 않는다. 재디스패치는 항상 최초 사유 재생.
    #[test]
    fn t53r1_latch_cause_first_failure_wins() {
        let mut e = g3_engine();
        e.acquire("g3-o", "ex").unwrap();
        let mut hook = LedgerHook(foundry_approval::ApprovalLedger::open_in_memory().unwrap());
        let r1 = e.dispatch_live("g3-o", "ex", &g3_approval_event(), 1, None, &mut hook).unwrap_err();
        assert!(matches!(&r1, EngineError::AuthorizationRefused { grant_id: None, .. }));
        // 다른 grant로 재시도 — 여전히 최초 사유 (None) 재생.
        let grant = GrantRef { grant_id: "g-other".into() };
        let r2 = e.dispatch_live("g3-o", "ex", &g3_approval_event(), 1, Some(&grant), &mut hook).unwrap_err();
        assert_eq!(r1, r2, "latched cause must NOT be overwritten by later failures");
    }

    /// (astra T53-r1 MAJOR 3) NoEdge는 grant를 소비하지 않는다 —
    /// 승인 상태가 아닌 곳에서 라이브 승인 디스패치.
    #[test]
    fn t53r1_noedge_does_not_consume_grant() {
        let mut e = WorkflowEngine::embedded_default();
        e.enroll(order("g3-n2"));
        e.acquire("g3-n2", "ex").unwrap();
        let mut ledger = foundry_approval::ApprovalLedger::open_in_memory().unwrap();
        // 승인 상태와 무관한 스코프로 발급 (매칭될 것).
        let sc = serde_json::json!({
            "order_id": "g3-n2", "intent": "ship", "artifact_digest": "dig-1", "root_scope": "",
        });
        ledger.issue_grant("g-n", "tok-n", &sc).unwrap();
        let mut hook = LedgerHook(ledger);
        let grant = GrantRef { grant_id: "g-n".into() };
        // Received에서 Approved — 부호화에 엣지 없음 = NoEdge.
        let out = e.dispatch_live("g3-n2", "ex", &g3_approval_event(), 1, Some(&grant), &mut hook).unwrap();
        assert_eq!(out, Outcome::NoEdge);
        // grant는 소비되지 않았다 — Issued.
        assert_eq!(hook.0.get_grant("g-n").unwrap().state, foundry_approval::GrantState::Issued, "NoEdge must NOT consume the grant");
    }

    /// (astra T53-r1 MAJOR 5) root-결속 라이브 승인 — active_root 엔진의
    /// 스코프는 root를 포함: 타 root grant는 거부, 정확 root grant는 통과.
    #[test]
    fn t53r1_root_bound_live_approval() {
        let mut e = g3_engine();
        e.set_active_root("wf-root-X");
        e.acquire("g3-o", "ex").unwrap();
        e.bind_root("ex", "wf-root-X").unwrap();
        let mut ledger = foundry_approval::ApprovalLedger::open_in_memory().unwrap();
        // 타 root로 발급된 (유효한 인간) grant — 거부되어야.
        let wrong_root = serde_json::json!({
            "order_id": "g3-o", "intent": "ship", "artifact_digest": "dig-1", "root_scope": "wf-root-OTHER",
        });
        ledger.issue_grant("g-w", "tok-w", &wrong_root).unwrap();
        let mut hook = LedgerHook(ledger);
        let grant = GrantRef { grant_id: "g-w".into() };
        let r = e.dispatch_live("g3-o", "ex", &g3_approval_event(), 1, Some(&grant), &mut hook);
        assert!(matches!(r, Err(EngineError::AuthorizationRefused { .. })), "{r:?}");
        assert_eq!(e.order("g3-o").unwrap().state(), OrderState::FurnaceHold);
    }

    /// (astra r5 BLOCKER) precheck 불능 경로에서 전이가 나지 않는다 —
    /// 예산 0 엔진의 라이브 승인: Applied 불가, grant 미소비.
    #[test]
    fn t53r5_precheck_failure_never_applies_or_consumes() {
        let mut e = WorkflowEngine::new(EmbeddedWorkflow::frozen_v1(), Budget { max_steps: 0 });
        let mut o = order("r5-o");
        o.force_state_for_test(OrderState::FurnaceHold, 1);
        e.enroll(o);
        e.acquire("r5-o", "ex").unwrap();
        let mut ledger = foundry_approval::ApprovalLedger::open_in_memory().unwrap();
        let sc = serde_json::json!({
            "order_id": "r5-o", "intent": "ship", "artifact_digest": "dig-1", "root_scope": "",
        });
        ledger.issue_grant("g-r5", "tok-r5", &sc).unwrap();
        let mut hook = LedgerHook(ledger);
        let grant = GrantRef { grant_id: "g-r5".into() };
        let out = e.dispatch_live("r5-o", "ex", &g3_approval_event(), 1, Some(&grant), &mut hook);
        // 예산 0 → BudgetExhausted (Applied 아님), grant는 미소비.
        assert!(matches!(out, Err(EngineError::BudgetExhausted { .. })), "{out:?}");
        assert_eq!(hook.0.get_grant("g-r5").unwrap().state, foundry_approval::GrantState::Issued, "precheck-ineligible dispatch must NOT consume");
        assert_eq!(e.order("r5-o").unwrap().state(), OrderState::FurnaceHold);
    }

    /// (astra r5 MAJOR) root 미결속 실행의 라이브 승인 — precheck에서
    /// 결격 → Applying 없이 거부, grant 무사.
    #[test]
    fn t53r5_unbound_exec_root_refusal_preserves_grant() {
        let mut e = WorkflowEngine::embedded_default();
        e.set_active_root("wf-root-Z");
        let mut o = order("r5-u");
        o.force_state_for_test(OrderState::FurnaceHold, 1);
        e.enroll(o);
        e.acquire("r5-u", "ex").unwrap();
        // ex는 root 결속 없음 — G5 위반 경로.
        let mut ledger = foundry_approval::ApprovalLedger::open_in_memory().unwrap();
        let sc = serde_json::json!({
            "order_id": "r5-u", "intent": "ship", "artifact_digest": "dig-1", "root_scope": "wf-root-Z",
        });
        ledger.issue_grant("g-u", "tok-u", &sc).unwrap();
        let mut hook = LedgerHook(ledger);
        let grant = GrantRef { grant_id: "g-u".into() };
        let r = e.dispatch_live("r5-u", "ex", &g3_approval_event(), 1, Some(&grant), &mut hook);
        assert!(matches!(r, Err(EngineError::EncodingDrift { .. })), "{r:?}");
        assert_eq!(hook.0.get_grant("g-u").unwrap().state, foundry_approval::GrantState::Issued, "root refusal must not strand Applying");
        assert_eq!(e.order("r5-u").unwrap().state(), OrderState::FurnaceHold);
    }

    /// (astra r6 조건) stale-bound 실행의 라이브 승인 — precheck에서
    /// 결격(root 불일치) → Applying 없이 거부, grant 무사.
    #[test]
    fn t53r6_stale_binding_root_mismatch_preserves_grant() {
        let mut e = WorkflowEngine::embedded_default();
        e.set_active_root("wf-root-NEW");
        let mut o = order("r6-o");
        o.force_state_for_test(OrderState::FurnaceHold, 1);
        e.enroll(o);
        e.acquire("r6-o", "ex").unwrap();
        e.bind_root("ex", "wf-root-OLD").unwrap(); // 결속은 있으나 active와 불일치
        let mut ledger = foundry_approval::ApprovalLedger::open_in_memory().unwrap();
        let sc = serde_json::json!({
            "order_id": "r6-o", "intent": "ship", "artifact_digest": "dig-1", "root_scope": "wf-root-NEW",
        });
        ledger.issue_grant("g-r6", "tok-r6", &sc).unwrap();
        let mut hook = LedgerHook(ledger);
        let grant = GrantRef { grant_id: "g-r6".into() };
        let r = e.dispatch_live("r6-o", "ex", &g3_approval_event(), 1, Some(&grant), &mut hook);
        assert!(matches!(r, Err(EngineError::EncodingDrift { .. })), "{r:?}");
        assert_eq!(hook.0.get_grant("g-r6").unwrap().state, foundry_approval::GrantState::Issued, "stale-binding refusal must not consume");
        assert_eq!(e.order("r6-o").unwrap().state(), OrderState::FurnaceHold);
    }

    /// 비승인 이벤트는 라이브여도 grant 불요 — 자율 구간.
    #[test]
    fn g3_live_non_approval_event_needs_no_grant() {
        let mut e = WorkflowEngine::embedded_default();
        e.enroll(order("g3-n"));
        e.acquire("g3-n", "ex").unwrap();
        let mut hook = LedgerHook(foundry_approval::ApprovalLedger::open_in_memory().unwrap());
        let out = e.dispatch_live("g3-n", "ex", &OrderEvent::Queued, 1, None, &mut hook);
        assert_eq!(out.unwrap(), Outcome::Applied);
    }

    /// 잠금 없는 디스패치 = 계약 위반 거부.
    #[test]
    fn dispatch_requires_lock() {
        let mut e = WorkflowEngine::embedded_default();
        e.enroll(order("o1"));
        let r = e.dispatch_inner("o1", "rogue", &OrderEvent::Queued, 1);
        assert!(matches!(r, Err(EngineError::LockHeld { .. })));
    }
}
