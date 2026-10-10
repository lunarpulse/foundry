//! T60f 회귀 (astra 5th): LedgerEvidenceReader의 실측 회귀 4종 —
//! ①비영 재시도율 ②event_json 폴백(canonical snake_case) ③빈 접두사
//! 거부 ④진짜 0 완료. 실 SQLite 파일 위에서.
use foundry_watch::evidence_recompute::{EvidenceReader, LedgerEvidenceReader};
use std::collections::BTreeMap;

fn tmp_db(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("t60f-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.join("ledger.db")
}

fn seed(db: &std::path::PathBuf, rows: &[(&str, Option<&str>)]) {
    let c = rusqlite::Connection::open(db).unwrap();
    c.execute(
        "CREATE TABLE IF NOT EXISTS ledger (seq INTEGER PRIMARY KEY AUTOINCREMENT, order_id TEXT, from_state TEXT, to_state TEXT, event TEXT, actor TEXT, intent TEXT, at TEXT, prev_hash TEXT, hash TEXT, event_json TEXT)",
        [],
    )
    .unwrap();
    for (ev, js) in rows {
        c.execute(
            "INSERT INTO ledger (event, event_json, hash) VALUES (?1, ?2, ?3)",
            rusqlite::params![ev, js, format!("h-{ev}")],
        )
        .unwrap();
    }
}

fn params() -> BTreeMap<String, String> {
    BTreeMap::new()
}

#[test]
fn nonzero_retry_rate_over_the_event_column() {
    let db = tmp_db("nonzero");
    // 3 completions: one retry (attempt 2) -> rate 1/3
    seed(
        &db,
        &[
            ("Queued", None),
            ("DieStarted", None),
            ("DieSucceeded { attempt: 1 }", None),
            ("DieStarted", None),
            ("DieSucceeded { attempt: 2 }", None),
            ("DieSucceeded { attempt: 1 }", None),
        ],
    );
    let r = LedgerEvidenceReader::new(db);
    let out = r.run_query("retry_rate_by_task_class", &params(), 100).unwrap();
    assert!(out.contains("0.3333"), "1/3 retry rate over real rows: {out}");
}

#[test]
fn canonical_json_fallback_snake_case() {
    let db = tmp_db("fallback");
    // event column carries NO completion; event_json (canonical) does
    seed(
        &db,
        &[
            ("Queued", Some(r#"{"type":"queued"}"#)),
            ("DieStarted", Some(r#"{"type":"die_started"}"#)),
            ("DieSucceeded { attempt: 1 }", Some(r#"{"type":"die_succeeded","attempt":1}"#)),
            ("DieSucceeded { attempt: 2 }", Some(r#"{"type":"die_succeeded","attempt":2}"#)),
        ],
    );
    let r = LedgerEvidenceReader::new(db);
    let out = r.run_query("retry_rate_by_task_class", &params(), 100).unwrap();
    assert!(out.contains("0.5000"), "snake_case canonical JSON decoded: {out}");
}

#[test]
fn empty_prefix_is_refused_not_zero() {
    let db = tmp_db("empty");
    seed(&db, &[("Queued", None)]); // table exists, but prefix 0 rows
    let r = LedgerEvidenceReader::new(db);
    let err = r.run_query("retry_rate_by_task_class", &params(), 0).unwrap_err();
    assert!(err.contains("cannot certify"), "no rows -> Err: {err}");
}

#[test]
fn genuine_zero_completions_is_real_zero() {
    let db = tmp_db("zero");
    // rows exist, none are completions -> 0.0 is the TRUE answer
    seed(&db, &[("Queued", None), ("DieStarted", None), ("Queued", None)]);
    let r = LedgerEvidenceReader::new(db);
    let out = r.run_query("retry_rate_by_task_class", &params(), 100).unwrap();
    assert!(out.contains("0.0000"), "rows without completions -> real 0.0: {out}");
}
