//! T60f 회귀 (astra 5th): LedgerEvidenceReader의 실측 회귀 4종 —
//! ①비영 재시도율 ②event_json 폴백(canonical snake_case) ③빈 접두사
//! 거부 ④진짜 0 완료. 실 SQLite 파일 위에서.
#![allow(clippy::needless_borrows_for_generic_args)] // W6a clippy debt: preserve this existing test/example logic, assertions, and cell sequence

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
    let out = r
        .run_query("retry_rate_by_task_class", &params(), 100)
        .unwrap();
    assert!(
        out.contains("0.3333"),
        "1/3 retry rate over real rows: {out}"
    );
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
    let out = r
        .run_query("retry_rate_by_task_class", &params(), 100)
        .unwrap();
    assert!(
        out.contains("0.5000"),
        "snake_case canonical JSON decoded: {out}"
    );
}

#[test]
fn empty_prefix_is_refused_not_zero() {
    let db = tmp_db("empty");
    seed(&db, &[("Queued", None)]); // table exists, but prefix 0 rows
    let r = LedgerEvidenceReader::new(db);
    let err = r
        .run_query("retry_rate_by_task_class", &params(), 0)
        .unwrap_err();
    assert!(err.contains("cannot certify"), "no rows -> Err: {err}");
}

#[test]
fn genuine_zero_completions_is_real_zero() {
    let db = tmp_db("zero");
    // rows exist, none are completions -> 0.0 is the TRUE answer
    seed(
        &db,
        &[("Queued", None), ("DieStarted", None), ("Queued", None)],
    );
    let r = LedgerEvidenceReader::new(db);
    let out = r
        .run_query("retry_rate_by_task_class", &params(), 100)
        .unwrap();
    assert!(
        out.contains("0.0000"),
        "rows without completions -> real 0.0: {out}"
    );
}

#[tokio::test]
async fn astra6_verified_profile_with_bundle_seed_executes_as_bundle_seed() {
    // B1 HIGH regression, FULL wiring path (astra 7th: the previous
    // version hand-built the profile and never asserted a recorded
    // hash — it could not detect the wiring regression):
    //   bundle (seed 7) -> BundleStore.put -> profile_from_bundle
    //   (VERIFIED load, consumption re-verified) -> the returned
    //   profile's hash IS the record's profile_hash -> execution via
    //   DryDie (as run_casting now receives the executed_profile) ->
    //   artifact must match a seed-7 execution and DIFFER from seed 0.
    use foundry_domain::ports::Harness as _;
    use foundry_watch::bundles::{
        BundleProfile, BundleStore, CandidateBundle, DependencyPins, LessonSnapshot,
    };
    use foundry_watch::casting_wiring::profile_from_bundle;

    fn lesson_hash(content: &str) -> String {
        use sha2::Digest as _;
        let mut h = sha2::Sha256::new();
        h.update(&(content.len() as u64).to_be_bytes());
        h.update(content.as_bytes());
        format!("{:x}", h.finalize())
    }

    let mut store = BundleStore::default();
    let bundle = CandidateBundle::build(
        "cand-b1",
        BundleProfile {
            model: "deepseek-v4".into(),
            temperature: Some(0.0),
            seed: Some(7),
            max_output_tokens: 512,
        },
        "solve {{task}}",
        vec![LessonSnapshot {
            lesson_id: "L1".into(),
            content_hash: lesson_hash("prefer map_err"),
            content: "prefer map_err".into(),
        }],
        DependencyPins {
            image_digest: "sha256:aa".into(),
            tool_digests: [("cargo".to_string(), "sha256:bb".into())]
                .into_iter()
                .collect(),
            model_revision: Some("rev-9".into()),
        },
    )
    .unwrap();
    let digest = store.put(bundle);

    // the VERIFIED load — this is the wiring entry casting main uses
    let (verified, record) = profile_from_bundle(&store, &digest, "run-b1").unwrap();
    assert_eq!(
        verified.seed, 7,
        "the bundle's seed is the verified profile's seed"
    );
    assert!(record.consumption_verified);
    // the recorded hash IS the hash of the profile we will execute
    assert_eq!(
        record.profile_hash,
        verified.hash(),
        "recorded hash describes the EXECUTED profile"
    );

    // execute THROUGH the verified profile (run_casting's contract)
    let dir = std::env::temp_dir().join(format!("t60f-exec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let sandbox = foundry_watch::dry_die::Sandbox::new(&dir, &verified).unwrap();
    let order = foundry_domain::Order::new(
        "order-b1".into(),
        foundry_domain::ExternalRef::cron("casting", "casting"),
        chrono::Utc::now(),
    );
    let die = foundry_watch::dry_die::DryDie::new(verified.clone(), &sandbox.root);
    let out = die
        .run(&order, 1, std::path::Path::new("wd"))
        .await
        .unwrap();

    // the executed artifact is a function of seed 7: a seed-0 profile
    // (what the OLD hardcoded reconstruction produced from CLI seed 0)
    // yields a DIFFERENT artifact
    let seed0 = foundry_watch::dry_die::CandidateProfile {
        seed: 0,
        ..verified.clone()
    };
    let die0 = foundry_watch::dry_die::DryDie::new(seed0, &sandbox.root);
    let out0 = die0
        .run(&order, 1, std::path::Path::new("wd"))
        .await
        .unwrap();
    assert_ne!(
        out.result_commit, out0.result_commit,
        "executed artifact follows the VERIFIED seed (7), not a CLI seed (0)"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
