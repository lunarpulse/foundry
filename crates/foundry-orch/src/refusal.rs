//! (M7 T57 / I절) 거부 코드→수락 조건 레지스트리 — 타입드·전수 매핑.
//!
//! 계약:
//! - 거부 코드는 **이 열거형으로만** 존재한다 — 미등록 문자열 코드
//!   방출 금지 (타입 시스템 강제).
//! - 모든 코드는 수락 조건(사람 말)과 (가능시) 재디에이션 작업 종류를
//!   동반 — 열거형 매칭이 전수성을 강제한다 (새 코드 추가 시 컴파일러가
//!   매핑 누락을 거부).
//! - presets/README 의 진단 설명은 이 레지스트리에서 파생한다(단일
//!   소스 — 드리프트 원천 차단).

use crate::OrchestrationKind;

/// 거부 코드 — 인간 대면 거부 경로가 방출할 수 있는 전 집합.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefusalCode {
    /// 사이징 승인(preset_bound) 없는 발행.
    UnsizedSpawn,
    /// plan_declared 없는 주문.
    UndeclaredOrder,
    /// 선언의 의존성에 사이클.
    DeclarationCycle,
    /// 선언이 존재하지 않는 선행 주문을 참조.
    MissingPredecessor,
    /// 같은 plan 의 중복 선언.
    DuplicateDeclaration,
    /// 핸드오프 매니페스트 3자 불일치 (원장↔재계산↔로그).
    HandoffMismatch,
    /// 승인 이벤트가 게이트를 우회함 (방화벽) — 재디에이션 없음(구조 위반).
    GateBypass,
    /// 프리셋 검증 실패 (v9: 워크플로 문서 계층).
    PresetInvalid,
    /// (T58 astra r2) 회복 정지 중 입장 차단.
    QuiescenceBlocked,
    /// (T58 astra r2) 선행 주문 미출하로 not ready.
    NotReady,
    /// (T58 astra r2) dispatch 파라미터 id ≠ order.id.
    CheckedIdMismatch,
}

impl RefusalCode {
    /// 전집 — registry 전수성 검증의 기준 (수동 유지, 게이트가 대조).
    /// (astra r2 M4) 전수성 회귀가 이 목록과 entry_for 의 match 를
    /// 양방향 대조한다 — 어느 쪽이 새 코드를 놓쳐도 테스트 실패.
    pub const VARIANTS: &'static [RefusalCode] = &[
        RefusalCode::UnsizedSpawn,
        RefusalCode::UndeclaredOrder,
        RefusalCode::DeclarationCycle,
        RefusalCode::MissingPredecessor,
        RefusalCode::DuplicateDeclaration,
        RefusalCode::HandoffMismatch,
        RefusalCode::GateBypass,
        RefusalCode::PresetInvalid,
        RefusalCode::QuiescenceBlocked,
        RefusalCode::NotReady,
        RefusalCode::CheckedIdMismatch,
    ];
}

/// 재디에이션 작업 종류 — 조건 충족 작업의 분류 (체이너가 제안).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemediationKind {
    /// 사이징 주문을 열어 인간 게이트에서 스케일 확정.
    RunSizing,
    /// 계획 선언 주문(또는 declare_plan 제안)을 수행.
    DeclarePlan,
    /// 핸드오프 재결속 제안 (매니페스트 재계산·재발급).
    RebindHandoff,
    /// 재디에이션 불가 — 인간 에스컬레이션으로 종결 (깊이 1 제한).
    Escalate,
}

/// 레지스트리 항목 — 사람 말 이유·수락 조건·재디에이션.
pub struct RefusalEntry {
    pub code: RefusalCode,
    /// 사람 말 이유 (승인 프롬프트·RefusalRecord 에 기록).
    pub reason_text: &'static str,
    /// 수락 조건 — "무엇이 참이면 통과하는가".
    pub acceptance_condition: &'static str,
    /// 조건을 만드는 작업 (Escalate 면 제안 없음).
    pub remediation: RemediationKind,
}

/// 전수 조회 — **전수 match 가 단일 정의** (astra r3 M4: 순환 제거).
/// enum 변형 추가 시 이 match 가 누락을 강제하고, VARIANTS 는 이
/// 함수와 별도로 대조 검증된다(순환 없음).
pub fn all_codes_exhaustive() -> Vec<RefusalCode> {
    // (astra r4 최종) 컴파일러 전수 강제 + 나열 일체형: 매크로로 각
    // 변형을 한 번만 언급 — 더미 match 가 비전수면 컴파일 오류, vec!
    // 나열은 같은 매크로 토큰에서 생성(분리된 두 목록이 아님).
    macro_rules! codes {
        ($($v:ident),* $(,)?) => {{
            fn assert_exhaustive(c: RefusalCode) {
                match c {
                    $(RefusalCode::$v => ()),*
                }
            }
            vec![$(RefusalCode::$v),*]
        }};
    }
    codes!(
        UnsizedSpawn,
        UndeclaredOrder,
        DeclarationCycle,
        MissingPredecessor,
        DuplicateDeclaration,
        HandoffMismatch,
        GateBypass,
        PresetInvalid,
        QuiescenceBlocked,
        NotReady,
        CheckedIdMismatch,
    )
}

/// registry — all_codes_exhaustive 에서 파생.
pub fn registry() -> Vec<RefusalEntry> {
    all_codes_exhaustive().into_iter().map(entry_for).collect()
}

/// 코드로 항목 조회 — **전수 match** (astra r1 M4): 새 코드 추가 시
/// 이 match 가 컴파일 오류로 매핑 누락을 강제한다 (러스트 전수성 검사).
pub fn entry_for(code: RefusalCode) -> RefusalEntry {
    let (reason_text, acceptance_condition, remediation) = match code {
        RefusalCode::UnsizedSpawn => (
            "사이징 승인 없이 발행된 주문입니다",
            "사이징 게이트에서 프리셋이 확정되어야 합니다 (preset_bound)",
            RemediationKind::RunSizing,
        ),
        RefusalCode::UndeclaredOrder => (
            "계획 선언 없는 주문입니다",
            "plan_declared 행이 이 주문을 포함해야 합니다",
            RemediationKind::DeclarePlan,
        ),
        RefusalCode::DeclarationCycle => (
            "선언의 의존성에 사이클이 있습니다",
            "의존성 간선이 비순환이어야 합니다",
            RemediationKind::Escalate,
        ),
        RefusalCode::MissingPredecessor => (
            "존재하지 않는 선행 주문을 참조합니다",
            "선행 주문이 (같은 선언 안에) 먼저 선언되어야 합니다",
            RemediationKind::DeclarePlan,
        ),
        RefusalCode::DuplicateDeclaration => (
            "같은 계획이 두 번 선언되었습니다",
            "기존 선언을 갱신하거나 새 plan id 를 사용해야 합니다",
            RemediationKind::DeclarePlan,
        ),
        RefusalCode::HandoffMismatch => (
            "핸드오프 매니페스트가 로그·원장과 일치하지 않습니다",
            "매니페스트가 가리키는 로그 행 기준으로 재결속되어야 합니다",
            RemediationKind::RebindHandoff,
        ),
        RefusalCode::GateBypass => (
            "승인이 게이트를 우회하려 했습니다 (방화벽)",
            "인간 게이트(dispatch_live + grant)가 유일한 승인 경로입니다",
            RemediationKind::Escalate,
        ),
        RefusalCode::PresetInvalid => (
            "프리셋이 검증기를 통과하지 못했습니다",
            "검증기 진단(예: Prepare 배치·default_route)이 해소되어야 합니다",
            RemediationKind::Escalate,
        ),
        RefusalCode::QuiescenceBlocked => (
            "회복 정지(quiescence) 중입니다 — 새 디스패치가 차단되었습니다",
            "회복 코디네이터의 재개(드레인 완료 또는 타임아웃)를 기다려야 합니다",
            RemediationKind::Escalate,
        ),
        RefusalCode::NotReady => (
            "선행 주문이 아직 출하되지 않았습니다",
            "의존성 그래프의 선행 주문이 Shipped 여야 합니다",
            // (astra r3 정정) 선언 추가는 선행을 출하시키지 않는다 —
            // 선행 완료 대기 또는 인간 개입.
            RemediationKind::Escalate,
        ),
        RefusalCode::CheckedIdMismatch => (
            "디스패치 요청 id가 주문 실제 id와 일치하지 않습니다",
            "요청 id와 order.id 가 동일해야 합니다",
            RemediationKind::Escalate,
        ),
    };
    RefusalEntry { code, reason_text, acceptance_condition, remediation }
}

/// OrchError → 코드 매핑 — 저장소 오류를 인간 대면 코드로.
pub fn code_for_orch_error(e: &crate::OrchError) -> Option<RefusalCode> {
    use crate::OrchError as E;
    use RefusalCode::*;
    Some(match e {
        // (astra r1 M4 정정) 발행 시점의 UndeclaredOrder 는 unsized-spawn
        // 계약 위반(사이징 결속 없는 발행)이 본질 — 동일 코드로 매핑.
        // DuplicateOrder/StreamMismatch 도 발행 규율 위반의 변형.
        E::UndeclaredOrder { .. } | E::DuplicateOrder { .. } | E::StreamMismatch { .. } => UnsizedSpawn,
        E::MissingPredecessor { .. } => MissingPredecessor,
        E::Cycle { .. } => DeclarationCycle,
        E::DuplicateDeclaration { .. } => DuplicateDeclaration,
        _ => return None, // Io/ChainBroken/NoRow/FieldTooLarge/RawAppend 는 기술 오류
    })
}

/// RefusalRecord 를 로그에 append — 3요소 동반 단정 (I절 게이트 지원).
pub fn record_refusal(
    log: &crate::OrchestrationLog,
    code: RefusalCode,
    refused_request: &str,
    remediation_proposal_id: Option<&str>,
) -> Result<crate::OrchRow, crate::OrchError> {
    let e = entry_for(code);
    log.append(crate::OrchestrationKind::Refusal {
        refused_request: refused_request.into(),
        refusal_code: format!("{code:?}"),
        reason_text: e.reason_text.into(),
        acceptance_condition: e.acceptance_condition.into(),
        remediation_proposal_id: remediation_proposal_id.map(|s| s.into()),
    })
}

/// presets/README 진단 표 파생 (astra r1 M4 — 문서와 레지스트리 같은 소스).
/// 마크다운 행들을 반환: | 코드 | 이유 | 수락 조건 | 재디에이션 |
pub fn readme_diagnostic_rows() -> Vec<String> {
    registry()
        .into_iter()
        .map(|e| {
            format!(
                "| `{}` | {} | {} | {:?} |",
                format!("{:?}", e.code).replace("RefusalCode::", ""),
                e.reason_text,
                e.acceptance_condition,
                e.remediation
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 전수성 — registry 가 모든 코드를 커버 (양방향: VARIANTS ↔ match).
    #[test]
    fn registry_covers_all_codes() {
        let all = registry();
        assert_eq!(all.len(), RefusalCode::VARIANTS.len());
        for code in RefusalCode::VARIANTS {
            assert!(all.iter().any(|e| e.code == *code), "{code:?} missing");
        }
        // 중복 없음 — VARIANTS 가 같은 코드를 두 번 나열해도 잡음.
        let mut seen = std::collections::HashSet::new();
        for e in &all {
            assert!(seen.insert(e.code), "{:?} duplicated", e.code);
        }
        // (astra r3 M4) 비순환 대조 — 수동 VARIANTS 와 전수 match 나열이
        // 정확히 일치(양쪽 집합 동일). 어느 한쪽만 새 코드를 추가해도 실패.
        let exhaustive: std::collections::HashSet<RefusalCode> =
            all_codes_exhaustive().into_iter().collect();
        let manual: std::collections::HashSet<RefusalCode> =
            RefusalCode::VARIANTS.iter().copied().collect();
        assert_eq!(exhaustive, manual, "VARIANTS and exhaustive match must agree");
    }

    /// (astra r3 M4) README 실배선 — presets/README.md 의 거부 코드 진단
    /// 표가 레지스트리 생성과 일치해야 한다(드리프트 감지).
    #[test]
    fn readme_refusal_table_matches_registry() {
        let rows = readme_diagnostic_rows();
        assert_eq!(rows.len(), all_codes_exhaustive().len());
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent().unwrap().parent().unwrap()
            .join("presets/README.md");
        let md = std::fs::read_to_string(&path).unwrap();
        for r in &rows {
            // (astra r4 강화) 전체 행 대조 — 이유·조건·재디에이션까지
            // 드리프트하면 실패 (코드 셀만이 아니라).
            assert!(
                md.contains(r.as_str()),
                "README.md missing full diagnostics row '{r}' — regenerate from readme_diagnostic_rows()"
            );
        }
    }

    /// 3요소 동반 — 모든 항목이 이유·조건·재디에이션을 가짐 (빈 문자열 금지).
    #[test]
    fn every_entry_carries_three_parts() {
        for e in registry() {
            assert!(!e.reason_text.is_empty(), "{:?} reason", e.code);
            assert!(!e.acceptance_condition.is_empty(), "{:?} condition", e.code);
        }
    }

    /// 거부 기록 — 레코드가 로그에 3요소로 남고 증거에서 배제된다.
    #[test]
    fn refusal_recorded_with_three_parts_and_excluded() {
        let mut p = std::env::temp_dir();
        p.push(format!("foundry-refusal-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let log = crate::OrchestrationLog::open(&p).unwrap();
        let row = record_refusal(
            &log,
            RefusalCode::UnsizedSpawn,
            "req-42",
            Some("prop-1"),
        )
        .unwrap();
        if let OrchestrationKind::Refusal {
            refusal_code,
            reason_text,
            acceptance_condition,
            ..
        } = &row.kind
        {
            assert_eq!(refusal_code, "UnsizedSpawn");
            assert!(reason_text.contains("사이징"));
            assert!(acceptance_condition.contains("preset_bound"));
        } else {
            panic!("not a refusal row");
        }
        // 증거 배제.
        assert_eq!(log.declaration_rows().unwrap().len(), 0);
    }

    /// 오류→코드 매핑 — 인간 대면 오류만 매핑 (Io 는 None).
    #[test]
    fn orch_error_maps_to_facing_codes() {
        use crate::OrchError as E;
        assert_eq!(
            code_for_orch_error(&E::UndeclaredOrder { order: "o".into() }),
            Some(RefusalCode::UnsizedSpawn)
        );
        assert_eq!(code_for_orch_error(&E::Io("x".into())), None);
    }
}
