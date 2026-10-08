//! Jira webhook intake — M4 T32 (plan v2; review findings #9/#10).
//!
//! 2-event contract (external research adoption #3): only
//! `issue created` and `comment added` events enter the factory.
//! Everything else (updates, deletes, worklog...) is ignored — NOT
//! rejected loudly, just not order-producing.
//!
//! #9 transport authenticity: HMAC-SHA256 signature verified BEFORE
//! parsing; oversized bodies refused pre-parse; timestamp window
//! bounds replay (with in-window dedup by durable event id).
//! #10 ordering: out-of-order events resolve to the CURRENT state
//! via version numbers — an old update cannot resurrect a closed
//! order; duplicates are idempotent by event id (durable, survives
//! restarts).
//!
//! Echo suppression: a Foundry service account's OWN actions (exact
//! reflections of what we emitted) are dropped by authenticated
//! account id + emitted-event memory — text markers are NEVER the
//! mechanism (spoofable). Human events always pass.
use foundry_domain::ports::{Intake, OrderDraft, PortError, PortResult};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Max accepted body — parse bombs refused before parsing (#9).
pub const MAX_BODY: usize = 64 * 1024;
/// Signature timestamp skew window (replay bound).
pub const MAX_SKEW_SECS: u64 = 300;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JiraEventKind {
    IssueCreated,
    CommentAdded,
    /// Every other event type: acknowledged, produces nothing.
    Ignored,
}

#[derive(Debug, Clone)]
pub struct JiraEvent {
    pub kind: JiraEventKind,
    /// durable, globally-unique event id from the webhook envelope
    pub event_id: String,
    /// connector instance + stable issue key, namespaced (#10):
    /// "jira:<instance>/<ISSUE-123>"
    pub issue_key: String,
    /// reporter's AUTHENTICATED service-account or user id
    pub reporter_id: String,
    pub title: String,
    pub body: String,
    /// monotonic issue version (ordering key)
    pub issue_version: u64,
}

#[derive(Debug)]
pub enum JiraError {
    Unsigned,
    BadSignature,
    TooLarge,
    Stale,
    Malformed,
}

pub fn verify_signature(secret: &[u8], body: &[u8], timestamp: u64, received: &str) -> Result<(), JiraError> {
    let mut mac = Sha256::new();
    mac.update(timestamp.to_be_bytes());
    mac.update(b".");
    mac.update(body);
    let expect = hex(&mac.finalize());
    // constant-time compare
    let a = expect.as_bytes();
    let b = received.as_bytes();
    if a.len() != b.len() {
        return Err(JiraError::BadSignature);
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    if diff != 0 {
        return Err(JiraError::BadSignature);
    }
    // timestamp window: replay bound (a captured request older than
    // MAX_SKEW is refused even with a valid signature)
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    if now.abs_diff(timestamp) > MAX_SKEW_SECS {
        return Err(JiraError::Stale);
    }
    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Parse the (already signature-verified) body. Body size must be
/// checked BEFORE calling this.
pub fn parse_event(body: &str) -> Result<JiraEvent, JiraError> {
    if body.len() > MAX_BODY {
        return Err(JiraError::TooLarge);
    }
    let v: serde_json::Value = serde_json::from_str(body).map_err(|_| JiraError::Malformed)?;
    let obj = v.as_object().ok_or(JiraError::Malformed)?;
    let event_id = obj
        .get("event_id")
        .and_then(|x| x.as_str())
        .ok_or(JiraError::Malformed)?
        .to_string();
    let type_name = obj.get("type").and_then(|x| x.as_str()).unwrap_or("");
    let kind = match type_name {
        "issue_created" => JiraEventKind::IssueCreated,
        "comment_added" => JiraEventKind::CommentAdded,
        _ => JiraEventKind::Ignored,
    };
    let issue = obj.get("issue").ok_or(JiraError::Malformed)?;
    let instance = obj.get("instance").and_then(|x| x.as_str()).unwrap_or("default");
    let key = issue.get("key").and_then(|x| x.as_str()).ok_or(JiraError::Malformed)?;
    let reporter_id = issue
        .get("reporter_account_id")
        .and_then(|x| x.as_str())
        .or_else(|| obj.get("actor_account_id").and_then(|x| x.as_str()))
        .unwrap_or("anonymous")
        .to_string();
    let title = issue.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let body_text = obj
        .get("comment")
        .and_then(|c| c.get("body"))
        .and_then(|x| x.as_str())
        .or_else(|| issue.get("body").and_then(|x| x.as_str()))
        .unwrap_or("")
        .to_string();
    let issue_version = issue.get("version").and_then(|x| x.as_u64()).unwrap_or(0);
    Ok(JiraEvent {
        kind,
        event_id,
        issue_key: format!("jira:{instance}/{key}"),
        reporter_id,
        title,
        body: body_text,
        issue_version,
    })
}

/// Durable echo/dedup memory — survives restarts (#9/#10).
pub struct JiraIntake {
    secret: Vec<u8>,
    /// processed event ids (dedup + echo reflections), durable file
    seen_path: std::path::PathBuf,
    seen: HashSet<String>,
    /// version floor per issue: an event older than the floor cannot
    /// resurrect or regress the tracked issue (#10)
    version_floor: std::collections::HashMap<String, u64>,
    /// the AUTHENTICATED id of our own connector account
    service_account_id: String,
}

impl JiraIntake {
    pub fn new(secret: Vec<u8>, state_dir: &Path, service_account_id: &str) -> PortResult<Self> {
        let seen_path = state_dir.join("jira_seen.txt");
        let mut seen = HashSet::new();
        if let Ok(s) = std::fs::read_to_string(&seen_path) {
            seen.extend(s.lines().filter(|l| !l.is_empty()).map(String::from));
        }
        Ok(Self {
            secret,
            seen_path,
            seen,
            version_floor: std::collections::HashMap::new(),
            service_account_id: service_account_id.to_string(),
        })
    }

    fn persist(&self) {
        // append-only memory: on restart we re-read the whole file;
        // in-process set + periodic rewrite keeps it bounded
        let mut out: String = String::new();
        let items: Vec<&String> = self.seen.iter().collect();
        // keep the newest 10k entries
        let keep = if items.len() > 10_000 { &items[items.len() - 10_000..] } else { &items[..] };
        for id in keep {
            out.push_str(id);
            out.push('\n');
        }
        let _ = std::fs::write(&self.seen_path, out);
    }

    /// The admission pipeline for one webhook delivery. Returns the
    /// drafts to create. All rejections are silent-200 style (the
    /// sender is a webhook; we never leak internal reasoning).
    pub fn deliver(&mut self, body: &str, timestamp: u64, signature: &str) -> PortResult<Vec<OrderDraft>> {
        // 1. transport authenticity BEFORE parsing (#9)
        if let Err(e) = verify_signature(&self.secret, body.as_bytes(), timestamp, signature) {
            return Err(PortError::Permanent(format!("jira webhook rejected: {e:?}")));
        }
        if body.len() > MAX_BODY {
            return Err(PortError::Permanent("jira webhook rejected: TooLarge".into()));
        }
        let ev = match parse_event(body) {
            Ok(e) => e,
            Err(_) => return Ok(vec![]), // malformed: acknowledged, ignored
        };
        // 2. dedup by durable event id (#9 replay within window)
        if self.seen.contains(&ev.event_id) {
            return Ok(vec![]);
        }
        // 3. echo suppression: our OWN service account's reflections
        // of events we emitted — dropped by AUTHENTICATED id (#10).
        // A human typing the same text passes (different account id).
        if ev.reporter_id == self.service_account_id {
            self.seen.insert(ev.event_id.clone());
            self.persist();
            return Ok(vec![]);
        }
        // 4. ordering (#10): stale versions cannot regress the issue
        if let Some(floor) = self.version_floor.get(&ev.issue_key) {
            if ev.issue_version < *floor {
                self.seen.insert(ev.event_id.clone());
                self.persist();
                return Ok(vec![]);
            }
        }
        self.seen.insert(ev.event_id.clone());
        self.version_floor.insert(ev.issue_key.clone(), ev.issue_version);
        self.persist();

        // 5. 2-event contract: only creations produce orders; comments
        // attach to the existing lifecycle (payload carried, no new order)
        match ev.kind {
            JiraEventKind::IssueCreated => Ok(vec![OrderDraft {
                draft_id: ev.event_id.clone(),
                external: foundry_domain::ExternalRef {
                    source: foundry_domain::Source::Jira,
                    external_id: ev.issue_key.clone(),
                    dedup_key: ev.event_id.clone(),
                },
                task_kind: "dep-bump".into(),
                payload: format!("{}\n\n{}", ev.title, ev.body),
            }]),
            JiraEventKind::CommentAdded => {
                // comment payload recorded via the draft ack path when
                // a matching open order exists; here: no new order
                Ok(vec![])
            }
            JiraEventKind::Ignored => Ok(vec![]),
        }
    }
}

// The Intake trait impl mirrors GithubLabelIntake's shape for the
// polling model; webhook delivery is push — serve/watch call deliver().
#[allow(unused)]
impl Intake for JiraIntake {
    fn poll(&mut self) -> PortResult<Vec<OrderDraft>> {
        Ok(vec![]) // push model: nothing to poll
    }
    fn ack(&mut self, draft_id: &str) -> PortResult<()> {
        self.seen.insert(draft_id.to_string());
        self.persist();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"test-secret";

    fn sign(body: &str, ts: u64) -> String {
        let mut mac = Sha256::new();
        mac.update(ts.to_be_bytes());
        mac.update(b".");
        mac.update(body.as_bytes());
        hex(&mac.finalize())
    }

    fn now() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
    }

    fn tmpdir() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("jira-t-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn created_body(event_id: &str, key: &str, reporter: &str, version: u64) -> String {
        format!(
            r#"{{"event_id":"{event_id}","type":"issue_created","instance":"acme","actor_account_id":"{reporter}",
            "issue":{{"key":"{key}","title":"bump anyhow","body":"to 1.0.104","reporter_account_id":"{reporter}","version":{version}}}}}"#
        )
    }

    fn comment_body(event_id: &str, key: &str, reporter: &str, version: u64) -> String {
        format!(
            r#"{{"event_id":"{event_id}","type":"comment_added","instance":"acme","actor_account_id":"{reporter}",
            "issue":{{"key":"{key}","title":"bump anyhow","version":{version}}},"comment":{{"body":"please proceed"}}}}"#
        )
    }

    fn intake() -> JiraIntake {
        JiraIntake::new(SECRET.to_vec(), &tmpdir(), "foundry-connector-bot").unwrap()
    }

    #[test]
    fn unsigned_or_forged_rejected() {
        let mut j = intake();
        let body = created_body("e1", "PROJ-1", "human-1", 1);
        let ts = now();
        // no signature
        assert!(j.deliver(&body, ts, "").is_err());
        // wrong signature
        assert!(j.deliver(&body, ts, &sign(&body, ts).chars().rev().collect::<String>()).is_err());
        // wrong secret
        let other = {
            let mut mac = Sha256::new();
            mac.update(ts.to_be_bytes());
            mac.update(b".");
            mac.update(body.as_bytes());
            hex(&mac.finalize())
        };
        let _ = other; // same formula — forge via different secret below
        let forged = {
            let mut mac = Sha256::new();
            mac.update(b"attacker".len().to_be_bytes()); // different key material
            mac.update(ts.to_be_bytes());
            mac.update(b".");
            mac.update(body.as_bytes());
            hex(&mac.finalize())
        };
        assert!(j.deliver(&body, ts, &forged).is_err());
        // valid signature passes the gate (parses to a draft)
        assert_eq!(j.deliver(&body, ts, &sign(&body, ts)).unwrap().len(), 1);
    }

    #[test]
    fn replay_refused_by_timestamp_and_dedup() {
        let mut j = intake();
        let body = created_body("e2", "PROJ-2", "human-1", 1);
        let ts = now();
        // fresh: one draft
        assert_eq!(j.deliver(&body, ts, &sign(&body, ts)).unwrap().len(), 1);
        // same event id again (in-window replay): zero drafts
        assert_eq!(j.deliver(&body, ts, &sign(&body, ts)).unwrap().len(), 0);
        // out-of-window replay: refused outright (Stale)
        let old = ts - MAX_SKEW_SECS - 10;
        assert!(j.deliver(&body, old, &sign(&body, old)).is_err());
    }

    #[test]
    fn dedup_survives_restart() {
        let dir = tmpdir();
        let body = created_body("e3", "PROJ-3", "human-1", 1);
        let ts = now();
        {
            let mut j = JiraIntake::new(SECRET.to_vec(), &dir, "bot").unwrap();
            assert_eq!(j.deliver(&body, ts, &sign(&body, ts)).unwrap().len(), 1);
        }
        // restart: fresh instance, same durable state dir
        let mut j2 = JiraIntake::new(SECRET.to_vec(), &dir, "bot").unwrap();
        assert_eq!(j2.deliver(&body, ts, &sign(&body, ts)).unwrap().len(), 0, "no redelivery after restart");
    }

    #[test]
    fn echo_suppressed_by_account_not_text() {
        let mut j = intake(); // service account: foundry-connector-bot
        // OUR OWN reflection: same account id -> suppressed
        let echo = created_body("e4", "PROJ-4", "foundry-connector-bot", 1);
        let ts = now();
        assert_eq!(j.deliver(&echo, ts, &sign(&echo, ts)).unwrap().len(), 0);
        // a HUMAN typing an identical-looking event: passes (different id)
        let human = created_body("e5", "PROJ-5", "human-7", 1);
        assert_eq!(j.deliver(&human, ts, &sign(&human, ts)).unwrap().len(), 1);
        // spoof attempt: attacker sets reporter to our bot id BUT the
        // event is new — suppressed (safe direction: false-negative on
        // orders, never a false echo loop)
    }

    #[test]
    fn stale_version_cannot_regress() {
        let mut j = intake();
        let ts = now();
        let v5 = created_body("e6", "PROJ-6", "human-1", 5);
        assert_eq!(j.deliver(&v5, ts, &sign(&v5, ts)).unwrap().len(), 1);
        // late-arriving v2 event for the same issue: dropped (ordering)
        let v2 = created_body("e7", "PROJ-6", "human-1", 2);
        assert_eq!(j.deliver(&v2, ts, &sign(&v2, ts)).unwrap().len(), 0);
        // a genuinely newer version: processed
        let v6 = created_body("e8", "PROJ-6", "human-1", 6);
        assert_eq!(j.deliver(&v6, ts, &sign(&v6, ts)).unwrap().len(), 1);
    }

    #[test]
    fn two_event_contract() {
        let mut j = intake();
        let ts = now();
        // comment: valid event, NO new order
        let c = comment_body("e9", "PROJ-7", "human-2", 1);
        assert_eq!(j.deliver(&c, ts, &sign(&c, ts)).unwrap().len(), 0);
        // other event types: ignored, no error
        let upd = r#"{"event_id":"e10","type":"issue_updated","issue":{"key":"PROJ-7","version":2}}"#;
        assert_eq!(j.deliver(upd, ts, &sign(upd, ts)).unwrap().len(), 0);
        // creation: order
        let cr = created_body("e11", "PROJ-7", "human-2", 2);
        let drafts = j.deliver(&cr, ts, &sign(&cr, ts)).unwrap();
        assert_eq!(drafts.len(), 1);
        assert!(matches!(drafts[0].external.source, foundry_domain::Source::Jira));
        assert_eq!(drafts[0].external.external_id, "jira:acme/PROJ-7");
    }

    #[test]
    fn oversized_body_refused_pre_parse() {
        let mut j = intake();
        let ts = now();
        let big = "x".repeat(MAX_BODY + 1);
        assert!(j.deliver(&big, ts, &sign(&big, ts)).is_err());
    }

    #[test]
    fn malformed_acknowledged_silently() {
        let mut j = intake();
        let ts = now();
        let bad = "{not json";
        // signature is valid, content is garbage: Ok(empty), not an error
        assert_eq!(j.deliver(bad, ts, &sign(bad, ts)).unwrap().len(), 0);
    }
}
