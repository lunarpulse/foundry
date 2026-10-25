//! T53 — 규칙 전용 라우터 (M6 플랜 v6, C-W3).
//!
//! **계약**:
//! * **규칙만 권위** — 매칭은 결정론적 규칙 평가뿐. 모델 자문은 v2 연기
//!   (M4 웹훅 교훈: 미인증 분류가 특권 경로를 선택하는 혼성대리 금지).
//! * **공유 검증 계약 (플랜 v6 T54-T53 seam)**: 라우터는 검증 규칙을
//!   재구현하지 않는다 — `validate_routing`이 유일한 검증 코드이고
//!   T54 검증기가 v3 문서 전체 검증에, T53 라우터가 실행 시 매칭에
//!   각각 **같은 함수**를 쓴다.
//! * **default_route 필수** — 암묵 폴백 금지. 매칭 실패는 오류가 아니라
//!   명시적 default 결정 (저널 1급 이벤트).
//! * **라우팅 결정의 완전한 증거** (G6): RoutingDecision은 입력 신호·
//!   매칭된 규칙(또는 default 사용 표식)·선택된 워크플로 id를 모두 담아
//!   저널 이벤트로 직렬화 가능하다 — 감사 재계산이 결정을 재현할 수 있다.
//! * **misroute 복구 reroute**: 승인-게이트 전이. `reroute`는 이미 진행
//!   중인 주문을 새 워크플로 digest에 결속 직전 상태에서 이동시키는
//!   명시적 조작이다 (C-W1의 무단 이주 금지 예외 — 인간 승인 필요).
//!
//! ## 멀티프리셋 경계 (M6 보강 B3 — 스케일 실증 74f94be 교훈)
//!
//! 각 프리셋의 routing 테이블은 **자기완결 우주**다: `default_route` 는
//! 반드시 자기 workflow id(검증기 `default_route_mismatch` 강제). 교차
//! 프리셋 default는 거부된다.
//!
//! 소스 패턴으로 **여러 프리셋 중 선택**하는 것은 이 라우터의 규칙
//! 매칭이 아니라, 각 프리셋이 자기 테이블에 서로 다른 소스 패턴을
//! 선언하는 방식으로 이루어진다(예: quickfix:*→quickloop, feature:*→
//! feature-prd). **플릿 수준의 통합 라우팅 테이블(패턴 우선순위 병합)은
//! 존재하지 않는다** — 그 계층은 M7 오케스트레이션 로그의 preset_bound
//! 선언이 소유한다(사이즈 라우팅: 인테이크 부트스트랩 → 인간 사이징
//! 게이트 → preset_bound → 발행).

#![allow(clippy::needless_borrows_for_generic_args)] // W6a clippy debt: preserve explicit generic-call borrow boundaries
#![allow(unused_imports)] // W6a clippy debt: retain the existing feature and test configuration import set

use crate::dsl::{RouteRule, RoutingRules};
use serde::{Deserialize, Serialize};

/// 라우팅 오류.
#[derive(Debug, Clone, PartialEq)]
pub enum RouteError {
    /// 규칙의 workflow_id가 후보 집합에 없다 (dangling 참조).
    UnknownWorkflowRef { workflow_id: String },
    /// 소스 패턴이 문법을 위반 (예: 닫히지 않은 괄호).
    BadPattern { pattern: String, reason: String },
    /// reroute가 터미널 주문을 이동시키려 함.
    RerouteTerminal { order_id: String },
    /// reroute에 인간 승인 토큰 없음 (C-W1: 이주는 승인 게이트 전이).
    RerouteUnauthorized { order_id: String },
}

/// **공유 검증 계약** — T54 검증기와 T53 라우터가 같이 쓰는 유일한
/// 라우팅 검증 (재구현 금지, 플랜 v6 seam).
///
/// 검증 내용:
/// * 각 규칙의 workflow_id가 후보 워크플로 id 집합에 존재.
/// * 소스 패턴 문법 타당 (와일드카드 `*`는 접두/접미 1회씩만).
/// * default_route가 후보 집합에 존재.
/// * (오류 아님) 규칙 우선순위: 목록 순서 — 첫 매칭 승리. 문서에 명시.
pub fn validate_routing(
    rules: &RoutingRules,
    known_workflows: &[String],
) -> Result<(), Vec<RouteError>> {
    let mut errors = Vec::new();
    for r in &rules.rules {
        if !known_workflows.contains(&r.workflow_id) {
            errors.push(RouteError::UnknownWorkflowRef {
                workflow_id: r.workflow_id.clone(),
            });
        }
        if let Err(reason) = check_pattern(&r.source_pattern) {
            errors.push(RouteError::BadPattern {
                pattern: r.source_pattern.clone(),
                reason,
            });
        }
    }
    if let Err(reason) = check_pattern(&default_source_of(rules)) {
        let _ = reason; // default는 패턴 아님 — 스킵
    }
    if !known_workflows.contains(&rules.default_route) {
        errors.push(RouteError::UnknownWorkflowRef {
            workflow_id: rules.default_route.clone(),
        });
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// 패턴 문법 — `prefix*`, `*suffix`, `정확히일치` 만 허용.
/// `*`는 최대 1개, 접두 또는 접미. 빈 패턴 거부. 내부 `*` 거부.
/// 검증기 공개 진입점 — T54/T53 공유 계약 (재구현 금지).
pub fn check_pattern_public(p: &str) -> Result<(), String> {
    check_pattern(p)
}

fn check_pattern(p: &str) -> Result<(), String> {
    if p.is_empty() {
        return Err("empty pattern".into());
    }
    let stars = p.matches('*').count();
    if stars > 1 {
        return Err("at most one '*' per pattern".into());
    }
    if stars == 1 && !p.ends_with('*') && !p.starts_with('*') {
        return Err("'*' must be a prefix or suffix wildcard".into());
    }
    Ok(())
}

fn default_source_of(_rules: &RoutingRules) -> String {
    String::new()
}

/// 패턴 매칭 — 검증된 패턴만 받는다 (실행 시 재검증 아님: 로드 시
/// validate_routing을 통과한 규칙만 라우터에 적재된다).
fn matches(pattern: &str, source: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('*') {
        source.starts_with(prefix)
    } else if let Some(suffix) = pattern.strip_prefix('*') {
        source.ends_with(suffix)
    } else {
        pattern == source
    }
}

/// **라우팅 결정** — G6 감사 증거의 전체 (직렬화 가능 = 저널 1급 이벤트).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingDecision {
    /// 라우팅된 입력 신호 (원천 식별 — "cron:5m", "github:pr#12" ...).
    pub source: String,
    /// 매칭된 규칙의 인덱스 (None = default 사용 — 명시적 기록).
    pub matched_rule: Option<usize>,
    /// 선택된 워크플로 id.
    pub workflow_id: String,
    /// 결정 시점의 규칙 테이블 digest (같은 테이블로 재현 가능함의 증거).
    pub rules_digest: String,
}

/// 규칙 테이블 digest — 결정 재현성 결속 (규칙이 바뀌면 다른 digest).
pub fn rules_digest(rules: &RoutingRules) -> String {
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    h.update(b"foundry-router-v1");
    for r in &rules.rules {
        h.update(&(r.source_pattern.len() as u64).to_be_bytes());
        h.update(r.source_pattern.as_bytes());
        h.update(&(r.workflow_id.len() as u64).to_be_bytes());
        h.update(r.workflow_id.as_bytes());
    }
    h.update(&(rules.default_route.len() as u64).to_be_bytes());
    h.update(rules.default_route.as_bytes());
    format!("route-{:x}", h.finalize())
}

/// **라우터** — 검증된 규칙 테이블만 적재 가능 (생성 시 validate 통과분).
pub struct Router {
    rules: RoutingRules,
    digest: String,
}

impl Router {
    /// 검증된 규칙만 받는다 — 검증 실패 테이블은 라우터가 존재할 수 없다.
    pub fn new(rules: RoutingRules, known_workflows: &[String]) -> Result<Self, Vec<RouteError>> {
        validate_routing(&rules, known_workflows)?;
        let digest = rules_digest(&rules);
        Ok(Self { rules, digest })
    }

    /// 라우팅 — 첫 매칭 규칙 승리, 없으면 명시적 default.
    /// 결정은 전부 증거를 담는다 (G6).
    pub fn route(&self, source: &str) -> RoutingDecision {
        for (i, r) in self.rules.rules.iter().enumerate() {
            if matches(&r.source_pattern, source) {
                return RoutingDecision {
                    source: source.into(),
                    matched_rule: Some(i),
                    workflow_id: r.workflow_id.clone(),
                    rules_digest: self.digest.clone(),
                };
            }
        }
        RoutingDecision {
            source: source.into(),
            matched_rule: None,
            workflow_id: self.rules.default_route.clone(),
            rules_digest: self.digest.clone(),
        }
    }

    pub fn rules_digest(&self) -> &str {
        &self.digest
    }

    /// **reroute — 승인-게이트 misroute 복구** (C-W3).
    /// 진행 중 주문을 새 워크플로로 이동. 요구: (i) 주문 비터미널
    /// (ii) 인간 승인 토큰 제시 (인가는 호출자 계층 — 여기선 요구 사실만).
    /// 저널 이벤트용 결정 구조 반환: 이동은 호출자(엔진 결속 계층)가 실행.
    pub fn reroute(
        &self,
        order_id: &str,
        order_terminal: bool,
        human_token_present: bool,
        new_workflow_id: &str,
    ) -> Result<ReroutePlan, RouteError> {
        if order_terminal {
            return Err(RouteError::RerouteTerminal {
                order_id: order_id.into(),
            });
        }
        if !human_token_present {
            return Err(RouteError::RerouteUnauthorized {
                order_id: order_id.into(),
            });
        }
        // 새 대상도 현재 테이블이 아는 워크플로여야 (dangling 방지).
        let known = self
            .rules
            .rules
            .iter()
            .any(|r| r.workflow_id == new_workflow_id)
            || self.rules.default_route == new_workflow_id;
        if !known {
            return Err(RouteError::UnknownWorkflowRef {
                workflow_id: new_workflow_id.into(),
            });
        }
        Ok(ReroutePlan {
            order_id: order_id.into(),
            new_workflow_id: new_workflow_id.into(),
            rules_digest: self.digest.clone(),
        })
    }
}

/// reroute 실행 계획 — 저널 1급 이벤트 (구 실행 종결 + 새 digest 결속은
/// 엔진의 G5 결속 규율이 소유).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReroutePlan {
    pub order_id: String,
    pub new_workflow_id: String,
    pub rules_digest: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::{RouteRule, RoutingRules};

    fn table() -> RoutingRules {
        RoutingRules {
            rules: vec![
                RouteRule {
                    source_pattern: "github:*".into(),
                    workflow_id: "wf-gh".into(),
                },
                RouteRule {
                    source_pattern: "cron:5m".into(),
                    workflow_id: "wf-cron5".into(),
                },
                RouteRule {
                    source_pattern: "*webhook".into(),
                    workflow_id: "wf-hook".into(),
                },
            ],
            default_route: "wf-default".into(),
        }
    }

    fn known() -> Vec<String> {
        vec![
            "wf-gh".into(),
            "wf-cron5".into(),
            "wf-hook".into(),
            "wf-default".into(),
        ]
    }

    // 검증 — dangling 참조·나쁜 패턴·나쁜 default 거부.
    #[test]
    fn validation_rejects_dangling_and_bad_patterns() {
        assert!(Router::new(table(), &known()).is_ok());
        // dangling 참조
        let mut t = table();
        t.rules[0].workflow_id = "wf-ghost".into();
        let errs = validate_routing(&t, &known()).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, RouteError::UnknownWorkflowRef { .. }))
        );
        // 내부 와일드카드
        let mut t = table();
        t.rules[1].source_pattern = "a*b*c".into();
        let errs = validate_routing(&t, &known()).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, RouteError::BadPattern { .. }))
        );
        // default도 dangling이면 거부
        let mut t = table();
        t.default_route = "wf-nowhere".into();
        assert!(Router::new(t, &known()).is_err());
        // 빈 패턴 거부
        let mut t = table();
        t.rules[2].source_pattern = String::new();
        assert!(Router::new(t, &known()).is_err());
    }

    // 매칭 — 접두/정확/접미 와일드카드 + 첫 매칭 승리 순서.
    #[test]
    fn matching_prefix_exact_suffix_first_wins() {
        let r = Router::new(table(), &known()).unwrap();
        assert_eq!(r.route("github:pr#12").workflow_id, "wf-gh");
        assert_eq!(r.route("cron:5m").workflow_id, "wf-cron5");
        assert_ne!(r.route("cron:1h").workflow_id, "wf-cron5"); // 정확 매칭 아님
        assert_eq!(r.route("svc/webhook").workflow_id, "wf-hook");
        // 미매칭 → 명시적 default (matched_rule=None — 암묵 폴백 아님)
        let d = r.route("slack:cmd");
        assert_eq!(d.workflow_id, "wf-default");
        assert_eq!(d.matched_rule, None);
        // 첫 매칭 승리: 두 규칙이 같은 소스에 걸리면 인덱스 순.
        let t = RoutingRules {
            rules: vec![
                RouteRule {
                    source_pattern: "x:*".into(),
                    workflow_id: "wf-first".into(),
                },
                RouteRule {
                    source_pattern: "x:y".into(),
                    workflow_id: "wf-second".into(),
                },
            ],
            default_route: "wf-default".into(),
        };
        let r2 = Router::new(t, &{
            let mut k = known();
            k.push("wf-first".into());
            k.push("wf-second".into());
            k
        })
        .unwrap();
        assert_eq!(r2.route("x:y").workflow_id, "wf-first");
    }

    // G6 — 결정의 완전한 증거: 같은 테이블에서 같은 입력은 같은 결정
    // (재현성), 테이블이 바뀌면 digest가 변해 구 결정과 구별된다.
    #[test]
    fn g6_decision_is_reproducible_and_table_bound() {
        let r = Router::new(table(), &known()).unwrap();
        let d1 = r.route("github:pr#9");
        let d2 = r.route("github:pr#9");
        assert_eq!(d1, d2, "same table + same source = identical decision");
        // 직렬화 가능 (저널 1급 이벤트).
        let js = serde_json::to_string(&d1).unwrap();
        assert!(js.contains("\"workflow_id\":\"wf-gh\""));
        assert!(js.contains("\"matched_rule\":0"));
        // 규칙 추가 → digest 변화 → 구 결정과 새 결정의 rules_digest 상이.
        let mut t2 = table();
        t2.rules.push(RouteRule {
            source_pattern: "slack:*".into(),
            workflow_id: "wf-slack".into(),
        });
        let mut k = known();
        k.push("wf-slack".into());
        let r2 = Router::new(t2, &k).unwrap();
        assert_ne!(r.rules_digest(), r2.rules_digest());
        assert_ne!(r.route("x").rules_digest, r2.route("x").rules_digest);
    }

    // reroute — 터미널 거부·무토큰 거부·dangling 거부·정상 계획.
    #[test]
    fn reroute_is_approval_gated() {
        let r = Router::new(table(), &known()).unwrap();
        assert!(matches!(
            r.reroute("o-1", true, true, "wf-gh"),
            Err(RouteError::RerouteTerminal { .. })
        ));
        assert!(matches!(
            r.reroute("o-1", false, false, "wf-gh"),
            Err(RouteError::RerouteUnauthorized { .. })
        ));
        assert!(matches!(
            r.reroute("o-1", false, true, "wf-ghost"),
            Err(RouteError::UnknownWorkflowRef { .. })
        ));
        let plan = r.reroute("o-1", false, true, "wf-hook").unwrap();
        assert_eq!(plan.new_workflow_id, "wf-hook");
        assert_eq!(plan.rules_digest, r.rules_digest());
        // 계획도 직렬화 가능 — 저널 이벤트.
        assert!(serde_json::to_string(&plan).unwrap().contains("wf-hook"));
    }
}
