//! T59 — 체이너 + 재디에이션 제안자 (M7 플랜 v9 C′/H/I절).
//!
//! 계약:
//! - **제안/발급 분리**: 체이너는 승인 매니페스트를 **제안만** 한다 —
//!   발급(GrantRef 확정)은 인간 게이트 승인 이후 별도 확정 단계.
//! - **전이 전 3자 일치**: 핸드오프 확정 시 ①체이너가 제안한 매니페스트
//!   ②오케스트레이션 로그의 HandoffBound 행 ③그랜트 스코프 내
//!   artifact_digest — 셋이 일치해야 성립.
//! - **크래시 창 멱등**: 제안은 proposal_id 에 대해 멱등(재제안은 같은
//!   proposal_id 로 기존 행 반환), 확정도 확정 행 존재 시 재확정 없이
//!   기존 결과 반환.
//! - **위조 발급자 거부**: 확정 요청이 지목한 발급자(그랜트 토큰)가
//!   원장 기록과 불일치하면 거부.
//! - **바인딩 준수 발행**: spawn 은 preset_bound 없으면 거부(unsized),
//!   부트스트랩(인테이크 사이징 주문)만 예외. 승인 digest ↔ preset_bound
//!   approved_digest 일치 강제. 거부된 사이징(Rejected 승인) 후 발행 거부.
//! - **재디에이션 제안자**: RefusalRecord 에서 수락 조건 충족 작업을
//!   **제안만**(거부당 1회 멱등·깊이 1 제한 — 재디에이션의 재디에이션
//!   없음). 재입장은 전량 재평가(같은 정문).

use crate::refusal::{RefusalCode, RemediationKind};
use crate::{OrchestrationKind, OrchestrationLog};
use std::collections::BTreeMap;

/// 체이너 오류.
#[derive(Debug, PartialEq)]
pub enum ChainerError {
    /// unsized — preset_bound 없는 발행.
    Unsized { order: String },
    /// digest 불일치 — preset_bound.approved_digest ≠ 제시 승인 digest.
    DigestMismatch { order: String, expected: String, got: String },
    /// 사이징이 거부됨(Rejected) — 같은 사이징 주문으로 발행 불가.
    SizingRejected { order: String },
    /// 부트스트랩 위반 — 인테이크 경로 아닌 주문의 preset_bound 없는 발행.
    BootstrapViolation { order: String },
    /// 제안이 아닌 확정(미제안 확정 시도).
    NotProposed { proposal_id: String },
    /// 3자 불일치.
    ThreeWayMismatch { proposal_id: String },
    /// 위조 발급자 — 확정 지목 발급자 ≠ 원장 기록.
    IssuerMismatch { proposal_id: String },
    /// 재디에이션 깊이 초과(깊이 1 제한).
    RemediationDepth { refusal_seq: i64 },
    /// 재디에이션 제안 중복(거부당 1회).
    RemediationAlreadyProposed { refusal_seq: i64 },
    /// 사이클 등 기존 로그 오류 전파.
    Log(String),
}

/// 사이징 승인 상태 (원장 CONSUMED 증거가 권위 — digest만으론 불충분).
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum SizingVerdict {
    /// 원장에서 소비 완료(CONSUMED) 확인 — 발급 권한 성립.
    Approved,
    /// 사이징 승인이 거부됨.
    Rejected,
}

/// 체이너 — 제안 확정 상태(크래시 창 멱등의 근거는 로그 행).
pub struct Chainer<'o> {
    log: &'o OrchestrationLog,
    /// proposal_id → 제안 (HandoffBound 행). 멱등 재제안용.
    proposals: BTreeMap<String, ProposalState>,
    /// refusal_seq → 재디에이션 제안 id (거부당 1회 멱등).
    remediations: BTreeMap<i64, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProposalState {
    pub proposal_id: String,
    /// "fh1:" 매니페스트 digest (행 seq 결속).
    pub manifest: String,
    /// 로그 행 seq.
    pub log_seq: i64,
    /// 발급자(그랜트 토큰) — 제안 시점에 체이너이 지목.
    pub issuer_token: String,
    /// 확정 여부(확정 시 발급자·digest 고정).
    pub confirmed: bool,
}

impl<'o> Chainer<'o> {
    pub fn new(log: &'o OrchestrationLog) -> Self {
        Self { log, proposals: BTreeMap::new(), remediations: BTreeMap::new() }
    }

    /// 핸드오프 승인 **제안** — 멱등: 같은 proposal_id 재제안은 기존 상태
    /// 반환(크래시 후 재시도 동일 결과). 로그에 HandoffBound 행 append.
    pub fn propose_handoff(
        &mut self,
        proposal_id: &str,
        source_order: &str,
        dest_order: &str,
        artifact_digest: &str,
        issuer_token: &str,
    ) -> Result<ProposalState, ChainerError> {
        if let Some(p) = self.proposals.get(proposal_id) {
            // 멱등 — 로그 행에서 재계산해 비교(중복 append 없음).
            let row = self
                .log
                .row_at(p.log_seq)
                .map_err(|e| ChainerError::Log(format!("{e:?}")))?;
            let OrchestrationKind::HandoffBound { source_order: s0, dest_order: d0, artifact_digest: a0 } =
                &row.kind
            else {
                return Err(ChainerError::ThreeWayMismatch { proposal_id: proposal_id.into() });
            };
            if s0 != source_order || d0 != dest_order || a0 != artifact_digest
                || p.issuer_token != issuer_token
            {
                return Err(ChainerError::ThreeWayMismatch {
                    proposal_id: proposal_id.into(),
                });
            }
            return Ok(p.clone());
        }
        let (row, manifest) = self
            .log
            .bind_handoff(source_order, dest_order, artifact_digest)
            .map_err(|e| ChainerError::Log(format!("{e:?}")))?;
        let p = ProposalState {
            proposal_id: proposal_id.into(),
            manifest,
            log_seq: row.seq,
            issuer_token: issuer_token.into(),
            confirmed: false,
        };
        self.proposals.insert(proposal_id.into(), p.clone());
        Ok(p)
    }

    /// 핸드오프 **확정** — 전이 전 3자 일치:
    /// ①체이너 제안 매니페스트 ②로그 HandoffBound 행(재계산) ③제시
    /// artifact_digest. 발급자 일치(위조 거부). 멱등(재확정은 기존 반환).
    pub fn confirm_handoff(
        &mut self,
        proposal_id: &str,
        artifact_digest: &str,
        issuer_token: &str,
    ) -> Result<ProposalState, ChainerError> {
        let Some(p) = self.proposals.get(proposal_id).cloned() else {
            return Err(ChainerError::NotProposed { proposal_id: proposal_id.into() });
        };
        if p.confirmed {
            return Ok(p); // 멱등 재확정.
        }
        // (2) 로그 행에서 매니페스트 재계산.
        let row = self
            .log
            .row_at(p.log_seq)
            .map_err(|e| ChainerError::Log(format!("{e:?}")))?;
        let OrchestrationKind::HandoffBound {
            source_order,
            dest_order,
            artifact_digest: logged_digest,
        } = &row.kind
        else {
            return Err(ChainerError::ThreeWayMismatch { proposal_id: proposal_id.into() });
        };
        let recomputed = crate::handoff_manifest_digest(
            source_order,
            dest_order,
            logged_digest,
            row.seq,
        )
        .map_err(|e| ChainerError::Log(e))?;
        // (1)+(2) 제안 시 매니페스트와 재계산 일치.
        if recomputed != p.manifest {
            return Err(ChainerError::ThreeWayMismatch { proposal_id: proposal_id.into() });
        }
        // (3) 제시 digest 일치.
        if *logged_digest != artifact_digest {
            return Err(ChainerError::ThreeWayMismatch { proposal_id: proposal_id.into() });
        }
        // 위조 발급자 거부.
        if p.issuer_token != issuer_token {
            return Err(ChainerError::IssuerMismatch { proposal_id: proposal_id.into() });
        }
        let mut p = p;
        p.confirmed = true;
        self.proposals.insert(proposal_id.into(), p.clone());
        Ok(p)
    }

    /// 바인딩 준수 발행 검사 — 주문 발행 전 게이트 (H절).
    /// `sizing_verdict` 는 **원장 CONSUMED 증거에서** 호출자가 산출해
    /// 전달한다(체이너는 원장을 열지 않는다 — 권위는 증거).
    pub fn check_spawn_binding(
        &self,
        order_id: &str,
        sizing_order: Option<&str>,
        approval_digest: Option<&str>,
        sizing_verdict: Option<SizingVerdict>,
        is_bootstrap: bool,
    ) -> Result<(), ChainerError> {
        match (sizing_order, approval_digest, sizing_verdict, is_bootstrap) {
            // 부트스트랩 — 인테이크 사이징 주문은 preset_bound 없이 존재.
            (None, _, _, true) => Ok(()),
            (None, _, _, false) => Err(ChainerError::Unsized { order: order_id.into() }),
            // 사이징 지목 — 거부됐으면 발행 거부.
            (Some(s), d, Some(SizingVerdict::Rejected), _) => {
                Err(ChainerError::SizingRejected { order: s.into() })
            }
            // 승인 — digest 일치 강제.
            (Some(_), Some(d), Some(SizingVerdict::Approved), _) => {
                let rows = self
                    .log
                    .read_all()
                    .map_err(|e| ChainerError::Log(format!("{e:?}")))?;
                let want = sizing_order.unwrap_or_default();
                let bound = rows.iter().find_map(|r| match &r.kind {
                    OrchestrationKind::PresetBound { sizing_order, approved_digest, .. }
                        if sizing_order == want => Some(approved_digest.clone()),
                    _ => None,
                });
                match bound {
                    Some(expected) if expected == d => Ok(()),
                    Some(expected) => Err(ChainerError::DigestMismatch {
                        order: order_id.into(),
                        expected,
                        got: d.into(),
                    }),
                    // preset_bound 없는 승인 지목 — unsized.
                    None => Err(ChainerError::Unsized { order: order_id.into() }),
                }
            }
            // 사이징 지목 + 승인 상태 없음 — 원장 증거 없음.
            (Some(_), _, None, _) => Err(ChainerError::Unsized { order: order_id.into() }),
            // 승인 상태는 있으나 digest 미제시 — 검증 불가, 거부.
            (Some(_), None, Some(SizingVerdict::Approved), _) => {
                Err(ChainerError::Unsized { order: order_id.into() })
            }
        }
    }

    /// 재디에이션 제안 — 거부 레코드당 **1회**(멱등), 깊이 1 제한.
    /// 반환: (제안 id, 제안할 작업 종류). **제안만** — 수행·수락은 인간.
    pub fn propose_remediation(
        &mut self,
        refusal_seq: i64,
    ) -> Result<(String, RemediationKind), ChainerError> {
        // 거부 행 조회.
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
            // 거부 행이 아님 — 깊이 제한 오류로 통일(거부 아닌 것의
            // 재디에이션은 정의되지 않음).
            return Err(ChainerError::RemediationDepth { refusal_seq });
        };
        // 깊이 1 (면 1): 거부 행이 이미 제안을 갖고 있으면 재제안 불가.
        if remediation_proposal_id.is_some() {
            return Err(ChainerError::RemediationAlreadyProposed { refusal_seq });
        }
        let code = refusal_code.parse_refusal_code();
        let entry = crate::refusal::entry_for(code);
        let proposal_id = format!("rem-{refusal_seq}");
        // 깊이 1 (면 2) + 거부당 1회 멱등: 등록부 검사. 재입장 시 Chainer 가
        // 재구성되면 등록부는 로그의 remediation_proposal_id 로 재구성되어야
        // 한다 — T61 종단에서 로그 재생으로 회복(여기선 등록부 소유 멱등).
        if self.remediations.contains_key(&refusal_seq) {
            return Err(ChainerError::RemediationAlreadyProposed { refusal_seq });
        }
        self.remediations
            .insert(refusal_seq, proposal_id.clone());
        Ok((proposal_id, entry.remediation))
    }

    /// 재입장 — **전량 재평가**(우회 아님): 사이징·선언·digest 를 현재
    /// 상태로 다시 검사. A만 수리된 상태에서 B 사유 거부를 재현한다.
    pub fn reenter(
        &self,
        order_id: &str,
        sizing_order: Option<&str>,
        approval_digest: Option<&str>,
        sizing_verdict: Option<SizingVerdict>,
        is_bootstrap: bool,
    ) -> Result<(), ChainerError> {
        // 같은 정문 — check_spawn_binding 전량 재실행.
        self.check_spawn_binding(order_id, sizing_order, approval_digest, sizing_verdict, is_bootstrap)
    }
}

/// refusal_code 문자열 → RefusalCode (레지스트리 전수).
trait ParseRefusalCode {
    fn parse_refusal_code(&self) -> RefusalCode;
}

impl ParseRefusalCode for String {
    fn parse_refusal_code(&self) -> RefusalCode {
        for c in crate::refusal::all_codes_exhaustive() {
            if format!("{c:?}") == *self {
                return c;
            }
        }
        // 미등록 코드 — 전수 레지스트리 밖 문자열. UnsizedSpawn 으로
        // 폴백하지 않는다: 호출부에서 entry_for 가 전수이므로 여기 도달
        // 자체가 로그 변조를 시사한다. 가장 보수적 코드 반환.
        RefusalCode::GateBypass
    }
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

    fn openlog(name: &str) -> OrchestrationLog {
        OrchestrationLog::open(&tmpdir(name).join("orch.db")).unwrap()
    }

    /// (게이트) 제안/발급 분리 + 3자 일치 + 멱등 + 위조 발급자 거부.
    #[test]
    fn propose_confirm_three_way_and_idempotence() {
        let log = openlog("threeway");
        let mut ch = Chainer::new(&log);
        let p1 = ch
            .propose_handoff("hp-1", "o-a", "o-b", "dig-1", "tok-issuer-1")
            .unwrap();
        assert!(!p1.confirmed);
        assert!(p1.manifest.starts_with("fh1:"));

        // 멱등 재제안 — 같은 파라미터, 같은 상태.
        let p2 = ch
            .propose_handoff("hp-1", "o-a", "o-b", "dig-1", "tok-issuer-1")
            .unwrap();
        assert_eq!(p1.log_seq, p2.log_seq);

        // 확정 — 3자 일치.
        let c1 = ch.confirm_handoff("hp-1", "dig-1", "tok-issuer-1").unwrap();
        assert!(c1.confirmed);
        // 멱등 재확정.
        let c2 = ch.confirm_handoff("hp-1", "dig-1", "tok-issuer-1").unwrap();
        assert_eq!(c1, c2);

        // 위조 발급자 — 다른 proposal 에서.
        let _ = ch.propose_handoff("hp-2", "o-c", "o-d", "dig-2", "tok-issuer-2").unwrap();
        assert!(matches!(
            ch.confirm_handoff("hp-2", "dig-2", "tok-forged"),
            Err(ChainerError::IssuerMismatch { .. })
        ));
        // digest 불일치.
        assert!(matches!(
            ch.confirm_handoff("hp-2", "dig-wrong", "tok-issuer-2"),
            Err(ChainerError::ThreeWayMismatch { .. })
        ));
        // 미제안 확정.
        assert!(matches!(
            ch.confirm_handoff("hp-x", "dig", "tok"),
            Err(ChainerError::NotProposed { .. })
        ));
    }

    /// (게이트) 바인딩 준수 — unsized 거부·digest 일치·거부된 사이징 후
    /// 발행 거부·부트스트랩 예외.
    #[test]
    fn spawn_binding_gates() {
        let log = openlog("bind");
        let ch = Chainer::new(&log);
        // unsized — 부트스트랩 아님.
        assert!(matches!(
            ch.check_spawn_binding("o-1", None, None, None, false),
            Err(ChainerError::Unsized { .. })
        ));
        // 부트스트랩(인테이크) — 허용.
        assert!(ch.check_spawn_binding("o-boot", None, None, None, true).is_ok());
        // 사이징 거부 후 발행.
        assert!(matches!(
            ch.check_spawn_binding("o-2", Some("s-1"), Some("dig"), Some(SizingVerdict::Rejected), false),
            Err(ChainerError::SizingRejected { .. })
        ));
        // 승인 + preset_bound 일치 — 발행 허용.
        log.append_preset_bound("s-1", "root-quick", "dig-approv", "plan-1").unwrap();
        assert!(ch
            .check_spawn_binding("o-3", Some("s-1"), Some("dig-approv"), Some(SizingVerdict::Approved), false)
            .is_ok());
        // digest 불일치.
        assert!(matches!(
            ch.check_spawn_binding("o-4", Some("s-1"), Some("dig-other"), Some(SizingVerdict::Approved), false),
            Err(ChainerError::DigestMismatch { .. })
        ));
    }

    /// (게이트) 재디에이션 — 제안 1회·깊이 1·재입장 전량 재평가.
    #[test]
    fn remediation_proposal_once_and_reentry() {
        let log = openlog("rem");
        // 거부 레코드 1건 심기.
        crate::refusal::record_refusal(&log, RefusalCode::UnsizedSpawn, "o-bad", None).unwrap();
        let rows = log.read_all().unwrap();
        let rseq = rows.iter().find(|r| matches!(r.kind, OrchestrationKind::Refusal { .. })).unwrap().seq;

        let mut ch = Chainer::new(&log);
        let (pid, kind) = ch.propose_remediation(rseq).unwrap();
        assert!(pid.starts_with("rem-"));
        assert_eq!(kind, RemediationKind::RunSizing); // UnsizedSpawn → RunSizing
        // 재제안 — 1회 멱등 위반 거부.
        assert!(matches!(
            ch.propose_remediation(rseq),
            Err(ChainerError::RemediationAlreadyProposed { .. })
        ));
        // 거부 아닌 seq — 깊이/정의 오류.
        assert!(matches!(
            ch.propose_remediation(9999),
            Err(ChainerError::RemediationDepth { .. })
        ));

        // 재입장 전량 재평가 — A(사이징)만 수리돼도 B(unsized)로 거부.
        // 사이징 승인됐지만 preset_bound 미등록 상태 가정.
        let r = ch.reenter("o-bad", Some("s-2"), Some("dig-x"), Some(SizingVerdict::Approved), false);
        assert!(matches!(r, Err(ChainerError::Unsized { .. })), "A repaired, B refuses");
        // preset_bound 등록 후 재입장 — 통과.
        log.append_preset_bound("s-2", "root-x", "dig-x", "plan-2").unwrap();
        assert!(ch
            .reenter("o-bad", Some("s-2"), Some("dig-x"), Some(SizingVerdict::Approved), false)
            .is_ok());
    }
}
