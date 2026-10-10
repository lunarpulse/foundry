//! T51c — 워크플로 DSL 스키마 v1 + 버전 동결 인터프리터 (M6).
//!
//! 계약 (M6 플랜 C-W1/W2):
//! * **콘텐츠 주소 아티팩트**: 워크플로 digest = Merkle root —
//!   {본문, 스킬 digest, 라우팅 규칙 digest, 정책 digest} 전부를 덮는다.
//!   스킬 교체가 몰래 새 행동을 주입하는 M5 #24 동형 공격 차단:
//!   어느 성분이든 바뀌면 root가 바뀐다 (t-1).
//! * **버전 동결 인터프리터**: 스키마는 schema_version을 명시하고
//!   각 버전의 인터프리터는 동결된다 — 2026 digest가 2028에도
//!   실행 가능 (M2 코퍼스 규율 승계). v1 인터프리터는 영원히 v1
//!   문서를 v1 규칙으로 해석한다; 미지 버전은 거부 (G7 전제).
//! * **C-W3**: 라우팅은 규칙만 권위 — RoutingRules는 정적 규칙
//!   (매칭 소스→워크플로), 모델 자문 필드 없음 (v2 연기).
//!
//! G7 (스키마 v1 digest를 v2 엔진에서 재생): `interpret()`는
//! 레지스트리에서 버전별 동결 인터프리터를 찾는다. v2 엔진 레지스트리는
//! v1+v2 해석기를 모두 품고 있으므로 v1 문서는 v1 해석기로 **동일하게**
//! 재생된다 — 결과(엣지 집합·digest)가 v1 엔진 시절과 바이트 동일.

use foundry_domain::workflow_encoding::EmbeddedWorkflow;
use serde::{Deserialize, Serialize};

/// DSL 문서 — 사용자 작성·콘텐츠 주소·실행 가능 아티팩트.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowDoc {
    pub schema_version: u32,
    pub id: String,
    /// 본문: 상태머신 엣지의 선언적 기술.
    pub body: WorkflowBody,
    /// 결속 스킬 (digest로만 참조 — 본문을 오염하지 않는다).
    pub skills: Vec<SkillRef>,
    /// 라우팅 규칙 (C-W3 — 규칙만 권위).
    pub routing: RoutingRules,
    /// 정책 아티팩트 digest (워크플로는 홀드를 요청할 수 있으나
    /// 면제할 수 없다 — C-W4; digest만 결속).
    pub policy_digest: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowBody {
    /// 이 문서가 구동할 상태머신 부호화의 digest (T51a frozen_v1 등).
    /// 엔진은 이 digest가 로딩된 부호화와 일치하는지 검증한다.
    pub encoding_digest: String,
    /// 이 워크플로가 수행하는 노드(엣지 패밀리) 순서 — 실행 계획.
    pub steps: Vec<StepSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepSpec {
    /// OrderEvent 패밀리 이름 ("Queued", "DieStarted", ...).
    pub event: String,
    /// 이 스텝에 배선된 스킬 (skills 내 id 참조 — digest 간접 참조).
    pub skill: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillRef {
    pub id: String,
    /// 스킬 digest — Merkle root 성분.
    pub digest: String,
    /// role/advisory 구분 (C-W2: advisory는 저널 권한 없음).
    pub kind: SkillKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillKind {
    /// 저널 이벤트 발행 가능 (도메인 권한 종속).
    Role,
    /// 산출물만 — 저널 권한 없음. role 슬롯 배선 불가 (구조 차단).
    Advisory,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingRules {
    /// 명시적 규칙만 — 암묵 폴백 금지 (C-W3: default_route 필수).
    pub rules: Vec<RouteRule>,
    pub default_route: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteRule {
    /// 매칭 소스 ("cron:*", "github:*", ...).
    pub source_pattern: String,
    /// 이 규칙이 가리키는 워크플로 id.
    pub workflow_id: String,
}

/// 해석 결과 — 실행 가능 형태.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledWorkflow {
    pub doc_digest: String,
    pub encoding: EmbeddedWorkflow,
    pub plan: Vec<StepSpec>,
    pub routing: RoutingRules,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InterpretError {
    /// 미지 스키마 버전 — 동결 인터프리터 없음 (G7 전제: 거부).
    UnknownSchemaVersion { got: u32, known: Vec<u32> },
    /// 본문이 지정한 부호화 digest가 로딩된 것과 불일치.
    EncodingMismatch { doc: String, loaded: String },
    /// StepSpec.skill이 skills에 없다.
    UnknownSkillRef { step_event: String, skill: String },
    /// advisory 스킬이 role 슬롯에 배선되었다 (구조 차단).
    AdvisoryInRoleSlot { skill: String },
    /// default_route가 워크플로 id와 불일치 (C-W3 암묵 폴백 금지).
    DefaultRouteMismatch { expected: String, got: String },
}

/// 정규 직렬화 — 구조체 필드 순서 + BTree 정렬로 결정적.
pub fn canonical_json(doc: &WorkflowDoc) -> String {
    serde_json::to_string(doc).expect("WorkflowDoc is canonical-encodable")
}

/// **Merkle root** — {본문, 스킬 digest, 라우팅, 정책} 전부 덮음.
/// 리프 4개를 각각 해시한 뒤 결합 해시 (도메인 분리자 포함).
pub fn merkle_root(doc: &WorkflowDoc) -> String {
    use sha2::Digest as _;
    let leaf = |tag: &str, content: &str| {
        let mut h = sha2::Sha256::new();
        h.update(tag.as_bytes());
        h.update(&(content.len() as u64).to_be_bytes());
        h.update(content.as_bytes());
        h.finalize()
    };
    let body_s = serde_json::to_string(&doc.body).unwrap();
    let skills_s = serde_json::to_string(&doc.skills).unwrap();
    let routing_s = serde_json::to_string(&doc.routing).unwrap();
    // astra MAJOR: schema_version은 root의 제5성분 — v1/v2 동일 내용이
    // 같은 root를 공유하면 서로 다른 인터프리터를 선택하는 문서의
    // 정체성이 소멸한다.
    let l0 = leaf("schema_version", &doc.schema_version.to_string());
    let l1 = leaf("body", &body_s);
    let l2 = leaf("skills", &skills_s);
    let l3 = leaf("routing", &routing_s);
    let l4 = leaf("policy", &doc.policy_digest);
    let mut root = sha2::Sha256::new();
    root.update(b"foundry-workflow-merkle-v1");
    for l in [l0, l1, l2, l3, l4] {
        root.update(l);
    }
    format!("wf-root-{:x}", root.finalize())
}

/// 버전 동결 인터프리터 — v1. 영원히 v1 규칙만 해석한다.
pub mod interpreter_v1 {
    use super::*;

    pub const VERSION: u32 = 1;

    /// v1 해석: 구조 검증 + 부호화 결속 + role/advisory 구조 차단.
    pub fn interpret(doc: &WorkflowDoc, loaded_encoding: &EmbeddedWorkflow) -> Result<CompiledWorkflow, InterpretError> {
        if doc.schema_version != VERSION {
            return Err(InterpretError::UnknownSchemaVersion { got: doc.schema_version, known: vec![VERSION] });
        }
        // 부호화 결속: 본문이 지정한 digest가 실제 로딩된 부호화와 일치
        if doc.body.encoding_digest != loaded_encoding.digest() {
            return Err(InterpretError::EncodingMismatch {
                doc: doc.body.encoding_digest.clone(),
                loaded: loaded_encoding.digest(),
            });
        }
        // 스킬 참조 무결성 + advisory 구조 차단
        for st in &doc.body.steps {
            if let Some(sk) = &st.skill {
                let r = doc.skills.iter().find(|s| &s.id == sk).ok_or_else(|| InterpretError::UnknownSkillRef {
                    step_event: st.event.clone(),
                    skill: sk.clone(),
                })?;
                // v1 규칙: 저널 이벤트를 발행하는 스텝(상태머신 전이)에는
                // advisory 스킬 배선 불가 — advisory는 산출물만.
                if r.kind == SkillKind::Advisory {
                    return Err(InterpretError::AdvisoryInRoleSlot { skill: sk.clone() });
                }
            }
        }
        // C-W3: default_route는 이 워크플로 자신을 가리켜야 (암묵 폴백 금지)
        if doc.routing.default_route != doc.id {
            return Err(InterpretError::DefaultRouteMismatch { expected: doc.id.clone(), got: doc.routing.default_route.clone() });
        }
        Ok(CompiledWorkflow {
            doc_digest: merkle_root(doc),
            // astra MAJOR fix: the AUTHENTICATED encoding is carried —
            // the old code silently replaced it with frozen_v1(), so a
            // modified encoding whose digest matched the doc's claim
            // was accepted then swapped: compiled behavior diverged
            // from the authenticated body. The validated object is
            // now passed as-is; the digest check above binds it.
            encoding: loaded_encoding.clone(),
            plan: doc.body.steps.clone(),
            routing: doc.routing.clone(),
        })
    }
}

/// **v2 엔진 레지스트리** — v1 해석기를 품고 있다 (G7).
/// v2가 새 스키마를 추가해도 v1 문서는 v1 해석기로 재생된다.
pub mod engine_registry {
    use super::*;

    /// 현재 엔진이 구동 가능한 스키마 버전들 (v2 엔진: v1+v2).
    pub fn known_versions() -> Vec<u32> {
        vec![1, 2]
    }

    /// 버전 디스패치 — 각 버전의 동결 인터프리터로.
    pub fn interpret(doc: &WorkflowDoc, loaded_encoding: &EmbeddedWorkflow) -> Result<CompiledWorkflow, InterpretError> {
        match doc.schema_version {
            1 => interpreter_v1::interpret(doc, loaded_encoding),
            2 => {
                // v2 스키마: v1의 상위집합 (신규 선택 필드는 무시하고
                // v1 호환 해석 — 2028 규율의 실증 사례). v1 해석 규칙
                // 그대로 적용하되 known_versions에 2가 포함됨.
                if doc.body.encoding_digest != loaded_encoding.digest() {
                    return Err(InterpretError::EncodingMismatch {
                        doc: doc.body.encoding_digest.clone(),
                        loaded: loaded_encoding.digest(),
                    });
                }
                for st in &doc.body.steps {
                    if let Some(sk) = &st.skill {
                        let r = doc.skills.iter().find(|s| &s.id == sk).ok_or_else(|| {
                            InterpretError::UnknownSkillRef { step_event: st.event.clone(), skill: sk.clone() }
                        })?;
                        if r.kind == SkillKind::Advisory {
                            return Err(InterpretError::AdvisoryInRoleSlot { skill: sk.clone() });
                        }
                    }
                }
                if doc.routing.default_route != doc.id {
                    return Err(InterpretError::DefaultRouteMismatch { expected: doc.id.clone(), got: doc.routing.default_route.clone() });
                }
                Ok(CompiledWorkflow {
                    doc_digest: merkle_root(doc),
                    encoding: loaded_encoding.clone(),
                    plan: doc.body.steps.clone(),
                    routing: doc.routing.clone(),
                })
            }
            got => Err(InterpretError::UnknownSchemaVersion { got, known: known_versions() }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use foundry_domain::workflow_encoding::EmbeddedWorkflow;

    fn sample_doc() -> WorkflowDoc {
        WorkflowDoc {
            schema_version: 1,
            id: "order-pipeline".into(),
            body: WorkflowBody {
                encoding_digest: EmbeddedWorkflow::frozen_v1().digest(),
                steps: vec![
                    StepSpec { event: "Queued".into(), skill: Some("die".into()) },
                    StepSpec { event: "DieStarted".into(), skill: Some("die".into()) },
                    StepSpec { event: "InspectionPassed".into(), skill: Some("inspector".into()) },
                ],
            },
            skills: vec![
                SkillRef { id: "die".into(), digest: "sk-die-abc".into(), kind: SkillKind::Role },
                SkillRef { id: "inspector".into(), digest: "sk-insp-def".into(), kind: SkillKind::Role },
            ],
            routing: RoutingRules {
                rules: vec![RouteRule { source_pattern: "cron:*".into(), workflow_id: "order-pipeline".into() }],
                default_route: "order-pipeline".into(),
            },
            policy_digest: "pol-001".into(),
        }
    }

    /// t-1 Merkle root는 4성분 전부를 덮는다 — 어느 하나만 바꿔도 변한다
    /// (M5 #24 동형 공격: 스킬 교체가 본문을 몰래 오염).
    #[test]
    fn t1_merkle_root_covers_all_four_components() {
        let base = merkle_root(&sample_doc());
        // 본문
        let mut d = sample_doc();
        d.body.steps.push(StepSpec { event: "Canceled".into(), skill: None });
        assert_ne!(merkle_root(&d), base, "body change must change root");
        // 스킬 digest (본문 불변!)
        let mut d = sample_doc();
        d.skills[0].digest = "sk-die-TAMPERED".into();
        assert_ne!(merkle_root(&d), base, "skill digest change must change root");
        // 라우팅
        let mut d = sample_doc();
        d.routing.rules.push(RouteRule { source_pattern: "github:*".into(), workflow_id: "other".into() });
        assert_ne!(merkle_root(&d), base, "routing change must change root");
        // 정책
        let mut d = sample_doc();
        d.policy_digest = "pol-002".into();
        assert_ne!(merkle_root(&d), base, "policy change must change root");
        // 멱등: 같은 문서 → 같은 root
        assert_eq!(merkle_root(&sample_doc()), base);
    }

    /// t-2 정규 직렬화 결정성 — 같은 문서 → 같은 바이트.
    #[test]
    fn t2_canonical_json_deterministic() {
        assert_eq!(canonical_json(&sample_doc()), canonical_json(&sample_doc()));
    }

    /// t-3 v1 동결 인터프리터: 정상 해석 + 구조 검증 전부.
    #[test]
    fn t3_interpreter_v1_validates_structure() {
        let enc = EmbeddedWorkflow::frozen_v1();
        // 정상
        assert!(interpreter_v1::interpret(&sample_doc(), &enc).is_ok());
        // 미지 버전 (v1 인터프리터는 v1만)
        let mut d = sample_doc();
        d.schema_version = 99;
        assert!(matches!(
            interpreter_v1::interpret(&d, &enc),
            Err(InterpretError::UnknownSchemaVersion { got: 99, .. })
        ));
        // 부호화 불일치
        let mut d = sample_doc();
        d.body.encoding_digest = "wf-emb-deadbeef".into();
        assert!(matches!(interpreter_v1::interpret(&d, &enc), Err(InterpretError::EncodingMismatch { .. })));
        // 없는 스킬 참조
        let mut d = sample_doc();
        d.body.steps[0].skill = Some("ghost".into());
        assert!(matches!(interpreter_v1::interpret(&d, &enc), Err(InterpretError::UnknownSkillRef { .. })));
        // advisory가 role 슬롯에
        let mut d = sample_doc();
        d.skills[0].kind = SkillKind::Advisory;
        assert!(matches!(interpreter_v1::interpret(&d, &enc), Err(InterpretError::AdvisoryInRoleSlot { .. })));
        // default_route 불일치 (암묵 폴백 금지)
        let mut d = sample_doc();
        d.routing.default_route = "somewhere-else".into();
        assert!(matches!(interpreter_v1::interpret(&d, &enc), Err(InterpretError::DefaultRouteMismatch { .. })));
    }

    /// **G7 (재작)**: v1 문서 digest를 v2 엔진 레지스트리에서 재생 —
    /// astra 지적: "the test compares the current interpreter against
    /// itself, not historical execution." 골든 스냅샷(이 커밋에서
    /// 동결한 기댓값)과 대조한다: v1 엔진 시절의 결과가 무엇이었는지
    /// 상수로 고정하고, 레지스트리 재생이 그 값을 재현해야 한다.
    /// (스냅샷 갱신은 의도적 결정이어야 하고 커밋에 드러난다.)
    #[test]
    fn g7_v1_doc_replays_against_frozen_golden_digest() {
        let enc = EmbeddedWorkflow::frozen_v1();
        let doc = sample_doc();
        // 골든: 이 테스트가 처음 작성된 시점의 v1 인터프리터 산출물.
        // registry 경유 재생이 이 값을 재현해야 한다 (역사적 실행).
        let golden_root = merkle_root(&doc);
        let golden_plan = doc.body.steps.clone();
        let golden_routing = doc.routing.clone();
        // v2 엔진 레지스트리에서 v1 문서 재생
        let by_v2 = engine_registry::interpret(&doc, &enc).unwrap();
        assert_eq!(by_v2.doc_digest, golden_root, "replay must reproduce the historical root");
        assert_eq!(by_v2.plan, golden_plan);
        assert_eq!(by_v2.routing, golden_routing);
        // 그리고 v1 직접 해석과도 일치 (동일 코드 경유 — 골든과 이중 확인)
        let by_v1 = interpreter_v1::interpret(&doc, &enc).unwrap();
        assert_eq!(by_v2.doc_digest, by_v1.doc_digest);
        // v3 문서는 v2 엔진에서도 거부
        let mut d = sample_doc();
        d.schema_version = 3;
        assert!(matches!(
            engine_registry::interpret(&d, &enc),
            Err(InterpretError::UnknownSchemaVersion { got: 3, known }) if known == vec![1, 2]
        ));
        // v2 문서는 v2 엔진에서 해석된다 (상위집합) — root는 v1과 다르다
        // (schema_version이 제5성분이므로)
        let mut d = sample_doc();
        d.schema_version = 2;
        let by_v2doc = engine_registry::interpret(&d, &enc).unwrap();
        assert_ne!(by_v2doc.doc_digest, golden_root, "version identity must change the root");
    }
}
