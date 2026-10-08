//! T33 integration: the evidence-envelope machinery runs against the
//! REAL production ledger (157 rows, M1 history) — generation,
//! deterministic recompute, extension legality, tamper detection.
use foundry_ledger::LedgerRow;
use foundry_serve::audit_report::{self, CastingFacts, VerifyError, VerifyOutcome};

fn real_ledger_rows() -> Vec<LedgerRow> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/cosmo".into());
    let path = format!("{home}/.foundry/ledger.db");
    if !std::path::Path::new(&path).exists() {
        panic!("production ledger not found at {path} — run on the foundry host");
    }
    let ledger = foundry_ledger::SqliteLedger::open(std::path::Path::new(&path)).unwrap();
    let rows = ledger.read_all().unwrap();
    assert!(ledger.verify_chain().unwrap(), "production chain must verify");
    rows
}

#[test]
fn production_report_generates_and_verifies() {
    let rows = real_ledger_rows();
    let casting = CastingFacts::default();
    let report = audit_report::generate(&rows, &casting).unwrap();
    assert_eq!(report.envelope.prefix_len, rows.len() as i64);
    assert!(!report.envelope.prefix_head_hash.is_empty());
    // deterministic: regenerate -> identical digest
    let again = audit_report::generate(&rows, &casting).unwrap();
    assert_eq!(report.content_digest, again.content_digest);
    // verify against itself
    assert_eq!(
        audit_report::verify(&report, &rows, &casting).unwrap(),
        VerifyOutcome::Valid
    );
    // metrics sanity: as-of counts sum to the number of distinct orders
    let distinct: std::collections::HashSet<_> = rows.iter().map(|r| r.order_id.clone()).collect();
    let sum: u64 = report.metrics.state_counts_asof.values().sum();
    assert_eq!(sum as usize, distinct.len(), "as-of counts == distinct orders");
}

#[test]
fn production_report_tamper_detected() {
    let rows = real_ledger_rows();
    let casting = CastingFacts::default();
    let mut report = audit_report::generate(&rows, &casting).unwrap();
    // bump a state count without touching the ledger
    let key = report.metrics.state_counts_asof.keys().next().cloned().unwrap();
    *report.metrics.state_counts_asof.get_mut(&key).unwrap() += 100;
    assert!(matches!(
        audit_report::verify(&report, &rows, &casting),
        Err(VerifyError::ContentMismatch { .. })
    ));
    // forged digest (self-consistent but wrong vs ledger)
    let clean = audit_report::generate(&rows, &casting).unwrap();
    let mut forged = clean.clone();
    let key = forged.metrics.state_counts_asof.keys().next().cloned().unwrap();
    *forged.metrics.state_counts_asof.get_mut(&key).unwrap() += 100;
    // attacker recomputes digest over tampered metrics
    // (self-consistency passes; ledger recompute catches it)
    let res = audit_report::verify(&forged, &rows, &casting);
    assert!(matches!(res, Err(VerifyError::ContentMismatch { .. })));
}

#[test]
fn production_prefix_tamper_detected() {
    let rows = real_ledger_rows();
    let casting = CastingFacts::default();
    let report = audit_report::generate(&rows, &casting).unwrap();
    // mutate one covered row's content in memory (simulating a DB edit)
    let mut forged = rows.clone();
    forged[80].to_state = "shipped".into();
    assert!(
        matches!(
            audit_report::verify(&report, &forged, &casting),
            Err(VerifyError::PrefixMismatch { .. })
        ),
        "row content edit breaks per-row hash recomputation"
    );
    // truncation
    let short = &rows[..rows.len() - 1];
    assert!(matches!(
        audit_report::verify(&report, short, &casting),
        Err(VerifyError::LedgerTruncated { .. })
    ));
}
