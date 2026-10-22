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
#[serde(deny_unknown_fields)]
pub struct WorkflowDoc {
    pub schema_version: u32,
    pub id: String,
    /// 본문: 상태머신 엣지의 선언적 기술.
    pub body: WorkflowBody,
    /// 결속 스킬 (digest로만 참조 — 본문을 오염하지 않는다).
    pub skills: Vec<SkillRef>,
    /// 라우팅 규칙 (C-W3 — 규칙만 권위).
    pub routing: RoutingRules,
    /// 정책 아티팩트 digest (v1/v2 — 워크플로는 홀드를 요청할 수 있으나
    /// 면제할 수 없다 — C-W4; digest만 결속).
    pub policy_digest: String,
    /// **v3**: 내장 PolicyDoc (플랜 v6 — digest 참조가 아니라 내용이 곧
    /// 커밋먼트). v1/v2 문서는 None (직렬화에서 제외 — 레거시 root 불변).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<PolicyDoc>,
    /// **v4 (T29b)**: 결속 검사 팩 — `foundry-pack` 스냅샷의 manifest
    /// digest로만 참조 (팩 내용이 워크플로 본문을 오염하지 않는다).
    /// v1/v2/v3 문서는 None (직렬화 제외 — 레거시 root 불변). v4 문서는
    /// 필수 (인터프리터가 부재 시 구조 오류 거부).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_pack: Option<ReviewPackRef>,
}

/// **ReviewPackRef (v4, T29b)** — 검사 팩 결속 참조.
/// digest는 PackSnapshot의 canonical manifest digest (스펙 v0.71 §2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewPackRef {
    /// 결속 팩 digest — Merkle 리프 6의 성분.
    pub pack_digest: String,
}

/// **PolicyDoc (v3, T54)** — 승인 정책의 내용 스키마 (플랜 v6 C-W4a).
/// deny_unknown_fields — 불명 필드 조용히 버림 금지 (astra r2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyDoc {
    pub schema_version: u32,
    pub approvals: Vec<PolicyApproval>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyApproval {
    /// 대상 노드 (body.steps의 StepId — 검증기가 해석).
    pub node_ref: String,
    pub level: PolicyLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyLevel {
    /// 승인 게이트를 요청 (실행 시 approval_prepared 후 인간 날인 대기).
    Prepare,
    /// 승인 면제 — **금지 선언**: 표현 가능하나 검증기가 작성 시 오류 거부
    /// (G3의 공격 재료 — 존재 자체가 신호).
    Waive,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowBody {
    /// 이 문서가 구동할 상태머신 부호화의 digest (T51a frozen_v1 등).
    /// 엔진은 이 digest가 로딩된 부호화와 일치하는지 검증한다.
    pub encoding_digest: String,
    /// 이 워크플로가 수행하는 노드(엣지 패밀리) 순서 — 실행 계획.
    pub steps: Vec<StepSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepSpec {
    /// **StepId (v3 계약, 플랜 v6)**: 비어있지 않음·문서 내 유일.
    /// v1/v2 문서는 공백(직렬화 제외 — 레거시 root 불변).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    /// OrderEvent 패밀리 이름 ("Queued", "DieStarted", ...).
    pub event: String,
    /// 이 스텝에 배선된 스킬 (skills 내 id 참조 — digest 간접 참조).
    pub skill: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct RoutingRules {
    /// 명시적 규칙만 — 암묵 폴백 금지 (C-W3: default_route 필수).
    pub rules: Vec<RouteRule>,
    pub default_route: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteRule {
    /// 매칭 소스 ("cron:*", "github:*", ...).
    pub source_pattern: String,
    /// 이 규칙이 가리키는 워크플로 id.
    pub workflow_id: String,
}

/// **프리셋 로더 (T55)** — presets/ 디렉토리의 JSON 문서를 읽어
/// WorkflowDoc으로. encoding_digest는 문서의 선언값 대신 **로딩 시점의
/// frozen_v1 실측 digest**로 결속한다 (프리셋 파일이 부호화 진화에
/// 뒤처지지 않게 — digest는 문서 내용이 아니라 부호화 정체성의 참조).
pub fn load_preset(path: &std::path::Path) -> Result<WorkflowDoc, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("preset read failed: {e}"))?;
    load_preset_from_str(&raw)
}

/// 문자열 프리셋 로딩 (테스트·변조 프로브용 — 동일 결속 규율).
pub fn load_preset_from_str(raw: &str) -> Result<WorkflowDoc, String> {
    let mut doc: WorkflowDoc =
        serde_json::from_str(&raw).map_err(|e| format!("preset parse failed: {e}"))?;
    // deny_unknown_fields는 파싱에서 이미 시행됨 — 프리셋에 불명 필드가
    // 있으면 여기서 실패한다.
    // (astra T55 MAJOR) 치환은 무조건이 아니다: 빈 값 또는 센티넬
    // "enc-frozen-v1"만 템플릿으로 간주해 실측 digest로 결속한다.
    // **다른 모든 선언값은 정확히 일치해야** — 거짓 digest 선언을
    // 로더가 마스킹하지 않는다.
    let live = foundry_domain::workflow_encoding::EmbeddedWorkflow::frozen_v1().digest();
    let declared = doc.body.encoding_digest.trim().to_string();
    if declared.is_empty() || declared == "enc-frozen-v1" {
        doc.body.encoding_digest = live;
    } else if declared != live {
        return Err(format!(
            "preset declares encoding_digest '{declared}' but the live frozen_v1 digest is '{live}' — refusing to mask a mismatched declaration"
        ));
    }
    Ok(doc)
}

/// 해석 결과 — 실행 가능 형태.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledWorkflow {
    pub doc_digest: String,
    pub encoding: EmbeddedWorkflow,
    pub plan: Vec<StepSpec>,
    pub routing: RoutingRules,
    /// v3 문서의 내장 정책 (v1/v2는 None).
    pub policy: Option<PolicyDoc>,
    /// v4 문서의 결속 검사 팩 digest (v1/v2/v3는 None).
    pub review_pack: Option<String>,
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
    /// v4 문서에 review_pack 결속이 없다 (구조 오류).
    MissingReviewPack,
    /// 디스패치 직전 재계산한 팩 digest가 결속값과 불일치 — 변조.
    PackTampered { bound: String, live: String },
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
    // v3 (플랜 v6): 리프 5 = 내장 PolicyDoc의 canonical 직렬화.
    // v1/v2: 기존 policy_digest 문자열 — 레거시 root 불변.
    // astra T54 MAJOR: 리프 선택은 **schema_version 기준** (Option 존재
    // 유무가 아님) — v1/v2 문서에 policy를 주입해도 리프는 policy_digest로
    // 고정된다. v3에 policy가 없으면(구조 오류) 직렬화로는 불가능하지만
    // 방어적으로 빈 문자열 → root가 policy_digest=None과 구별됨.
    let policy_s = match doc.schema_version {
        3 => doc
            .policy
            .as_ref()
            .map(|p| serde_json::to_string(p).unwrap())
            .unwrap_or_default(),
        _ => doc.policy_digest.clone(),
    };
    let l4 = leaf("policy", &policy_s);
    // v4 (T29b): 리프 6 = 결속 검사 팩 digest. **리프 수 자체가
    // schema_version으로 결정** — v1~v3는 5리프 그대로 (레거시 root
    // 바이트 불변), v4만 6리프 (동일 pack_digest여도 버전이 다르면
    // root가 다름 — 리프 0가 이미 버전을 덮음).
    let mut root = sha2::Sha256::new();
    root.update(b"foundry-workflow-merkle-v1");
    for l in [l0, l1, l2, l3, l4] {
        root.update(l);
    }
    if doc.schema_version >= 4 {
        let pack_s = doc
            .review_pack
            .as_ref()
            .map(|r| r.pack_digest.clone())
            .unwrap_or_default();
        let l5 = leaf("review_pack", &pack_s);
        root.update(l5);
    }
    format!("wf-root-{:x}", root.finalize())
}

/// 버전 동결 인터프리터 — v1. 영원히 v1 규칙만 해석한다.
pub mod interpreter_v1 {
    use super::*;

    pub const VERSION: u32 = 1;

    /// v1 해석: 구조 검증 + 부호화 결속 + role/advisory 구조 차단.
    pub fn interpret(
        doc: &WorkflowDoc,
        loaded_encoding: &EmbeddedWorkflow,
    ) -> Result<CompiledWorkflow, InterpretError> {
        if doc.schema_version != VERSION {
            return Err(InterpretError::UnknownSchemaVersion {
                got: doc.schema_version,
                known: vec![VERSION],
            });
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
                let r = doc.skills.iter().find(|s| &s.id == sk).ok_or_else(|| {
                    InterpretError::UnknownSkillRef {
                        step_event: st.event.clone(),
                        skill: sk.clone(),
                    }
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
            return Err(InterpretError::DefaultRouteMismatch {
                expected: doc.id.clone(),
                got: doc.routing.default_route.clone(),
            });
        }
        Ok(CompiledWorkflow {
            doc_digest: merkle_root(doc),
            policy: None,
            review_pack: None,
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

    /// 현재 엔진이 구동 가능한 스키마 버전들 (T54 이후: v1+v2+v3).
    pub fn known_versions() -> Vec<u32> {
        vec![1, 2, 3, 4]
    }

    /// 버전 디스패치 — 각 버전의 동결 인터프리터로.
    pub fn interpret(
        doc: &WorkflowDoc,
        loaded_encoding: &EmbeddedWorkflow,
    ) -> Result<CompiledWorkflow, InterpretError> {
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
                            InterpretError::UnknownSkillRef {
                                step_event: st.event.clone(),
                                skill: sk.clone(),
                            }
                        })?;
                        if r.kind == SkillKind::Advisory {
                            return Err(InterpretError::AdvisoryInRoleSlot { skill: sk.clone() });
                        }
                    }
                }
                if doc.routing.default_route != doc.id {
                    return Err(InterpretError::DefaultRouteMismatch {
                        expected: doc.id.clone(),
                        got: doc.routing.default_route.clone(),
                    });
                }
                Ok(CompiledWorkflow {
                    doc_digest: merkle_root(doc),
                    encoding: loaded_encoding.clone(),
                    plan: doc.body.steps.clone(),
                    routing: doc.routing.clone(),
                    policy: None,
                    review_pack: None,
                })
            }
            // v3 (T54, 플랜 v6): v1 구조 검증 + 내장 정책 전달.
            // 정책의 의미 검증(Waive/Prepare)은 작성 시 검증기가 소유 —
            // 인터프리터는 구조만 (동결 시점의 v3 규칙).
            3 => {
                if doc.body.encoding_digest != loaded_encoding.digest() {
                    return Err(InterpretError::EncodingMismatch {
                        doc: doc.body.encoding_digest.clone(),
                        loaded: loaded_encoding.digest(),
                    });
                }
                for st in &doc.body.steps {
                    if let Some(sk) = &st.skill {
                        let r = doc.skills.iter().find(|s| &s.id == sk).ok_or_else(|| {
                            InterpretError::UnknownSkillRef {
                                step_event: st.event.clone(),
                                skill: sk.clone(),
                            }
                        })?;
                        if r.kind == SkillKind::Advisory {
                            return Err(InterpretError::AdvisoryInRoleSlot { skill: sk.clone() });
                        }
                    }
                }
                if doc.routing.default_route != doc.id {
                    return Err(InterpretError::DefaultRouteMismatch {
                        expected: doc.id.clone(),
                        got: doc.routing.default_route.clone(),
                    });
                }
                Ok(CompiledWorkflow {
                    doc_digest: merkle_root(doc),
                    encoding: loaded_encoding.clone(),
                    plan: doc.body.steps.clone(),
                    routing: doc.routing.clone(),
                    policy: doc.policy.clone(),
                    review_pack: None,
                })
            }
            // v4 (T29b): v3 규칙 + 결속 검사 팩 필수·전달.
            4 => {
                if doc.body.encoding_digest != loaded_encoding.digest() {
                    return Err(InterpretError::EncodingMismatch {
                        doc: doc.body.encoding_digest.clone(),
                        loaded: loaded_encoding.digest(),
                    });
                }
                let pack_digest = doc
                    .review_pack
                    .as_ref()
                    .map(|r| r.pack_digest.clone())
                    .ok_or(InterpretError::MissingReviewPack)?;
                for st in &doc.body.steps {
                    if let Some(sk) = &st.skill {
                        let r = doc.skills.iter().find(|s| &s.id == sk).ok_or_else(|| {
                            InterpretError::UnknownSkillRef {
                                step_event: st.event.clone(),
                                skill: sk.clone(),
                            }
                        })?;
                        if r.kind == SkillKind::Advisory {
                            return Err(InterpretError::AdvisoryInRoleSlot { skill: sk.clone() });
                        }
                    }
                }
                if doc.routing.default_route != doc.id {
                    return Err(InterpretError::DefaultRouteMismatch {
                        expected: doc.id.clone(),
                        got: doc.routing.default_route.clone(),
                    });
                }
                Ok(CompiledWorkflow {
                    doc_digest: merkle_root(doc),
                    encoding: loaded_encoding.clone(),
                    plan: doc.body.steps.clone(),
                    routing: doc.routing.clone(),
                    policy: doc.policy.clone(),
                    review_pack: Some(pack_digest),
                })
            }
            got => Err(InterpretError::UnknownSchemaVersion {
                got,
                known: known_versions(),
            }),
        }
    }
}

/// **디스패치 직전 변조 거부 (T29b)** — 결속 팩 digest를 현재 live
/// 스냅샷의 재계산 digest와 대조. 로더는 스냅샷 적재 시점의 digest를,
/// 이 함수는 디스패치 직전의 live digest를 받아 워크플로 결속값과
/// 비교한다 — 사이에 팩 파일이 바뀌면 거부.
pub fn verify_pack_binding(
    doc: &WorkflowDoc,
    live_pack_digest: &str,
) -> Result<(), InterpretError> {
    match &doc.review_pack {
        None => Err(InterpretError::MissingReviewPack),
        Some(r) if r.pack_digest == live_pack_digest => Ok(()),
        Some(r) => Err(InterpretError::PackTampered {
            bound: r.pack_digest.clone(),
            live: live_pack_digest.to_string(),
        }),
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
                    StepSpec {
                        id: String::new(),
                        event: "Queued".into(),
                        skill: Some("die".into()),
                    },
                    StepSpec {
                        id: String::new(),
                        event: "DieStarted".into(),
                        skill: Some("die".into()),
                    },
                    StepSpec {
                        id: String::new(),
                        event: "InspectionPassed".into(),
                        skill: Some("inspector".into()),
                    },
                ],
            },
            skills: vec![
                SkillRef {
                    id: "die".into(),
                    digest: "sk-die-abc".into(),
                    kind: SkillKind::Role,
                },
                SkillRef {
                    id: "inspector".into(),
                    digest: "sk-insp-def".into(),
                    kind: SkillKind::Role,
                },
            ],
            routing: RoutingRules {
                rules: vec![RouteRule {
                    source_pattern: "cron:*".into(),
                    workflow_id: "order-pipeline".into(),
                }],
                default_route: "order-pipeline".into(),
            },
            policy_digest: "pol-001".into(),
            policy: None,
            review_pack: None,
        }
    }

    /// t-1 Merkle root는 4성분 전부를 덮는다 — 어느 하나만 바꿔도 변한다
    /// (M5 #24 동형 공격: 스킬 교체가 본문을 몰래 오염).
    #[test]
    fn t1_merkle_root_covers_all_four_components() {
        let base = merkle_root(&sample_doc());
        // 본문
        let mut d = sample_doc();
        d.body.steps.push(StepSpec {
            id: String::new(),
            event: "Canceled".into(),
            skill: None,
        });
        assert_ne!(merkle_root(&d), base, "body change must change root");
        // 스킬 digest (본문 불변!)
        let mut d = sample_doc();
        d.skills[0].digest = "sk-die-TAMPERED".into();
        assert_ne!(
            merkle_root(&d),
            base,
            "skill digest change must change root"
        );
        // 라우팅
        let mut d = sample_doc();
        d.routing.rules.push(RouteRule {
            source_pattern: "github:*".into(),
            workflow_id: "other".into(),
        });
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
        assert!(matches!(
            interpreter_v1::interpret(&d, &enc),
            Err(InterpretError::EncodingMismatch { .. })
        ));
        // 없는 스킬 참조
        let mut d = sample_doc();
        d.body.steps[0].skill = Some("ghost".into());
        assert!(matches!(
            interpreter_v1::interpret(&d, &enc),
            Err(InterpretError::UnknownSkillRef { .. })
        ));
        // advisory가 role 슬롯에
        let mut d = sample_doc();
        d.skills[0].kind = SkillKind::Advisory;
        assert!(matches!(
            interpreter_v1::interpret(&d, &enc),
            Err(InterpretError::AdvisoryInRoleSlot { .. })
        ));
        // default_route 불일치 (암묵 폴백 금지)
        let mut d = sample_doc();
        d.routing.default_route = "somewhere-else".into();
        assert!(matches!(
            interpreter_v1::interpret(&d, &enc),
            Err(InterpretError::DefaultRouteMismatch { .. })
        ));
    }

    /// **G7 (r2 재작)**: 골든은 **리터럴 상수**다 — merkle_root()를 다시
    /// 호출해 기대값을 만드는 자기참조를 폐지했다 (astra: "hashing
    /// regressions change expected and actual together"). 이 상수는
    /// eae4c23 다음 커밋에서 실측으로 동결했다: registry 재생이 이
    /// 리터럴을 재현하지 못하면 해시 규율이 회귀한 것이다. 갱신은
    /// 의도적 결정이어야 하고 diff에 드러난다.
    #[test]
    fn g7_v1_doc_replays_against_frozen_golden_literal() {
        let enc = EmbeddedWorkflow::frozen_v1();
        let doc = sample_doc();
        // 동결된 골든 리터럴 (실측 기록 — 코드 재계산 아님)
        // T33 ADDENDUM 24/29/31: intentional pin update after embedded edge additions.
        const GOLDEN_V1_ROOT: &str =
            "wf-root-58023c9e74af689335440e3b3069b6d8bf3a18ec446ff581b6dce8a4df488afb";
        const GOLDEN_PLAN_LEN: usize = 3;
        const GOLDEN_FIRST_STEP_EVENT: &str = "Queued";
        const GOLDEN_DEFAULT_ROUTE: &str = "order-pipeline";
        const GOLDEN_ENCODING_DIGEST_PREFIX: &str = "wf-emb-";
        // v2 엔진 레지스트리에서 v1 문서 재생
        let by_v2 = engine_registry::interpret(&doc, &enc).unwrap();
        assert_eq!(
            by_v2.doc_digest, GOLDEN_V1_ROOT,
            "replay must reproduce the FROZEN literal root"
        );
        assert_eq!(by_v2.plan.len(), GOLDEN_PLAN_LEN);
        assert_eq!(by_v2.plan[0].event, GOLDEN_FIRST_STEP_EVENT);
        assert_eq!(by_v2.routing.default_route, GOLDEN_DEFAULT_ROUTE);
        assert!(
            by_v2
                .encoding
                .digest()
                .starts_with(GOLDEN_ENCODING_DIGEST_PREFIX)
        );
        // v1 직접 해석과도 일치
        let by_v1 = interpreter_v1::interpret(&doc, &enc).unwrap();
        assert_eq!(by_v2.doc_digest, by_v1.doc_digest);
        // 미지 버전은 여전히 거부 — T29b가 v4 인터프리터를 추가하면서
        // 미지 프로브는 v5로 이동 (의도적·가시적 갱신).
        let mut d = sample_doc();
        d.schema_version = 5;
        assert!(matches!(
            engine_registry::interpret(&d, &enc),
            Err(InterpretError::UnknownSchemaVersion { got: 5, known }) if known == vec![1, 2, 3, 4]
        ));
        // v2 문서 root는 v1과 다르다 (schema_version 제5성분)
        let mut d = sample_doc();
        d.schema_version = 2;
        let by_v2doc = engine_registry::interpret(&d, &enc).unwrap();
        assert_ne!(
            by_v2doc.doc_digest, GOLDEN_V1_ROOT,
            "version identity must change the root"
        );
    }
}

/// **T29b v4 게이트 시험** — 리프 6 재현·v1/v3 골든 root 불변·
/// 직전 변조 거부·advisory 배선 거부·팩 부재 거부.
#[cfg(test)]
mod t29b_tests {
    use super::*;
    use foundry_domain::workflow_encoding::EmbeddedWorkflow;

    fn v4_doc() -> WorkflowDoc {
        WorkflowDoc {
            schema_version: 4,
            id: "m8-review".into(),
            body: WorkflowBody {
                encoding_digest: EmbeddedWorkflow::frozen_v1().digest(),
                steps: vec![StepSpec {
                    id: "s1".into(),
                    event: "Queued".into(),
                    skill: Some("lens".into()),
                }],
            },
            skills: vec![SkillRef {
                id: "lens".into(),
                digest: "sk-x".into(),
                kind: SkillKind::Role,
            }],
            routing: RoutingRules {
                rules: vec![],
                default_route: "m8-review".into(),
            },
            policy_digest: String::new(),
            policy: None,
            review_pack: Some(ReviewPackRef {
                pack_digest: "pack-abc".into(),
            }),
        }
    }

    /// 게이트 1a: v4 리프 6은 pack_digest를 덮는다 — 팩이 바뀌면 root 변화.
    #[test]
    fn v4_leaf_covers_review_pack() {
        let d = v4_doc();
        let r1 = merkle_root(&d);
        let mut d2 = d.clone();
        d2.review_pack = Some(ReviewPackRef {
            pack_digest: "pack-tampered".into(),
        });
        assert_ne!(r1, merkle_root(&d2), "review-pack leaf must be covered");
    }

    /// 게이트 1b: v1/v3 골든 root 불변 — 필드 추가·v4 코드 경로가
    /// 레거시 root 바이트를 건드리지 않음 (변경 전 실측 핀).
    #[test]
    fn legacy_roots_unchanged() {
        let d3 = WorkflowDoc {
            schema_version: 3,
            id: "m8-review".into(),
            body: WorkflowBody {
                encoding_digest: EmbeddedWorkflow::frozen_v1().digest(),
                steps: vec![StepSpec {
                    id: "s1".into(),
                    event: "Queued".into(),
                    skill: Some("lens".into()),
                }],
            },
            skills: vec![SkillRef {
                id: "lens".into(),
                digest: "sk-x".into(),
                kind: SkillKind::Role,
            }],
            routing: RoutingRules {
                rules: vec![],
                default_route: "m8-review".into(),
            },
            policy_digest: String::new(),
            policy: Some(PolicyDoc {
                schema_version: 1,
                approvals: vec![PolicyApproval {
                    node_ref: "s1".into(),
                    level: PolicyLevel::Prepare,
                }],
            }),
            review_pack: Some(ReviewPackRef {
                pack_digest: "pack-abc".into(),
            }),
        };
        // v3 pin — T33 ADDENDUM 24/29/31 내장 엣지 추가로 인한 의도적 갱신
        // (review_pack 주입에도 불변:
        // 리프 6은 schema_version 4 미만에서는 root에 기여하지 않음).
        assert_eq!(
            merkle_root(&d3),
            "wf-root-cf0ef4a3e8df3d00f8f36c1a6cb9be3421eda12cc28e2a7525230826e37541ea"
        );
        let mut d1 = d3;
        d1.schema_version = 1;
        d1.policy = None;
        d1.policy_digest = "pol-001".into();
        assert_eq!(
            merkle_root(&d1),
            // T33 ADDENDUM 24/29/31 내장 엣지 추가로 인한 의도적 갱신.
            "wf-root-08f12ea36c2f31fb9333dc5ff0a7f68e3cf591780a8c0a79e66103168900447b"
        );
    }

    /// 게이트 1c: 디스패치 직전 변조 거부 — 결속 digest ≠ live 재계산.
    #[test]
    fn pre_dispatch_tamper_rejected() {
        let d = v4_doc();
        assert!(verify_pack_binding(&d, "pack-abc").is_ok());
        match verify_pack_binding(&d, "pack-changed") {
            Err(InterpretError::PackTampered { bound, live }) => {
                assert_eq!(bound, "pack-abc");
                assert_eq!(live, "pack-changed");
            }
            other => panic!("expected PackTampered, got {other:?}"),
        }
        let mut bare = d;
        bare.review_pack = None;
        assert!(matches!(
            verify_pack_binding(&bare, "pack-abc"),
            Err(InterpretError::MissingReviewPack)
        ));
    }

    /// 게이트 1d: v4 해석 — 팩 부재 거부·정상 문서는 팩 digest 전달.
    #[test]
    fn v4_interpreter_binds_pack() {
        let enc = EmbeddedWorkflow::frozen_v1();
        let mut d = v4_doc();
        d.review_pack = None;
        assert!(matches!(
            engine_registry::interpret(&d, &enc),
            Err(InterpretError::MissingReviewPack)
        ));
        let ok = engine_registry::interpret(&v4_doc(), &enc).unwrap();
        assert_eq!(ok.review_pack.as_deref(), Some("pack-abc"));
    }

    /// 게이트 1e: advisory는 v4에서도 role 슬롯 배선 불가.
    #[test]
    fn v4_advisory_wiring_rejected() {
        let enc = EmbeddedWorkflow::frozen_v1();
        let mut d = v4_doc();
        d.skills[0].kind = SkillKind::Advisory;
        assert!(matches!(
            engine_registry::interpret(&d, &enc),
            Err(InterpretError::AdvisoryInRoleSlot { .. })
        ));
    }

    /// 게이트 1f: v4 문서 직렬화는 review_pack 포함, v3는 제외
    /// (레거시 canonical 바이트 불변).
    #[test]
    fn v4_serialization_contract() {
        let s4 = canonical_json(&v4_doc());
        assert!(
            s4.contains("review_pack"),
            "v4 must serialize the binding: {s4}"
        );
        let mut d3 = v4_doc();
        d3.schema_version = 3;
        d3.review_pack = None;
        assert!(!canonical_json(&d3).contains("review_pack"));
    }
}
