// T16 실측: 실전 ledger를 새 리플레이 엔진으로 전체 리플레이 — timeline equality 예행
use foundry_ledger::replay::{ReplayError, replay};
use foundry_ledger::{LedgerRow, SqliteLedger};

fn main() {
    let led = SqliteLedger::open(std::path::Path::new("/home/cosmo/.foundry/ledger.db"))
        .expect("open ledger");
    assert!(led.verify_chain().unwrap(), "chain must verify");
    let rows = led.read_all().expect("read_all");
    let t0 = std::time::Instant::now();
    let out = replay(&rows, |rs| {
        rs.iter()
            .zip(rs.iter().skip(1))
            .all(|(a, b)| b.prev_hash == a.hash)
    });
    let dt = t0.elapsed();
    match out {
        Ok(lifecycles) => {
            use std::collections::BTreeMap;
            let mut by_state: BTreeMap<String, usize> = BTreeMap::new();
            for l in &lifecycles {
                *by_state.entry(l.order.state().to_string()).or_default() += 1;
            }
            println!("replayed {} lifecycles in {:?}", lifecycles.len(), dt);
            for (s, n) in &by_state {
                println!("  {s}: {n}");
            }
            // FurnaceHold digest 바인딩 확인
            let waiting: Vec<_> = lifecycles
                .iter()
                .filter(|l| l.order.state().to_string() == "waiting")
                .collect();
            let bound = waiting
                .iter()
                .filter(|l| l.order.artifact_digest.clone().unwrap_or_default().len() >= 7)
                .count();
            println!(
                "waiting: {} (digest-bound in-engine: 0 expected — binding is rehydrate's job)",
                waiting.len()
            );
            assert_eq!(
                lifecycles.len(),
                30,
                "expected 30 lifecycles (python crosscheck, id-grouped)"
            );
            assert_eq!(by_state.get("shipped"), Some(&17));
            assert_eq!(by_state.get("waiting"), Some(&7));
            assert_eq!(by_state.get("publishing"), Some(&5));
            assert_eq!(by_state.get("working"), Some(&1));
            println!("TIMELINE MATCH: python crosscheck == rust engine (30 lifecycles)");
        }
        Err(ReplayError::IllegalRow { order_id, seq, err }) => {
            eprintln!("ILLEGAL ROW {order_id} seq={seq}: {err}");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("replay error: {e:?}");
            std::process::exit(1);
        }
    }
}
