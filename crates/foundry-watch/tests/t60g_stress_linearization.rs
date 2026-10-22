//! T60g — B7 approximation (astra B7: "promotion/issuance
//! linearization and process-group cancellation are not observed").
//!
//!   L1 LINEARIZATION: N threads race promote_with_store_token on
//!      the SAME shared BundleStore + the SAME single-use token.
//!      Exactly ONE wins; every loser gets a named refusal. The
//!      token cannot be consumed twice no matter the interleaving.
//!      Also: N threads race issue_bundle_token_for_domain through
//!      a single-use domain gate — exactly one token is issued.
//!
//!   L2 PROCESS-GROUP CANCELLATION: the crash child runs in its OWN
//!      process group; we SIGKILL the GROUP (kill -9 -pgid) mid-run
//!      and observe the same durability contract as T58w: the file
//!      opens, the stream is valid, committed facts survive, the
//!      journal continues at the right seq.
use foundry_watch::bundles::{
    BundleProfile, BundleStore, CandidateBundle, DependencyPins, LessonSnapshot,
};
use foundry_watch::casting_wiring::{
    DomainTokenGate, WiringError, issue_bundle_token_for_domain, profile_from_bundle,
    promote_with_store_token,
};
use foundry_watch::governance_events::GovernanceEvent;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

fn lesson_hash(content: &str) -> String {
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    h.update(&(content.len() as u64).to_be_bytes());
    h.update(content.as_bytes());
    format!("{:x}", h.finalize())
}

fn store_with_bundle() -> (BundleStore, String) {
    let mut st = BundleStore::default();
    let b = CandidateBundle::build(
        "cand-lin",
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
    let d = st.put(b);
    (st, d)
}

/// Single-use domain gate: the FIRST consume wins, all later sees false.
struct OneShotGate(AtomicBool);
impl DomainTokenGate for OneShotGate {
    fn consume_for(&self, _d: &str) -> Result<bool, String> {
        Ok(!self.0.swap(true, Ordering::SeqCst))
    }
}

#[test]
fn l1_concurrent_promote_linearizes_to_exactly_one() {
    let (mut st, d) = store_with_bundle();
    // issue one token through a one-shot domain gate
    let mut events = Vec::new();
    let gate = OneShotGate(AtomicBool::new(false));
    issue_bundle_token_for_domain(&mut st, &mut events, &gate, &d, "tok-race").unwrap();
    // the run record for the bundle route
    let (_, record) = profile_from_bundle(&st, &d, "run-race").unwrap();

    let shared = Arc::new(Mutex::new(st));
    let rec = Arc::new(record);
    const N: usize = 8;
    let mut handles = Vec::new();
    for _ in 0..N {
        let shared = shared.clone();
        let rec = rec.clone();
        handles.push(std::thread::spawn(move || {
            let mut st = shared.lock().unwrap();
            promote_with_store_token(&mut st, &rec, "tok-race")
        }));
    }
    let mut ok = 0;
    let mut refused = 0;
    for h in handles {
        match h.join().unwrap() {
            Ok(()) => ok += 1,
            Err(WiringError::DomainRefusal { .. }) => refused += 1, // token already consumed
            Err(e) => panic!("unexpected error shape: {e:?}"),
        }
    }
    assert_eq!(ok, 1, "exactly ONE promotion wins the race");
    assert_eq!(refused, N as i64 - 1, "every loser is a named refusal");
}

#[test]
fn l1_concurrent_token_issuance_linearizes_to_exactly_one() {
    let (st, d) = store_with_bundle();
    let shared = Arc::new(Mutex::new(st));
    // one shared one-shot domain token, N racing issuance calls
    let gate = Arc::new(OneShotGate(AtomicBool::new(false)));
    const N: usize = 6;
    let mut handles = Vec::new();
    for i in 0..N {
        let shared = shared.clone();
        let gate = gate.clone();
        let d = d.clone();
        handles.push(std::thread::spawn(move || {
            let mut st = shared.lock().unwrap();
            let mut ev = Vec::new();
            issue_bundle_token_for_domain(&mut st, &mut ev, as_gate(&gate), &d, &format!("tok-{i}"))
        }));
    }
    let mut issued = 0;
    let mut denied = 0;
    for h in handles {
        match h.join().unwrap() {
            Ok(()) => issued += 1,
            Err(WiringError::TokenWithoutDomain) => denied += 1,
            Err(e) => panic!("unexpected: {e:?}"),
        }
    }
    assert_eq!(
        issued, 1,
        "the single-use domain token authorizes exactly ONE issuance"
    );
    assert_eq!(denied, N as i64 - 1);
    let st = shared.lock().unwrap();
    assert_eq!(st.tokens_len(), 1);
}

// helper: view Arc<OneShotGate> as &dyn DomainTokenGate
fn as_gate(g: &Arc<OneShotGate>) -> &dyn DomainTokenGate {
    g.as_ref()
}

const BIN: &str = env!("CARGO_BIN_EXE_crash_child");

#[test]
fn l2_process_group_cancellation_durability() {
    use std::process::Stdio;
    let dir = std::env::temp_dir().join(format!("t60g-{t}", t = std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("gov.db");
    let ready = dir.join("ready");

    let mut child = std::process::Command::new("setsid")
        .arg("-w")
        .arg(BIN)
        .arg(&db)
        .arg(&ready)
        .arg(dir.join("done"))
        .arg(dir.join("pid"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn crash_child in its own group");

    // wait for readiness (3 committed pairs on disk)
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !ready.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(ready.exists(), "child never became ready");
    std::thread::sleep(std::time::Duration::from_millis(35)); // inside the final window

    // SIGKILL the ENTIRE GROUP (negative pid = process group).
    // process_group(0) made the child its own group leader, so
    // child.id() IS the pgid — std tracked it, no pidfile guessing.
    let pgid = child.id().to_string();
    let status = std::process::Command::new("kill")
        .args(["-9", &format!("-{pgid}")])
        .status()
        .expect("kill");
    // /usr/bin/kill exits 0 on success; a shell-builtin fallback would
    // still deliver the signal — the durability assertions below are
    // the real proof the kill LANDED.
    let _ = status;
    let _ = child.wait();
    // the kill must have landed BEFORE completion: the done marker
    // (written after the final committed pair) must be absent —
    // otherwise this run proved nothing about cancellation (astra 3rd).
    assert!(
        !dir.join("done").exists(),
        "child completed before the kill — run proved nothing"
    );

    // durability contract holds after group cancellation
    let mut coord = foundry_watch::coordinator::GovernanceCoordinator::open(&db)
        .expect("surviving file opens after group SIGKILL");
    let stream = coord.replay().unwrap();
    assert!(
        foundry_watch::governance_events::verify_stream(&stream).is_ok(),
        "stream structurally valid after group kill"
    );
    let committed = stream
        .iter()
        .filter(|e| matches!(e.phase, foundry_watch::governance_events::Phase::Committed))
        .count();
    assert!(
        committed >= 3,
        "committed facts survive group cancellation: {committed}"
    );
    let next = coord
        .append(foundry_watch::governance_events::GovernanceEvent {
            schema_version: 1,
            seq: 0,
            op: foundry_watch::governance_events::OpKind::LessonSeal,
            phase: foundry_watch::governance_events::Phase::Pending,
            op_id: "post-group-kill".into(),
            payload_digest: "h".into(),
            actor: "test".into(),
            at: "t".into(),
        })
        .unwrap();
    assert_eq!(
        next,
        stream.last().unwrap().seq + 1,
        "journal continues at the right seq"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
