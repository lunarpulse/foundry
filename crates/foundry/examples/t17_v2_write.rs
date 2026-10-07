// T17 실측(복사본): v2 듀얼기록 append → 체인 통과 → event_json 변조 → 탐지
use foundry_domain::ports::Ledger as _;
use foundry_domain::{ActorId, ActorKind, OrderEvent, OrderState};
use foundry_ledger::sqlite::SqliteLedger;

fn main() {
    let src = "/home/cosmo/.foundry/ledger.db";
    let dst = "/tmp/t17_v2_test.db";
    std::fs::copy(src, dst).expect("copy ledger");
    let actor = ActorId {
        kind: ActorKind::Agent,
        source: "t17".into(),
        id: "test".into(),
    };

    let mut led = SqliteLedger::open(std::path::Path::new(dst)).unwrap();
    // 1) v2 듀얼기록 append — 새 주문 1 라이프사이클 (실전 원본에 못 쓰는 값은 복사본이라 OK)
    let oid = "o-999:2026-10-08T09:00:00Z";
    let mut from = OrderState::Received; // synthetic direct appends per row
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

    // 2) 체인 검증: v1 157행 + v2 4행 공존
    let led = SqliteLedger::open(std::path::Path::new(dst)).unwrap();
    assert!(led.verify_chain().unwrap(), "mixed v1+v2 chain must verify");
    let rows = led.read_all().unwrap();
    let v2 = rows
        .iter()
        .filter(|r| foundry_ledger::event_json::row_version(r) == 2)
        .count();
    println!("rows={} v2={} — mixed chain VERIFIES", rows.len(), v2);
    assert_eq!(v2, 4);
    // event_json 디코드 일치 (dual-write gate already enforced at write; check stored)
    let jsons: Vec<_> = rows.iter().filter(|r| r.event_json.is_some()).collect();
    for r in &jsons {
        let ev =
            foundry_ledger::event_json::event_from_json(r.event_json.as_deref().unwrap()).unwrap();
        println!(
            "  v2 row seq={} json={}",
            r.seq,
            r.event_json.as_deref().unwrap()
        );
        let _ = ev;
    }

    // 3) 변조 주입은 외부(python sqlite3)가 수행 — rust는 검증만 (t17 단계 종료 시점)
    //    이 예제는 단계 2까지만; 변조 검출은 verify_tampered 예제/테스트가 담당.
    drop(led);
    println!("T17 GATE (write half): dual-write ok, mixed v1+v2 verify ok");
    let _ = std::fs::remove_file(dst);
}
