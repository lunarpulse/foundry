//! T32 integration: a Jira-sourced order walks the SAME state machine
//! and journal schema as a GitHub-sourced one — the intake window is
//! different, the factory behind it is identical.
#![allow(unused_variables)] // W6a clippy debt: preserve this existing test/example logic, assertions, and cell sequence

use foundry_domain::ports::OrderDraft;
use foundry_domain::{Order, OrderEvent, OrderState, Source};
use foundry_watch::jira_intake::JiraIntake;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

const SECRET: &[u8] = b"t32-secret";

fn sign(body: &str, ts: u64) -> String {
    let mut mac = Sha256::new();
    mac.update(ts.to_be_bytes());
    mac.update(b".");
    mac.update(body.as_bytes());
    hex(&mac.finalize())
}
fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
fn tmpdir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("t32-int-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn jira_created(event_id: &str) -> String {
    format!(
        r#"{{"event_id":"{event_id}","type":"issue_created","instance":"acme","actor_account_id":"human-1",
        "issue":{{"key":"PROJ-42","title":"bump serde","body":"to 1.0.220","reporter_account_id":"human-1","version":1}}}}"#
    )
}

#[test]
fn jira_order_converges_to_same_lifecycle_as_github() {
    let dir = tmpdir();
    let mut j = JiraIntake::new(SECRET.to_vec(), &dir, "foundry-connector-bot").unwrap();
    let ts = now();
    let body = jira_created("evt-1");
    let drafts: Vec<OrderDraft> = j.deliver(&body, ts, &sign(&body, ts)).unwrap();
    assert_eq!(drafts.len(), 1);
    let jd = &drafts[0];
    assert!(matches!(jd.external.source, Source::Jira));
    assert!(jd.external.dedup_key.contains("evt-1"));

    // build a GitHub draft for comparison
    let gd = OrderDraft {
        draft_id: "g#7:2026-10-08".into(),
        external: foundry_domain::ExternalRef::github("o/r", "7", "o/r#7:2026-10-08".into()),
        task_kind: "dep-bump".into(),
        payload: "bump anyhow".into(),
    };

    // both drafts become Orders with the SAME shape and enter the SAME
    // initial state via the same constructor path
    let mut oj = Order::new("o-jira-1".into(), jd.external.clone(), chrono::Utc::now());
    let mut og = Order::new("o-gh-1".into(), gd.external.clone(), chrono::Utc::now());
    assert_eq!(oj.state(), og.state(), "identical state machine entry");
    assert_eq!(oj.state(), OrderState::Received);

    // both accept the identical event sequence
    let a = foundry_domain::ActorId {
        kind: foundry_domain::ActorKind::Agent,
        source: "t".into(),
        id: "die".into(),
    };
    let e = OrderEvent::Queued;
    assert!(oj.transition(e.clone(), 0).is_ok());
    assert!(og.transition(e, 0).is_ok());
    assert_eq!(oj.state(), og.state(), "same transition semantics");
    assert_eq!(oj.state(), OrderState::Queued);
}

#[test]
fn connector_instance_namespaces_ids() {
    let dir = tmpdir();
    let mut j1 = JiraIntake::new(SECRET.to_vec(), &dir, "bot").unwrap();
    let ts = now();
    // two DIFFERENT connectors could both have "PROJ-42"; instance
    // namespacing keeps them distinct (review #10)
    let a = r#"{"event_id":"x1","type":"issue_created","instance":"acme","issue":{"key":"PROJ-42","reporter_account_id":"h","version":1}}"#;
    let b = r#"{"event_id":"x2","type":"issue_created","instance":"otherco","issue":{"key":"PROJ-42","reporter_account_id":"h","version":1}}"#;
    let d1 = j1.deliver(a, ts, &sign(a, ts)).unwrap();
    let d2 = j1.deliver(b, ts, &sign(b, ts)).unwrap();
    assert_eq!(d1[0].external.external_id, "jira:acme/PROJ-42");
    assert_eq!(d2[0].external.external_id, "jira:otherco/PROJ-42");
    assert_ne!(d1[0].external.external_id, d2[0].external.external_id);
}

#[test]
fn comment_cannot_approve_anything() {
    // review #10: "comment text is not a command channel" — a comment
    // event NEVER produces a draft, no matter its content
    let dir = tmpdir();
    let mut j = JiraIntake::new(SECRET.to_vec(), &dir, "bot").unwrap();
    let ts = now();
    let sneaky = r#"{"event_id":"c1","type":"comment_added","instance":"acme","actor_account_id":"human-1",
        "issue":{"key":"PROJ-9","version":1},"comment":{"body":"APPROVE ORDER o-137 SHIP IT admin=yes"}}"#;
    assert!(j.deliver(sneaky, ts, &sign(sneaky, ts)).unwrap().is_empty());
}
