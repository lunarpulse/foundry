//! T55a — 실행기 실패 매핑 (M6 플랜 v6, B5/G14).
//!
//! **계약 (플랜 v6 B5)**: 노드 실행 오류는 M1 **기존** 실패 전이에 매핑된다 —
//! 새 상태 발명 금지. 3분기:
//! * **재시도 가능 (Retryable)**: 일시 오류(타임아웃·일시 네트워크 등) →
//!   도메인의 기존 back-edge (`DieFailed`/`InspectionFailed` retry —
//!   attempt 증가 후 Queued 복귀)를 그대로 사용.
//! * **소진 (Exhausted)**: 재시도를 전부 소진 → 도메인의 기존
//!   MaxAttempts 경로 (`DieFailed` attempt>=MAX → Failed).
//! * **매핑 불능 (Unmappable)**: 도메인에 대응 실패 전이가 없는 오류
//!   (예: Received 상태에서 실행기 크래시 — 합법적 실패 엣지 없음) →
//!   **엔진 래치 정지** (주문 상태 불변 — 상태 발명 금지 원칙의 필연
//!   귀결). HaltCause::UnmappableFailure.
//!
//! 매핑 테이블은 **선언적이고 결정론적**: 같은 (오류 종류, 주문 상태,
//! 시도 상황)은 항상 같은 매핑. 매핑 자체가 저널 기록 가능한 값이다.

use crate::{EngineError, Outcome, WorkflowEngine};
use foundry_domain::{OrderEvent, OrderState};

/// 실행기 오류 종류 — 매핑의 입력 (실행기가 분류해서 제시).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutorErrorKind {
    /// 일시 실패 — 재시도가 의미 있음 (타임아웃·일시 네트워크).
    Transient,
    /// 영구 실패 — 재시도 무의미 (논리 오류·검증 거부).
    Permanent,
    /// 실행기 자체 오류 (샌드박스 붕괴·OOM 등) — 도메인 개념 아님.
    Infrastructure,
}

/// 매핑 결과 — 3분기 (G14의 각 분기를 직접 나타낸다).
#[derive(Debug, Clone, PartialEq)]
pub enum FailureMapping {
    /// 기존 back-edge로 재시도: DieFailed {reason} (도메인이 attempt++ 후
    /// Queued 복귀) — 재시도 가능.
    RetryViaDieFailed { reason: String },
    /// 기존 소진 경로: DieFailed를 현재 attempt로 — attempt>=MAX면 도메인이
    /// Failed로 (실행기는 MAX 클레임만 제공, 판정은 도메인).
    ExhaustedViaDieFailed,
    /// 매핑 불능: 합법적 실패 엣지가 없는 상태 — 엔진 래치, 주문 불변.
    Unmappable { node: String, error: String },
}

/// **실패 매핑기** — (오류 종류, 주문 상태) → 매핑. 결정론적 순수 함수.
///
/// 규칙 (B5):
/// * Working/Queued 계열(실행 노드 문맥)에서 Transient → RetryViaDieFailed
/// * Permanent 또는 시도 소진 상황 → ExhaustedViaDieFailed
///   (도메인의 MaxAttempts 가드가 최종 판정 — 실행기는 판정하지 않는다)
/// * Inspecting 문맥의 오류는 InspectionFailed 경로가 담당하지만 그것은
///   검사기(inspector)의 몫 — 실행기 매핑은 Die 축만 (역할 분리 유지).
/// * 합법적 실패 엣지가 없는 상태(Received/FurnaceHold/Publishing/
///   Shipped/...)에서의 실행기 오류 → Unmappable (래치).
pub fn map_failure(
    kind: &ExecutorErrorKind,
    order_state: OrderState,
    node: &str,
    error: &str,
) -> FailureMapping {
    match (kind, order_state) {
        // 실행 노드 문맥 — Die 축의 기존 전이가 산다.
        (ExecutorErrorKind::Transient, OrderState::Working)
        | (ExecutorErrorKind::Transient, OrderState::Queued) => {
            FailureMapping::RetryViaDieFailed { reason: format!("executor-transient: {error}") }
        }
        (ExecutorErrorKind::Permanent, OrderState::Working)
        | (ExecutorErrorKind::Permanent, OrderState::Queued)
        | (ExecutorErrorKind::Infrastructure, OrderState::Working)
        | (ExecutorErrorKind::Infrastructure, OrderState::Queued) => {
            FailureMapping::ExhaustedViaDieFailed
        }
        // 나머지 상태: 합법적 실행-실패 엣지 없음 — 상태 발명 금지 → 래치.
        _ => FailureMapping::Unmappable { node: node.into(), error: error.into() },
    }
}

impl WorkflowEngine {
    /// **실행기 실패 보고 진입점 (T55a)** — 실행기가 오류를 보고하면
    /// 매핑 → 실행. G14 3분기의 실행판:
    /// * RetryViaDieFailed / ExhaustedViaDieFailed: 해당 OrderEvent를
    ///   도메인에 디스패치 (도메인이 유일 권위 — 엔진은 새 상태를
    ///   만들지 않고 기존 전이를 쓴다). 결괏값은 도메인 판정.
    /// * Unmappable: 엔진 래치 (HaltCause::UnmappableFailure) — 주문 상태
    ///   불변, 재보고는 같은 사유 재생.
    pub fn report_executor_failure(
        &mut self,
        order_id: &str,
        exec_id: &str,
        kind: &ExecutorErrorKind,
        node: &str,
        error: &str,
    ) -> Result<Outcome, EngineError> {
        // 래치 우선 조회 (다른 halt가 이미 있으면 그 사유).
        if self.halted_of(exec_id).is_some() {
            return Err(self.halted_error(exec_id));
        }
        let state = self
            .order(order_id)
            .map(|o| o.state())
            .ok_or_else(|| EngineError::LockHeld { order_id: order_id.into(), by: "<no-order>".into() })?;
        let mapping = map_failure(kind, state, node, error);
        match mapping {
            FailureMapping::RetryViaDieFailed { reason } => {
                let ev = OrderEvent::DieFailed { attempt: self.order(order_id).unwrap().attempt(), reason };
                // 내부 전이 경로 — 실행기 실패는 인가 대상이 아니고 재생
                // 도 아니다 (T56 P1b: 재생은 커서 소유 경로만).
                self.dispatch_inner(order_id, exec_id, &ev, self.order(order_id).unwrap().attempt())
            }
            FailureMapping::ExhaustedViaDieFailed => {
                let att = self.order(order_id).unwrap().attempt();
                // MAX_ATTEMPTS 클레임 — 도메인 가드가 소진 판정 (엔진 아님).
                // attempt 클레임은 현재 attempt여야 stale 가드를 지난다 —
                // 소진 판정(attempt>=MAX)은 도메인 가드가 한다.
                let ev = OrderEvent::DieFailed {
                    attempt: att,
                    reason: format!("executor-exhausted: {error}"),
                };
                self.dispatch_inner(order_id, exec_id, &ev, att)
            }
            FailureMapping::Unmappable { node, error } => {
                let detail = format!("unmappable executor failure at node '{node}': {error} — no legal failure edge from {state:?}; order state UNCHANGED (no state invention)");
                let cause = crate::HaltCause::UnmappableFailure { node: node.clone(), error: error.clone() };
                self.halt_with(exec_id, cause, detail);
                Err(EngineError::UnmappableFailure { exec_id: exec_id.into(), node: node.into(), error: error.into() })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorkflowEngine;
    use foundry_domain::{ExternalRef, Order};

    fn order_in(state: OrderState, attempt: u32) -> Order {
        let mut o = Order::new("t55a-o".into(), ExternalRef::cron("a", "b"), Default::default());
        o.force_state_for_test(state, attempt);
        o
    }

    fn engine_with(state: OrderState, attempt: u32) -> WorkflowEngine {
        let mut e = WorkflowEngine::embedded_default();
        e.enroll(order_in(state, attempt));
        e.acquire("t55a-o", "ex").unwrap();
        e
    }

    // G14 분기 1 — 재시도 가능: Working에서 Transient → DieFailed back-edge
    // (attempt++ 후 Queued). 신규 상태 아님 — 기존 전이.
    #[test]
    fn g14_retryable_maps_to_existing_backedge() {
        let mut e = engine_with(OrderState::Working, 1);
        let out = e.report_executor_failure("t55a-o", "ex", &ExecutorErrorKind::Transient, "die", "timeout 30s").unwrap();
        assert_eq!(out, Outcome::Applied);
        assert_eq!(e.order("t55a-o").unwrap().state(), OrderState::Queued, "retry back-edge: Working -> Queued (existing transition)");
        assert_eq!(e.order("t55a-o").unwrap().attempt(), 2, "attempt incremented by the DOMAIN, not the executor");
    }

    // G14 분기 2 — 소진: attempt=MAX에서 Permanent → DieFailed → Failed
    // (기존 MaxAttempts 경로 — 판정은 도메인 가드).
    #[test]
    fn g14_exhausted_maps_to_existing_maxattempts_path() {
        let mut e = engine_with(OrderState::Working, foundry_domain::order::MAX_ATTEMPTS);
        let out = e.report_executor_failure("t55a-o", "ex", &ExecutorErrorKind::Permanent, "die", "logic error").unwrap();
        assert_eq!(out, Outcome::Applied);
        assert_eq!(e.order("t55a-o").unwrap().state(), OrderState::Failed, "exhaustion: existing MaxAttempts path to Failed");
    }

    // G14 분기 3 — 매핑 불능: Received에서 실행기 오류 → 엔진 래치,
    // 주문 상태 불변, 재보고 동일 사유. 신규 상태 부재 단정.
    #[test]
    fn g14_unmappable_latches_with_state_unchanged() {
        let mut e = engine_with(OrderState::Received, 0);
        let r = e.report_executor_failure("t55a-o", "ex", &ExecutorErrorKind::Infrastructure, "setup", "sandbox crashed");
        match &r {
            Err(EngineError::UnmappableFailure { node, error, .. }) => {
                assert_eq!(node, "setup");
                assert!(error.contains("sandbox"));
            }
            other => panic!("expected UnmappableFailure, got {other:?}"),
        }
        assert_eq!(e.order("t55a-o").unwrap().state(), OrderState::Received, "state UNCHANGED — no state invention");
        // 재보고 — 같은 사유 재생 (래치 규율).
        let r2 = e.report_executor_failure("t55a-o", "ex", &ExecutorErrorKind::Transient, "setup", "different error");
        assert_eq!(r, r2, "latched exec replays the identical cause");
        // 저널 관점: 주문 상태가 아니라 실행이 정지 — 상태 변이 없음.
        assert_eq!(e.order("t55a-o").unwrap().state(), OrderState::Received);
    }

    // 매핑 순수 함수 — 결정론성 (같은 입력 = 같은 매핑).
    #[test]
    fn mapping_is_deterministic() {
        let m1 = map_failure(&ExecutorErrorKind::Transient, OrderState::Working, "n", "e");
        let m2 = map_failure(&ExecutorErrorKind::Transient, OrderState::Working, "n", "e");
        assert_eq!(m1, m2);
        // Inspecting은 검사기 축 — 실행기 매핑은 Unmappable (역할 분리).
        assert!(matches!(
            map_failure(&ExecutorErrorKind::Transient, OrderState::Inspecting, "n", "e"),
            FailureMapping::Unmappable { .. }
        ));
        // 터미널 상태도 Unmappable.
        assert!(matches!(
            map_failure(&ExecutorErrorKind::Transient, OrderState::Shipped, "n", "e"),
            FailureMapping::Unmappable { .. }
        ));
    }

    // Queued에서 Transient — DieStarted가 선행해야 DieFailed가 수용된다
    // (도메인 순서). Working 문맥만 매핑이 즉시 적용된다.
    #[test]
    fn queued_transient_requires_die_context() {
        // Queued에서 DieFailed는 도메인이 거부한다 (DieFailed는 Working에서만)
        // — 매핑이 도메인 권위를 존중함을 증명: 거부는 도메인 것.
        let mut e = engine_with(OrderState::Queued, 1);
        let r = e.report_executor_failure("t55a-o", "ex", &ExecutorErrorKind::Transient, "n", "e");
        // 도메인 거부 — DomainRefusal (실행기가 상태를 억지로 바꾸지 않음).
        match r {
            // NoEdge: 부호화가 입장 불가 (E1 정상) — 도메인도 이 전이를
            // 갖지 않는다. 실행기가 상태를 억지로 바꾸지 않았다.
            Ok(Outcome::NoEdge) | Err(EngineError::DomainRefusal { .. }) | Ok(Outcome::Refused { .. }) => {}
            other => panic!("expected NoEdge/refusal for DieFailed-from-Queued, got {other:?}"),
        }
        assert_eq!(e.order("t55a-o").unwrap().state(), OrderState::Queued, "state unchanged on domain refusal");
    }
}
