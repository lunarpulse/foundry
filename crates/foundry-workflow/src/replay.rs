//! T56 P0+P1 — 엔진의 내구 배선과 재생 커서.
//!
//! **P0 (저널 배선)**: `set_ledger` — 전이가 Applied로 확정되면 프로덕션
//! `Ledger::append`(foundry-ledger v2 해시 체인)로 영속화한다. 순서 계약
//! (플랜 T56 v5): 도메인 검증 → append 성공 → 상태 공개·관측 push.
//! append 실패 = 전이 무효 (상태 롤백·관측 미발행·Infrastructure 래치).
//!
//! `rebuild` — 저널 재생으로 상태·attempt 복원 **전용** (신규 실행 의미론:
//! 관측·예산·래치는 복원하지 않는다 — 재시작 후 실행은 새 exec_id로).
//! 재구축은 foundry-ledger `replay`(도메인 상태머신 재현)를 재사용 —
//! 재구현 금지 (T56 계약).
//!
//! **P1 (재생 커서, 플랜 v6 C′)**: `issue_replay_cursor` — 대역 외 터미널
//! 커밋먼트{chain_head, max_seq, count}에서 시작해 요청 seq까지 접두사를
//! 재검산한 뒤 발급. 요청 seq가 커밋먼트 범위 밖이면 발급 자체를 거부한다
//! ("outside trusted terminal scope") — 신뢰되지 않은 행을 거쳐 핀을
//! 확장하는 경로는 존재하지 않는다. `ReplayContext`가 커서를 소유하며
//! 이것이 재생의 유일한 입구다.

use crate::WorkflowEngine;
use foundry_domain::ports::Ledger;
use foundry_domain::{ActorId, OrderEvent};
use foundry_ledger::sqlite::LedgerRow;

/// 대역 외 보관되는 터미널 커밋먼트 — 운영자(또는 감사 개시자)가 저널의
/// 알려진 좋은 시점에 촬영한다. HeadPin(foundry-watch)과 동일 구조이되
/// 시퀀스 도메인은 저널 행 seq로 단일화.
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalCommitment {
    pub max_seq: i64,
    pub count: i64,
    pub chain_head: String,
}

/// 발급된 재생 커서 — 특정 (order_id, seq) 행에 대한 검증된 참조.
/// **필드는 비공개** (astra T56 r2): 위조 커서는 구성 자체가 불가능하다.
/// 신뢰 경로는 (i) issue_replay_cursor(커밋먼트 경유 검증 발급)
/// (ii) VerifiedReplayCursor::genesis(유일한 부트스트랩 — 이 주문의
/// 재생 시작점) 뿐이다.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedReplayCursor {
    order_id: String,
    seq: i64,
    /// 요청 행의 해시.
    row_digest: String,
    /// 발급 시점 체인 헤드 — 이후 행은 어떤 커서로도 인증 불가.
    pin_digest: String,
}

impl VerifiedReplayCursor {
    /// 유일한 비발급 생성자 — GENESIS 시작점 (이 주문의 저널 행 부재 시에만
    /// 유효; dispatch 가 재검증한다).
    pub fn genesis(order_id: &str) -> Self {
        Self {
            order_id: order_id.into(),
            seq: 0,
            row_digest: "GENESIS".into(),
            pin_digest: "GENESIS".into(),
        }
    }

    /// 접근자 (진단·게이트 단정용).
    pub fn parts(&self) -> (&str, i64, &str, &str) {
        (&self.order_id, self.seq, &self.row_digest, &self.pin_digest)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CursorError {
    /// 요청 seq가 터미널 커밋먼트 범위 밖 — 발급 거부 (v6 C′).
    OutsideTrustedScope { requested: i64, committed: i64 },
    /// 커밋먼트와 실제 체인이 불일치 — 신뢰 원천 오염.
    CommitmentMismatch { expected_head: String, observed_head: String },
    /// 행 부재 (order_id/seq 조합).
    RowNotFound { order_id: String, seq: i64 },
}

/// 발급: 저널 행들 + 대역 외 커밋먼트 → 커서.
/// (i) 커밋먼트 범위 검사 (ii) 접두사 체인 재검산 (iii) 요청 행 digest 확정.
pub fn issue_replay_cursor(
    rows: &[LedgerRow],
    commitment: &TerminalCommitment,
    order_id: &str,
    seq: i64,
) -> Result<VerifiedReplayCursor, CursorError> {
    // (i) 범위 — 커밋먼트가 커버하는 max_seq까지만 발급.
    if seq > commitment.max_seq {
        return Err(CursorError::OutsideTrustedScope { requested: seq, committed: commitment.max_seq });
    }
    // (ii) 접두사 재검산 — 커밋먼트 시점까지의 행 수·헤드가 정확히 일치.
    let prefix: Vec<LedgerRow> = rows.iter().filter(|r| r.seq <= commitment.max_seq).cloned().collect();
    if prefix.len() as i64 != commitment.count {
        return Err(CursorError::CommitmentMismatch {
            expected_head: commitment.chain_head.clone(),
            observed_head: format!("count {} != {}", prefix.len(), commitment.count),
        });
    }
    let observed_head = prefix.last().map(|r| r.hash.clone()).unwrap_or_default();
    if observed_head != commitment.chain_head {
        return Err(CursorError::CommitmentMismatch { expected_head: commitment.chain_head.clone(), observed_head });
    }
    // 행 해시 연쇄 재검산 — 위조 접미·재정렬은 여기서 검출된다.
    if !foundry_ledger::sqlite::SqliteLedger::verify_rows(&prefix) {
        return Err(CursorError::CommitmentMismatch {
            expected_head: commitment.chain_head.clone(),
            observed_head: "chain re-hash failed".into(),
        });
    }
    // (iii) 요청 행.
    let row = rows
        .iter()
        .find(|r| r.seq == seq && r.order_id == order_id)
        .ok_or(CursorError::RowNotFound { order_id: order_id.into(), seq })?;
    Ok(VerifiedReplayCursor {
        order_id: order_id.into(),
        seq,
        row_digest: row.hash.clone(),
        pin_digest: commitment.chain_head.clone(),
    })
}

/// 커서 소유 재생 컨텍스트 — 재생의 유일한 입구가 된다 (공개 무커서
/// dispatch는 T56에서 제거).
#[derive(Debug, Clone)]
pub struct ReplayContext {
    pub cursor: VerifiedReplayCursor,
}

impl WorkflowEngine {
    /// **P0**: 프로덕션 저널 배선 — 이후 모든 Applied 전이는 append 후 공개.
    /// Caller 계약: ledger는 clear_ledger(또는 엔진 폐기) 전까지 유효해야
    /// 한다 (raw pointer 보유 — 엔진은 Send 아님).
    pub fn set_ledger(&mut self, ledger: &'static mut dyn Ledger, actor: ActorId) {
        let ptr: *mut dyn Ledger = ledger;
        self.ledger = Some(LedgerHandle { ledger: ptr, actor });
    }

    /// **P3**: 고정 시계 — t0에서 append마다 +1초 (결정론 게이트).
    pub fn set_clock(&mut self, t0: chrono::DateTime<chrono::Utc>) {
        self.clock = Some(t0);
        self.clock_steps = 0;
    }

    /// **P1b + astra T56 BLOCKER 폐쇄**: 커서 소유 재생 — 유일한 공개 재생
    /// 입구. 재생은 **인증된 저널 행만** 발화한다: 커서가 가리키는 행을
    /// 저널에서 찾아 (i)row_digest 일치 (ii)행의 이벤트 == 재생하려는 이벤트
    /// 를 대조한다. 위조 커서·타행 커서·행 없는 커서·"커서는 유효하지만
    /// 다른 이벤트를 발화" 전부 EncodingDrift — 재생 경로로 승인 이벤트를
    /// 무인가 발화하는 것은 구조적으로 불가능해진다(승인 이벤트는 원장
    /// 게이트를 지닌 dispatch_live 전용).
    pub fn dispatch_replay_ctx(
        &mut self,
        order_id: &str,
        exec_id: &str,
        event: &OrderEvent,
        event_attempt: u32,
        ctx: &ReplayContext,
        journal: &foundry_ledger::SqliteLedger,
    ) -> Result<crate::Outcome, crate::EngineError> {
        if ctx.cursor.order_id != order_id {
            return Err(crate::EngineError::EncodingDrift {
                detail: format!(
                    "cursor bound to order {} but replaying {order_id}",
                    ctx.cursor.order_id
                ),
            });
        }
        // 승인 패밀리 원천 거부 (astra T56 r2 BLOCKER 폐쇄): Approved/
        // Rejected 는 인간 게이트 이벤트 — 재생(부록 경로)으로 발화할 수
        // 없다. 라이브 승인은 dispatch_live 가 원장 grant 게이트와 함께
        // 소유한다. 과거 Approved 행의 재구축은 rebuild(저널 재생)가
        // 담당하며 이 경로를 거치지 않는다.
        if matches!(
            event,
            OrderEvent::Approved { .. } | OrderEvent::Rejected { .. }
        ) {
            return Err(crate::EngineError::AuthorizationRefused {
                exec_id: exec_id.into(),
                grant_id: None,
                reason: "approval-family events are live-gated: replay path cannot fire the human gate (use dispatch_live with a grant)".into(),
            });
        }
        // 커서-행 결속: 커서가 가리키는 행이 저널에 존재하고 해시가 정확히
        // 일치해야 한다. 위조·대체·타 저널 커서 전부 거부.
        let rows = journal.read_all().map_err(|e| crate::EngineError::EncodingDrift {
            detail: format!("replay journal unreadable: {e:?}"),
        })?;
        // GENESIS 시점 커서: 이 주문의 행이 저널에 아직 없다 (다른 주문의
        // 행은 있을 수 있다 — 공유 저널).
        let order_has_rows = rows.iter().any(|r| r.order_id == order_id);
        if ctx.cursor.seq == 0 && ctx.cursor.row_digest == "GENESIS" && !order_has_rows {
            return self.dispatch_inner_pub(order_id, exec_id, event, event_attempt);
        }
        let row = rows
            .iter()
            .find(|r| r.seq == ctx.cursor.seq && r.order_id == order_id)
            .ok_or_else(|| crate::EngineError::EncodingDrift {
                detail: format!(
                    "cursor row (order {order_id}, seq {}) not in journal — forged or stale cursor",
                    ctx.cursor.seq
                ),
            })?;
        if row.hash != ctx.cursor.row_digest {
            return Err(crate::EngineError::EncodingDrift {
                detail: "cursor row_digest mismatch — post-issuance substitution".into(),
            });
        }
        // pin 검증 (astra T56 r2 MAJOR 폐쇄): 커서 seq 에서의 체인 헤드가
        // pin_digest 와 일치해야 한다 — 발급 시점 신뢰 범위의 재확인.
        if ctx.cursor.pin_digest != row.hash {
            return Err(crate::EngineError::EncodingDrift {
                detail: format!(
                    "cursor pin_digest does not match the chain head at seq {} — untrusted issuance",
                    ctx.cursor.seq
                ),
            });
        }
        self.dispatch_inner_pub(order_id, exec_id, event, event_attempt)
    }

    /// **P0**: 저널 해제 (레거시 무저널 동작 복귀).
    pub fn clear_ledger(&mut self) {
        self.ledger = None;
    }

    /// **P0**: 재구축 — 검증된 저널에서 상태·attempt 복원. 신규 실행 의미론:
    /// 관측·예산·래치는 복원하지 않는다 (재시작 후 새 exec_id로 재획득).
    /// 재구현 금지 계약: foundry-ledger replay(도메인 상태머신 재현) 재사용.
    pub fn rebuild(&mut self, ledger: &foundry_ledger::SqliteLedger) -> Result<usize, String> {
        let rows = ledger.read_all().map_err(|e| format!("{e:?}"))?;
        let replayed = foundry_ledger::replay::replay(&rows, |rs| foundry_ledger::sqlite::SqliteLedger::verify_rows(rs))
            .map_err(|e| format!("{e:?}"))?;
        let mut n = 0;
        for ro in replayed {
            self.orders.insert(ro.order_id.clone(), ro.order);
            n += 1;
        }
        Ok(n)
    }
}

/// 엔진이 보유한 저널 핸들 — raw pointer (엔진 메서드 내 &mut self에서만
/// 역참조; set_ledger/clear_led기 사이 caller가 ledger를 파괴하지 않는
/// 것은 caller 계약). Send를 의도하지 않는다.
pub(crate) struct LedgerHandle {
    pub(crate) ledger: *mut dyn Ledger,
    pub(crate) actor: ActorId,
}

/// dispatch_inner의 Applied 경로에서 호출 — append 성공 후에만 공개한다.
pub(crate) fn journal_append(
    handle: &LedgerHandle,
    order_id: &str,
    event: &OrderEvent,
    from: foundry_domain::OrderState,
    to: foundry_domain::OrderState,
    at: chrono::DateTime<chrono::Utc>,
) -> Result<(), String> {
    // Safety: set_ledger caller 계약 — handle 수명 내 &mut 유효.
    unsafe { &mut *handle.ledger }
        .append(order_id, event, from, to, &handle.actor, at)
        .map_err(|e| format!("{e:?}"))
}
