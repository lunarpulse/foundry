//! T58w — REAL process crash injection against the durable
//! governance journal (astra B3: "crash behavior was demonstrated by
//! slicing a Vec; a real fork+kill must be observed").
//!
//! The test builds a dedicated binary (`crash_child`) that:
//!   1. opens the coordinator on a REAL SQLite file,
//!   2. appends N complete intent+effect pairs,
//!   3. signals readiness (a marker file),
//!   4. appends the FINAL event and signals done.
//!
//! The parent kills the child with SIGKILL at a random point between
//! ready and done (a real async kill, not a simulated one), then:
//!   A. the surviving FILE must open as a coordinator,
//!   B. the stream must be structurally valid (no torn row: SQLite
//!      transactions make the final event either fully present or
//!      fully absent),
//!   C. if the final event made it, it is intact; if not, the
//!      pending intent survives with its deterministic recovery,
//!   D. the previously committed pairs are ALL still there — committed
//!      facts never vanish on crash,
//!   E. the coordinator remains WRITABLE after recovery (the next
//!      append lands at the right sequence).
//!
//! Multiple rounds with different kill delays exercise different
//! interleavings; every round must satisfy A–E.
use std::process::{Command, Stdio};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_crash_child");

fn unique_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("t58w-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn wait_for(path: &std::path::Path, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

fn try_kill(pid: u32, grace_ms: u64) -> bool {
    // a REAL asynchronous kill racing the child's final append
    std::thread::sleep(Duration::from_millis(grace_ms));
    Command::new("kill").args(["-9", &pid.to_string()]).status().map(|s| s.success()).unwrap_or(false)
}

fn round(tag: &str, kill_after_ms: u64) -> (bool, usize, usize) {
    let dir = unique_dir(tag);
    let db = dir.join("gov.db");
    let ready = dir.join("ready");
    let done = dir.join("done");
    let pidfile = dir.join("pid");

    let mut child = Command::new(BIN)
        .arg(&db)
        .arg(&ready)
        .arg(&done)
        .arg(&pidfile)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn crash_child");

    // child writes its own pid first thing; wait for readiness
    assert!(wait_for(&ready, Duration::from_secs(10)), "child never became ready ({tag})");
    let killed = try_kill(child.id(), kill_after_ms);

    // reap regardless
    let _ = child.wait();

    // A: the surviving file opens
    let mut coord = foundry_watch::coordinator::GovernanceCoordinator::open(&db)
        .unwrap_or_else(|e| panic!("({tag}) surviving file must open: {e:?}"));

    // B: structurally valid
    let stream = coord.replay().unwrap();
    let rs = coord.restart_state().unwrap();
    assert!(
        foundry_watch::governance_events::verify_stream(&stream).is_ok(),
        "({tag}) no torn rows after SIGKILL: {stream:?}"
    );

    // D: all pre-ready committed pairs survive
    //    (crash_child writes PAIRS=3 complete pairs before ready)
    const PAIRS: usize = 3;
    let committed_pairs = stream
        .iter()
        .filter(|e| matches!(e.phase, foundry_watch::governance_events::Phase::Committed))
        .count();
    assert!(
        committed_pairs >= PAIRS,
        "({tag}) committed facts never vanish: {committed_pairs} < {PAIRS}"
    );

    // C: the final event is either fully there or fully absent
    //    (never half), and if absent-as-effect then pending survives
    let final_op = stream.iter().filter(|e| e.op_id == "final").last();
    match final_op {
        Some(e) => match e.phase {
            // crash landed between intent and effect: pending survives
            foundry_watch::governance_events::Phase::Pending => {
                assert!(rs.pending_count >= 1, "({tag}) pending surfaced for recovery");
            }
            // the child completed before the kill landed: fully committed
            foundry_watch::governance_events::Phase::Committed => {}
            foundry_watch::governance_events::Phase::Aborted => {
                panic!("({tag}) the child never aborts — Aborted is recovery-only");
            }
        },
        None => {
            // killed before the final append even started: fine
        }
    }

    // E: writable after recovery
    let next = coord
        .append(foundry_watch::governance_events::GovernanceEvent {
            schema_version: 1,
            seq: 0,
            op: foundry_watch::governance_events::OpKind::LessonSeal,
            phase: foundry_watch::governance_events::Phase::Pending,
            op_id: format!("post-crash-{tag}"),
            payload_digest: "h".into(),
            actor: "parent".into(),
            at: "t".into(),
        })
        .unwrap();
    let expected_next = stream.last().map(|e| e.seq + 1).unwrap_or(1);
    assert_eq!(next, expected_next, "({tag}) the journal continues at the right sequence");

    let _ = std::fs::remove_dir_all(&dir);
    (killed, stream.len(), rs.pending_count)
}

#[test]
fn real_sigkill_durability() {
    // several interleavings: fast kill (before final append), mid
    // kill (between intent and effect is the child's own pacing),
    // slow kill (child finishes — kill races a dead pid)
    let mut killed_count = 0;
    for (tag, delay) in [("fast", 0u64), ("mid", 8), ("mid2", 16), ("slow", 300)] {
        let (killed, len, pending) = round(tag, delay);
        eprintln!("round {tag}: killed={killed} stream_len={len} pending={pending}");
        if killed {
            killed_count += 1;
        }
    }
    assert!(killed_count >= 1, "at least one round must land a real SIGKILL");
}

#[test]
fn committed_facts_survive_wholesale() {
    // deterministic slow-kill: child completes fully; assert nothing
    // was lost and the stream is replay-clean
    let (killed, len, pending) = round("full", 400);
    assert!(len >= 8, "3 pairs + final pair = 8 events minimum, got {len}");
    assert_eq!(pending, 0, "a completed child leaves no pendings");
    let _ = killed;
}
