//! T51a-R — 상태머신→내장 워크플로 동등성 증명 + 부호화 (M6, 재작판).
//!
//! astra 중간검증(옵션 A) BLOCKER 수용: "E1/E2 never verify destinations/
//! token effects; DieFailed guards omit stale-attempt conjunction;
//! inspection retry omits non-no-op condition. Incorrect encodings can
//! pass." 재작 내용:
//! * 가드는 **연접(conjunction)** 표현 — DieFailed는 stale∧(MAX|재시도),
//!   검사 재시도는 ¬no-op∧(MAX|재시도). 도메인 검사의 논리구조 그대로.
//! * E1은 **목적지+토큰 효과**까지 대조 — 도달 상태와 시도 변화가
//!   엣지 목적지와 일치해야 한다.
//! * E2는 실현가능성 — 각 엣지가 자기 가드 하에서 실제로 실현된다.
//! * E7 부정 프로브 — 가드를 위반하는 조합은 매칭 엣지 어느 것의
//!   가드도 만족하지 않으면 도메인이 거부한다.
//! * digest는 **schema_version 포함** (astra MAJOR: 버전 정체성).
//!
//! 설계 원칙 (불변): 부호화는 번역이 아니라 결속 — 모든 엣지는
//! `Order::transition`의 해당 매치 암에 결속. 정지(hold)는 전이가
//! 아니다 — FurnaceHold/Unknown은 외부 재개를 동반하는 정지.
//! "bounded cycles with explicit re-entry tokens".

use crate::order::{OrderEvent, OrderState};

/// 가드: 전이가 합법이기 위한 조건의 논리식. 도메인 검사와 1:1.
#[derive(Debug, Clone, PartialEq)]
pub enum Guard {
    /// DieStarted의 event_attempt == attempt.max(1)
    AttemptClaimMatches,
    /// Die 계열 payload attempt == 현재 attempt (stale fence)
    AttemptNotStale,
    /// attempt >= MAX_ATTEMPTS
    MaxAttemptsExhausted,
    /// attempt < MAX_ATTEMPTS
    RetriesRemain,
    /// InspectionFailed.reason이 "no-op"으로 시작 (T19: 종단)
    NoOpTaxonomy,
    /// InspectionFailed.reason이 "no-op"으로 시작하지 **않는다**
    NoNoOp,
    /// Approved.artifact_digest 비어있지 않음 (§11 출처 결속)
    ApprovalBindsArtifact,
    /// 무가드 — 무조건 합법
    None,
    /// 연접 — 전부 참이어야 한다 (도메인 검사 순서의 논리구조).
    Conj(Vec<Guard>),
}

/// 가드 평가기 — 대표 조건 구성(E2)과 부정 프로브(E7) 전용.
/// 최종 권위는 언제나 `Order::transition`이다.
pub fn guard_satisfied(g: &Guard, ev: &OrderEvent, current_attempt: u32) -> bool {
    match g {
        Guard::AttemptClaimMatches => matches!(ev, OrderEvent::DieStarted),
        Guard::AttemptNotStale => match ev {
            OrderEvent::DieSucceeded { attempt } | OrderEvent::DieFailed { attempt, .. } => {
                *attempt == current_attempt
            }
            _ => true,
        },
        Guard::MaxAttemptsExhausted => current_attempt >= crate::order::MAX_ATTEMPTS,
        Guard::RetriesRemain => current_attempt < crate::order::MAX_ATTEMPTS,
        Guard::NoOpTaxonomy => {
            matches!(ev, OrderEvent::InspectionFailed { reason } if reason.starts_with("no-op"))
        }
        Guard::NoNoOp => match ev {
            OrderEvent::InspectionFailed { reason } => !reason.starts_with("no-op"),
            _ => true,
        },
        Guard::ApprovalBindsArtifact => match ev {
            OrderEvent::Approved {
                artifact_digest, ..
            } => !artifact_digest.is_empty(),
            _ => true,
        },
        Guard::None => true,
        Guard::Conj(gs) => gs.iter().all(|g| guard_satisfied(g, ev, current_attempt)),
    }
}

/// 정지(hold): 전이가 아니라 외부 재개를 동반하는 정지 상태.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Hold {
    /// 인간 승인 대기 (Approved/Rejected로 재개)
    FurnaceHold,
    /// 발행 결과 불명 — 조정(reconcile)이 재판정
    UnknownOutcome,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EncodedEdge {
    pub event: EdgeEvent,
    pub from: OrderState,
    pub guard: Guard,
    pub to: EdgeTarget,
}

/// 엣지 이벤트 패밀리(페이로드 무시 — 가드가 검사).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EdgeEvent {
    Queued,
    DieStarted,
    DieSucceeded,
    DieFailed,
    InspectionPassed,
    ReviewHeld,
    InspectionFailed,
    Approved,
    Rejected,
    PublishConfirmed,
    PublishUnknown,
    CancelRequested,
    Canceled,
    CancelConfirmed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EdgeTarget {
    State(OrderState),
    /// back-edge: 재진입 토큰(attempt+1)을 소비하고 to로 복귀.
    BackEdge {
        token: &'static str,
        to: OrderState,
    },
}

/// 내장 워크플로 — M1-M5 주문 상태머신의 전체·구조 보존 부호화.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddedWorkflow {
    pub schema_version: u32,
    pub edges: Vec<EncodedEdge>,
    pub holds: Vec<Hold>,
}

impl EmbeddedWorkflow {
    /// 내장 부호화 (동결). `Order::transition`의 매치 암 전수 열거 —
    /// 가드는 도메인 검사의 논리구조(연접 포함)를 그대로 담는다.
    pub fn frozen_v1() -> Self {
        use EdgeEvent as E;
        use EdgeTarget as T;
        use Guard::*;
        use OrderState as S;
        EmbeddedWorkflow {
            schema_version: 1,
            edges: vec![
                // ---- happy path ----
                EncodedEdge {
                    event: E::Queued,
                    from: S::Received,
                    guard: None,
                    to: T::State(S::Queued),
                },
                EncodedEdge {
                    event: E::DieStarted,
                    from: S::Queued,
                    guard: AttemptClaimMatches,
                    to: T::State(S::Working),
                },
                EncodedEdge {
                    event: E::DieSucceeded,
                    from: S::Working,
                    guard: AttemptNotStale,
                    to: T::State(S::Inspecting),
                },
                // DieFailed: stale 펜스 ∧ (MAX | 재시도) — 연접 (astra BLOCKER)
                EncodedEdge {
                    event: E::DieFailed,
                    from: S::Working,
                    guard: Conj(vec![AttemptNotStale, MaxAttemptsExhausted]),
                    to: T::State(S::Failed),
                },
                EncodedEdge {
                    event: E::DieFailed,
                    from: S::Working,
                    guard: Conj(vec![AttemptNotStale, RetriesRemain]),
                    to: T::BackEdge {
                        token: "attempt+1",
                        to: S::Queued,
                    },
                },
                EncodedEdge {
                    event: E::InspectionPassed,
                    from: S::Inspecting,
                    guard: None,
                    to: T::State(S::FurnaceHold),
                },
                EncodedEdge {
                    event: E::InspectionPassed,
                    from: S::FurnaceHold,
                    guard: None,
                    to: T::State(S::FurnaceHold),
                },
                EncodedEdge {
                    event: E::ReviewHeld,
                    from: S::Inspecting,
                    guard: None,
                    to: T::State(S::FurnaceHold),
                },
                // InspectionFailed: no-op 즉시 종단; ¬no-op ∧ (MAX | 재시도)
                EncodedEdge {
                    event: E::InspectionFailed,
                    from: S::Inspecting,
                    guard: NoOpTaxonomy,
                    to: T::State(S::Failed),
                },
                EncodedEdge {
                    event: E::InspectionFailed,
                    from: S::Inspecting,
                    guard: Conj(vec![NoNoOp, MaxAttemptsExhausted]),
                    to: T::State(S::Failed),
                },
                EncodedEdge {
                    event: E::InspectionFailed,
                    from: S::Inspecting,
                    guard: Conj(vec![NoNoOp, RetriesRemain]),
                    to: T::BackEdge {
                        token: "attempt+1",
                        to: S::Queued,
                    },
                },
                EncodedEdge {
                    event: E::InspectionFailed,
                    from: S::FurnaceHold,
                    guard: NoOpTaxonomy,
                    to: T::State(S::Failed),
                },
                EncodedEdge {
                    event: E::InspectionFailed,
                    from: S::FurnaceHold,
                    guard: Conj(vec![NoNoOp, MaxAttemptsExhausted]),
                    to: T::State(S::Failed),
                },
                EncodedEdge {
                    event: E::InspectionFailed,
                    from: S::FurnaceHold,
                    guard: Conj(vec![NoNoOp, RetriesRemain]),
                    to: T::BackEdge {
                        token: "attempt+1",
                        to: S::Queued,
                    },
                },
                // hold: FurnaceHold (정지 — 외부 재개)
                EncodedEdge {
                    event: E::Approved,
                    from: S::FurnaceHold,
                    guard: ApprovalBindsArtifact,
                    to: T::State(S::Publishing),
                },
                EncodedEdge {
                    event: E::Rejected,
                    from: S::FurnaceHold,
                    guard: None,
                    to: T::State(S::Rejected),
                },
                // approval → Shipped 직행 금지 (v3 비평 #1): 엣지 부재가 증명
                EncodedEdge {
                    event: E::PublishConfirmed,
                    from: S::Publishing,
                    guard: None,
                    to: T::State(S::Shipped),
                },
                EncodedEdge {
                    event: E::PublishUnknown,
                    from: S::Publishing,
                    guard: None,
                    to: T::State(S::Unknown),
                },
                // hold: UnknownOutcome (정지 — 조정이 재판정)
                EncodedEdge {
                    event: E::PublishConfirmed,
                    from: S::Unknown,
                    guard: None,
                    to: T::State(S::Shipped),
                },
                EncodedEdge {
                    event: E::Rejected,
                    from: S::Unknown,
                    guard: None,
                    to: T::State(S::Rejected),
                },
                // ---- cancellation ----
                EncodedEdge {
                    event: E::Canceled,
                    from: S::Received,
                    guard: None,
                    to: T::State(S::Canceled),
                },
                EncodedEdge {
                    event: E::Canceled,
                    from: S::Queued,
                    guard: None,
                    to: T::State(S::Canceled),
                },
                EncodedEdge {
                    event: E::CancelRequested,
                    from: S::Working,
                    guard: None,
                    to: T::State(S::Canceling),
                },
                EncodedEdge {
                    event: E::CancelRequested,
                    from: S::Publishing,
                    guard: None,
                    to: T::State(S::Canceling),
                },
                EncodedEdge {
                    event: E::CancelRequested,
                    from: S::Unknown,
                    guard: None,
                    to: T::State(S::Canceling),
                },
                EncodedEdge {
                    event: E::CancelConfirmed,
                    from: S::Canceling,
                    guard: None,
                    to: T::State(S::Canceled),
                },
            ],
            holds: vec![Hold::FurnaceHold, Hold::UnknownOutcome],
        }
    }

    /// 부호화 콘텐츠 digest — **schema_version 포함** (astra MAJOR:
    /// 버전 정체성 누락 시 v1/v2 동일 내용이 같은 digest 공유).
    pub fn digest(&self) -> String {
        use sha2::Digest as _;
        let mut h = sha2::Sha256::new();
        h.update(b"foundry-embedded-workflow-v1");
        h.update((self.schema_version as u64).to_be_bytes());
        for e in &self.edges {
            for part in [
                format!("{:?}", e.event),
                format!("{:?}", e.from),
                format!("{:?}", e.guard),
                match &e.to {
                    EdgeTarget::State(s) => format!("S:{s:?}"),
                    EdgeTarget::BackEdge { token, to } => format!("B:{token}>{to:?}"),
                },
            ] {
                h.update((part.len() as u64).to_be_bytes());
                h.update(part.as_bytes());
            }
        }
        for hd in &self.holds {
            h.update(format!("{hd:?}").as_bytes());
        }
        format!("wf-emb-{:.16x}", h.finalize())
    }

    /// (이벤트 패밀리, 원천) 매칭 엣지 전부 — 증명·엔진 공용.
    pub fn edges_matching(&self, ev: &EdgeEvent, from: OrderState) -> Vec<&EncodedEdge> {
        self.edges
            .iter()
            .filter(|e| &e.event == ev && e.from == from)
            .collect()
    }
}

/// OrderEvent → 엣지 이벤트 패밀리 (페이로드는 가드가 소비).
pub fn edge_event_of(ev: &OrderEvent) -> EdgeEvent {
    match ev {
        OrderEvent::Queued => EdgeEvent::Queued,
        OrderEvent::DieStarted => EdgeEvent::DieStarted,
        OrderEvent::DieSucceeded { .. } => EdgeEvent::DieSucceeded,
        OrderEvent::DieFailed { .. } => EdgeEvent::DieFailed,
        OrderEvent::InspectionPassed => EdgeEvent::InspectionPassed,
        OrderEvent::ReviewHeld { .. } => EdgeEvent::ReviewHeld,
        OrderEvent::InspectionFailed { .. } => EdgeEvent::InspectionFailed,
        OrderEvent::Approved { .. } => EdgeEvent::Approved,
        OrderEvent::Rejected { .. } => EdgeEvent::Rejected,
        OrderEvent::PublishConfirmed { .. } => EdgeEvent::PublishConfirmed,
        OrderEvent::PublishUnknown { .. } => EdgeEvent::PublishUnknown,
        OrderEvent::CancelRequested { .. } => EdgeEvent::CancelRequested,
        OrderEvent::Canceled { .. } => EdgeEvent::Canceled,
        OrderEvent::CancelConfirmed => EdgeEvent::CancelConfirmed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::order::{ExternalRef, MAX_ATTEMPTS, Order};

    fn mk(s: OrderState, attempt: u32) -> Order {
        let mut o = Order::new("x".into(), ExternalRef::cron("a", "b"), Default::default());
        o.force_state_for_test(s, attempt);
        o
    }

    fn states() -> Vec<OrderState> {
        vec![
            OrderState::Received,
            OrderState::Queued,
            OrderState::Working,
            OrderState::Inspecting,
            OrderState::FurnaceHold,
            OrderState::Publishing,
            OrderState::Unknown,
            OrderState::Shipped,
            OrderState::Failed,
            OrderState::Rejected,
            OrderState::Canceling,
            OrderState::Canceled,
        ]
    }

    /// 탐색용 페이로드 풀 — payload attempt는 base로 정규화해 제공.
    fn payloads(base: u32) -> Vec<(OrderEvent, u32)> {
        vec![
            (OrderEvent::Queued, 1),
            (OrderEvent::DieStarted, base.max(1)),
            (OrderEvent::DieSucceeded { attempt: base }, 1),
            (OrderEvent::DieSucceeded { attempt: 0 }, 1), // stale 프로브
            (
                OrderEvent::DieFailed {
                    attempt: base,
                    reason: "transient".into(),
                },
                1,
            ),
            (
                OrderEvent::DieFailed {
                    attempt: base.saturating_sub(1),
                    reason: "transient".into(),
                },
                1,
            ), // stale 프로브
            (OrderEvent::InspectionPassed, 1),
            (
                OrderEvent::ReviewHeld {
                    reason: "review held".into(),
                },
                1,
            ),
            (
                OrderEvent::InspectionFailed {
                    reason: "lint".into(),
                },
                1,
            ),
            (
                OrderEvent::InspectionFailed {
                    reason: "no-op: empty".into(),
                },
                1,
            ),
            (
                OrderEvent::Approved {
                    intent: "i".into(),
                    artifact_digest: "d".into(),
                },
                1,
            ),
            (
                OrderEvent::Approved {
                    intent: "i".into(),
                    artifact_digest: String::new(),
                },
                1,
            ),
            (OrderEvent::Rejected { intent: "i".into() }, 1),
            (OrderEvent::PublishConfirmed { pr_url: "u".into() }, 1),
            (
                OrderEvent::PublishUnknown {
                    operation_id: "op".into(),
                },
                1,
            ),
            (OrderEvent::CancelRequested { reason: "r".into() }, 1),
            (OrderEvent::Canceled { reason: "r".into() }, 1),
            (OrderEvent::CancelConfirmed, 1),
        ]
    }

    /// **E1 (재작) 완전성+목적지+토큰**: 도메인이 수용하는 모든 전이에
    /// 대해 매칭 엣지 중 목적지(도달 상태+시도 효과)까지 일치하는
    /// 것이 존재한다. 존재만 보는 구식 E1과 달리 목적지 불일치=실패.
    #[test]
    fn e1_every_legal_transition_matches_destination_and_token() {
        let wf = EmbeddedWorkflow::frozen_v1();
        let mut legal = 0;
        for base in [1u32, MAX_ATTEMPTS] {
            for s in states() {
                for (ev, claim) in payloads(base) {
                    let mut o = mk(s, base);
                    let before = o.attempt();
                    if let Ok(reached) = o.transition(ev.clone(), claim) {
                        legal += 1;
                        let fam = edge_event_of(&ev);
                        let matching = wf.edges_matching(&fam, s);
                        assert!(
                            !matching.is_empty(),
                            "legal {s:?} --{fam:?}--> {reached:?} has NO edge (E1 existence)"
                        );
                        // astra BLOCKER (r2): destination match requires the
                        // edge's OWN guard to hold for this probe — not just
                        // any edge with the right destination.
                        let dest_ok = matching.iter().any(|e| {
                            guard_satisfied(&e.guard, &ev, base)
                                && match &e.to {
                                    EdgeTarget::State(t) => {
                                        *t == reached
                                            && (o.attempt() == before
                                                || (e.event == EdgeEvent::DieStarted
                                                    && o.attempt() == before.max(1)))
                                    }
                                    EdgeTarget::BackEdge { to, .. } => {
                                        *to == reached && o.attempt() == before + 1
                                    }
                                }
                        });
                        assert!(
                            dest_ok,
                            "legal {s:?} --{fam:?}--> reached {reached:?} (attempt {before}->{}) but NO edge matches DESTINATION (E1 destination)",
                            o.attempt()
                        );
                    }
                }
            }
        }
        assert!(legal >= 22, "sanity: {legal} legal transitions");
    }

    /// **E2 (재작) 실현가능성**: 각 엣지에 대해 자기 가드를 만족하는
    /// (페이로드, 베이스)가 존재하고, 그 조건에서 전이가 성공하며
    /// 목적지(상태/백엣지+토큰)가 실현된다.
    #[test]
    fn e2_every_edge_is_realizable_with_destination() {
        let wf = EmbeddedWorkflow::frozen_v1();
        for e in &wf.edges {
            assert!(
                edge_realizable(e, &wf),
                "edge {e:?} NOT realizable under its own guard (E2 destination/token)"
            );
        }
    }

    /// **E3 (강화) 연접 가드 하중**: 각 성분이 실제로 거부를 만든다.
    #[test]
    fn e3_conjunction_guards_are_load_bearing() {
        // stale DieFailed: 재시도 여지가 있어도(attempt 1 < MAX) 거부
        let mut o = mk(OrderState::Working, 1);
        assert!(
            o.transition(
                OrderEvent::DieFailed {
                    attempt: 0,
                    reason: "x".into()
                },
                1
            )
            .is_err()
        );
        // stale DieFailed: MAX여도(attempt 3, payload 2) 거부 — stale 성분
        let mut o = mk(OrderState::Working, MAX_ATTEMPTS);
        assert!(
            o.transition(
                OrderEvent::DieFailed {
                    attempt: MAX_ATTEMPTS - 1,
                    reason: "x".into()
                },
                1
            )
            .is_err()
        );
        // no-op: 재시도 여지가 있어도(attempt 1) 즉시 종단
        let mut o = mk(OrderState::Inspecting, 1);
        assert_eq!(
            o.transition(
                OrderEvent::InspectionFailed {
                    reason: "no-op: empty".into()
                },
                1
            )
            .unwrap(),
            OrderState::Failed
        );
        // 비 no-op MAX: 종단 (¬no-op ∧ MAX)
        let mut o = mk(OrderState::Inspecting, MAX_ATTEMPTS);
        assert_eq!(
            o.transition(
                OrderEvent::InspectionFailed {
                    reason: "lint".into()
                },
                1
            )
            .unwrap(),
            OrderState::Failed
        );
        // 비 no-op 재시도: 백엣지+토큰 (¬no-op ∧ 재시도)
        let mut o = mk(OrderState::Inspecting, 1);
        assert_eq!(
            o.transition(
                OrderEvent::InspectionFailed {
                    reason: "lint".into()
                },
                1
            )
            .unwrap(),
            OrderState::Queued
        );
        assert_eq!(o.attempt(), 2, "re-entry token: attempt+1");
        // ApprovalBindsArtifact: 빈 digest 거부
        let mut o = mk(OrderState::FurnaceHold, 1);
        assert!(
            o.transition(
                OrderEvent::Approved {
                    intent: "i".into(),
                    artifact_digest: String::new()
                },
                1
            )
            .is_err()
        );
    }

    /// **E4 금지 전이 부재** (v3 비평 #1).
    #[test]
    fn e4_forbidden_edges_absent() {
        let wf = EmbeddedWorkflow::frozen_v1();
        assert!(!wf.edges.iter().any(|e| e.event == EdgeEvent::Approved
            && matches!(e.to, EdgeTarget::State(OrderState::Shipped))));
        assert!(
            !wf.edges
                .iter()
                .any(|e| e.event == EdgeEvent::Approved && e.from == OrderState::Publishing)
        );
    }

    /// **E5 결정성**: 같은 스트림 → 같은 전이 흔적.
    #[test]
    fn e5_deterministic_replay_shape() {
        let run = |n: u64| {
            let mut o = Order::new(
                format!("o{n}"),
                ExternalRef::cron("a", "b"),
                Default::default(),
            );
            let evs = [
                OrderEvent::Queued,
                OrderEvent::DieStarted,
                OrderEvent::DieSucceeded { attempt: 1 },
                OrderEvent::InspectionPassed,
                OrderEvent::Approved {
                    intent: "i".into(),
                    artifact_digest: "d".into(),
                },
                OrderEvent::PublishConfirmed { pr_url: "u".into() },
            ];
            evs.iter()
                .map(|ev| {
                    format!(
                        "{:?}",
                        o.transition(ev.clone(), 1)
                            .map(|s| format!("{s:?}"))
                            .map_err(|e| e.to_string())
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(run(1), run(2));
    }

    /// **E6 (신규, astra MAJOR) digest는 schema_version을 덮는다**.
    #[test]
    fn e6_digest_covers_schema_version() {
        let v1 = EmbeddedWorkflow::frozen_v1();
        let mut v2 = EmbeddedWorkflow::frozen_v1();
        v2.schema_version = 2;
        assert_ne!(v1.digest(), v2.digest(), "version identity must be covered");
        assert_eq!(
            v1.digest(),
            EmbeddedWorkflow::frozen_v1().digest(),
            "idempotent"
        );
        let mut tampered = EmbeddedWorkflow::frozen_v1();
        tampered.edges[3].guard = Guard::None;
        assert_ne!(
            v1.digest(),
            tampered.digest(),
            "content change still covered"
        );
    }

    /// **E8 (r3 재작)**: 위조 엣지 주입 — **E2와 동일한 검사 루프**를
    /// 함수로 추출해 그것을 통과시킨다 (astra r3: "E8's separate
    /// hardcoded loop does not regression-test E2; removing E2's family
    /// filter leaves E8 green"). 위조 (Queued, Working) 엣지는 이
    /// 루프에서 반드시 unrealizable로 판정된다.
    #[test]
    fn e8_spurious_edge_is_rejected_by_the_proof() {
        let mut wf = EmbeddedWorkflow::frozen_v1();
        wf.edges.push(EncodedEdge {
            event: EdgeEvent::Queued,
            from: OrderState::Working,
            guard: Guard::None,
            to: EdgeTarget::State(OrderState::Inspecting),
        });
        // E2와 동일 논리 — edge_realizable()은 E2 본테스트가 쓰는 함수.
        // 방향: 진짜 엣지 전부 realizable + 위조 엣지는 NOT realizable.
        let spurious = wf.edges.pop().expect("injected spurious edge on top");
        for e in &wf.edges {
            assert!(
                edge_realizable(e, &wf),
                "genuine edge must be realizable: {e:?}"
            );
        }
        assert!(
            !edge_realizable(&spurious, &wf),
            "spurious edge must be caught by the SAME check E2 runs: {spurious:?}"
        );

        // astra r4 MINOR: 같은 패밀리의 엣지가 선언 가드 밖에서만 실현
        // 가능한 경우 — 가드 필터 제거 시 E8이 빨개지는지 검증.
        // DieSucceeded/Working에 모순 가드(MaxAttemptsExhausted∧RetriesRemain
        // — Die 계열에서 항상 거짓은 아니지만 이 이벤트엔 무의미)를
        // 단 엣지: 가드 평가에서 AttemptNotStale 아닌 조건이 걸리므로
        // 가드를 존중하는 checker는 실현 프로브를 거부한다.
        let guard_evading = EncodedEdge {
            event: EdgeEvent::DieSucceeded,
            from: OrderState::Working,
            guard: Guard::Conj(vec![Guard::MaxAttemptsExhausted, Guard::RetriesRemain]), // 모순
            to: EdgeTarget::State(OrderState::Inspecting),
        };
        assert!(
            !edge_realizable(&guard_evading, &wf),
            "same-family edge with a contradictory guard must NOT be realizable — the guard filter is load-bearing in the shared checker"
        );
        // 그리고 이 모순 가드 없이는 실현 가능함을 확인 (가드가 거부의 원인)
        let legitimate = EncodedEdge {
            guard: Guard::AttemptNotStale,
            ..guard_evading.clone()
        };
        assert!(
            edge_realizable(&legitimate, &wf),
            "the same edge with its true guard IS realizable — the family/destination logic is sound"
        );
    }

    /// E2의 실현가능성 검사 — E2 본테스트와 E8 회귀가 공유한다.
    fn edge_realizable(e: &EncodedEdge, _wf: &EmbeddedWorkflow) -> bool {
        for base in [1u32, MAX_ATTEMPTS] {
            for (ev, claim) in payloads(base) {
                if edge_event_of(&ev) != e.event {
                    continue;
                }
                if !guard_satisfied(&e.guard, &ev, base) {
                    continue;
                }
                let mut o = mk(e.from, base);
                let before = o.attempt();
                if let Ok(reached) = o.transition(ev.clone(), claim) {
                    let ok = match &e.to {
                        EdgeTarget::State(t) => {
                            *t == reached
                                && (o.attempt() == before
                                    || (e.event == EdgeEvent::DieStarted
                                        && o.attempt() == before.max(1)))
                        }
                        EdgeTarget::BackEdge { to, .. } => {
                            *to == reached && o.attempt() == before + 1
                        }
                    };
                    if ok {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// **E7 (신규) 부정 프로브**: 매칭 엣지들의 가드를 모두 위반하는
    /// (이벤트, 상태) 조합은 도메인이 거부한다 — 부호화·도메인의
    /// 거부 집합이 어긋나지 않는다는 증명.
    #[test]
    fn e7_guard_violations_are_refused_by_domain() {
        let wf = EmbeddedWorkflow::frozen_v1();
        let mut probes = 0;
        for base in [1u32, MAX_ATTEMPTS] {
            for s in states() {
                for (ev, claim) in payloads(base) {
                    let fam = edge_event_of(&ev);
                    let matching = wf.edges_matching(&fam, s);
                    if matching.is_empty() {
                        continue; // E1 영역 밖 (엣지 없음 = 도메인도 거부해야 — E1이 이미 검증)
                    }
                    let any_guard_ok = matching
                        .iter()
                        .any(|e| guard_satisfied(&e.guard, &ev, base));
                    if !any_guard_ok {
                        probes += 1;
                        let mut o = mk(s, base);
                        assert!(
                            o.transition(ev.clone(), claim).is_err(),
                            "{s:?} --{fam:?}--> violates EVERY matching guard yet the domain ACCEPTS (E7 leak at base {base})"
                        );
                    }
                }
            }
        }
        assert!(probes > 0, "sanity: negative probes exercised ({probes})");
    }
}
