//! 전수주기 통합 — 고정 입력 풀 시퀀스 (플랜 v5).
//!
//! 프리셋 로드 → 활성화 → 자율 구간(커서 소유 재생) → 인간 승인 게이트
//! (dispatch_live+grant) → 출하 → 재시작 3단(원장 reopen·저널 verify·신규
//! 엔진 rebuild) → 발급 커서 재생 상태 동등.

use crate::common::{actor, terminal_commitment_from};
use foundry_domain::order::ExternalRef;
use foundry_domain::ports::Ledger as _;
use foundry_domain::{OrderEvent, OrderState};
use foundry_workflow::replay::{issue_replay_cursor, ReplayContext, VerifiedReplayCursor};
use foundry_workflow::recovery::RecoveryRunner;
use foundry_workflow::{ApprovalLedgerAdapter as LedgerHook, GrantRef, Outcome, WorkflowEngine};

const ORDER: &str = "o-full";
const EXEC: &str = "ex-full";
const GRANT: &str = "g-full";

fn sc() -> serde_json::Value {
    serde_json::json!({
        "order_id": ORDER, "intent": "ship", "artifact_digest": "dig-full", "root_scope": "",
    })
}

fn replay_ctx(rows: &[foundry_ledger::sqlite::LedgerRow]) -> ReplayContext {
    let commit = terminal_commitment_from(rows);
    ReplayContext {
        cursor: VerifiedReplayCursor {
            order_id: ORDER.into(),
            seq: commit.max_seq,
            row_digest: commit.chain_head.clone(),
            pin_digest: commit.chain_head.clone(),
        },
    }
}

#[test]
fn full_cycle_preset_to_shipped_with_cursor_replay() {
    // ── 고정 아티팩트 경로.
    let ap_path = std::env::temp_dir().join(format!("gate-fc-ap-{}.db", std::process::id()));
    let jr_path = std::env::temp_dir().join(format!("gate-fc-jr-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&ap_path);
    let _ = std::fs::remove_file(&jr_path);

    // ── 프리셋 로드·검증 (출하 조건).
    let preset_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent().unwrap().parent().unwrap()
        .join("presets/bmad-intake-pool.v3.json");
    let doc = foundry_workflow::dsl::load_preset(&preset_path).unwrap();
    assert!(foundry_workflow::validator::validate(
        &doc,
        &foundry_domain::workflow_encoding::EmbeddedWorkflow::frozen_v1(),
    ).is_ok());

    // ── 원장: 활성화 토큰 + 승인 grant (G13 결속은 별도 게이트).
    let mut ap = foundry_approval::ApprovalLedger::open(&ap_path).unwrap();
    ap.activate_workflow("root-fc", "ship", "tok-fc").unwrap();
    assert!(ap.is_activated("root-fc", "ship").unwrap());
    ap.issue_grant(GRANT, "tok-grant", &sc()).unwrap();

    // ── 배선 엔진 — 전이마다 프로덕션 저널에 append.
    let jr_static: &'static mut foundry_ledger::SqliteLedger =
        Box::leak(Box::new(foundry_ledger::SqliteLedger::open(&jr_path).unwrap()));
    let mut e = WorkflowEngine::embedded_default();
    e.set_ledger(jr_static, actor("full-cycle"));
    e.set_clock(chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap());
    e.enroll(foundry_domain::order::Order::new(
        ORDER.into(),
        ExternalRef::cron("src", "ext"),
        chrono::Utc::now(),
    ));
    e.acquire(ORDER, EXEC).unwrap();

    // ── 자율 구간 — 커서 소유 재생 경로 (Received→FurnaceHold).
    let autonomous = [
        OrderEvent::Queued,
        OrderEvent::DieStarted,
        OrderEvent::DieSucceeded { attempt: 1 },
        OrderEvent::InspectionPassed,
    ];
    let mut states_seen = vec![OrderState::Received];
    for ev in &autonomous {
        let att = e.order(ORDER).unwrap().attempt().max(1);
        let rows = foundry_ledger::SqliteLedger::open(&jr_path).unwrap().read_all().unwrap();
        e.dispatch_replay_ctx(ORDER, EXEC, ev, att, &replay_ctx(&rows)).unwrap();
        states_seen.push(e.order(ORDER).unwrap().state());
    }
    assert_eq!(e.order(ORDER).unwrap().state(), OrderState::FurnaceHold);

    // ── 인간 승인 게이트 — dispatch_live + grant.
    let mut hook = LedgerHook(ap);
    let grant = GrantRef { grant_id: GRANT.into() };
    let out = e.dispatch_live(
        ORDER, EXEC,
        &OrderEvent::Approved { intent: "ship".into(), artifact_digest: "dig-full".into() },
        1, Some(&grant), &mut hook,
    ).unwrap();
    assert_eq!(out, Outcome::Applied);
    states_seen.push(OrderState::Publishing);
    assert_eq!(e.order(ORDER).unwrap().state(), OrderState::Publishing);
    // 원장 CONSUMED.
    assert_eq!(hook.0.get_grant(GRANT).unwrap().state, foundry_approval::GrantState::Consumed);
    drop(hook);

    // ── 출하 확정.
    let rows = foundry_ledger::SqliteLedger::open(&jr_path).unwrap().read_all().unwrap();
    e.dispatch_replay_ctx(ORDER, EXEC, &OrderEvent::PublishConfirmed { pr_url: "https://pr/full".into() }, 1, &replay_ctx(&rows)).unwrap();
    states_seen.push(OrderState::Shipped);
    assert_eq!(e.order(ORDER).unwrap().state(), OrderState::Shipped);

    // ── 저널 체인 green + 상태열·순서 단정.
    let rows = foundry_ledger::SqliteLedger::open(&jr_path).unwrap().read_all().unwrap();
    assert!(foundry_ledger::SqliteLedger::verify_rows(&rows), "chain verifies");
    let expected_states = [
        OrderState::Received, OrderState::Queued, OrderState::Working,
        OrderState::Inspecting, OrderState::FurnaceHold,
        OrderState::Publishing, OrderState::Shipped,
    ];
    assert_eq!(states_seen, expected_states, "state series exact");
    // 관측 순서 — to_state 열이 상태열과 동일 (전이 순서=관측 순서).
    // 저널 to_state 명명은 도메인 직렬화 계약("inspecting"="review",
    // "furnace_hold"="waiting") — 순서·개수로 동등 단정 (명명 매핑 포함).
    let to_states: Vec<String> = rows.iter().map(|r| r.to_state.clone()).collect();
    let name_of = |s: OrderState| -> String {
        foundry_ledger::SqliteLedger::open(&jr_path) // 사용하지 않음 — 컴파일 통과용
            .map(|_| ())
            .ok();
        match s {
            OrderState::Queued => "queued",
            OrderState::Working => "working",
            OrderState::Inspecting => "review",
            OrderState::FurnaceHold => "waiting",
            OrderState::Publishing => "publishing",
            OrderState::Shipped => "shipped",
            _ => "other",
        }
        .to_string()
    };
    let want: Vec<String> = expected_states[1..].iter().map(|s| name_of(*s)).collect();
    assert_eq!(to_states, want, "observation order exact (journal to_state series)");

    // ── 재시작 3단 — 회복·검증·재구축.
    let mut ap2 = foundry_approval::ApprovalLedger::open(&ap_path).unwrap();
    let jr2 = foundry_ledger::SqliteLedger::open(&jr_path).unwrap();
    let mut fresh = WorkflowEngine::embedded_default();
    let report = RecoveryRunner::run(&mut ap2, &jr2, &mut fresh).unwrap();
    assert!(report.journal_verified);
    assert_eq!(report.rebuilt_orders, 1);
    assert_eq!(fresh.order(ORDER).unwrap().state(), OrderState::Shipped, "rebuilt to terminal");

    // ── 발급 커서 재생 — 상태 재구축 동일 (M1-M5 동등 인코딩).
    let commit = terminal_commitment_from(&rows);
    let cursor = issue_replay_cursor(&rows, &commit, ORDER, rows.last().unwrap().seq).unwrap();
    let mut replayed = WorkflowEngine::embedded_default();
    replayed.rebuild(&jr2).unwrap();
    assert_eq!(replayed.order(ORDER).unwrap().state(), fresh.order(ORDER).unwrap().state());
    assert_eq!(cursor.pin_digest, commit.chain_head);
}
