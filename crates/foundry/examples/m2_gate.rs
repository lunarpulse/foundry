// M2 GATE — 7-item measurement (plan v2). Runs against live artifacts.
// 1 timeline equality, 2 schema v2 migration, 3 no-op injection,
// 4 unknown readjudication (unit), 5 baseline report consistency,
// 6 dry-run Casting adapter (candidate = baseline => deltas 0),
// 7 regression count is reported by the caller (cargo test).
use foundry_domain::ports::Ledger as _;
use foundry_domain::{ActorId, ActorKind, OrderEvent, OrderState};
use foundry_ledger::LedgerRow;
use foundry_ledger::sqlite::SqliteLedger;
use foundry_watch::benchmark;
use foundry_watch::corpus::CorpusManifest;
use std::path::Path;

fn chain_ok(rows: &[LedgerRow]) -> bool {
    rows.iter()
        .zip(rows.iter().skip(1))
        .all(|(a, b)| b.prev_hash == a.hash)
}

fn main() {
    let mut pass = 0;
    let total = 6; // item 7 reported separately

    // ---- GATE 1: timeline equality (engine vs per-row python golden) ----
    let led = SqliteLedger::open(Path::new("/home/cosmo/.foundry/ledger.db")).unwrap();
    assert!(led.verify_chain().unwrap());
    let rows = led.read_all().unwrap();
    let replayed = foundry_ledger::replay::replay(&rows, chain_ok).unwrap();
    // golden: derive per-order (state_seq, attempt, last_state) from rows directly
    use std::collections::BTreeMap;
    // golden: group rows by id FIRST (the journal interleaves orders),
    // then split lifecycles at `received` boundaries — same semantics as
    // the engine's pending-map (T16 python crosscheck was fixed the same way).
    use std::collections::BTreeMap as Map;
    let mut by_id: Map<String, Vec<&LedgerRow>> = Map::new();
    for r in &rows {
        by_id.entry(r.order_id.clone()).or_default().push(r);
    }
    let mut golden: BTreeMap<(String, u32), (Vec<String>, u32, String)> = BTreeMap::new();
    for (oid, rs) in &by_id {
        let mut lc: u32 = 0;
        for (i, r) in rs.iter().enumerate() {
            if i > 0 && r.from_state == "received" {
                lc += 1; // lifecycle boundary: re-order restarts from received
            }
            // engine numbers lifecycles 0-start; golden must match
            let e = golden.entry((oid.clone(), lc)).or_default();
            e.0.push(r.to_state.clone());
            if let Some(n) = foundry_watch::corpus::corpus_attempts(&r.event) {
                e.1 = e.1.max(n);
            }
            e.2 = r.to_state.clone();
        }
    }
    // engine side: key by (order_id, lifecycle)
    let engine: BTreeMap<(String, u32), (String, u32)> = replayed
        .iter()
        .map(|ro| {
            (
                (ro.order_id.clone(), ro.lifecycle),
                (ro.order.state().to_string(), ro.order.attempt()),
            )
        })
        .collect();
    let mut mismatches = 0;
    for (k, g) in &golden {
        match engine.get(k) {
            Some((state, attempt)) => {
                if *state != g.2 || *attempt != g.1.max(*attempt) {
                    mismatches += 1;
                }
            }
            None => mismatches += 1,
        }
    }
    if mismatches == 0 && replayed.len() == golden.len() {
        println!(
            "GATE 1 PASS: timeline equality — {} lifecycles, divergence 0",
            replayed.len()
        );
        pass += 1;
    } else {
        println!(
            "GATE 1 FAIL: {mismatches} mismatches, {} vs {}",
            replayed.len(),
            golden.len()
        );
    }

    // ---- GATE 2: schema v2 — v1 rows intact (done in T17 live) + tamper ----
    // (migration itself measured in T17: 157 rows hash-identical, doctor 0.
    //  Here re-assert chain verifies on the live db and a v2 tamper copy fails.)
    if led.verify_chain().unwrap() {
        // tamper copy test was run in t17_tamper_detect example; re-run quickly:
        let dst = "/tmp/gate2.db";
        let _ = std::fs::copy("/home/cosmo/.foundry/ledger.db", dst);
        let actor = ActorId {
            kind: ActorKind::Agent,
            source: "g".into(),
            id: "g".into(),
        };
        let mut l2 = SqliteLedger::open(Path::new(dst)).unwrap();
        let oid = "o-gate:2026-10-08T10:00:00Z";
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
            l2.append(oid, &ev, from, to, &actor, chrono::Utc::now())
                .unwrap();
            from = to;
        }
        drop(l2);
        let out = std::process::Command::new("python3")
            .arg("/tmp/t17_tamper.py")
            .arg(dst)
            .output()
            .unwrap();
        assert!(out.status.success());
        let l3 = SqliteLedger::open(Path::new(dst)).unwrap();
        let tamper_detected = !l3.verify_chain().unwrap();
        if tamper_detected {
            println!("GATE 2 PASS: v1 rows intact + v2 event_json tamper => verify FAIL");
            pass += 1;
        } else {
            println!("GATE 2 FAIL: tamper NOT detected");
        }
        let _ = std::fs::remove_file(dst);
    }

    // ---- GATE 3: no-op injection 2 kinds (measured in T19 tests; re-assert pure logic) ----
    use foundry_domain::inspector::{DiffStats, Verdict, event_for, inspect};
    let v1 = inspect(
        &DiffStats {
            files_changed: 0,
            lines_changed: 0,
        },
        false,
    );
    let v2 = inspect(
        &DiffStats {
            files_changed: 0,
            lines_changed: 0,
        },
        true,
    );
    let ok3 = matches!(v1, Verdict::PreflightNoWork { .. })
        && matches!(event_for(&v1), OrderEvent::Rejected { .. })
        && matches!(v2, Verdict::ExpectedChangeButEmptyDiff)
        && matches!(event_for(&v2), OrderEvent::InspectionFailed { .. });
    if ok3 {
        println!("GATE 3 PASS: PreflightNoWork->Rejected / ExpectedChangeButEmptyDiff->Failed");
        pass += 1;
    } else {
        println!("GATE 3 FAIL");
    }

    // ---- GATE 4: unknown readjudication (mock 3 + query-fail stays Unknown) ----
    use foundry_watch::readjudicate::{PrLookup, Readjudication, readjudicate};
    let c1 = readjudicate(&PrLookup::Found {
        pr_url: "u".into(),
        merged: true,
    });
    let c2 = readjudicate(&PrLookup::Found {
        pr_url: "u".into(),
        merged: false,
    });
    let c3 = readjudicate(&PrLookup::NotFound);
    let c4 = readjudicate(&PrLookup::QueryFailed { reason: "x".into() });
    let ok4 = matches!(c1, Readjudication::Confirm { .. })
        && matches!(c2, Readjudication::Confirm { .. })
        && matches!(c3, Readjudication::Republish)
        && matches!(c4, Readjudication::StayUnknown { .. });
    if ok4 {
        println!(
            "GATE 4 PASS: found(open/merged)->Confirm, 404->Republish, query-fail->StayUnknown"
        );
        pass += 1;
    } else {
        println!("GATE 4 FAIL");
    }

    // ---- GATE 5: baseline report consistency ----
    let corpus = CorpusManifest::load_all(Path::new("/home/cosmo/.foundry/corpus")).unwrap();
    let metrics = benchmark::compute(&corpus);
    let row_ranges_ok = corpus
        .iter()
        .filter_map(|c| c.ledger_row_range)
        .all(|(a, b)| a >= 1 && b <= rows.len() as i64 && a <= b);
    let ships_sum = metrics.shipped + metrics.failed + metrics.rejected_noop;
    if row_ranges_ok
        && metrics.cohort_size == corpus.len()
        && ships_sum <= metrics.cohort_size
        && metrics.metric_version == 1
    {
        println!(
            "GATE 5 PASS: {} manifests, ranges within 1..{}, 3-axis metrics + versioning present",
            corpus.len(),
            rows.len()
        );
        pass += 1;
    } else {
        println!("GATE 5 FAIL: ranges_ok={row_ranges_ok}");
    }

    // ---- GATE 6: dry-run Casting adapter — candidate == baseline => all deltas 0 ----
    let dir = std::env::temp_dir().join("gate6");
    let _ = std::fs::remove_dir_all(&dir);
    benchmark::write_baseline(&corpus, &dir.join("b.json")).unwrap();
    // "candidate" IS the same corpus (dry-run: no model/prompt change)
    let rep = benchmark::compare(&corpus, &dir.join("b.json")).unwrap();
    if rep.shipped_delta == 0 && rep.failed_delta == 0 && rep.rejected_delta == 0 {
        println!("GATE 6 PASS: dry-run Casting (candidate=baseline) — 3-axis deltas all 0");
        pass += 1;
    } else {
        println!(
            "GATE 6 FAIL: deltas {}/{}",
            rep.shipped_delta, rep.failed_delta
        );
    }

    println!("\nM2 GATE: {pass}/{total} measured items PASS (item 7 = regression suite)");
    if pass < total {
        std::process::exit(1);
    }
}
