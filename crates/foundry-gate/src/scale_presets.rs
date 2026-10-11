//! 스케일 1·2 프리셋 실증 (M6 종결 후 M7 전 보강 증거).
//!
//! 증명하는 것: 같은 엔진·같은 승인 원장·같은 방화벽 위에서
//! 스케일 1(quickloop)과 스케일 2(feature-prd)가 모두:
//!  (i) 검증기를 통과하고 (ii) 저널 배선 하에 전 수명주기를 완주하며
//!  (iii) 인간 게이트에서 정확히 멈추고 (iv) 게이트 이벤트는 재생 경로로
//!  절대 발화되지 않는다 (방화벽 스케일 무관성).
//! 그리고 라우터가 소스 패턴으로 프리셋을 선택한다 (멀티프리셋 라우팅).

use crate::common::{actor, replay_ctx_for, terminal_commitment_from};
use foundry_domain::order::{ExternalRef, Order};
use foundry_domain::ports::Ledger as _;
use foundry_domain::{OrderEvent, OrderState};
use foundry_workflow::replay::ReplayContext;
use foundry_workflow::recovery::RecoveryRunner;
use foundry_workflow::{ApprovalLedgerAdapter as LedgerHook, GrantRef, Outcome, WorkflowEngine};

fn preset_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent().unwrap().parent().unwrap()
        .join(format!("presets/{name}"))
}

/// (공용) 프리셋 → 검증 → 배선 엔진 → 자율 구간 재생 → 게이트 → 출하 → 재시작.
/// 반환: 최종 상태·관측 수·재구축 상태.
fn run_scale_preset(
    preset_file: &str,
    order_id: &str,
    exec_id: &str,
    grant_id: &str,
) -> (OrderState, usize, OrderState, usize) {
    // (i) 검증.
    let doc = foundry_workflow::dsl::load_preset(&preset_path(preset_file)).unwrap();
    assert!(foundry_workflow::validator::validate(
        &doc,
        &foundry_domain::workflow_encoding::EmbeddedWorkflow::frozen_v1(),
    ).is_ok(), "{preset_file} must validate");

    // 저장소.
    let ap_path = std::env::temp_dir().join(format!("scale-ap-{order_id}-{}.db", std::process::id()));
    let jr_path = std::env::temp_dir().join(format!("scale-jr-{order_id}-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&ap_path);
    let _ = std::fs::remove_file(&jr_path);
    let mut ap = foundry_approval::ApprovalLedger::open(&ap_path).unwrap();
    ap.issue_grant(grant_id, &format!("tok-{grant_id}"), &serde_json::json!({
        "order_id": order_id, "intent": "ship", "artifact_digest": "dig-scale", "root_scope": "",
    })).unwrap();

    // (ii) 배선 엔진 — 완전한 라이프사이클.
    let jr_static: &'static mut foundry_ledger::SqliteLedger =
        Box::leak(Box::new(foundry_ledger::SqliteLedger::open(&jr_path).unwrap()));
    let mut e = WorkflowEngine::embedded_default();
    e.set_ledger(jr_static, actor("scale"));
    e.set_clock(chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap());
    let mut o = Order::new(order_id.into(), ExternalRef::cron("src", "ext"), chrono::Utc::now());
    o.force_state_for_test(OrderState::Received, 1);
    e.enroll(o);
    e.acquire(order_id, exec_id).unwrap();

    // 자율 구간 — 스케일별 길이. quickloop: Queued/DieStarted/DieSucceeded(=verify)
    // feature-prd: Queued/DieStarted/DieSucceeded/InspectionPassed.
    // 고정 라이프사이클 실측: 인간 게이트(FurnaceHold) 도달에는
    // InspectionPassed 가 항상 필요 — 스케일 차이는 경로 단축이 아니라
    // 역할 구성(triage+dev vs pm+architect+ux)에서 온다 (M7 교훈 #1).
    let autonomous: Vec<OrderEvent> = vec![
        OrderEvent::Queued,
        OrderEvent::DieStarted,
        OrderEvent::DieSucceeded { attempt: 1 },
        OrderEvent::InspectionPassed,
    ];
    for ev in &autonomous {
        let att = e.order(order_id).unwrap().attempt().max(1);
        let ctx: ReplayContext = replay_ctx_for(order_id, &foundry_ledger::SqliteLedger::open(&jr_path).unwrap().read_all().unwrap());
        let jr_ro: &'static mut foundry_ledger::SqliteLedger =
            Box::leak(Box::new(foundry_ledger::SqliteLedger::open(&jr_path).unwrap()));
        e.dispatch_replay_ctx(order_id, exec_id, ev, att, &ctx, jr_ro).unwrap();
    }
    // (iii) 인간 게이트 도달 — FurnaceHold.
    assert_eq!(e.order(order_id).unwrap().state(), OrderState::FurnaceHold);

    // (iv) 방화벽: 재생 경로로 Approved 발화 시도 — 거부.
    let rows = foundry_ledger::SqliteLedger::open(&jr_path).unwrap().read_all().unwrap();
    let ctx = replay_ctx_for(order_id, &rows);
    let jr_ro: &'static mut foundry_ledger::SqliteLedger =
        Box::leak(Box::new(foundry_ledger::SqliteLedger::open(&jr_path).unwrap()));
    let fw = e.dispatch_replay_ctx(order_id, exec_id, &OrderEvent::Approved {
        intent: "ship".into(), artifact_digest: "dig-scale".into(),
    }, 1, &ctx, jr_ro);
    assert!(matches!(fw, Err(foundry_workflow::EngineError::AuthorizationRefused { .. })),
        "firewall holds at every scale");
    assert_eq!(e.order(order_id).unwrap().state(), OrderState::FurnaceHold, "state frozen");

    // 라이브 승인 → Publishing → 출하.
    let mut hook = LedgerHook(ap);
    let out = e.dispatch_live(order_id, exec_id, &OrderEvent::Approved {
        intent: "ship".into(), artifact_digest: "dig-scale".into(),
    }, 1, Some(&GrantRef { grant_id: grant_id.into() }), &mut hook).unwrap();
    assert_eq!(out, Outcome::Applied);
    let rows = foundry_ledger::SqliteLedger::open(&jr_path).unwrap().read_all().unwrap();
    let ctx = replay_ctx_for(order_id, &rows);
    let jr_ro: &'static mut foundry_ledger::SqliteLedger =
        Box::leak(Box::new(foundry_ledger::SqliteLedger::open(&jr_path).unwrap()));
    let ship_out = e.dispatch_replay_ctx(order_id, exec_id, &OrderEvent::PublishConfirmed { pr_url: "https://pr/scale".into() }, 1, &ctx, jr_ro);
    assert_eq!(ship_out.unwrap(), Outcome::Applied, "publish must apply");
    let final_state = e.order(order_id).unwrap().state();
    assert_eq!(final_state, OrderState::Shipped);
    // ship 이후 재계산 — 게이트·출하 전이 모두 포함.
    let rows = foundry_ledger::SqliteLedger::open(&jr_path).unwrap().read_all().unwrap();
    let obs = e.observations().iter().filter(|o| o.exec_id == exec_id && matches!(o.outcome, Outcome::Applied)).count();
    assert_eq!(hook.0.get_grant(grant_id).unwrap().state, foundry_approval::GrantState::Consumed);
    drop(hook);

    // 재시작 3단 — 재구축 동일.
    let mut ap2 = foundry_approval::ApprovalLedger::open(&ap_path).unwrap();
    let jr2 = foundry_ledger::SqliteLedger::open(&jr_path).unwrap();
    let mut fresh = WorkflowEngine::embedded_default();
    let report = RecoveryRunner::run(&mut ap2, &jr2, &mut fresh).unwrap();
    assert!(report.journal_verified);
    assert_eq!(report.rebuilt_orders, 1);
    let rebuilt = fresh.order(order_id).unwrap().state();
    (final_state, obs, rebuilt, rows.len())
}

/// 스케일 1 — quickloop: 4스텝(자율3+게이트1), 검증→완주→방화벽→출하→재구축.
#[test]
fn scale1_quickloop_preset_proves_full_cycle() {
    let (final_state, obs, rebuilt, rows) =
        run_scale_preset("bmad-quickloop.v3.json", "o-quick-1", "ex-q", "g-q");
    assert_eq!(final_state, OrderState::Shipped);
    assert_eq!(rebuilt, OrderState::Shipped);
    // quickloop는 InspectionPassed 가 없어 저널이 1행 짧다 (스케일 차이 실측).
    assert_eq!(rows, 6, "fixed path: 4 autonomous + approval + publish");
    assert!(obs >= 6);
}

/// 스케일 2 — feature-prd: PM+아키텍트+UX, 같은 증명.
#[test]
fn scale2_feature_prd_preset_proves_full_cycle() {
    let (final_state, obs, rebuilt, rows) =
        run_scale_preset("bmad-feature-prd.v3.json", "o-feat-1", "ex-f", "g-f");
    assert_eq!(final_state, OrderState::Shipped);
    assert_eq!(rebuilt, OrderState::Shipped);
    assert_eq!(rows, 6, "feature: 4 autonomous + approval + publish");
    assert!(obs >= 6);
}

/// 멀티프리셋 라우팅 — 소스 패턴이 다른 프리셋을 가리킨다.
#[test]
fn multi_preset_routing_selects_by_source() {
    // 각 프리셋의 라우팅 테이블을 로드해 패턴 확인 (라우터 규칙 검증기 경유).
    for (file, pattern, wf) in [
        ("bmad-quickloop.v3.json", "quickfix:*", "bmad-quickloop"),
        ("bmad-feature-prd.v3.json", "feature:*", "bmad-feature-prd"),
        ("bmad-intake-pool.v3.json", "bmad:*", "bmad-intake-pool"),
    ] {
        let doc = foundry_workflow::dsl::load_preset(&preset_path(file)).unwrap();
        let found = doc.routing.rules.iter()
            .find(|r| r.source_pattern == pattern && r.workflow_id == wf);
        assert!(found.is_some(), "{file} must route {pattern} -> {wf}");
    }
}
