//! T59 — 체이너 + 재디에이션 제안자 (M7 플랜 v9 C′/H/I절). astra r1 재작.
//!
//! **내구 재생 설계**(r1 BLOCKER 폐쇄): Chainer 는 어떤 제안 상태도
//! 메모리에 소유하지 않는다 — 모든 제안/확정/재디에이션 연결은
//! 오케스트레이션 로그 행에서 **재생(replay)** 된다. 같은 로그를 연
//! 어떤 Chainer 인스턴스도 동일한 상태를 본다(크래시 창 멱등).
//!
//! - **제안/발급 분리**: 체이너는 핸드오프 승인을 **제안만** — 발급
//!   권한은 인간 게이트(원장 그랜트)에 있다.
//! - **전이 전 3자 일치(원장 앵커)**: 확정은 ①로그 HandoffBound 행에서
//!   재계산한 "fh1:" 매니페스트 ②제시 매니페스트 ③**원장 그랜트 스코프의
//!   artifact_digest** — 셋이 일치할 때만 성립. 발급자 앵커: 그랜트
//!   레코드의 token_id (체이너이 자기 저장 문자열과 비교하지 않는다).
//! - **위조 발급자 거부**: 확정 지목 grant 의 token_id 가 제안 행에
//!   기록된 issuer_token 과 불일치 → 거부.
//! - **바인딩 준수 발행(H절)**: unsized-spawn 거부, 부트스트랩(인테이크
//!   소스 패턴)만 예외 — 불리언이 아니라 소스 패턴 검사. 거부된 사이징
//!   후 발행 거부. 승인 digest ↔ preset_bound 일치 + **목적지
//!   plan/root 도 preset_bound 와 일치**해야 한다.
//! - **재디에이션(I절)**: 제안만·거부당 1회(로그 행 연결로 멱등)·깊이 1
//!   (재디에이션 작업 주문 자신의 거부는 제안 불가 — 부모 연결 검사).
//! - **재입장 = 전량 재평가**: sizing/바인딩/선언/승인 상태를 현재
//!   로그·원장 기준으로 전부 재검사 — A 수리가 B 를 우회하지 않는다.

use crate::refusal::{RefusalCode, RemediationKind};
use crate::{OrchestrationKind, OrchestrationLog, OrchRow};
use foundry_approval::{ApprovalLedger, GrantState};

/// 체이너 오류.
#[derive(Debug, PartialEq)]
pub enum ChainerError {
    /// unsized — preset_bound 없는 발행.
    Unsized { order: String },
    /// digest 불일치.
    DigestMismatch { order: String, expected: String, got: String },
    /// 목적지 plan/root 불일치 — preset_bound.spawn_plan/selected_root 와.
    DestinationMismatch { order: String, expected: String, got: String },
    /// 사이징이 거부됨(Rejected) — 같은 사이징 주문으로 발행 불가.
    SizingRejected { order: String },
    /// (astra r3) 재입장 선언 재검사 실패 — 전용 사유(Unsized 와 구분).
    DeclarationMissing { order: String },
    /// 부트스트랩 위반 — 인테이크 소스 패턴 아닌 주문의 예외 주장.
    BootstrapViolation { order: String },
    /// 미제안 확정 시도.
    NotProposed { proposal_id: String },
    /// 3자 불일치(로그 재계산·제시·원장).
    ThreeWayMismatch { proposal_id: String },
    /// 위조 발급자 — 확정 지목 그랜트의 token_id 불일치.
    IssuerMismatch { proposal_id: String },
    /// 재디에이션 깊이 초과(깊이 1) — 부재 행·비거부 행·재디에이션
    /// 작업 자신의 거부.
    RemediationDepth { refusal_seq: i64 },
    /// 재디에이션 제안 중복(거부당 1회).
    RemediationAlreadyProposed { refusal_seq: i64 },
    /// 원장 조회 실패(그랜트 부재 포함).
    Ledger(String),
    /// 로그 오류 전파.
    Log(String),
}

/// 사이징 승인 상태 — 원장 CONSUMED 증거가 권위.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum SizingVerdict {
    Approved,
    Rejected,
}

/// 인테이크 부트스트랩 소스 패턴 (H절 — 최초 사이징 주문만).
pub const INTAKE_SOURCE_PATTERN: &str = "intake:";

/// (T60) 사이징 digest 규약 — "szv1:<outcome>:<root>:<plan>".
/// 목적지·판정이 승인의 결속 대상(플랜 v9: 프리셋 선택 = 해시 결속).
/// 라이브 소비(scope_for 4필드 스코프)와 양립하는 유일한 인코딩 자리.
pub fn sizing_digest(outcome: &str, selected_root: &str, spawn_plan: &str) -> String {
    format!("szv1:{outcome}:{selected_root}:{spawn_plan}")
}

fn parse_sizing_digest(digest: &str) -> Option<(String, String, String)> {
    let rest = digest.strip_prefix("szv1:")?;
    let mut it = rest.splitn(3, ':');
    let outcome = it.next()?.to_string();
    let root = it.next()?.to_string();
    let plan = it.next()?.to_string();
    if outcome.is_empty() || root.is_empty() || plan.is_empty() {
        return None;
    }
    Some((outcome, root, plan))
}

fn is_intake_bootstrap(order_source: &str) -> bool {
    order_source.starts_with(INTAKE_SOURCE_PATTERN)
}

/// 로그에서 제안 상태 재생 — HandoffBound 행 순회. proposal_id 는
/// 행의 manifest 에서 파생 불가하므로(행에는 id 없음) 제안 id 를
/// **행 식별자로 인코딩**: manifest 필드에 proposal_id 를
/// prefix 로 결속한다(아래 PROPOSE 결속 참조).
pub struct Chainer<'o, 'a> {
    log: &'o OrchestrationLog,
    ledger: &'a ApprovalLedger,
}

/// 제안의 내구 표현 — HandoffBound 행 + issuer 결속. issuer 결속은
/// artifact_digest 필드에 **len-prefix 구조화 인코딩**으로 저장(체인
/// 해시에 포함 — 변조 감지). 고정폭 16진 len-prefix 로 필드 경계가
/// 항상 유일 — 구분자 주입("|tok:" 등을 포함한 id)으로 identity 를
/// 바꾸는 공격이 원천 불가능하다 (astra r2 MAJOR).
const PROPOSAL_ENCODING_LABEL: &str = "fp1";

/// 인코딩 필드 상한 — 4자리 hex len-prefix 의 표현 한계.
const PROPOSAL_FIELD_CAP: usize = 0xFFFF;

fn encode_proposal_digest(proposal_id: &str, issuer_token: &str, artifact: &str) -> Result<String, ChainerError> {
    // (astra r3) 4자리 초과 필드는 인코딩 불가 — append 전 거부(재생에서
    // 침묵 누락되는 사고 방지).
    for (label, part) in [("proposal_id", proposal_id), ("issuer", issuer_token), ("artifact", artifact)] {
        if part.len() > PROPOSAL_FIELD_CAP {
            return Err(ChainerError::Log(format!(
                "{label} exceeds {PROPOSAL_FIELD_CAP} bytes"
            )));
        }
    }
    let mut out = format!("{PROPOSAL_ENCODING_LABEL}:").into_bytes();
    for part in [proposal_id, issuer_token, artifact] {
        out.extend_from_slice(format!("{:04x}:", part.len()).as_bytes());
        out.extend_from_slice(part.as_bytes());
    }
    Ok(String::from_utf8(out).expect("hex prefix + utf8 parts"))
}

fn decode_proposal_digest(encoded: &str) -> Option<(String, String, String)> {
    let mut rest = encoded.strip_prefix(format!("{PROPOSAL_ENCODING_LABEL}:").as_str())?.as_bytes();
    let mut parts = Vec::new();
    for _ in 0..3 {
        // 4 hex + ':'.
        if rest.len() < 5 {
            return None;
        }
        let len = usize::from_str_radix(std::str::from_utf8(&rest[..4]).ok()?, 16).ok()?;
        if rest[4] != b':' || rest.len() < 5 + len {
            return None;
        }
        parts.push(String::from_utf8(rest[5..5 + len].to_vec()).ok()?);
        rest = &rest[5 + len..];
    }
    if !rest.is_empty() {
        return None;
    }
    let mut it = parts.into_iter();
    let pid = it.next()?;
    let tok = it.next()?;
    let dig = it.next()?;
    Some((pid, tok, dig))
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProposalView {
    pub proposal_id: String,
    pub manifest: String,
    pub log_seq: i64,
    pub issuer_token: String,
    pub artifact_digest: String,
}

impl<'o, 'a> Chainer<'o, 'a> {
    pub fn new(log: &'o OrchestrationLog, ledger: &'a ApprovalLedger) -> Self {
        Self { log, ledger }
    }

    /// 로그에서 제안 전체 재생 (크래시 창 멱등의 근거).
    fn replay_proposals(&self) -> Result<Vec<ProposalView>, ChainerError> {
        // (astra r2) 경합으로 동일 proposal_id 행이 2개 append 돼도
        // 재생은 seq 순 첫 행만 취급 — 멱등 유지(나중 행은 사본).
        let mut seen = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        for row in self.log.read_all().map_err(|e| ChainerError::Log(format!("{e:?}")))? {
            if let OrchestrationKind::HandoffBound { artifact_digest, source_order: s0, dest_order: d0 } =
                &row.kind
            {
                if let Some((pid, tok, dig)) = decode_proposal_digest(artifact_digest) {
                    if !seen.insert(pid.clone()) {
                        continue; // 사본 행 — 첫 행이 권위.
                    }
                    let manifest = crate::handoff_manifest_digest(
                        s0,
                        d0,
                        artifact_digest,
                        row.seq,
                    )
                    .map_err(ChainerError::Log)?;
                    out.push(ProposalView {
                        proposal_id: pid,
                        manifest,
                        log_seq: row.seq,
                        issuer_token: tok,
                        artifact_digest: dig,
                    });
                }
            }
        }
        Ok(out)
    }

    /// 핸드오프 승인 **제안** — 로그 행 append 가 곧 내구 기록. 같은
    /// proposal_id 재제안은 기존 행 반환(멱등 — 재생으로 조회, 중복
    /// append 없음). 파라미터 불일치 재제안은 거부.
    pub fn propose_handoff(
        &self,
        proposal_id: &str,
        source_order_param: &str,
        dest_order_param: &str,
        artifact_digest: &str,
        issuer_token: &str,
    ) -> Result<ProposalView, ChainerError> {
        // 재생 우선 — 기존 제안이면 멱등 반환. digest 불일치는 거부
        // (astra r2: 재시도가 다른 경로를 덮지 못하게). issuer_token 은
        // 제안 식별자가 아니라 confirm 의 그랜트 앵커가 검증한다 —
        // 같은 핸드오프의 재발급 그랜트(다른 토큰)도 같은 제안을
        // 재사용한다(M7.1 재리뷰 r2: 정문 재시도 경로).
        for p in self.replay_proposals()? {
            if p.proposal_id == proposal_id {
                if p.artifact_digest != artifact_digest {
                    return Err(ChainerError::ThreeWayMismatch {
                        proposal_id: proposal_id.into(),
                    });
                }
                let row = self
                    .log
                    .row_at(p.log_seq)
                    .map_err(|e| ChainerError::Log(format!("{e:?}")))?;
                let OrchestrationKind::HandoffBound { source_order, dest_order, .. } = &row.kind
                else {
                    return Err(ChainerError::ThreeWayMismatch { proposal_id: proposal_id.into() });
                };
                if source_order != source_order_param || dest_order != dest_order_param {
                    return Err(ChainerError::ThreeWayMismatch {
                        proposal_id: proposal_id.into(),
                    });
                }
                return Ok(p);
            }
        }
        let encoded = encode_proposal_digest(proposal_id, issuer_token, artifact_digest)?;
        let _ = self
            .log
            .bind_handoff(source_order_param, dest_order_param, &encoded)
            .map_err(|e| ChainerError::Log(format!("{e:?}")))?;
        // (astra r3) append 후 **재생 재조회** — 경합에서 이 append 가
        // 루저(사본 행)면 승자(첫 행)를 반환한다. 호출자는 항상 권위 행을 본다.
        self.replay_proposals()?
            .into_iter()
            .find(|p| p.proposal_id == proposal_id)
            .ok_or(ChainerError::Log("proposal vanished after append".into()))
    }

    /// 핸드오프 **확정** — 전이 전 3자 일치 (원장 앵커):
    /// ①로그 행 재계산 매니페스트 ②제시 매니페스트 ③**원장 그랜트
    /// 스코프의 artifact_digest**. 발급자 앵커: 그랜트 token_id.
    /// 재확정 멱등: 이미 CONSUMED 그랜트면 기존 성립으로 반환.
    /// (M7.1 재리뷰 r2) 확정 **사전 검증** — 3자 일치+6필드 결속만
    /// (그랜트 상태 무관). 정문이 소비 전 위조를 차단하는 게이트.
    pub fn verify_handoff(
        &self,
        proposal_id: &str,
        grant_id: &str,
    ) -> Result<ProposalView, ChainerError> {
        self.confirm_handoff_inner(proposal_id, grant_id, true)
    }

    pub fn confirm_handoff(
        &self,
        proposal_id: &str,
        grant_id: &str,
    ) -> Result<ProposalView, ChainerError> {
        self.confirm_handoff_inner(proposal_id, grant_id, false)
    }

    fn confirm_handoff_inner(
        &self,
        proposal_id: &str,
        grant_id: &str,
        pre_verify_only: bool,
    ) -> Result<ProposalView, ChainerError> {
        let Some(p) = self
            .reply_proposal(proposal_id)?
        else {
            return Err(ChainerError::NotProposed { proposal_id: proposal_id.into() });
        };
        // (원장) 그랜트 조회 — 존재·상태·스코프.
        let grant = self
            .ledger
            .get_grant(grant_id)
            .map_err(|e| ChainerError::Ledger(format!("{e:?}")))?;
        // (M7.1 재리뷰 r2) 발급자 앵커 — 제안 행의 최초 토큰 대신
        // **매니페스트 결속**(아래 6필드 검증의 handoff_manifest)이
        // 그랜트를 이 제안·이 행에 잠근다: 같은 핸드오프의 재발급
        // 그랜트(다른 토큰)도 정당하며, 무관 그랜트는 매니페스트가
        // 맞지 않아 거부된다.
        // (astra r2 BLOCKER) 스토프 **전체** 결속 — digest 뿐 아니라
        // source/dest 주문·intent·root_scope 가 그랜트 스코프와 일치해야
        // 한다(같은 digest 로 무관 경로 승인 차단).
        let row = self
            .log
            .row_at(p.log_seq)
            .map_err(|e| ChainerError::Log(format!("{e:?}")))?;
        let OrchestrationKind::HandoffBound { source_order, dest_order, artifact_digest: encoded } =
            &row.kind
        else {
            return Err(ChainerError::ThreeWayMismatch { proposal_id: proposal_id.into() });
        };
        // (astra r4) 3자 완성 — 로그 행에서 매니페스트 **재계산** 후 그
        // 매니페스트가 그랜트 스코프에 결속돼 있는지까지 확인. 매니페스트는
        // 행 seq 를 포함하므로 제안별 유일 — 동일 스코프/발급자의 별개
        // 제안이 하나의 소비된 그랜트를 재사용할 수 없다.
        let recomputed = crate::handoff_manifest_digest(
            source_order,
            dest_order,
            encoded,
            row.seq,
        )
        .map_err(ChainerError::Log)?;
        if recomputed != p.manifest {
            return Err(ChainerError::ThreeWayMismatch { proposal_id: proposal_id.into() });
        }
        let scope: serde_json::Value =
            serde_json::from_str(&grant.scope_json).map_err(|e| ChainerError::Ledger(format!("{e:?}")))?;
        let scope_str = |k: &str| {
            scope.get(k).and_then(|v| v.as_str()).map(|s| s.to_string())
        };
        let pairs = [
            ("order_id", source_order.clone()),
            ("dest_order_id", dest_order.clone()),
            ("intent", "handoff".to_string()),
            ("artifact_digest", p.artifact_digest.clone()),
            // (astra r4) 매니페스트 결속 — 스코프가 지목한 승인이 바로
            // 이 제안(이 행)이다.
            ("handoff_manifest", recomputed),
            // (astra r3) 핸드오프 그랜트의 root 는 비어야.
            ("root_scope", String::new()),
        ];
        for (k, want) in pairs {
            match scope_str(k) {
                Some(got) if got == want => {}
                _ => {
                    return Err(ChainerError::ThreeWayMismatch {
                        proposal_id: proposal_id.into(),
                    })
                }
            }
        }
        // (astra r2 MAJOR) 승인 성립 증거 — **소비 완료(CONSUMED)** 만
        // 인간 게이트 통과. Pending 은 되돌릴 수 있고(reservation release),
        // Applying 은 확정 전이다. 회복이 APPLYING→CONSUMED 를 완결하므로
        // CONSUMED 가 유일한 내구 증거.
        match grant.state {
            GrantState::Consumed => Ok(p),
            GrantState::Issued | GrantState::Pending | GrantState::Applying => {
                if pre_verify_only {
                    Ok(p)
                } else {
                    Err(ChainerError::ThreeWayMismatch {
                        proposal_id: proposal_id.into(),
                    })
                }
            }
        }
    }

    fn reply_proposal(&self, proposal_id: &str) -> Result<Option<ProposalView>, ChainerError> {
        Ok(self
            .reply_proposals()?
            .into_iter()
            .find(|p| p.proposal_id == proposal_id))
    }

    fn reply_proposals(&self) -> Result<Vec<ProposalView>, ChainerError> {
        self.replay_proposals()
    }

    /// (astra r7) 사이징 승인 상태를 **원장에서 직접 산출** — 호출자
    /// 제시 Approved 는 신뢰하지 않는다(위조 차단). 사이징 그랜트 규약:
    /// id "sizing-<order>", 스코프 sizing_outcome("approved"/"rejected").
    ///   - CONSUMED + approved → Approved
    ///   - sizing_outcome=rejected → Rejected (상태 무관 — 인간 거부 판정)
    ///   - 그 외(미소비/부재) → None (미승인)
    fn sizing_verdict_from_ledger(
        &self,
        sizing_order: &str,
    ) -> Result<Option<SizingVerdict>, ChainerError> {
        let grant_id = format!("sizing-{sizing_order}");
        let Ok(grant) = self.ledger.get_grant(&grant_id) else {
            return Ok(None);
        };
        let scope: serde_json::Value = serde_json::from_str(&grant.scope_json)
            .map_err(|e| ChainerError::Ledger(format!("{e:?}")))?;
        // (T60 astra r1) digest 사전이미지에서 판정 파생 — 승인은
        // dispatch_live 를 통해서만 소비되고, 그 스코프는 scope_for 의
        // 4필드와 정확 일치해야 한다. 따라서 목적지·판정은 digest 안에
        // 인코딩된다(플랜 v9: "프리셋 선택 자체가 해시 결속 대상").
        let digest = scope
            .get("artifact_digest")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let Some((outcome, _root, _plan)) = parse_sizing_digest(digest) else {
            return Ok(None);
        };
        match outcome.as_str() {
            "rejected" => Ok(Some(SizingVerdict::Rejected)),
            "approved" if grant.state == foundry_approval::GrantState::Consumed => {
                Ok(Some(SizingVerdict::Approved))
            }
            _ => Ok(None),
        }
    }

    /// (astra r8) preset_bound 의 **유일한 작성 경로** — 원장 CONSUMED
    /// 증거 검증 후 멱등 append(이미 같은 sizing_order 결속이 있으면
    /// 재append 없이 Ok). 인간 사이징 게이트 통과 산출물만 결속된다.
    pub fn bind_preset_from_ledger(
        &self,
        sizing_order: &str,
        selected_root: &str,
        spawn_plan: &str,
    ) -> Result<(), ChainerError> {
        // 원장에서 승인 산출 — Approved 여야.
        match self.sizing_verdict_from_ledger(sizing_order)? {
            Some(SizingVerdict::Approved) => {}
            _ => return Err(ChainerError::SizingRejected { order: sizing_order.into() }),
        }
        // 승인 digest — 그랜트 스코프의 artifact_digest. (T60 astra r1)
        // 목적지는 **digest 사전이미지**에 인코딩 — 라이브 소비 경로
        // (scope_for 4필드)와 양립하면서 선택이 해시 결속된다. 호출자
        // 제시 root/plan 은 digest 파생값과 일치해야(리다이렉션 차단,
        // astra r11 계약 승계).
        let grant = self
            .ledger
            .get_grant(&format!("sizing-{sizing_order}"))
            .map_err(|e| ChainerError::Ledger(format!("{e:?}")))?;
        let scope: serde_json::Value = serde_json::from_str(&grant.scope_json)
            .map_err(|e| ChainerError::Ledger(format!("{e:?}")))?;
        let digest = scope
            .get("artifact_digest")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ChainerError::SizingRejected { order: sizing_order.into() })?
            .to_string();
        let Some((_outcome, dig_root, dig_plan)) = parse_sizing_digest(&digest) else {
            return Err(ChainerError::SizingRejected { order: sizing_order.into() });
        };
        if dig_root != selected_root {
            return Err(ChainerError::DestinationMismatch {
                order: sizing_order.into(),
                expected: dig_root,
                got: selected_root.into(),
            });
        }
        if dig_plan != spawn_plan {
            return Err(ChainerError::DestinationMismatch {
                order: sizing_order.into(),
                expected: dig_plan,
                got: spawn_plan.into(),
            });
        }
        // 멱등 — 같은 sizing_order 결속이 있으면 재append 없음.
        let rows = self
            .log
            .read_all()
            .map_err(|e| ChainerError::Log(format!("{e:?}")))?;
        let already = rows.iter().any(|r| match &r.kind {
            OrchestrationKind::PresetBound { sizing_order: s0, .. } => s0 == sizing_order,
            _ => false,
        });
        if !already {
            self.log
                .append_preset_bound(sizing_order, selected_root, &digest, spawn_plan)
                .map_err(|e| ChainerError::Log(format!("{e:?}")))?;
        }
        Ok(())
    }

    /// 바인딩 준수 발행 검사 (H절) — 승인 상태는 원장에서 산출.
    pub fn check_spawn_binding(
        &self,
        order_id: &str,
        order_source: &str,
        sizing_order: Option<&str>,
        approval_digest: Option<&str>,
        dest_plan: &str,
        dest_root: &str,
    ) -> Result<(), ChainerError> {
        match sizing_order {
            None => {
                // 부트스트랩 — 인테이크 소스 패턴만.
                if is_intake_bootstrap(order_source) {
                    Ok(())
                } else {
                    Err(ChainerError::BootstrapViolation { order: order_id.into() })
                }
            }
            Some(s) => match self.sizing_verdict_from_ledger(s)? {
                Some(SizingVerdict::Rejected) => Err(ChainerError::SizingRejected { order: s.into() }),
                Some(SizingVerdict::Approved) => {
                    let Some(d) = approval_digest else {
                        return Err(ChainerError::Unsized { order: order_id.into() });
                    };
                    let rows = self
                        .log
                        .read_all()
                        .map_err(|e| ChainerError::Log(format!("{e:?}")))?;
                    let bound = rows.iter().find_map(|r| match &r.kind {
                        OrchestrationKind::PresetBound {
                            sizing_order,
                            selected_root,
                            approved_digest,
                            spawn_plan,
                        } if sizing_order == s => {
                            Some((approved_digest.clone(), spawn_plan.clone(), selected_root.clone()))
                        }
                        _ => None,
                    });
                    let Some((expected_digest, expected_plan, expected_root)) = bound else {
                        return Err(ChainerError::Unsized { order: order_id.into() });
                    };
                    if expected_digest != d {
                        return Err(ChainerError::DigestMismatch {
                            order: order_id.into(),
                            expected: expected_digest,
                            got: d.into(),
                        });
                    }
                    if expected_plan != dest_plan {
                        return Err(ChainerError::DestinationMismatch {
                            order: order_id.into(),
                            expected: expected_plan,
                            got: dest_plan.into(),
                        });
                    }
                    if expected_root != dest_root {
                        return Err(ChainerError::DestinationMismatch {
                            order: order_id.into(),
                            expected: expected_root,
                            got: dest_root.into(),
                        });
                    }
                    Ok(())
                }
                None => Err(ChainerError::Unsized { order: order_id.into() }),
            },
        }
    }

    /// 재디에이션 제안 — 거부당 1회(로그 멱등)·깊이 1.
    /// 연결의 내구화: 거부 레코드의 `remediation_proposal_id` 가
    /// 채워진 행을 **별도 Refusal 아닌 행**으로 남길 수 없으므로(불변
    /// 체인), 제안 사실을 HandoffBound 가 아닌 **신규 거부 유사 행**이
    /// 아니라 로그 재생으로 판정한다: 제안 id "rem-{seq}" 가 이미
    /// **remediation 연결 행**(아래 append)에 존재하면 거부.
    pub fn propose_remediation(&self, refusal_seq: i64) -> Result<(String, RemediationKind), ChainerError> {
        let rows = self
            .log
            .read_all()
            .map_err(|e| ChainerError::Log(format!("{e:?}")))?;
        let row = rows
            .iter()
            .find(|r| r.seq == refusal_seq)
            .ok_or(ChainerError::RemediationDepth { refusal_seq })?;
        let OrchestrationKind::Refusal { refusal_code, remediation_proposal_id, .. } = &row.kind
        else {
            return Err(ChainerError::RemediationDepth { refusal_seq });
        };
        // 거부당 1회 — 이미 연결 행이 있으면(또는 원 거부 행 자체에
        // proposal id 가 기록돼 있으면) 재제안 불가.
        if remediation_proposal_id.is_some() {
            return Err(ChainerError::RemediationAlreadyProposed { refusal_seq });
        }
        let linked = rows.iter().any(|r| match &r.kind {
            OrchestrationKind::Refusal { refused_request, remediation_proposal_id, .. } => {
                // 연결 행: refused_request 가 "rem-{seq}" 를 지목.
                refused_request == &format!("rem-link-{refusal_seq}")
                    && remediation_proposal_id.is_some()
            }
            _ => false,
        });
        if linked {
            return Err(ChainerError::RemediationAlreadyProposed { refusal_seq });
        }
        // 깊이 1 — 이 거부가 **재디에이션 작업 주문 자신**의 거부인지:
        // 재디에이션 작업은 "rem-" 주문이고, 그 거부의 refused_request 가
        // "rem-" 로 시작하면 깊이 초과.
        let OrchestrationKind::Refusal { refused_request, .. } = &row.kind else {
            unreachable!()
        };
        if refused_request.starts_with("rem-") {
            return Err(ChainerError::RemediationDepth { refusal_seq });
        }
        let code = parse_refusal_code(refusal_code);
        let entry = crate::refusal::entry_for(code);
        let proposal_id = format!("rem-{refusal_seq}");
        // (astra r5) 연결의 내구화 — **원자적 검증+append**: BEGIN IMMEDIATE
        // 안에서 연결 부재를 재확인 후 append — 동시 재디에이션 호출이
        // 둘 다 성립하는 경쟁을 차단한다.
        let link_req = format!("rem-link-{refusal_seq}");
        self.log
            .append_with_validation(
                crate::OrchestrationKind::Refusal {
                    refused_request: link_req,
                    refusal_code: format!("{code:?}"),
                    reason_text: entry.reason_text.into(),
                    acceptance_condition: entry.acceptance_condition.into(),
                    remediation_proposal_id: Some(proposal_id.clone()),
                },
                |rows_now| {
                    // 트랜잭션 내 재검증 — 연결 행·원 행의 직접 연결 부재.
                    let already = rows_now.iter().any(|r| match &r.kind {
                        crate::OrchestrationKind::Refusal {
                            refused_request, ..
                        } => refused_request == &format!("rem-link-{refusal_seq}"),
                        _ => false,
                    });
                    if already {
                        return Err(crate::OrchError::DuplicateDeclaration {
                            plan_id: format!("remediation already linked: {refusal_seq}"),
                        });
                    }
                    Ok(())
                },
            )
            .map_err(|e| match e {
                crate::OrchError::DuplicateDeclaration { .. } => {
                    ChainerError::RemediationAlreadyProposed { refusal_seq }
                }
                other => ChainerError::Log(format!("{other:?}")),
            })?;
        Ok((proposal_id, entry.remediation))
    }

    /// 재입장 — **전량 재평가**(같은 정문): 바인딩+선언+승인 전부를
    /// 현재 로그·원장 기준으로 재검사.
    pub fn reenter(
        &self,
        order_id: &str,
        order_source: &str,
        sizing_order: Option<&str>,
        approval_digest: Option<&str>,
        dest_plan: &str,
        dest_root: &str,
    ) -> Result<(), ChainerError> {
        // (T61 remainder ①) 선언 재검사 — **항상**, 로그에서 직접 파생
        // (호출자 플래그 제거 — 우회 불가).
        {
            let rows = self
                .log
                .read_all()
                .map_err(|e| ChainerError::Log(format!("{e:?}")))?;
            let is_declared = rows.iter().any(|r| match &r.kind {
                OrchestrationKind::PlanDeclared { edges, .. } => {
                    edges.iter().any(|(o, _)| o == order_id)
                }
                _ => false,
            });
            if !is_declared {
                return Err(ChainerError::DeclarationMissing { order: order_id.into() });
            }
        }
        // 바인딩 전량 재검사 — 승인 상태는 원장에서 재산출(전량 재평가).
        // (M7.1 리뷰 MAJOR-3) 결과를 **로그에 기록** — 입장/거부가
        // 저널 증거로 남는다(검증만 하고 흔적 없는 재입장 폐지).
        match self.check_spawn_binding(
            order_id,
            order_source,
            sizing_order,
            approval_digest,
            dest_plan,
            dest_root,
        ) {
            Ok(()) => {
                self.log
                    .append(OrchestrationKind::ReentryAdmitted {
                        order: order_id.into(),
                        dest_plan: dest_plan.into(),
                    })
                    .map_err(|e| ChainerError::Log(format!("{e:?}")))?;
                Ok(())
            }
            Err(e) => {
                let _ = self.log.append(OrchestrationKind::ReentryRefused {
                    order: order_id.into(),
                    reason: format!("{e:?}"),
                });
                Err(e)
            }
        }
    }
}

/// refusal_code 문자열 → RefusalCode — 미등록 코드는 오류가 아니라
/// 보수 폴백이 아니라 **명시적 거부**(r1 MINOR: 침묵 오분류 금지).
fn parse_refusal_code(s: &str) -> RefusalCode {
    for c in crate::refusal::all_codes_exhaustive() {
        if format!("{c:?}") == s {
            return c;
        }
    }
    // 미등록 — 로그 변조 시사. GateBypass(가장 보수적)로 분류하되
    // entry_for 는 전수라 여기 도달은 비정상이다.
    RefusalCode::GateBypass
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("foundry-t59-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn setup(name: &str) -> (std::path::PathBuf, OrchestrationLog, ApprovalLedger) {
        let d = tmpdir(name);
        let log = OrchestrationLog::open(&d.join("orch.db")).unwrap();
        let ledger = ApprovalLedger::open(&d.join("ap.db")).unwrap();
        (d, log, ledger)
    }

    /// (게이트 1+2+3) 제안/발급 분리·3자 일치·크래시 멱등(재생 기반).
    #[test]
    fn propose_confirm_replay_idempotence() {
        let (_d, log, mut ledger) = setup("threeway");
        let ch = Chainer::new(&log, &ledger);
        let p1 = ch.propose_handoff("hp-1", "o-a", "o-b", "dig-1", "tok-issuer-1").unwrap();
        assert!(p1.manifest.starts_with("fh1:"));

        // 발급 그랜트 — issuer 토큰 일치·digest+**매니페스트** 결속.
        let manifest1 = p1.manifest.clone();
        let scope1 = serde_json::json!({
            "order_id": "o-a", "dest_order_id": "o-b", "intent": "handoff",
            "artifact_digest": "dig-1", "handoff_manifest": manifest1, "root_scope": "",
        });
        ledger.issue_grant("g-1", "tok-issuer-1", &scope1).unwrap();
        let res = ledger.reserve_grant("g-1", &scope1, "ex-1", 1).unwrap();
        ledger.begin_apply("g-1", "ex-1", 1, res.fence_token).unwrap();
        ledger.finalize_grant("g-1", "ex-1", 1, res.fence_token).unwrap();

        // 크래시 재현 — 새 Chainer(로그만)가 같은 제안을 본다.
        let ch2 = Chainer::new(&log, &ledger);
        let p2 = ch2.propose_handoff("hp-1", "o-a", "o-b", "dig-1", "tok-issuer-1").unwrap();
        assert_eq!(p1, p2, "replay-based idempotence across instances");

        let c1 = ch2.confirm_handoff("hp-1", "g-1").unwrap();
        assert_eq!(c1, p1);
        // 멱등 재확정.
        assert_eq!(ch2.confirm_handoff("hp-1", "g-1").unwrap(), c1);

        // 위조 발급자 — 다른 토큰의 그랜트.
        let _ = ch2.propose_handoff("hp-2", "o-c", "o-d", "dig-2", "tok-issuer-2").unwrap();
        drop(ch2);
        let scope2 = serde_json::json!({
            "order_id": "o-c", "intent": "handoff", "artifact_digest": "dig-2", "root_scope": "",
        });
        ledger.issue_grant("g-2", "tok-FORGED", &scope2).unwrap();
        // hp-3 제안 — ch3 경유(재생 멱등이 hp-2/hp-3 도 커버).
        let ch_pre = Chainer::new(&log, &ledger);
        let _ = ch_pre.propose_handoff("hp-3", "o-e", "o-f", "dig-3", "tok-3").unwrap();
        drop(ch_pre);
        let scope3 = serde_json::json!({
            "order_id": "o-e", "intent": "handoff", "artifact_digest": "dig-WRONG", "root_scope": "",
        });
        ledger.issue_grant("g-3", "tok-3", &scope3).unwrap();
        let ch3 = Chainer::new(&log, &ledger);
        // (M7.1 재리뷰 r2) 발급자 앵커는 매니페스트 결속이 담당 — 무관
        // 그랜트(g-2: handoff_manifest 부재)는 ThreeWayMismatch 로 거부.
        assert!(matches!(
            ch3.confirm_handoff("hp-2", "g-2"),
            Err(ChainerError::ThreeWayMismatch { .. })
        ));
        assert!(matches!(
            ch3.confirm_handoff("hp-3", "g-3"),
            Err(ChainerError::ThreeWayMismatch { .. })
        ));
        // 미제안 확정.
        assert!(matches!(
            ch3.confirm_handoff("hp-x", "g-1"),
            Err(ChainerError::NotProposed { .. })
        ));
    }

    /// (게이트 5) 바인딩 — unsized·부트스트랩 소스 패턴·거부된 사이징·
    /// digest·목적지 plan/root 일치 — 승인 상태는 원장에서 산출.
    #[test]
    fn spawn_binding_gates() {
        let (_d, log, mut ledger) = setup("bind");
        // (astra r7) 사이징 승인 그랜트(규약 id "sizing-s-1") — CONSUMED.
        let sscope = serde_json::json!({
            "order_id": "s-1", "intent": "sizing",
            "artifact_digest": crate::chainer::sizing_digest("approved", "root-quick", "plan-1"),
            "root_scope": "intake",
        });
        ledger.issue_grant("sizing-s-1", "tok-sizing", &sscope).unwrap();
        let r0 = ledger.reserve_grant("sizing-s-1", &sscope, "ex-s", 1).unwrap();
        ledger.begin_apply("sizing-s-1", "ex-s", 1, r0.fence_token).unwrap();
        ledger.finalize_grant("sizing-s-1", "ex-s", 1, r0.fence_token).unwrap();
        // 사이징 거부 그랜트 — sizing_outcome: rejected.
        let rscope = serde_json::json!({
            "order_id": "s-rej", "intent": "sizing",
            "artifact_digest": crate::chainer::sizing_digest("rejected", "r", "p"),
            "root_scope": "intake",
        });
        ledger.issue_grant("sizing-s-rej", "tok-r", &rscope).unwrap();
        // (astra r7 회귀용) 미소비 그랜트 — Issued 상태로 방치.
        let iscope = serde_json::json!({
            "order_id": "s-issued", "intent": "sizing",
            "artifact_digest": crate::chainer::sizing_digest("approved", "ri", "pi"),
            "root_scope": "intake",
        });
        ledger.issue_grant("sizing-s-issued", "tok-i", &iscope).unwrap();
        let ch = Chainer::new(&log, &ledger);
        // unsized — 비인테이크 소스.
        assert!(matches!(
            ch.check_spawn_binding("o-1", "ext:webhook", None, None, "p-any", "r-any"),
            Err(ChainerError::BootstrapViolation { .. })
        ));
        // 부트스트랩 — 인테이크 소스 패턴.
        assert!(ch
            .check_spawn_binding("o-boot", "intake:ticket-1", None, None, "p-x", "r-x")
            .is_ok());
        // 사이징 거부 후 발행 — 원장 sizing_outcome=rejected 에서 산출.
        assert!(matches!(
            ch.check_spawn_binding("o-2", "intake:t", Some("s-rej"), Some("dig"), "p", "r"),
            Err(ChainerError::SizingRejected { .. })
        ));
        // 승인 + preset_bound 일치.
        log.append_preset_bound("s-1", "root-quick", "dig-approv", "plan-1").unwrap();
        assert!(ch
            .check_spawn_binding("o-3", "ext:x", Some("s-1"), Some("dig-approv"), "plan-1", "root-quick")
            .is_ok());
        // digest 불일치.
        assert!(matches!(
            ch.check_spawn_binding("o-4", "ext:x", Some("s-1"), Some("dig-other"), "plan-1", "root-quick"),
            Err(ChainerError::DigestMismatch { .. })
        ));
        // 목적지 plan 불일치.
        assert!(matches!(
            ch.check_spawn_binding("o-5", "ext:x", Some("s-1"), Some("dig-approv"), "plan-OTHER", "root-quick"),
            Err(ChainerError::DestinationMismatch { .. })
        ));
        // 목적지 root 불일치.
        assert!(matches!(
            ch.check_spawn_binding("o-6", "ext:x", Some("s-1"), Some("dig-approv"), "plan-1", "root-OTHER"),
            Err(ChainerError::DestinationMismatch { .. })
        ));

        assert!(matches!(
            ch.check_spawn_binding("o-7", "ext:x", Some("s-issued"), Some("dig-i"), "p", "r"),
            Err(ChainerError::Unsized { .. })
        ), "unconsumed sizing grant is not approval evidence");
    }
    /// (게이트 6) 재디에이션 — 제안 1회(로그 멱등·인스턴스 무관)·깊이 1.
    #[test]
    fn remediation_once_depth1() {
        let (_d, log, ledger) = setup("rem");
        crate::refusal::record_refusal(&log, RefusalCode::UnsizedSpawn, "o-bad", None).unwrap();
        let rows = log.read_all().unwrap();
        let rseq = rows
            .iter()
            .find(|r| matches!(r.kind, OrchestrationKind::Refusal { .. }))
            .unwrap()
            .seq;

        let ch = Chainer::new(&log, &ledger);
        let (pid, kind) = ch.propose_remediation(rseq).unwrap();
        assert_eq!(pid, format!("rem-{rseq}"));
        assert_eq!(kind, RemediationKind::RunSizing);

        // 새 인스턴스에서도 재제안 불가(내구 연결).
        let ch2 = Chainer::new(&log, &ledger);
        assert!(matches!(
            ch2.propose_remediation(rseq),
            Err(ChainerError::RemediationAlreadyProposed { .. })
        ));

        // 깊이 1 — 재디에이션 작업 주문 자신의 거부.
        crate::refusal::record_refusal(&log, RefusalCode::NotReady, "rem-99", None).unwrap();
        let rows2 = log.read_all().unwrap();
        let rseq2 = rows2
            .iter()
            .filter(|r| matches!(r.kind, OrchestrationKind::Refusal { .. }))
            .map(|r| r.seq)
            .max()
            .unwrap();
        assert!(matches!(
            ch2.propose_remediation(rseq2),
            Err(ChainerError::RemediationDepth { .. })
        ));
        // 부재 행.
        assert!(matches!(
            ch2.propose_remediation(9999),
            Err(ChainerError::RemediationDepth { .. })
        ));
    }

    /// (게이트 7) 재입장 전량 재평가 — A 수리돼도 B 거부.
    #[test]
    fn reentry_full_reevaluation() {
        let (_d, log, mut ledger) = setup("reentry");
        // (astra r7) 사이징 승인 그랜트(규약 id "sizing-s-2") — CONSUMED.
        let sscope = serde_json::json!({
            "order_id": "s-2", "intent": "sizing",
            "artifact_digest": crate::chainer::sizing_digest("approved", "root-x", "plan-2"),
            "root_scope": "intake",
        });
        ledger.issue_grant("sizing-s-2", "tok-s2", &sscope).unwrap();
        let r0 = ledger.reserve_grant("sizing-s-2", &sscope, "ex-s2", 1).unwrap();
        ledger.begin_apply("sizing-s-2", "ex-s2", 1, r0.fence_token).unwrap();
        ledger.finalize_grant("sizing-s-2", "ex-s2", 1, r0.fence_token).unwrap();
        let ch = Chainer::new(&log, &ledger);
        // (T61) 전량 재평가 순서: ①선언 없음 → DeclarationMissing.
        assert!(matches!(
            ch.reenter("o-bad", "ext:x", Some("s-2"), Some("dig-x"), "plan-2", "root-x"),
            Err(ChainerError::DeclarationMissing { .. })
        ));
        log.declare_plan(
            "plan-2",
            vec![("o-bad".into(), vec![])],
            vec![("o-bad".into(), "s-x".into())],
        )
        .unwrap();
        // ②사이징 승인(A 수리) — 그러나 preset_bound 미등록(B) → 여전히 거부.
        let r = ch.reenter("o-bad", "ext:x", Some("s-2"), Some("dig-x"), "plan-2", "root-x");
        assert!(matches!(r, Err(ChainerError::Unsized { .. })), "A repaired, B refuses");
        // ③preset_bound 등록 — 통과.
        log.append_preset_bound("s-2", "root-x", "dig-x", "plan-2").unwrap();
        assert!(ch
            .reenter("o-bad", "ext:x", Some("s-2"), Some("dig-x"), "plan-2", "root-x")
            .is_ok());
        // 선언 재검사 — declared=true 인데 로그에 없으면 거부(새 B).
        // (T61) 미선언 주문의 재입장 — 항상 DeclarationMissing.
        assert!(matches!(
            ch.reenter("o-undeclared", "ext:x", Some("s-2"), Some("dig-x"), "plan-2", "root-x"),
            Err(ChainerError::DeclarationMissing { .. })
        ));
    }

    /// (astra r8) preset_bound 유일 작성 경로 — 원장 승인 검증+멱등.
    #[test]
    fn bind_preset_from_ledger_gates() {
        let (_d, log, mut ledger) = setup("bindpreset");
        // 미승인(그랜트 부재) — 결속 거부.
        let ch = Chainer::new(&log, &ledger);
        assert!(matches!(
            ch.bind_preset_from_ledger("s-none", "root", "plan"),
            Err(ChainerError::SizingRejected { .. })
        ));
        // 승인 그랜트 — CONSUMED (ch 재생성 앞에 완료).
        let sscope = serde_json::json!({
            "order_id": "s-9", "intent": "sizing",
            "artifact_digest": crate::chainer::sizing_digest("approved", "root-9", "plan-9"),
            "root_scope": "intake",
        });
        ledger.issue_grant("sizing-s-9", "tok-9", &sscope).unwrap();
        let r0 = ledger.reserve_grant("sizing-s-9", &sscope, "ex-9", 1).unwrap();
        ledger.begin_apply("sizing-s-9", "ex-9", 1, r0.fence_token).unwrap();
        ledger.finalize_grant("sizing-s-9", "ex-9", 1, r0.fence_token).unwrap();
        // 거부된 사이징 그랜트.
        let rscope = serde_json::json!({
            "order_id": "s-rej9", "intent": "sizing",
            "artifact_digest": crate::chainer::sizing_digest("rejected", "r9", "p9"),
            "root_scope": "intake",
        });
        ledger.issue_grant("sizing-s-rej9", "tok-r9", &rscope).unwrap();
        let ch = Chainer::new(&log, &ledger);
        // 결속 성공 — digest 는 그랜트 스코프에서.
        ch.bind_preset_from_ledger("s-9", "root-9", "plan-9").unwrap();
        // 멱등 — 재호출 이중 행 없음.
        ch.bind_preset_from_ledger("s-9", "root-9", "plan-9").unwrap();
        let rows = log.read_all().unwrap();
        let bounds = rows
            .iter()
            .filter(|r| matches!(&r.kind, OrchestrationKind::PresetBound { sizing_order, .. } if sizing_order == "s-9"))
            .count();
        assert_eq!(bounds, 1, "idempotent single preset_bound row");
        // 거부된 사이징 — 결속 거부.
        assert!(matches!(
            ch.bind_preset_from_ledger("s-rej9", "root", "plan"),
            Err(ChainerError::SizingRejected { .. })
        ));
        // (astra r12 P3) 리다이렉션 회귀 — 잘못된 root/plan 결속 거부.
        assert!(matches!(
            ch.bind_preset_from_ledger("s-9", "root-WRONG", "plan-9"),
            Err(ChainerError::DestinationMismatch { .. })
        ));
        assert!(matches!(
            ch.bind_preset_from_ledger("s-9", "root-9", "plan-WRONG"),
            Err(ChainerError::DestinationMismatch { .. })
        ));
    }
}
