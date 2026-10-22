//! M8 라이브 배선 데모 — 실 Die(rustain → foundary-demo 커밋)를
//! M7 스트림의 start/finish 세그먼트 창에서 실행한다.
//!
//! 사이클: 이슈 #139(dep-bump: rand) 주문 → 실 Die 세그먼트
//! (foundry-die 래퍼가 foundary-demo 클론, rustain이 범프+커밋) →
//! FurnaceHold(인간 승인 대기) → 원장 그랜트 승인 → 결과 커밋이
//! artifact_digest로 결속 — PR 발행 직전까지 실주행.
//!
//! 실행: cargo run -p foundry --example m8_live_die -- --issue 139
//!
//! 전제: ~/.foundry/bin/foundry-die, rustain, die.env (M0 게이트 구성).

use std::path::PathBuf;

use foundry_domain::order::{ExternalRef, Order};
use foundry_domain::{OrderEvent, OrderState};
use foundry_orch::OrchestrationKind;
use foundry_orch::OrchestrationLog;
use foundry_orch::chainer::Chainer;
use foundry_orch::stream::{Stream, StreamRuntime};

fn main() {
    let mut issue = String::from("139");
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--issue" {
            if let Some(v) = args.next() {
                issue = v;
            }
        } else if let Some(v) = a.strip_prefix("--issue=") {
            issue = v.to_string();
        } else if !a.starts_with('-') {
            issue = a;
        }
    }

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║  FOUNDRY M8 LIVE — 실 Die(rustain) → foundary-demo 커밋       ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!("타깃 이슈: lunarpulse/foundary-demo#{issue} (dep-bump)\n");

    let d = std::env::temp_dir().join(format!("foundry-m8-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    println!("작업 디렉터리: {}\n", d.display());

    let ap_db = d.join("ap.db");
    let orch_db = d.join("orch.db");

    // ── 셋업: 플랜 + 스트림 + 원장 ─────────────────────────────
    let orch = OrchestrationLog::open(&orch_db).unwrap();
    let order_id = format!("o-{issue}");
    orch.declare_plan(
        "plan-live",
        vec![(order_id.clone(), vec![])],
        vec![(order_id.clone(), "s-live".into())],
    )
    .unwrap();

    let rt = StreamRuntime::new(OrchestrationLog::open(&orch_db).unwrap());
    let orch = rt.orch.clone();
    rt.add_stream(Stream::new_with_root("s-live", &d, "bmad-quickloop").unwrap())
        .unwrap();
    rt.bind_approval_ledger(foundry_approval::ApprovalLedger::open(&ap_db).unwrap())
        .unwrap();
    println!("플랜 선언 + 스트림 s-live(quickloop) + 승인 원장 바인딩");

    // ── 입장: Jira/Intake 대신 이슈 참조 직접 구성 ─────────────
    // Intake 부트스트랩(unsized 예외)으로 정문 입장 — M0 게이트 패턴.
    let ext = ExternalRef::intake(
        &order_id,
        &format!("lunarpulse/foundary-demo#{issue}:{}", chrono::Utc::now()),
    );
    let mut o = Order::new(order_id.clone(), ext, chrono::Utc::now());
    o.force_state_for_test(OrderState::Received, 1);
    rt.dispatch(&order_id, o).unwrap();
    println!("주문 {order_id} 입장 (Intake — 이슈 #{issue} 결속)");

    // ── 실 Die 세그먼트 ────────────────────────────────────────
    println!("\n── 실 Die 세그먼트 ──");
    let st = rt
        .start_die_segment("s-live", &order_id, "m8-live")
        .unwrap();
    println!("세그먼트 시작: {st:?} — foundry-die(클론→rustain→범프→커밋) 실행…");
    println!("   (cargo check 포함, 수 분 소요 가능 — 로그는 workdir 참조)\n");

    let harness = foundry_harness_cli::CliHarness::new(
        "/home/cosmo/.foundry/bin/foundry-die",
        "coding",
        900, // 15분 — rustain 에이전트 런타임 여유
    );
    let order = rt
        .stream("s-live")
        .unwrap()
        .order_snapshot(&order_id)
        .unwrap();
    let workdir = d.join("die");
    std::fs::create_dir_all(&workdir).unwrap();

    let die = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            use foundry_domain::ports::Harness as _;
            harness.run(&order, 1, &workdir).await
        });

    match die {
        Ok(out) if out.exit_code == 0 => {
            println!(
                "   Die 성공: exit={} result_commit={}",
                out.exit_code,
                &out.result_commit[..12.min(out.result_commit.len())]
            );
            let st = rt
                .finish_die_segment("s-live", &order_id, "m8-live", &out)
                .unwrap();
            println!("   세그먼트 종결: {st:?} — 실 커밋이 artifact_digest로 결속됨");
        }
        Ok(out) => {
            // exit != 0: foundry-die가 스스로 거부(예: 태스크 파싱 실패).
            // 남은 HEAD는 클론 베이스일 뿐 산출물이 아니다 — 결속 금지.
            eprintln!(
                "   Die 거부: exit={} (래퍼 로그 참조) — 산출물 아님, 세그먼트 미결속",
                out.exit_code
            );
            std::process::exit(3);
        }
        Err(e) => {
            eprintln!("   Die 실패: {e:?} — DieFailed 경로(라인 executor 백오프)가 소유");
            std::process::exit(2);
        }
    }

    // ── 인간 게이트: 원장 그랜트 라이브 승인 ───────────────────
    println!("\n── 인간 게이트(원장 증거 승인) ──");
    let digest = rt
        .stream("s-live")
        .unwrap()
        .order_snapshot(&order_id)
        .unwrap()
        .artifact_digest
        .unwrap_or_default();
    let grant_id = format!("ship-{order_id}");
    let out = rt
        .approve_at_gate("s-live", &order_id, "m8-live", &grant_id, "ship", &digest)
        .unwrap();
    println!("승인 적용: {out:?} — 그랜트 {grant_id} CONSUMED (digest = 실 커밋)");

    // 게이트 통과 확인(Shipped는 발행 확정 후 — PR 발행은 Publisher 포트가
    // 소유하며 이 데모는 게이트+결속까지만 실주행).
    let st = rt
        .stream("s-live")
        .unwrap()
        .order_snapshot(&order_id)
        .unwrap()
        .state();
    println!("주문 상태: {st:?} (발행 대기 — Publisher가 PR #을 만들면 Shipped)");

    // ── 저널 증거 ──────────────────────────────────────────────
    let rows = orch.read_all().unwrap();
    let journal = rt.stream("s-live").unwrap().journal_rows().unwrap();
    println!("\n── 증거 ──");
    println!("오케스트레이션 로그: {}행", rows.len());
    println!("스트림 저널: {}행 (해시체인)", journal.len());
    let kinds: std::collections::BTreeMap<String, u32> = {
        let mut m = std::collections::BTreeMap::new();
        for r in &rows {
            let k = match &r.kind {
                OrchestrationKind::Refusal { .. } => "refusal".into(),
                _ => "other".into(),
            };
            *m.entry(k).or_default() += 1;
        }
        m
    };
    println!("로그 구성: {kinds:?}");
    println!("Die 로그: {}", d.join("die/attempt-1.log").display());

    println!("\nM8 라이브 데모 완료 — 실 rustain Die가 foundary-demo에 커밋을");
    println!("만들고 그 커밋 해시가 원장 증거로 결속되었다. 다음 단계(별도):");
    println!("Publisher 포트 연결로 PR 생성 → 머지 → Shipped 확정.");
}
