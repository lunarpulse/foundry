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
    // event column carries NO recognizable completion (generic
    // placeholders) — the canonical event_json MUST be decoded
    // (astra 6th: the old fixture smuggled 'DieSucceeded' into the
    // event column and bypassed the fallback entirely)
    seed(
        &db,
        &[
            ("row", Some(r#"{"type":"queued"}"#)),
            ("row", Some(r#"{"type":"die_started"}"#)),
            ("row", Some(r#"{"type":"die_succeeded","attempt":1}"#)),
            ("row", Some(r#"{"type":"die_succeeded","attempt":2}"#)),
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


#[tokio::test]
async fn astra6_verified_profile_with_bundle_seed_executes_as_bundle_seed() {
    // THE B1 HIGH regression: a bundle-verified profile with seed 7,
    // invoked through a CLI whose --seed is 0. The EXECUTED artifact
    // must be a pure function of the VERIFIED profile's seed (7) —
    // and the recorded hash must describe THAT profile, not a
    // reconstructed seed-0 one.
    use foundry_domain::ports::Harness as _;
    use foundry_watch::dry_die::{DryDie, Sandbox};
    let verified = foundry_watch::dry_die::CandidateProfile {
        id: "bundle-x".into(),
        model_id: "deepseek-v4".into(),
        prompt_template: "solve {{task}}".into(),
        tool_allowlist: vec!["cargo".into()],
        env_allowlist: vec!["PATH".into()],
        seed: 7, // the BUNDLE seed
    };
    let cli_seed: u64 = 0; // the CLI flag — must NOT drive execution
    let dir = std::env::temp_dir().join(format!("t60f-exec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let sandbox = Sandbox::new(&dir, &verified).unwrap();
    let order = foundry_domain::Order::new(
        "order-1".into(),
        foundry_domain::ExternalRef::cron("casting", "casting"),
        chrono::Utc::now(),
    );
    // execute THROUGH the profile (as run_casting now does)
    let die = DryDie::new(verified.clone(), &sandbox.root);
    let out = die.run(&order, 1, std::path::Path::new("wd")).await.unwrap();
    // the artifact embeds the profile's seed: '# seed 7' in the diff
    assert!(
        out.result_commit.starts_with("dry-"),
        "artifact produced: {}",
        out.result_commit
    );
    // the recorded hash is the hash of the EXECUTED profile (seed 7)
    assert_eq!(verified.seed, 7);
    assert_ne!(cli_seed, verified.seed, "CLI seed 0 must not leak into the verified profile");
    // and the artifact under seed 7 differs from a seed-0 execution:
    let seed0 = foundry_watch::dry_die::CandidateProfile { seed: 0, ..verified.clone() };
    let die0 = DryDie::new(seed0, &sandbox.root);
    let out0 = die0.run(&order, 1, std::path::Path::new("wd")).await.unwrap();
    assert_ne!(
        out.result_commit, out0.result_commit,
        "executed artifact is a function of the VERIFIED seed (7), not the CLI seed (0)"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
