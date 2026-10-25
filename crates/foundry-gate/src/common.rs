//! 게이트 공용 픽스처 — 엔진·원장·저널·주문·컨텍스트 팩토리.

#![allow(deprecated)] // W6a clippy debt: preserve the pinned tempfile behavior in the existing fixture
#![allow(clippy::filter_next)] // W6a clippy debt: preserve the existing reverse-search iterator behavior
#![allow(unused_variables)] // W6a clippy debt: retain names used by conditional or diagnostic configurations

use foundry_domain::OrderState;
use foundry_domain::order::{ExternalRef, Order, OrderEvent};
use foundry_domain::ports::{ActorId, ActorKind};
use foundry_ledger::SqliteLedger;
use foundry_workflow::WorkflowEngine;

pub fn actor(tag: &str) -> ActorId {
    ActorId {
        kind: ActorKind::Agent,
        source: "gate".into(),
        id: tag.into(),
    }
}

/// FurnaceHold 에 도달한 주문 — 승인 게이트 검증의 표준 시작 상태.
pub fn furnace_order(id: &str) -> Order {
    let mut o = Order::new(
        id.to_string(),
        ExternalRef::cron("src", "ref"),
        chrono::Utc::now(),
    );
    o.force_state_for_test(OrderState::FurnaceHold, 1);
    o
}

pub fn approval_event(intent: &str, digest: &str) -> OrderEvent {
    OrderEvent::Approved {
        intent: intent.into(),
        artifact_digest: digest.into(),
    }
}

/// 게이트 표준 엔진 (frozen_v1·여유 예산).
pub fn engine() -> WorkflowEngine {
    WorkflowEngine::embedded_default()
}

/// 파일 기반 임시 저널 (A′ 재시작 검증용 — reopen 가능).
pub fn file_journal(tag: &str) -> (SqliteLedger, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.into_path().join(format!("gate-{tag}.db"));
    // caller 가 dir 을 버리지 않도록 경로를 /tmp 고정 위치로.
    let path = std::env::temp_dir().join(format!("gate-{tag}-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    (SqliteLedger::open(&path).unwrap(), path)
}

/// 게이트 표준 재생 진입 — 커서는 **발급 또는 제네시스** 경로로만 만든다
/// (P1b+r2 계약: 수동 커서 구성은 존재하지 않는다).
pub fn replay_event(
    e: &mut WorkflowEngine,
    journal: &SqliteLedger,
    order_id: &str,
    exec_id: &str,
    event: &foundry_domain::OrderEvent,
    attempt: u32,
) -> Result<foundry_workflow::Outcome, foundry_workflow::EngineError> {
    let ctx = replay_ctx_for(order_id, &journal.read_all().unwrap());
    e.dispatch_replay_ctx(order_id, exec_id, event, attempt, &ctx, journal)
}

/// 터미널 커밋먼트 — 저널 전체 행에서 촬영 (운영자 대역 외 보관 역할).
/// 이 주문의 직전 커밋 행을 가리키는 재생 컨텍스트 — 신뢰 경로:
/// 행이 있으면 커밋먼트 경유 **발급**(issue_replay_cursor), 없으면
/// **제네시스**(유일 부트스트랩).
pub fn replay_ctx_for(
    order: &str,
    rows: &[foundry_ledger::sqlite::LedgerRow],
) -> foundry_workflow::replay::ReplayContext {
    let last_mine = rows.iter().filter(|r| r.order_id == order).next_back();
    let c = terminal_commitment_from(rows);
    let cursor = match last_mine {
        Some(r) => foundry_workflow::replay::issue_replay_cursor(rows, &c, order, r.seq)
            .expect("trusted issuance from the terminal commitment"),
        None => foundry_workflow::replay::VerifiedReplayCursor::genesis(order),
    };
    foundry_workflow::replay::ReplayContext { cursor }
}

pub fn terminal_commitment_from(
    rows: &[foundry_ledger::sqlite::LedgerRow],
) -> foundry_workflow::replay::TerminalCommitment {
    foundry_workflow::replay::TerminalCommitment {
        max_seq: rows.last().map(|r| r.seq).unwrap_or(0),
        count: rows.len() as i64,
        chain_head: rows
            .last()
            .map(|r| r.hash.clone())
            .unwrap_or_else(|| "GENESIS".into()),
    }
}

pub fn terminal_commitment(journal: &SqliteLedger) -> foundry_workflow::replay::TerminalCommitment {
    terminal_commitment_from(&journal.read_all().unwrap())
}
