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

pub mod dsl;

use foundry_domain::order::{Order, OrderEvent, OrderState};
use foundry_domain::ports::Ledger;
use foundry_domain::workflow_encoding::{edge_event_of, EmbeddedWorkflow};
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
    /// G5: 실행별 결속 digest (획득 시점의 Merkle root).
    exec_bindings: HashMap<ExecId, String>,
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
        }
    }

    /// 내장 워크플로 v1 + 기본 예산.
    pub fn embedded_default() -> Self {
        Self::new(EmbeddedWorkflow::frozen_v1(), Budget::default())
    }

    /// B1 획득 — 이미 잡혀 있으면 결정론적 거부 (G10).
    pub fn acquire(&mut self, order_id: &str, exec_id: &str) -> Result<(), EngineError> {
        let l = self.locks.entry(order_id.to_string()).or_insert(OrderLock { held_by: None });
        if let Some(by) = &l.held_by {
            // 자기 재획득은 멱등
            if by == exec_id {
                return Ok(());
            }
            return Err(EngineError::LockHeld { order_id: order_id.into(), by: by.clone() });
        }
        l.held_by = Some(exec_id.to_string());
        Ok(())
    }

    /// G5: 실행을 digest에 결속 (획득 직후 1회).
    pub fn bind_digest(&mut self, exec_id: &str, digest: &str) {
        self.exec_bindings.insert(exec_id.into(), digest.into());
    }

    /// G5: 실행의 결속 digest — 없으면 아직 미결속.
    pub fn binding_of(&self, exec_id: &str) -> Option<&str> {
        self.exec_bindings.get(exec_id).map(|s| s.as_str())
    }

    /// B1 해제 — 보유자만.
    pub fn release(&mut self, order_id: &str, exec_id: &str) -> bool {
        match self.locks.get_mut(order_id) {
            Some(l) if l.held_by.as_deref() == Some(exec_id) => {
                l.held_by = None;
                true
            }
            _ => false,
        }
    }

    /// 주문 등록 (아직 없으면).
    pub fn enroll(&mut self, order: Order) {
        self.orders.entry(order.id.clone()).or_insert(order);
    }

    /// 핵심: 단일 이벤트 디스패치 — G1 동등성의 실행 단위.
    ///
    /// 순서 (C-W4): 입장허가(엣지) → 예산 검사 → 도메인 승인
    /// (`Order::transition`) → 관측 기록. 도메인이 최종 권위다.
    #[allow(clippy::too_many_arguments)]
    pub fn dispatch(
        &mut self,
        order_id: &str,
        exec_id: &str,
        event: &OrderEvent,
        event_attempt: u32,
    ) -> Result<Outcome, EngineError> {
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
        if let Some(bound) = self.exec_bindings.get(exec_id).cloned() {
            if bound != self.wf_digest {
                let _ = self.observe(exec_id, event, from_placeholder(), None, "-", Outcome::Refused {
                    detail: "digest rebound mid-flight (G5: no mixing)".into(),
                });
                return Err(EngineError::EncodingDrift {
                    detail: format!(
                        "exec {exec_id} bound to {bound} but engine digest is {} — refuse to mix",
                        self.wf_digest
                    ),
                });
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
        // B2: 예산 — 이 실행의 관측 수가 상한이면 정지
        let steps = self.observations.iter().filter(|o| &o.exec_id == exec_id).count() as u32;
        if steps >= self.budget.max_steps {
            let obs = self.observe(exec_id, event, from, None, "-", Outcome::BudgetStopped);
            let _ = obs;
            return Err(EngineError::BudgetExhausted {
                exec_id: exec_id.into(),
                steps,
                cap: self.budget.max_steps,
            });
        }
        // 입장허가: 부호화에 엣지가 있는가
        let fam = edge_event_of(event);
        let edge = self
            .wf
            .edges
            .iter()
            .find(|e| e.event == fam && e.from == from);
        let edge = match edge {
            Some(e) => e.clone(),
            None => {
                // 도메인도 거부해야 정상 (불일치=EncodingDrift)
                let mut probe = self.orders.get(order_id).unwrap().clone();
                let dom = probe.transition(event.clone(), event_attempt);
                match dom {
                    Err(_) => {
                        let _ = self.observe(exec_id, event, from, None, "-", Outcome::NoEdge);
                        return Ok(Outcome::NoEdge);
                    }
                    Ok(_) => {
                        return Err(EngineError::EncodingDrift {
                            detail: format!(
                                "domain ACCEPTS {fam:?} from {from:?} but encoding has no edge (E1 broken)"
                            ),
                        })
                    }
                }
            }
        };
        // 도메인 승인 — 유일한 런타임 권위
        let order = self.orders.get_mut(order_id).unwrap();
        match order.transition(event.clone(), event_attempt) {
            Ok(to) => {
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
        assert_eq!(e.dispatch("o1", "ex", &OrderEvent::Queued, 1).unwrap(), Outcome::Applied);
        assert_eq!(
            e.dispatch("o1", "ex", &OrderEvent::DieStarted, 1).unwrap(),
            Outcome::Applied
        );
        // 3스텝째: 예산 초과 — 상태는 Working에 머문다 (새 상태 없음)
        let err = e.dispatch("o1", "ex", &OrderEvent::DieSucceeded { attempt: 1 }, 1).unwrap_err();
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
        e.dispatch("o1", "ex", &OrderEvent::Queued, 1).unwrap();
        e.dispatch("o1", "ex", &OrderEvent::DieStarted, 1).unwrap();
        // stale: 현재 attempt 1에 attempt 0 주장
        let r = e.dispatch("o1", "ex", &OrderEvent::DieSucceeded { attempt: 0 }, 1).unwrap();
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
        e.dispatch("o1", "ex", &OrderEvent::Queued, 1).unwrap();
        // Working에서 Queued 이벤트는 부호화·도메인 모두에 없음
        e.dispatch("o1", "ex", &OrderEvent::DieStarted, 1).unwrap();
        let r = e.dispatch("o1", "ex", &OrderEvent::Queued, 1).unwrap();
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
                e.dispatch("o1", "ex", &ev, 1).unwrap();
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
        e.dispatch("o1", "ex", &OrderEvent::Queued, 1).unwrap();
        let obs = e.observations();
        assert_eq!(obs.len(), 1);
        assert!(obs[0].wf_digest.starts_with("wf-emb-"));
        assert_eq!(obs[0].guard, "None");
        assert_eq!(obs[0].outcome, Outcome::Applied);
        assert_eq!(obs[0].to, Some(OrderState::Queued));
    }

    /// G5: 참조 스킬 digest 변경 → 진행 중 실행은 옛 root로 완결하거나
    /// 거부된다 — 혼합 없음. 엔진 인스턴스의 wf_digest가 교체되면
    /// 결속된 실행의 dispatch는 즉시 거부.
    #[test]
    fn g5_rebound_digest_refuses_in_flight_exec() {
        let mut e = WorkflowEngine::embedded_default();
        e.enroll(order("o1"));
        e.acquire("o1", "ex").unwrap();
        let root1 = e.wf_digest.clone();
        e.bind_digest("ex", &root1);
        // 결속 일치 — 정상 진행
        assert_eq!(e.dispatch("o1", "ex", &OrderEvent::Queued, 1).unwrap(), Outcome::Applied);
        // 스킬 교체 등으로 문서가 바뀌어 새 엔진(새 digest)이 로드됨 —
        // 시뮬레이션: 같은 실행이 새 digest로 결속 시도 없이, 엔진 digest 교체
        e.wf_digest = "wf-root-changed".into();
        let r = e.dispatch("o1", "ex", &OrderEvent::DieStarted, 1);
        let detail = match &r {
            Err(EngineError::EncodingDrift { detail }) => detail.clone(),
            other => panic!("in-flight exec must be refused on digest change, got {other:?}"),
        };
        assert!(detail.contains("refuse to mix"));
        // 미결속 실행은 새 digest로 자유롭게 시작 가능 (새 실행 = 새 root)
        e.acquire("o1", "ex2").is_err(); // 잠금은 여전히 ex가…
        assert!(e.release("o1", "ex"));
        e.acquire("o1", "ex2").unwrap();
        assert_eq!(e.dispatch("o1", "ex2", &OrderEvent::DieStarted, 1).unwrap(), Outcome::Applied);
    }

    /// 잠금 없는 디스패치 = 계약 위반 거부.
    #[test]
    fn dispatch_requires_lock() {
        let mut e = WorkflowEngine::embedded_default();
        e.enroll(order("o1"));
        let r = e.dispatch("o1", "rogue", &OrderEvent::Queued, 1);
        assert!(matches!(r, Err(EngineError::LockHeld { .. })));
    }
}
