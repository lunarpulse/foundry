//! T54 — 작성 시 검증기 (M6 플랜 v6).
//!
//! 계약 (플랜 v6 C-W4a·F·태스크 T54):
//! * **v3 스키마**: WorkflowDoc schema_version=3은 PolicyDoc을 **내장**한다
//!   (digest 참조 아님 — 내용이 곧 커밋먼트). v1/v2는 policy_digest 리프를
//!   유지하며 동결 인터프리터로 재생된다 (레거시 root 불변).
//! * **deny_unknown_fields 전체 중첩**: v3 문서의 모든 중첩 타입에 엄격
//!   파싱 — 알 수 없는 필드가 조용히 버려지는 구멍 없음 (astra r2).
//! * **오류/경고 2종 진단**: 오류(로드 거부)와 경고(로드는 되고 저널에
//!   기록)만 존재. 검증기 통과는 런타임 권위를 일절 부여하지 않는다.
//! * **Waive = 금지 선언**: 승인-면제 선언은 표현 가능하되 검증기가
//!   작성 시 **오류로 거부**한다 (G3의 공격 재료 — 존재 자체가 신호).
//! * **Prepare = 전분기 승인게이트 규칙**: Prepare 노드 이후 도달 가능한
//!   모든 상태의 만족 가능한 출발 엣지가 전부 Approved 패밀리여야 한다
//!   (혼합 분기 = 오류). 정적 도달성 분석 — 방문 집합으로 종결.
//! * **StepId 계약**: v3 문서의 steps는 StepId(비어있지 않음·문서 내
//!   유일) 필수.

use crate::dsl::{InterpretError, PolicyLevel, WorkflowDoc, engine_registry, merkle_root};
use foundry_domain::OrderState;
use foundry_domain::workflow_encoding::{
    EdgeEvent, EdgeTarget, EmbeddedWorkflow, Guard, guard_satisfied,
};
use serde::{Deserialize, Serialize};

/// 검증 진단 — 오류/경고 2종만 (플랜 v6 C-W4a 수용 규칙).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Diagnostic {
    /// 로드 거부 — Waive 선언·미해결 node_ref·중복 StepId·불명 필드 등.
    Error { code: String, detail: String },
    /// 로드는 되고 저널에 기록 — advisory성 (역량 초과 선언 등).
    Warning { code: String, detail: String },
}

impl Diagnostic {
    fn err(code: &str, detail: impl Into<String>) -> Self {
        Diagnostic::Error {
            code: code.into(),
            detail: detail.into(),
        }
    }
    fn warn(code: &str, detail: impl Into<String>) -> Self {
        Diagnostic::Warning {
            code: code.into(),
            detail: detail.into(),
        }
    }
}

/// 검증 결과 — 오류 없으면 Valid (경고는 함께 반환).
#[derive(Debug, Clone, PartialEq)]
pub struct ValidationOutcome {
    pub doc_digest: String,
    pub warnings: Vec<Diagnostic>,
}

/// PolicyDoc 스키마 버전 — 동결 (v1만 지원, G12와 동일 규율).
pub const POLICY_SCHEMA_VERSIONS: &[u32] = &[1];

/// **검증기** — 작성 시 1회 전수 검사. 성공은 런타임 권위 부여가 아니다:
/// 런타임 인가는 도메인 승인 원장(T53a)이 소유한다.
pub fn validate(
    doc: &WorkflowDoc,
    encoding: &EmbeddedWorkflow,
) -> Result<ValidationOutcome, Vec<Diagnostic>> {
    let mut errors: Vec<Diagnostic> = Vec::new();
    let mut warnings: Vec<Diagnostic> = Vec::new();

    // (1) 스키마 버전 — v3만 검증기 대상 (v1/v2는 동결 인터프리터).
    if doc.schema_version != 3 {
        return Err(vec![Diagnostic::err(
            "schema_version",
            format!(
                "validator targets schema v3; got v{} (v1/v2 go through frozen interpreters)",
                doc.schema_version
            ),
        )]);
    }

    // (2) 인터프리터 해석 가능성 (v3 규칙 — engine_registry v3 분기).
    // 구조·부호화 결속·스킬 참조·advisory 차단·default_route 등.
    if let Err(e) = engine_registry::interpret(doc, encoding) {
        errors.push(interpret_error_to_diag(e));
    }

    // (2b) 공유 검증 계약 (플랜 v6 T54-T53 seam): 라우팅 패턴 문법은
    // router::validate_routing이 유일 소유 — 검증기는 그것을 호출한다
    // (재구현 금지). 대상 id 존재 검사는 라우터 적재 시점(외부 등록부)에
    // 완결 — 여기선 문법만.
    for r in &doc.routing.rules {
        if let Err(reason) = crate::router::check_pattern_public(&r.source_pattern) {
            errors.push(Diagnostic::err(
                "bad_route_pattern",
                format!("routing pattern '{}' invalid: {}", r.source_pattern, reason),
            ));
        }
    }

    // (3) StepId 계약 — 비어있지 않음·문서 내 유일.
    let mut seen_ids: Vec<&str> = Vec::new();
    for (i, st) in doc.body.steps.iter().enumerate() {
        if st.id.trim().is_empty() {
            errors.push(Diagnostic::err(
                "step_id_empty",
                format!("step[{i}] has an empty StepId"),
            ));
        }
        if seen_ids.contains(&st.id.as_str()) {
            errors.push(Diagnostic::err(
                "step_id_duplicate",
                format!("StepId '{}' is used more than once", st.id),
            ));
        }
        seen_ids.push(st.id.as_str());
        // 이벤트명 타당성 — 내장 부호화에 존재하는 패밀리인가.
        if event_family_from_name(&st.event).is_none() {
            errors.push(Diagnostic::err(
                "unknown_event",
                format!(
                    "step[{}] ('{}') names event '{}' which does not exist in the encoding",
                    i, st.id, st.event
                ),
            ));
        }
    }

    // (4) 스킬 중복 id / 종류·인터페이스 일관성.
    let mut skill_ids: Vec<&str> = Vec::new();
    for s in &doc.skills {
        if skill_ids.contains(&s.id.as_str()) {
            errors.push(Diagnostic::err(
                "skill_id_duplicate",
                format!("skill id '{}' declared twice", s.id),
            ));
        }
        skill_ids.push(s.id.as_str());
    }

    // (5) PolicyDoc 검증 — Waive 금지 + node_ref 해석 + Prepare 배치 규칙.
    validate_policy(doc, encoding, &mut errors, &mut warnings);

    if errors.is_empty() {
        Ok(ValidationOutcome {
            doc_digest: merkle_root(doc),
            warnings,
        })
    } else {
        Err(errors)
    }
}

fn interpret_error_to_diag(e: InterpretError) -> Diagnostic {
    let (code, detail) = match &e {
        InterpretError::UnknownSchemaVersion { got, known } => (
            "schema_version",
            format!("unknown schema version {got} (known: {known:?})"),
        ),
        InterpretError::EncodingMismatch { doc, loaded } => (
            "encoding_mismatch",
            format!("body names encoding {doc} but loaded encoding is {loaded}"),
        ),
        InterpretError::UnknownSkillRef { step_event, skill } => (
            "unknown_skill_ref",
            format!("step '{step_event}' references skill '{skill}' not in skills[]"),
        ),
        InterpretError::AdvisoryInRoleSlot { skill } => (
            "advisory_in_role_slot",
            format!("advisory skill '{skill}' wired to a role slot"),
        ),
        InterpretError::DefaultRouteMismatch { expected, got } => (
            "default_route_mismatch",
            format!("default_route '{got}' != workflow id '{expected}'"),
        ),
        InterpretError::MissingReviewPack => (
            "missing_review_pack",
            "v4 document must bind a review pack digest".to_string(),
        ),
        InterpretError::PackTampered { bound, live } => (
            "pack_tampered",
            format!("bound pack digest {bound} != live digest {live} — pack changed since binding"),
        ),
    };
    Diagnostic::err(code, detail)
}

fn event_family_from_name(name: &str) -> Option<EdgeEvent> {
    // 부호화에 실재하는 이벤트 패밀리만 인정 (StepSpec.event는 패밀리명) —
    // EdgeEvent 13종에 정합.
    Some(match name {
        "Queued" => EdgeEvent::Queued,
        "DieStarted" => EdgeEvent::DieStarted,
        "DieSucceeded" => EdgeEvent::DieSucceeded,
        "DieFailed" => EdgeEvent::DieFailed,
        "InspectionPassed" => EdgeEvent::InspectionPassed,
        "ReviewHeld" => EdgeEvent::ReviewHeld,
        "InspectionFailed" => EdgeEvent::InspectionFailed,
        "Approved" => EdgeEvent::Approved,
        "Rejected" => EdgeEvent::Rejected,
        "PublishConfirmed" => EdgeEvent::PublishConfirmed,
        "PublishUnknown" => EdgeEvent::PublishUnknown,
        "CancelRequested" => EdgeEvent::CancelRequested,
        "Canceled" => EdgeEvent::Canceled,
        "CancelConfirmed" => EdgeEvent::CancelConfirmed,
        _ => return None,
    })
}

/// 정책 검증 — C-W4a·F (플랜 v6).
fn validate_policy(
    doc: &WorkflowDoc,
    encoding: &EmbeddedWorkflow,
    errors: &mut Vec<Diagnostic>,
    warnings: &mut Vec<Diagnostic>,
) {
    // v3 문서는 내장 정책 필수 — policy 없음은 구조 오류 (T54 계약).
    let policy = match &doc.policy {
        None => {
            errors.push(Diagnostic::err(
                "policy_not_embedded",
                "schema v3 requires an embedded PolicyDoc (policy_digest refs are legacy v1/v2)",
            ));
            return;
        }
        Some(p) => {
            // astra T54 MAJOR: 지원 정책 스키마만 — 99/0은 거부.
            if !POLICY_SCHEMA_VERSIONS.contains(&p.schema_version) {
                errors.push(Diagnostic::err(
                    "policy_schema_version",
                    format!(
                        "PolicyDoc schema_version {} not supported (known: {:?})",
                        p.schema_version, POLICY_SCHEMA_VERSIONS
                    ),
                ));
                return;
            }
            p
        }
    };

    // Waive = 금지 선언 — 존재 자체가 오류 (G3).
    for rule in &policy.approvals {
        if rule.level == PolicyLevel::Waive {
            errors.push(Diagnostic::err(
                "waive_forbidden",
                format!(
                    "approval-waiver declared for node '{}' — workflows may REQUEST holds (Prepare) but never WAIVE approval gates (C-W4)",
                    rule.node_ref
                ),
            ));
        }
    }

    // node_ref 해석 — 본문 steps의 StepId를 가리켜야.
    let step_ids: Vec<&str> = doc.body.steps.iter().map(|s| s.id.as_str()).collect();
    for rule in &policy.approvals {
        if !step_ids.contains(&rule.node_ref.as_str()) {
            errors.push(Diagnostic::err(
                "node_ref_unresolved",
                format!(
                    "policy node_ref '{}' does not match any StepId in body.steps",
                    rule.node_ref
                ),
            ));
        }
    }
    // node_ref 중복 — 같은 노드에 충돌 규칙이면 모호하다.
    let mut refs: Vec<&str> = Vec::new();
    for rule in &policy.approvals {
        if refs.contains(&rule.node_ref.as_str()) {
            errors.push(Diagnostic::err(
                "node_ref_duplicate",
                format!(
                    "policy declares multiple rules for node '{}'",
                    rule.node_ref
                ),
            ));
        }
        refs.push(&rule.node_ref);
    }

    // Prepare 배치 규칙 — 전분기(all-branches) 승인게이트 (플랜 v6 F).
    for rule in &policy.approvals {
        if rule.level != PolicyLevel::Prepare {
            continue;
        }
        match prepare_placement_ok(doc, encoding, &rule.node_ref) {
            PreparePlacement::Ok => {}
            PreparePlacement::NoOutgoingEdges => errors.push(Diagnostic::err(
                "prepare_no_gate",
                format!(
                    "Prepare on '{}': the step's reachable states have no satisfiable outgoing edges — no approval gate exists there",
                    rule.node_ref
                ),
            )),
            PreparePlacement::MixedBranches { non_approval } => errors.push(Diagnostic::err(
                "prepare_mixed_branches",
                format!(
                    "Prepare on '{}': reachable states include a satisfiable non-approval outgoing edge ({}) — approval is not forced on all branches (all-branches rule)",
                    rule.node_ref, non_approval
                ),
            )),
        }
    }

    // 경고(예): 스킬이 승인 권한을 요구하는데 정책이 Prepare 없음 — advisory.
    let has_prepare = policy
        .approvals
        .iter()
        .any(|r| r.level == PolicyLevel::Prepare);
    if !has_prepare && !policy.approvals.is_empty() {
        warnings.push(Diagnostic::warn(
            "policy_without_prepare",
            "policy declares approvals but none is Prepare — no gates will be requested",
        ));
    }
}

/// Prepare 배치 분석 결과.
enum PreparePlacement {
    Ok,
    NoOutgoingEdges,
    MixedBranches { non_approval: String },
}

/// **게이트 패밀리** — FurnaceHold의 출구 엣지들 (실측):
/// * `Approved`: 인간 판정을 소비한다 (Line::approve가 ApprovalToken 검증).
/// * `Rejected`: 무조건 출구지만 **안전한 종단**이다 — 출하 없이 주문을
///   종결시킨다 (astra T54 MAJOR: "인간 판정 소비"가 아니라는 점을 정확히
///   기술한다. 승인 없이 진행 가능한 갈래가 아님은 보장된다).
/// Prepare의 전분기 규칙: 도달 상태의 만족 가능한 출구가 전부 이 집합
/// (승인 소비 또는 안전한 종단)이어야 한다.
// InspectionPassed may now self-loop at FurnaceHold after review re-entry.
// It preserves the gate state and cannot advance to publishing, so it is a
// safe gate-family edge for Prepare placement analysis.
const GATE_FAMILIES: &[EdgeEvent] = &[
    EdgeEvent::Approved,
    EdgeEvent::Rejected,
    EdgeEvent::InspectionPassed,
    EdgeEvent::InspectionFailed,
];

/// **전분기 도달성 분석** (플랜 v6 F): Prepare 노드까지의 스텝열을 따라
/// 도달 가능한 상태 집합을 계산하고, 그 상태들의 만족 가능한 출발 엣지가
/// **전부** 게이트 패밀리(승인 소비 또는 안전한 종단)인지 확인한다.
/// 유한 상태공간+방문 집합 → 종결.
/// 프로덕션 코드 — force_state_for_test 없이 순수 (엣지, 가드) 그래프 분석.
fn prepare_placement_ok(
    doc: &WorkflowDoc,
    encoding: &EmbeddedWorkflow,
    node_ref: &str,
) -> PreparePlacement {
    // node_ref 미해결이면 이미 node_ref_unresolved 오류가 push되었다 —
    // 분석은 skip (panic 없이).
    let Some(idx) = doc.body.steps.iter().position(|s| s.id == node_ref) else {
        return PreparePlacement::Ok;
    };

    // 시작: Received에서 steps[0..=idx]를 따라 도달 가능한 상태 집합.
    // 전이 가능성 = (동일 패밀리 엣지 존재) ∧ (가드가 만족 가능한 시도값 존재).
    let mut states: Vec<OrderState> = vec![OrderState::Received];
    for (i, st) in doc.body.steps.iter().enumerate() {
        if i > idx {
            break;
        }
        let fam = match event_family_from_name(&st.event) {
            Some(f) => f,
            None => return PreparePlacement::NoOutgoingEdges, // unknown_event가 이미 오류로 기록됨
        };
        let mut next: Vec<OrderState> = Vec::new();
        for &s in &states {
            for e in &encoding.edges {
                if e.event != fam || e.from != s {
                    continue;
                }
                // 가드 만족 가능성: 시도값 1..=MAX 범위에서 만족 가능한가.
                let guard = &e.guard;
                let satisfiable = (1u32..=foundry_domain::order::MAX_ATTEMPTS)
                    .any(|a| probes_satisfy_guard(guard, &fam, a));
                if !satisfiable {
                    continue;
                }
                let dest = match &e.to {
                    EdgeTarget::State(t) => *t,
                    EdgeTarget::BackEdge { to, .. } => *to,
                };
                if !next.contains(&dest) {
                    next.push(dest);
                }
            }
        }
        states = next;
        if states.is_empty() {
            return PreparePlacement::NoOutgoingEdges;
        }
    }

    // 검사: 도달 상태마다 만족 가능한 출발 엣지가 전부 게이트 패밀리인가
    // (승인 없이 진행하는 출구가 하나도 없어야 — Approved는 인간 승인을
    // 소비하고, Rejected는 출하 없는 안전한 종단이다).
    for &s in &states {
        let satisfiable: Vec<_> = encoding
            .edges
            .iter()
            .filter(|e| e.from == s)
            .filter(|e| {
                (1u32..=foundry_domain::order::MAX_ATTEMPTS)
                    .any(|a| probes_satisfy_guard(&e.guard, &e.event, a))
            })
            .collect();
        if satisfiable.is_empty() {
            // 출발 엣지 없음 = 게이트 없음 — Prepare는 잘못된 배치.
            return PreparePlacement::NoOutgoingEdges;
        }
        for e in &satisfiable {
            if !GATE_FAMILIES.contains(&e.event) {
                return PreparePlacement::MixedBranches {
                    non_approval: format!("{:?}", e.event),
                };
            }
        }
    }
    PreparePlacement::Ok
}

/// 패밀리의 대표 프로브 이벤트 (시도 base) — 실제 OrderEvent 변형에 정합.
/// 가드 평가에 필요한 payload만 채운다 (guard_satisfied이 payload를 본다).
fn probe_events(fam: EdgeEvent, base: u32) -> Vec<foundry_domain::OrderEvent> {
    use foundry_domain::OrderEvent;
    match fam {
        EdgeEvent::Queued => vec![OrderEvent::Queued],
        EdgeEvent::DieStarted => vec![OrderEvent::DieStarted],
        EdgeEvent::DieSucceeded => vec![OrderEvent::DieSucceeded { attempt: base }],
        EdgeEvent::DieFailed => vec![OrderEvent::DieFailed {
            attempt: base,
            reason: format!("err-{base}"),
        }],
        EdgeEvent::InspectionPassed => vec![OrderEvent::InspectionPassed],
        EdgeEvent::ReviewHeld => vec![OrderEvent::ReviewHeld {
            reason: "review held".into(),
        }],
        EdgeEvent::InspectionFailed => vec![
            OrderEvent::InspectionFailed {
                reason: format!("no-op-{base}"),
            },
            OrderEvent::InspectionFailed {
                reason: format!("real-{base}"),
            },
        ],
        EdgeEvent::Approved => vec![OrderEvent::Approved {
            intent: "i".into(),
            artifact_digest: "d".into(),
        }],
        EdgeEvent::Rejected => vec![OrderEvent::Rejected { intent: "i".into() }],
        EdgeEvent::PublishConfirmed => vec![OrderEvent::PublishConfirmed { pr_url: "u".into() }],
        EdgeEvent::PublishUnknown => vec![OrderEvent::PublishUnknown {
            operation_id: "o".into(),
        }],
        EdgeEvent::CancelRequested => vec![OrderEvent::CancelRequested { reason: "r".into() }],
        EdgeEvent::Canceled => vec![OrderEvent::Canceled { reason: "r".into() }],
        EdgeEvent::CancelConfirmed => vec![OrderEvent::CancelConfirmed],
    }
}

/// 가드가 (시도 a에서) 만족 가능한 프로브를 갖는가 — event는 참조로.
fn probes_satisfy_guard(g: &Guard, event: &EdgeEvent, a: u32) -> bool {
    probe_events(event.clone(), a)
        .iter()
        .any(|e| guard_satisfied(g, e, a))
}

// ============================================================================
// G3 대항 픽스처 — 검증기 우회 시나리오 (플랜 v6 게이트 G3, 4케이스).
// 실행 반전 검증(②무grant 라이브 거부 ③타스코프 ④유효 승인)은 T53a
// 구현 후 완결 — T54는 픽스처와 계약 동결을 인도한다.
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::{
        PolicyApproval, PolicyDoc, PolicyLevel, RouteRule, RoutingRules, SkillKind, SkillRef,
        StepSpec, WorkflowBody, WorkflowDoc, interpreter_v1, merkle_root,
    };
    use foundry_domain::workflow_encoding::EmbeddedWorkflow;

    fn v3_doc() -> WorkflowDoc {
        WorkflowDoc {
            schema_version: 3,
            id: "order-pipeline-v3".into(),
            body: WorkflowBody {
                encoding_digest: EmbeddedWorkflow::frozen_v1().digest(),
                steps: vec![
                    StepSpec {
                        id: "s1".into(),
                        event: "Queued".into(),
                        skill: Some("die".into()),
                    },
                    StepSpec {
                        id: "s2".into(),
                        event: "DieStarted".into(),
                        skill: Some("die".into()),
                    },
                ],
            },
            skills: vec![SkillRef {
                id: "die".into(),
                digest: "sk-die-abc".into(),
                kind: SkillKind::Role,
            }],
            routing: RoutingRules {
                rules: vec![RouteRule {
                    source_pattern: "cron:*".into(),
                    workflow_id: "order-pipeline-v3".into(),
                }],
                default_route: "order-pipeline-v3".into(),
            },
            policy_digest: String::new(),
            policy: Some(PolicyDoc {
                schema_version: 1,
                approvals: vec![],
            }),
            review_pack: None,
        }
    }

    /// **T55 프리셋 (BMad 풀)** — 저장소 동봉 프리셋이 (i) 로드되고
    /// (ii) 검증기를 통과하며 (iii) dev 노드의 Prepare 게이트가 살아있음을
    /// 증명. 프리셋은 문서 자체가 배포 자산 — 검증 통과가 출하 조건.
    #[test]
    fn t55_bmad_pool_preset_validates() {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let path = std::path::Path::new(manifest)
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("presets/bmad-intake-pool.v3.json");
        let doc =
            crate::dsl::load_preset(&path).unwrap_or_else(|e| panic!("preset must load: {e}"));
        assert_eq!(doc.id, "bmad-intake-pool");
        assert_eq!(
            doc.body.steps.len(),
            5,
            "4 BMad role steps + advisory review + ship gate"
        );
        assert_eq!(doc.skills.len(), 5);
        // 검증 통과 — 오류 0.
        let out = validate(&doc, &EmbeddedWorkflow::frozen_v1())
            .unwrap_or_else(|errs| panic!("preset must validate: {errs:?}"));
        assert!(
            out.warnings.is_empty(),
            "no warnings expected: {:?}",
            out.warnings
        );
        // 정책 — ship-gate 노드 Prepare (도메인 승인 게이트와 정합).
        let pol = doc.policy.as_ref().expect("v3 embeds policy");
        assert_eq!(pol.approvals.len(), 1);
        assert_eq!(
            pol.approvals[0].node_ref, "intake-review",
            "Prepare sits on the last autonomous step before the FurnaceHold approval gate"
        );
        assert!(matches!(pol.approvals[0].level, PolicyLevel::Prepare));
        // 스킬 카탈로그 — BMad 5역할. role: analyst/architect/dev(+pm 미배선),
        // advisory: review (저널 권한 없음 — 스텝 배선도 없음).
        let dev = doc.skills.iter().find(|s| s.id == "dev").unwrap();
        assert!(matches!(dev.kind, SkillKind::Role));
        let review = doc.skills.iter().find(|s| s.id == "review").unwrap();
        assert!(matches!(review.kind, SkillKind::Advisory));
    }

    /// (astra T55 MAJOR) 로더는 거짓 encoding_digest 선언을 마스킹하지
    /// 않는다 — 센티넬/빈값만 실측 치환, 불일치 선언은 거부.
    #[test]
    fn t55_preset_false_digest_declaration_refused() {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let path = std::path::Path::new(manifest)
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("presets/bmad-intake-pool.v3.json");
        let raw = std::fs::read_to_string(&path).unwrap();
        let tampered = raw.replace("\"enc-frozen-v1\"", "\"wf-emb-deadbeef\"");
        assert_ne!(tampered, raw, "tamper must take effect");
        let err = crate::dsl::load_preset_from_str(&tampered).unwrap_err();
        assert!(err.contains("refusing to mask"), "{err}");
        // 센티넬 그대로는 통과.
        assert!(crate::dsl::load_preset_from_str(&raw).is_ok());
    }

    /// 프리셋 변조 방어 —Waive 주입 시 검증기가 거부하는지 (프리셋 파일이
    /// 아닌 메모리 변형으로 증명).
    #[test]
    fn t55_bmad_pool_preset_waive_injection_rejected() {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let path = std::path::Path::new(manifest)
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("presets/bmad-intake-pool.v3.json");
        let mut doc = crate::dsl::load_preset(&path).unwrap();
        let pol = doc.policy.as_mut().unwrap();
        pol.approvals[0].level = PolicyLevel::Waive;
        let errs = validate(&doc, &EmbeddedWorkflow::frozen_v1()).unwrap_err();
        assert!(
            errs.iter()
                .any(|d| matches!(d, Diagnostic::Error { code, .. } if code.contains("waive"))),
            "waive must be refused: {errs:?}"
        );
    }

    // (1) 정상 v3 문서 — 검증 통과 (경고 없음).
    #[test]
    fn g3_case0_valid_v3_doc_passes() {
        let enc = EmbeddedWorkflow::frozen_v1();
        let out = validate(&v3_doc(), &enc).expect("valid doc must pass");
        assert!(out.warnings.is_empty());
        assert!(out.doc_digest.starts_with("wf-root-"));
    }

    // G3 케이스 ① — Waive 선언 → 작성 시 거부 (오류, 경고 아님).
    #[test]
    fn g3_case1_waive_declaration_rejected() {
        let enc = EmbeddedWorkflow::frozen_v1();
        let mut d = v3_doc();
        d.policy = Some(PolicyDoc {
            schema_version: 1,
            approvals: vec![PolicyApproval {
                node_ref: "s2".into(),
                level: PolicyLevel::Waive,
            }],
        });
        let errs = validate(&d, &enc).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, Diagnostic::Error { code, .. } if code == "waive_forbidden")),
            "Waive must be a hard error: {errs:?}"
        );
    }

    // (2) 불명 필드 — v3 전체 중첩 엄격 파싱 (deny_unknown_fields).
    #[test]
    fn g3_case2_unknown_field_rejected() {
        let enc = EmbeddedWorkflow::frozen_v1();
        // 정상 v3 직렬화에 불명 필드 심기
        let mut json = serde_json::to_value(&v3_doc()).unwrap();
        json["body"]["secret_backdoor"] = serde_json::json!(true);
        let doc: Result<WorkflowDoc, _> = serde_json::from_value(json);
        assert!(
            doc.is_err(),
            "unknown fields in v3 docs must be REJECTED, not discarded"
        );
        let _ = enc;
    }

    // (3) node_ref 미해결 — 정책이 없는 StepId를 가리킴.
    #[test]
    fn g3_case3_unresolved_node_ref_rejected() {
        let enc = EmbeddedWorkflow::frozen_v1();
        let mut d = v3_doc();
        d.policy = Some(PolicyDoc {
            schema_version: 1,
            approvals: vec![PolicyApproval {
                node_ref: "ghost-step".into(),
                level: PolicyLevel::Prepare,
            }],
        });
        let errs = validate(&d, &enc).unwrap_err();
        assert!(
            errs.iter().any(
                |e| matches!(e, Diagnostic::Error { code, .. } if code == "node_ref_unresolved")
            )
        );
    }

    // (4) StepId 계약 — 빈 id·중복 id 거부.
    #[test]
    fn g3_case4_step_id_contract() {
        let enc = EmbeddedWorkflow::frozen_v1();
        // 빈 id
        let mut d = v3_doc();
        d.body.steps[1].id = "  ".into();
        let errs = validate(&d, &enc).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, Diagnostic::Error { code, .. } if code == "step_id_empty"))
        );
        // 중복 id
        let mut d = v3_doc();
        d.body.steps[1].id = "s1".into();
        let errs = validate(&d, &enc).unwrap_err();
        assert!(
            errs.iter().any(
                |e| matches!(e, Diagnostic::Error { code, .. } if code == "step_id_duplicate")
            )
        );
    }

    // (5) 존재하지 않는 이벤트명 거부.
    #[test]
    fn g3_case5_unknown_event_rejected() {
        let enc = EmbeddedWorkflow::frozen_v1();
        let mut d = v3_doc();
        d.body.steps[1].event = "SelfDestruct".into();
        let errs = validate(&d, &enc).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, Diagnostic::Error { code, .. } if code == "unknown_event"))
        );
    }

    // (6) Prepare 혼합 분기 — 승인 없이 진행 가능한 갈래가 하나라도 있으면 오류.
    // 검증 대상: Prepare가 선언된 노드 뒤 도달 상태의 만족 가능한 출발 엣지가
    // 전부 Approved 패밀리여야. s2(DieStarted) 뒤는 Working — Working의 출발
    // 엣지는 DieSucceeded/DieFailed/CancelRequested 등 비승인 다수 → 오류.
    #[test]
    fn g3_case6_prepare_mixed_branches_rejected() {
        let enc = EmbeddedWorkflow::frozen_v1();
        let mut d = v3_doc();
        d.policy = Some(PolicyDoc {
            schema_version: 1,
            approvals: vec![PolicyApproval {
                node_ref: "s2".into(),
                level: PolicyLevel::Prepare,
            }],
        });
        let errs = validate(&d, &enc).unwrap_err();
        assert!(
            errs.iter().any(
                |e| matches!(e, Diagnostic::Error { code, .. } if code == "prepare_mixed_branches")
            ),
            "Prepare after DieStarted (reaching Working) must be a mixed-branch error: {errs:?}"
        );
    }

    // (7) Prepare 합법 배치 — 게이트 지점(FurnaceHold)에 도달하는 노드.
    // 실측: Inspecting --InspectionPassed--> FurnaceHold이고 FurnaceHold의
    // 출구는 {Approved, Rejected} 전부 게이트 패밀리(인간 승인 소개 또는
    // 안전한 종단) — 전분기 규칙 만족.
    #[test]
    fn g3_case7_prepare_legal_placement_passes() {
        let enc = EmbeddedWorkflow::frozen_v1();
        let mut d = v3_doc();
        d.body.steps = vec![
            StepSpec {
                id: "q".into(),
                event: "Queued".into(),
                skill: Some("die".into()),
            },
            StepSpec {
                id: "w".into(),
                event: "DieStarted".into(),
                skill: Some("die".into()),
            },
            StepSpec {
                id: "d".into(),
                event: "DieSucceeded".into(),
                skill: Some("die".into()),
            },
            StepSpec {
                id: "p".into(),
                event: "InspectionPassed".into(),
                skill: None,
            },
        ];
        d.policy = Some(PolicyDoc {
            schema_version: 1,
            approvals: vec![PolicyApproval {
                node_ref: "p".into(),
                level: PolicyLevel::Prepare,
            }],
        });
        let out = validate(&d, &enc);
        assert!(
            out.is_ok(),
            "legal Prepare (reaching FurnaceHold) must pass: {:?}",
            out.err()
        );
    }

    // (8) v1/v2 문서는 검증기 대상 아님 — 동결 인터프리터 경로 유지.
    #[test]
    fn g3_case8_legacy_docs_still_frozen_interpreters() {
        let enc = EmbeddedWorkflow::frozen_v1();
        let mut d = v3_doc();
        d.schema_version = 1;
        // v1 문서는 policy_digest 참조 — 기존 구조. 검증기는 v3만.
        d.policy = None;
        let errs = validate(&d, &enc).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, Diagnostic::Error { code, .. } if code == "schema_version"))
        );
        // 그리고 v1 인터프리터로는 여전히 해석된다 (레거시 재생 불변).
        assert!(interpreter_v1::interpret(&d, &enc).is_ok());
    }

    // 공유 라우팅 계약 — 나쁜 패턴은 문서 검증 단계에서도 잡힌다.
    #[test]
    fn t53_validator_uses_shared_pattern_contract() {
        let enc = EmbeddedWorkflow::frozen_v1();
        let mut d = v3_doc();
        d.routing.rules.push(crate::dsl::RouteRule {
            source_pattern: "a*b*c".into(),
            workflow_id: "order-pipeline-v3".into(),
        });
        let errs = validate(&d, &enc).unwrap_err();
        assert!(
            errs.iter().any(
                |e| matches!(e, Diagnostic::Error { code, .. } if code == "bad_route_pattern")
            ),
            "{errs:?}"
        );
    }

    // (astra T54 MAJOR 1) PolicyDoc 지원되지 않는 버전 → 거부.
    #[test]
    fn t54_policy_schema_version_checked() {
        let enc = EmbeddedWorkflow::frozen_v1();
        let mut d = v3_doc();
        d.policy = Some(PolicyDoc {
            schema_version: 99,
            approvals: vec![],
        });
        let errs = validate(&d, &enc).unwrap_err();
        assert!(errs.iter().any(
            |e| matches!(e, Diagnostic::Error { code, .. } if code == "policy_schema_version")
        ));
        let mut d = v3_doc();
        d.policy = Some(PolicyDoc {
            schema_version: 0,
            approvals: vec![],
        });
        let errs = validate(&d, &enc).unwrap_err();
        assert!(errs.iter().any(
            |e| matches!(e, Diagnostic::Error { code, .. } if code == "policy_schema_version")
        ));
    }

    // (astra T54 MAJOR 2) v1 문서에 policy 주입 + policy_digest 변경 —
    // 리프는 policy_digest 기준 (버전이 지배) → root는 정상 변환.
    #[test]
    fn t54_legacy_merkle_leaf_is_version_selected() {
        let mut d = v3_doc();
        d.schema_version = 1;
        d.policy = None;
        let base = merkle_root(&d);
        // policy 주입 (버전 1이므로 리프는 policy_digest 그대로)
        let mut with_policy = d.clone();
        with_policy.policy = Some(PolicyDoc {
            schema_version: 1,
            approvals: vec![],
        });
        assert_eq!(
            merkle_root(&with_policy),
            base,
            "v1 leaf selection must IGNORE injected policy (version governs)"
        );
        // policy_digest를 바꾸면 root 변화 (리프가 실제로 policy_digest임을 증명)
        let mut changed = d.clone();
        changed.policy_digest = "pol-CHANGED".into();
        assert_ne!(
            merkle_root(&changed),
            base,
            "v1 policy_digest change must change root"
        );
        // 조합 공격 (astra T54-r2): policy가 주입된 상태에서 policy_digest까지
        // 바꾸면 — 버전이 지배하므로 리프는 policy_digest → base와 달라야.
        let mut combo = with_policy.clone();
        combo.policy_digest = "pol-CHANGED".into();
        assert_ne!(
            merkle_root(&combo),
            base,
            "combined policy-injection + digest-change must NOT reproduce the base root"
        );
    }

    // (astra T54 MAJOR 3) 게이트 의미론 — Rejected는 안전한 종단으로 인정되고
    // 그 사실이 상수 문서에 정확히 기술되어 있는지 (인간 판정 소비가 아님).
    // 동작 회귀: FurnaceHold 도달 Prepare는 합법 유지 (Approved+Rejected 전부
    // 게이트 집합), Working 도달은 혼합 분기 오류 유지.
    #[test]
    fn t54_gate_families_documented_as_safe_terminal() {
        let enc = EmbeddedWorkflow::frozen_v1();
        // 합법: FurnaceHold 도달 (출구 Approved/Rejected 전부 게이트)
        let mut d = v3_doc();
        d.body.steps = vec![
            StepSpec {
                id: "q".into(),
                event: "Queued".into(),
                skill: Some("die".into()),
            },
            StepSpec {
                id: "w".into(),
                event: "DieStarted".into(),
                skill: Some("die".into()),
            },
            StepSpec {
                id: "d".into(),
                event: "DieSucceeded".into(),
                skill: Some("die".into()),
            },
            StepSpec {
                id: "p".into(),
                event: "InspectionPassed".into(),
                skill: None,
            },
        ];
        d.policy = Some(PolicyDoc {
            schema_version: 1,
            approvals: vec![PolicyApproval {
                node_ref: "p".into(),
                level: PolicyLevel::Prepare,
            }],
        });
        assert!(validate(&d, &enc).is_ok());
    }

    // (9) 검증기 통과는 런타임 권위를 부여하지 않는다 — merkle root 산출만.
    #[test]
    fn g3_case9_validation_confers_no_runtime_authority() {
        let enc = EmbeddedWorkflow::frozen_v1();
        let out = validate(&v3_doc(), &enc).unwrap();
        // doc_digest는 문서 정체성일 뿐 — 승인·활성화·grant와 무관함을
        // 구조적으로 확인: ValidationOutcome에는 권한 필드가 없다.
        assert_eq!(out.doc_digest, merkle_root(&v3_doc()));
    }
}
