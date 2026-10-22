//! crash_child — the victim binary for T58w crash injection.
//! Usage: crash_child <db> <ready-marker> <done-marker> <pidfile>
//!
//! Writes its pid, 3 complete intent+effect pairs, touches READY,
//! then writes the FINAL intent and touches DONE. The parent kills
//! this process with SIGKILL somewhere in that window.
use std::process::Command;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let db = std::path::PathBuf::from(&args[1]);
    let ready = std::path::PathBuf::from(&args[2]);
    let done = std::path::PathBuf::from(&args[3]);
    let pidfile = std::path::PathBuf::from(&args[4]);

    std::fs::write(&pidfile, std::process::id().to_string()).unwrap();

    let mut c = foundry_watch::coordinator::GovernanceCoordinator::open(&db).unwrap();

    let ev = |op_id: &str, phase: foundry_watch::governance_events::Phase| {
        foundry_watch::governance_events::GovernanceEvent {
            schema_version: 1,
            seq: 0,
            op: foundry_watch::governance_events::OpKind::LessonSeal,
            phase,
            op_id: op_id.into(),
            payload_digest: "h".into(),
            actor: "child".into(),
            at: "t".into(),
        }
    };

    // 3 complete pairs
    for i in 0..3 {
        c.append(ev(
            &format!("p{i}"),
            foundry_watch::governance_events::Phase::Pending,
        ))
        .unwrap();
        c.append(ev(
            &format!("p{i}"),
            foundry_watch::governance_events::Phase::Committed,
        ))
        .unwrap();
    }
    std::fs::write(&ready, b"ready").unwrap();

    // pacing: give the parent's kill a window to land in the middle
    std::thread::sleep(std::time::Duration::from_millis(30));

    // final intent, then effect
    c.append(ev(
        "final",
        foundry_watch::governance_events::Phase::Pending,
    ))
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(30));
    c.append(ev(
        "final",
        foundry_watch::governance_events::Phase::Committed,
    ))
    .unwrap();
    std::fs::write(&done, b"done").unwrap();

    let _ = Command::new("true").status();
}
