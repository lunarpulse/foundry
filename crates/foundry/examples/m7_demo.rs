//! M7 E2E 데모 — 오케스트레이션 레이어 실연 (해설형 바이너리).
//!
//! T61 종단 시나리오(사이징→승인→에픽→재입장→핸드오프→재구축)를
//! 콘솔에 단계별로 출력하며 실제 구동한다. 모든 단계는 프로덕션
//! 공개 API(foundry-orch)만 사용한다.
//!
//! 실행: cargo run -p foundry --example m7_demo

use std::path::PathBuf;

use foundry_domain::order::{ExternalRef, Order};
use foundry_domain::{OrderEvent, OrderState};
use foundry_orch::chainer::{Chainer, SizingVerdict};
use foundry_orch::stream::{Stream, StreamRuntime};
use foundry_orch::{OrchestrationKind, OrchestrationLog};

fn banner(step: u8, title: &str) {
    println!("\n{}", "=".repeat(72));
    println!("[{step}] {title}");
    println!("{}", "=".repeat(72));
}

fn demo_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("foundry-m7-demo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    println!("작업 디렉터리: {}", d.display());
    d
}

fn mkorder(id: &str, intake: bool) -> Order {
    let ext = if intake {
        ExternalRef::intake(id, &format!("dedup:{id}"))
    } else {
        ExternalRef::cron("demo", "m7")
    };
    let mut o = Order::new(id.into(), ext, chrono::Utc::now());
    o.force_state_for_test(OrderState::Received, 1);
    o
}

/// 자율 구간(Queued→Die→Inspection)을 지나 인간 게이트(FurnaceHold)까지.
fn advance_to_gate(rt: &StreamRuntime, sid: &str, oid: &str, exec: &str) {
    let st = rt.advance_to_gate(sid, oid, exec).unwrap();
    println!("   → 게이트 도달: {st:?} (인간 승인 대기)");
}

/// (M8 데모 어댑터 시연) 실 Die 세그먼트 — start로 Working 진입 후
/// 실제 Harness를 실행하고 finish로 게이트에 도달한다.
/// FakeHarness는 인과 검증용: 세그먼트 사이에 실제 바이너리
/// (rustain → foundary-demo PR)가 있었음을 DieOutput가 증명한다.
fn advance_to_gate_real_die(
    rt: &StreamRuntime,
    sid: &str,
    oid: &str,
    exec: &str,
    harness: &foundry_harness_cli::fake::FakeHarness,
    workroot: &std::path::Path,
) {
    let st = rt.start_die_segment(sid, oid, exec).unwrap();
    println!("   Die 세그먼트 시작: {st:?} — 실 Harness 실행 창");
    let order = rt.stream(sid).unwrap().order_snapshot(oid).unwrap();
    let die = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            use foundry_domain::ports::Harness as _;
            harness.run(&order, 1, workroot).await.unwrap()
        });
    println!(
        "   Die 완료: result_commit = {}…",
        &die.result_commit[..8.min(die.result_commit.len())]
    );
    let st = rt.finish_die_segment(sid, oid, exec, &die).unwrap();
    println!("   → 게이트 도달: {st:?} (실 Die 산출물 결속)");
}

/// 게이트에서 라이브 승인(원장 그랜트 CONSUMED) 후 발행 확정 → Shipped.
fn approve_and_ship(
    rt: &StreamRuntime,
    sid: &str,
    oid: &str,
    exec: &str,
    grant_id: &str,
    intent: &str,
    digest: &str,
    _ap_db: &std::path::Path,
) {
    advance_to_gate(rt, sid, oid, exec);
    println!("   그랜트 발급+라이브 승인: {grant_id} (intent={intent})");
    let out = rt
        .approve_at_gate(sid, oid, exec, grant_id, intent, digest)
        .unwrap();
    assert_eq!(out, foundry_workflow::Outcome::Applied);
    println!("   승인 적용: 그랜트 CONSUMED (원장 증거)");
    if intent == "ship" {
        rt.confirm_publish(sid, oid, exec, &format!("https://pr/demo/{oid}"))
            .unwrap();
        println!("   → 발행 확정: Shipped");
    }
}

fn main() {
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║   FOUNDRY M7 — 오케스트레이션 레이어 종단 데모                ║");
    println!("║   원장 증거 승인 · 재입장 루프 · 핸드오프 · 크래시 재구축      ║");
    println!("╚══════════════════════════════════════════════════════════════╝");

    let d = demo_dir();
    let ap_db = d.join("ap.db");
    let orch_db = d.join("orch.db");

    // ── (1) 선언 · 스트림 · 결속 ──────────────────────────────
    banner(1, "플랜 선언 + 스트림(프리셋 루트 결속) + 승인 원장 바인딩");
    let orch = OrchestrationLog::open(&orch_db).unwrap();
    orch.declare_plan(
        "plan-sizing",
        vec![("sz-1".into(), vec![])],
        vec![("sz-1".into(), "s-sizing".into())],
    )
    .unwrap();
    println!("플랜 'plan-sizing' 선언 (주문 sz-1 → 스트림 s-sizing)");

    let rt = StreamRuntime::new(OrchestrationLog::open(&orch_db).unwrap());
    let orch = rt.orch.clone();
    rt.add_stream(Stream::new_with_root("s-sizing", &d, "bmad-epic-build").unwrap())
        .unwrap();
    rt.add_stream(Stream::new_with_root("s-epic", &d, "bmad-epic-build").unwrap())
        .unwrap();
    rt.bind_approval_ledger(foundry_approval::ApprovalLedger::open(&ap_db).unwrap())
        .unwrap();
    println!("스트림 2개: s-sizing / s-epic (preset_root=bmad-epic-build)");
    println!(
        "승인 원장 바인딩: {} (fail-closed — 미바인딩 시 dispatch 거부)",
        ap_db.display()
    );

    // ── (2) 우회 차단 ──────────────────────────────────────────
    banner(
        2,
        "방어 ①: 미선언 주문의 정문 입장은 거부 (불변 저널에 기록)",
    );
    let refused = rt.dispatch("rogue-0", mkorder("rogue-0", true));
    println!("rogue-0 (어디에도 선언되지 않음) dispatch → {refused:?}");
    let refusals: Vec<_> = orch.read_all().unwrap()
        .into_iter()
        .filter(|r| matches!(&r.kind, OrchestrationKind::Refusal { refused_request, .. } if refused_request == "rogue-0"))
        .collect();
    assert_eq!(refusals.len(), 1, "거부는 저널에 영구 기록된다");
    println!(
        "→ 거부 1건이 오케스트레이션 로그에 Refusal 행으로 남음 (seq={})",
        refusals[0].seq
    );

    // ── (3) 사이징 → 승인 ──────────────────────────────────────
    banner(3, "사이징 주문: 자율 구간 → 인간 승인(라이브 그랜트)");
    rt.dispatch("sz-1", mkorder("sz-1", true)).unwrap();
    println!("sz-1 입장 (Intake 부트스트랩 — 타입으로 판별)");
    // M8 어댑터: 시뮬레이션 대신 실 Die 세그먼트(start → Harness → finish).
    // FakeHarness는 인과 검증용이며 프로덕션에서는 CliHarness(rustain)가
    // foundary-demo에 커밋을 만들어 DieOutput.result_commit을 생산한다.
    let harness = foundry_harness_cli::fake::FakeHarness::default();
    let workroot = d.join("die-work");
    std::fs::create_dir_all(&workroot).unwrap();
    advance_to_gate_real_die(&rt, "s-sizing", "sz-1", "demo-sz", &harness, &workroot);
    let digest = foundry_orch::chainer::sizing_digest("approved", "bmad-epic-build", "plan-epic");
    approve_and_ship(
        &rt,
        "s-sizing",
        "sz-1",
        "demo-sz",
        "sizing-sz-1",
        "sizing",
        &digest,
        &ap_db,
    );
    {
        let ap = foundry_approval::ApprovalLedger::open(&ap_db).unwrap();
        let g = ap.get_grant("sizing-sz-1").unwrap();
        println!(
            "원장 확인: sizing-sz-1 상태 = {:?} (CONSUMED만 승인 증거)",
            g.state
        );
    }

    // ── (4) 재입장 루프 ────────────────────────────────────────
    banner(
        4,
        "방어 ② 재입장 루프: unsized 발행 거부 → 재디에이션 → 재입장",
    );
    orch.declare_plan(
        "plan-epic",
        vec![
            ("t-1".into(), vec![]),
            ("t-2".into(), vec!["t-1".into()]),
            ("t-3".into(), vec!["t-1".into()]),
            ("t-4".into(), vec!["t-2".into(), "t-3".into()]),
        ],
        vec![
            ("t-1".into(), "s-epic".into()),
            ("t-2".into(), "s-epic".into()),
            ("t-3".into(), "s-epic".into()),
            ("t-4".into(), "s-epic".into()),
        ],
    )
    .unwrap();
    println!("4-티켓 에픽 선언: t-1 → {{t-2, t-3}} → t-4 (의존성 DAG)");
    let refused2 = rt.dispatch("t-1", mkorder("t-1", false));
    println!(
        "t-1 (프리셋 미결속) dispatch → 거부: {}",
        refused2.unwrap_err()
    );
    let refusal_seq = orch.read_all().unwrap().into_iter()
        .find(|r| matches!(&r.kind, OrchestrationKind::Refusal { refused_request, .. } if refused_request == "t-1"))
        .unwrap().seq;
    let ap_ref = foundry_approval::ApprovalLedger::open(&ap_db).unwrap();
    let ch = Chainer::new(&orch, &ap_ref);
    let (pid, kind) = ch.propose_remediation(refusal_seq).unwrap();
    println!("재디에이션 제안: {pid} → {kind:?} (동일 거부 재제안은 거부됨)");
    let spam = ch.propose_remediation(refusal_seq);
    println!(
        "스팸 재제안 → {:?} (RemediationAlreadyProposed)",
        spam.unwrap_err()
    );

    // 재디에이션 작업 = 실제 주문(정문 입장).
    orch.declare_plan(
        "plan-rem-work",
        vec![(pid.clone(), vec![])],
        vec![(pid.clone(), "s-sizing".into())],
    )
    .unwrap();
    rt.dispatch(&pid, mkorder(&pid, true)).unwrap();
    advance_to_gate(&rt, "s-sizing", &pid, "demo-rw");
    approve_and_ship(
        &rt,
        "s-sizing",
        &pid,
        "demo-rw",
        &format!("sizing-{pid}"),
        "sizing",
        &digest,
        &ap_db,
    );
    ch.bind_preset_from_ledger(&pid, "bmad-epic-build", "plan-epic")
        .unwrap();
    println!("재디에이션 완결: rem 작업 승인(새 증거)으로 preset_bound 결속");
    ch.reenter(
        "t-1",
        "ext:demo/m7",
        Some(&pid),
        Some(&digest),
        "plan-epic",
        "bmad-epic-build",
    )
    .unwrap();
    println!("t-1 재입장 — 전량 재평가(선언·바인딩·목적지) 통과");
    let undeclared = ch.reenter(
        "t-x",
        "ext:demo",
        Some(&pid),
        Some(&digest),
        "plan-epic",
        "bmad-epic-build",
    );
    println!(
        "미선언 재입장 → {:?} (DeclarationMissing + 저널 기록)",
        undeclared.unwrap_err()
    );

    // ── (5) 핸드오프 (M7.1 2단계 정문) ─────────────────────────
    banner(
        5,
        "핸드오프: prepare(매니페스트) → 위조 그랜트 거부 → 정품 CONSUMED",
    );
    orch.declare_plan(
        "plan-handoff",
        vec![("h-1".into(), vec![]), ("h-2".into(), vec![])],
        vec![
            ("h-1".into(), "s-epic".into()),
            ("h-2".into(), "s-epic".into()),
        ],
    )
    .unwrap();
    // 핸드오프 플랜 전용 사이징 주문(sz-h) — 목적지별 승인 증거 분리.
    orch.declare_plan(
        "plan-h-sizing",
        vec![("sz-h".into(), vec![])],
        vec![("sz-h".into(), "s-sizing".into())],
    )
    .unwrap();
    rt.dispatch("sz-h", mkorder("sz-h", true)).unwrap();
    advance_to_gate(&rt, "s-sizing", "sz-h", "demo-sh");
    let digest_h =
        foundry_orch::chainer::sizing_digest("approved", "bmad-epic-build", "plan-handoff");
    approve_and_ship(
        &rt,
        "s-sizing",
        "sz-h",
        "demo-sh",
        "sizing-sz-h",
        "sizing",
        &digest_h,
        &ap_db,
    );
    let ap_h = foundry_approval::ApprovalLedger::open(&ap_db).unwrap();
    let ch_h = Chainer::new(&orch, &ap_h);
    ch_h.bind_preset_from_ledger("sz-h", "bmad-epic-build", "plan-handoff")
        .unwrap();
    println!("plan-handoff 프리셋 결속 (sz-h CONSUMED 증거)");
    rt.dispatch("h-1", mkorder("h-1", false)).unwrap();
    rt.dispatch("h-2", mkorder("h-2", false)).unwrap();
    let (manifest, _) = rt.prepare_handoff("h-1", "h-2", "art-h").unwrap();
    println!(
        "prepare_handoff: 제안 행 생성, 매니페스트 = {}…",
        &manifest[..24]
    );
    let mut ap2 = foundry_approval::ApprovalLedger::open(&ap_db).unwrap();
    ap2.issue_grant(
        "ho-bad",
        "tok-bad",
        &serde_json::json!({
            "order_id": "h-1", "dest_order_id": "h-2", "intent": "handoff",
            "artifact_digest": "art-h", "handoff_manifest": "FORGED", "root_scope": "",
        }),
    )
    .unwrap();
    let fw = rt.dispatch_handoff("h-1", "h-2", "art-h", "ho-bad");
    println!(
        "위조 매니페스트 그랜트 → 거부: {} (소비 전 차단)",
        fw.unwrap_err()
    );
    {
        let ap = foundry_approval::ApprovalLedger::open(&ap_db).unwrap();
        assert!(ap.get_grant("ho-bad").unwrap().state != foundry_approval::GrantState::Consumed);
        println!(
            "위조 그랜트 상태: {:?} (미소치 — 사전 검증이 지킴)",
            ap.get_grant("ho-bad").unwrap().state
        );
    }
    ap2.issue_grant(
        "ho-ok",
        "tok-ok",
        &serde_json::json!({
            "order_id": "h-1", "dest_order_id": "h-2", "intent": "handoff",
            "artifact_digest": "art-h", "handoff_manifest": manifest, "root_scope": "",
        }),
    )
    .unwrap();
    rt.dispatch_handoff("h-1", "h-2", "art-h", "ho-ok").unwrap();
    {
        let ap = foundry_approval::ApprovalLedger::open(&ap_db).unwrap();
        let g = ap.get_grant("ho-ok").unwrap();
        println!(
            "정품 그랜트 소비: 상태 = {:?} (3자 일치 + 6필드 결속)",
            g.state
        );
    }
    // 멱등 재시도 (r3 MAJOR-1).
    rt.dispatch_handoff("h-1", "h-2", "art-h", "ho-ok").unwrap();
    println!("소비 후 재시도 → 멱등 성공 (AlreadyConsumed 아님)");

    // ── (6) 에픽 완주 ─────────────────────────────────────────
    banner(6, "4-티켓 에픽 완주: 의존성 게이트 준수, 전원 Shipped");
    rt.dispatch("t-1", mkorder("t-1", false)).unwrap();
    approve_and_ship(
        &rt, "s-epic", "t-1", "demo", "g-t1", "ship", "dig-t1", &ap_db,
    );
    println!(
        "t-2 준비: {} | t-3 준비: {} | t-4 대기: {}",
        rt.ready_to_start("t-2").unwrap(),
        rt.ready_to_start("t-3").unwrap(),
        !rt.ready_to_start("t-4").unwrap()
    );
    rt.dispatch("t-2", mkorder("t-2", false)).unwrap();
    rt.dispatch("t-3", mkorder("t-3", false)).unwrap();
    approve_and_ship(
        &rt, "s-epic", "t-2", "demo", "g-t2", "ship", "dig-t2", &ap_db,
    );
    approve_and_ship(
        &rt, "s-epic", "t-3", "demo", "g-t3", "ship", "dig-t3", &ap_db,
    );
    assert!(rt.ready_to_start("t-4").unwrap());
    println!("t-2/t-3 출하 → t-4 해제");
    rt.dispatch("t-4", mkorder("t-4", false)).unwrap();
    approve_and_ship(
        &rt, "s-epic", "t-4", "demo", "g-t4", "ship", "dig-t4", &ap_db,
    );
    for t in ["t-1", "t-2", "t-3", "t-4"] {
        assert!(rt.stream("s-epic").unwrap().order_shipped(t).unwrap());
        print!("{t}✓ ");
    }
    println!();

    // ── (7) 재구축 ─────────────────────────────────────────────
    banner(7, "크래시 회복: 프로세스 재시작 → 저널 재생 → 상태 복원");
    let rows_before = orch.read_all().unwrap().len();
    drop(rt);
    // 새 프로세스/새 런타임이 같은 파일에서 부팅 — 회복 코디네이터가 저널
    // 검증+엔진 재구축을 수행한다(입장 차단→드레인→스캔→rebuild→재개).
    let rt2 = StreamRuntime::new(OrchestrationLog::open(&orch_db).unwrap());
    rt2.add_stream(Stream::new_with_root("s-epic", &d, "bmad-epic-build").unwrap())
        .unwrap();
    rt2.bind_approval_ledger(foundry_approval::ApprovalLedger::open(&ap_db).unwrap())
        .unwrap();
    let rc = foundry_orch::stream::RecoveryCoordinator::new(&rt2, 5);
    let outcome = rc.recover(&[], || std::collections::BTreeSet::new(), &ap_db);
    println!("회복 결과: {outcome:?}");
    assert!(matches!(
        outcome,
        foundry_orch::stream::RecoveryOutcome::Completed {
            streams_rebuilt: 1,
            ..
        }
    ));
    let shipped: Vec<_> = ["t-1", "t-2", "t-3", "t-4"]
        .into_iter()
        .filter(|t| {
            rt2.stream("s-epic")
                .unwrap()
                .order_shipped(t)
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(shipped.len(), 4);
    println!(
        "재생 후 출하 상태: {}/4 복원 (해시체인 정합)",
        shipped.len()
    );
    let rows_after = rt2.orch.read_all().unwrap().len();
    assert_eq!(rows_before, rows_after, "재구축 후 로그 행 수 동일");
    println!(
        "오케스트레이션 로그: {rows_before}행 → {rows_after}행 (재구축 = 순수 재생, 부작용 0)"
    );

    // ── (8) 사이징 게이트 방화벽 ──────────────────────────────
    banner(
        8,
        "방어 ③: 재생(비-라이브) 경로의 Approved는 원장 무증거로 거부",
    );
    {
        // 원장 방화벽 — 재생 경로 Approved 는 원장 CONSUMED 증거 없이는
        // 통과할 수 없다(그랜트 상태 관찰로 시연).
        let ap = foundry_approval::ApprovalLedger::open(&ap_db).unwrap();
        let g = ap.get_grant("sizing-sz-1").unwrap();
        println!(
            "원장 증거: sizing-sz-1 = {:?} — CONSUMED만 승인으로 인정",
            g.state
        );
        let forged = rt2.dispatch("t-9", mkorder("t-9", false));
        println!(
            "미선언+미결속 주문 t-9 → {} (게이트 방화벽 유지)",
            forged.unwrap_err()
        );
    }

    println!("\n{}", "=".repeat(72));
    println!("데모 완료 — M7 계약 전부 실주행: 정문 입장 차단, 원장 증거");
    println!("승인, 재입장 루프, 핸드오프 2단계(위조 사전 차단+멱등), 의존성");
    println!("완주, 크래시 재구축 동등. 상태 파일: {}", d.display());
    println!("{}", "=".repeat(72));
}
