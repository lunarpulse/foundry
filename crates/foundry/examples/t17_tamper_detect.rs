// T17 실측 2/2: v2 행 event_json 변조 → 체인 붕괴 검출 (review #2 게이트)
use foundry_domain::ports::Ledger as _;
use foundry_ledger::sqlite::SqliteLedger;

fn main() {
    let src = "/home/cosmo/.foundry/ledger.db";
    let dst = "/tmp/t17_tamper_test.db";
    // 1) 원본 복사 후 v2 행 4개 추가 (t17_v2_write와 동일 경로)
    std::fs::copy(src, dst).expect("copy ledger");
    let actor = foundry_domain::ActorId {
        kind: foundry_domain::ActorKind::Agent,
        source: "t17".into(),
        id: "test".into(),
    };
    let mut led = SqliteLedger::open(std::path::Path::new(dst)).unwrap();
    let oid = "o-999:2026-10-08T09:00:00Z";
    use foundry_domain::{OrderEvent, OrderState};
    let mut from = OrderState::Received;
    for (ev, to) in [
        (OrderEvent::Queued, OrderState::Queued),
        (OrderEvent::DieStarted, OrderState::Working),
        (
            OrderEvent::DieSucceeded { attempt: 1 },
            OrderState::Inspecting,
        ),
        (OrderEvent::InspectionPassed, OrderState::FurnaceHold),
    ] {
        led.append(oid, &ev, from, to, &actor, chrono::Utc::now())
            .unwrap();
        from = to;
    }
    drop(led);

    // 2) 변조 주입: 외부 python 스크립트 (/tmp/t17_tamper.py) — 인라인 이스케이프 지옥 회피
    let out = std::process::Command::new("python3")
        .arg("/tmp/t17_tamper.py")
        .arg(dst)
        .output()
        .expect("python tamper");
    print!("{}", String::from_utf8_lossy(&out.stdout));
    assert!(
        out.status.success(),
        "tamper injector failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // 3) 검증: 변조 행의 해시가 더 이상 일치하지 않음 → verify 실패해야 함
    let led = SqliteLedger::open(std::path::Path::new(dst)).unwrap();
    let ok = led.verify_chain().unwrap();
    println!("chain verifies after event_json tamper: {ok}");
    assert!(!ok, "TAMPER MUST BE DETECTED (review #2 gate)");
    println!("T17 GATE (tamper half): event_json tampering breaks the chain — DETECTED");
    let _ = std::fs::remove_file(dst);
}
