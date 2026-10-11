//! T56 P2 — 프로덕션 회복 어댑터.
//!
//! 부팅 시퀀스 (플랜 v5):
//!   1. 승인 원장 `recovery_scan` — PENDING 릴리즈(fence 무효화)·
//!      APPLYING CONSUMED 완결(fail-closed 계약).
//!   2. 저널 `verify_chain` — 무결성 확인 후 재구축 재료로.
//!   3. 엔진 `rebuild` — 상태·attempt 복원 (신규 실행 의미론:
//!      관측·예산·래치는 복원하지 않는다 — 재시작 후 새 exec_id).
//!
//! astra r6 잔여(프로덕션 recovery_scan 호출자 부재)의 폐쇄.

use crate::WorkflowEngine;
use foundry_approval::ApprovalLedger;
use foundry_ledger::SqliteLedger;

/// 회복 결과 — 게이트가 단정하는 관측 표면.
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryReport {
    /// 회복이 릴리즈한 grant (fence 무효화 포함).
    pub released: Vec<String>,
    /// **항상 비어 있음**(astra T56 r2 MINOR 문서화): recovery_scan 의
    /// 반환값은 릴리즈만 다루고, CONSUMED 완결 상태는 게이트가 get_grant
    /// 로 직접 단정한다. 이 필드는 호환용으로 유지된다.
    pub consumed: Vec<String>,
    /// 저널 체인 검증.
    pub journal_verified: bool,
    /// 재구축된 주문 수.
    pub rebuilt_orders: usize,
}

#[derive(Debug)]
pub enum RecoveryError {
    Ledger(String),
    JournalUnverified,
    Rebuild(String),
}

/// 부팅 회복 — 원장·저널·엔진을 순서대로 정리·재구축.
pub struct RecoveryRunner;

impl RecoveryRunner {
    /// 원장 회복 + 저널 검증 + 엔진 재구축을 한 시퀀스로.
    /// caller는 reopen된 (파일 기반) 원장·저널과 **신규 엔진**을 전달한다.
    pub fn run(
        ledger: &mut ApprovalLedger,
        journal: &SqliteLedger,
        engine: &mut WorkflowEngine,
    ) -> Result<RecoveryReport, RecoveryError> {
        // (1) 원장 회복 — 즉시 쓰기 잠금 하 PENDING 정리·APPLYING 완결.
        let released = ledger
            .recovery_scan()
            .map_err(|e| RecoveryError::Ledger(format!("{e:?}")))?;
        // 소비 완결 목록 — consumed 는 scan 후 상태 조회로 산출.
        let consumed: Vec<String> = Vec::new(); // released 에 없는 PENDING/APPLYING 은
        // recovery_scan 내부에서 정리된다; 세부 역할은 게이트가
        // get_grant 상태로 단정한다.

        // (2) 저널 체인 검증 — 재구축 재료의 무결성.
        let journal_verified = journal
            .verify_chain()
            .map_err(|e| RecoveryError::Ledger(format!("{e:?}")))?;

        // (3) 엔진 재구축 — 상태·attempt 만 (신규 실행 의미론).
        let rebuilt_orders = engine
            .rebuild(journal)
            .map_err(RecoveryError::Rebuild)?;

        Ok(RecoveryReport { released, consumed, journal_verified, rebuilt_orders })
    }

    /// (M7 T58 astra r4) **스캔 없는** 재구축 — 회복 코디네이터가 전역
    /// 스캔을 이미 1회 수행한 뒤 스트림별로 호출한다 (N+1 스캔 제거:
    /// 원장 스캔은 전역 자산이라 1회면 족하다). 저널 verify + 엔진
    /// rebuild 만 수행.
    pub fn run_after_scan(
        journal: &SqliteLedger,
        engine: &mut WorkflowEngine,
    ) -> Result<RecoveryReport, RecoveryError> {
        let journal_verified = journal
            .verify_chain()
            .map_err(|e| RecoveryError::Ledger(format!("{e:?}")))?;
        let rebuilt_orders = engine
            .rebuild(journal)
            .map_err(RecoveryError::Rebuild)?;
        Ok(RecoveryReport {
            released: Vec::new(),
            consumed: Vec::new(),
            journal_verified,
            rebuilt_orders,
        })
    }
}
