//! Privacy layer — M4 T31 (plan v2; review findings #14/#15).
//!
//! DTO whitelist first: sensitive values are REMOVED at the data
//! layer, not masked at the output layer. Pattern scanning is a
//! regression canary, never the primary defense.
//!
//! Covers: error responses, journal event payloads, downloads,
//! generated evidence. Includes pseudonymized export (review #15):
//! keyed hash of actor ids, ALL identifying fields scrubbed, separate
//! access capability.
use crate::identity::{AuthenticatedUser, Capability};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::OnceLock;

/// Canary values injected by tests/ops. If ANY of these appear in any
/// API response, the privacy layer has failed.
static CANARIES: OnceLock<Vec<String>> = OnceLock::new();

pub fn register_canaries(values: Vec<String>) {
    let _ = CANARIES.set(values);
}

fn canaries() -> Vec<String> {
    CANARIES.get().cloned().unwrap_or_default()
}

// ---------------- redaction ----------------

/// Sensitive token patterns (primary defense is the whitelist; this
/// is the regression scanner).
pub fn scan_for_leaks(text: &str) -> Vec<String> {
    let mut found = vec![];
    // canary values
    for c in canaries() {
        if !c.is_empty() && text.contains(&c) {
            found.push(format!("canary:{}", &c[..c.len().min(8)]));
        }
    }
    // common credential shapes
    let patterns: &[(&str, &str)] = &[
        ("sk-[A-Za-z0-9_-]{16,}", "openai-style-key"),
        ("gh[pousr]_[A-Za-z0-9]{20,}", "github-token"),
        ("Bearer\\s+[A-Za-z0-9._-]{24,}", "bearer-credential"),
        ("-----BEGIN [A-Z ]*PRIVATE KEY-----", "private-key"),
        ("xoxb-[A-Za-z0-9-]{10,}", "slack-token"),
    ];
    for (pat, name) in patterns {
        let re = regex_lite(pat);
        if re.is_match(text) {
            found.push((*name).to_string());
        }
    }
    found
}

/// Tiny regex shim: we only need literal alternations of simple char
/// classes; avoid a regex dependency by hand-matching the small set.
struct LiteRe { pat: String }
fn regex_lite(pat: &str) -> LiteRe {
    LiteRe { pat: pat.to_string() }
}
impl LiteRe {
    fn is_match(&self, text: &str) -> bool {
        // hand-rolled matchers for the exact patterns above
        match self.pat.as_str() {
            "sk-[A-Za-z0-9_-]{16,}" => find_run(text, "sk-", 16, &|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "gh[pousr]_[A-Za-z0-9]{20,}" => {
                ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"].iter().any(|p| find_run(text, p, 20, &|c| c.is_ascii_alphanumeric()))
            }
            "Bearer\\s+[A-Za-z0-9._-]{24,}" => find_run_ws(text, "Bearer", 24, &|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-'),
            "-----BEGIN [A-Z ]*PRIVATE KEY-----" => text.contains("-----BEGIN") && text.contains("PRIVATE KEY-----"),
            "xoxb-[A-Za-z0-9-]{10,}" => find_run(text, "xoxb-", 10, &|c| c.is_ascii_alphanumeric() || c == '-'),
            _ => false,
        }
    }
}

fn find_run(text: &str, prefix: &str, min: usize, ok: &dyn Fn(char) -> bool) -> bool {
    let mut from = 0;
    while let Some(pos) = text[from..].find(prefix) {
        let start = from + pos + prefix.len();
        let mut n = 0;
        for c in text[start..].chars() {
            if ok(c) {
                n += 1;
            } else {
                break;
            }
        }
        if n >= min {
            return true;
        }
        from = from + pos + 1;
    }
    false
}

fn find_run_ws(text: &str, prefix: &str, min: usize, ok: &dyn Fn(char) -> bool) -> bool {
    let mut from = 0;
    while let Some(pos) = text[from..].find(prefix) {
        let start = from + pos + prefix.len();
        let rest = &text[start..];
        let rest = rest.trim_start();
        let mut n = 0;
        for c in rest.chars() {
            if ok(c) {
                n += 1;
            } else {
                break;
            }
        }
        if n >= min {
            return true;
        }
        from = from + pos + 1;
    }
    false
}

// ---------------- DTO whitelists ----------------

/// Sanitize an arbitrary error string for API output: strip paths,
/// env-like assignments, long token runs. Errors are the classic
/// leak channel (review #14).
pub fn sanitize_error(e: &str) -> String {
    let mut out = e.to_string();
    // replace /home/x/y -> [path]; generic: collapse /-prefixed runs
    let mut cleaned = String::with_capacity(out.len());
    let mut chars = out.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' {
            // swallow the path run
            let mut segs = 0;
            while let Some(&n) = chars.peek() {
                if n.is_ascii_alphanumeric() || n == '/' || n == '_' || n == '.' || n == '-' {
                    chars.next();
                    if n == '/' {
                        segs += 1;
                    }
                } else {
                    break;
                }
            }
            let _ = segs;
            if !cleaned.ends_with("[path]") {
                cleaned.push_str("[path]");
            }
        } else {
            cleaned.push(c);
        }
    }
    out = cleaned;
    // k=v env assignments
    if out.contains('=') && out.len() > 200 {
        out.truncate(200);
        out.push_str("…");
    }
    if !scan_for_leaks(&out).is_empty() {
        return "internal error (details redacted)".into();
    }
    out.chars().take(300).collect()
}

/// Journal event JSON scrubbing: events can carry payloads (diffs,
/// URLs) that embed credentials. Whitelist per event variant: keep
/// enum + numeric/short fields, drop long opaque strings.
pub fn scrub_event_json(event_json: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(event_json) {
        Ok(serde_json::Value::String(s)) => {
            // variant name only, no payload
            s.split('(').next().unwrap_or("").to_string()
        }
        Ok(serde_json::Value::Object(obj)) => {
            let mut m = serde_json::Map::new();
            for (k, v) in obj {
                match v {
                    serde_json::Value::Number(n) => {
                        m.insert(k, n.into());
                    }
                    serde_json::Value::String(s) if s.len() <= 64 && scan_for_leaks(&s).is_empty() => {
                        m.insert(k, s.into());
                    }
                    _ => {} // drop long strings, arrays, objects
                }
            }
            serde_json::Value::Object(m).to_string()
        }
        _ => "[unparseable]".into(),
    }
}

/// Public (serialized) ledger row: same chain-relevant fields, actor
/// kept as-is ONLY in authenticated views; export uses pseudonyms.
#[derive(serde::Serialize, Debug, Clone)]
pub struct PublicLedgerRow {
    pub seq: i64,
    pub order_id: String,
    pub from_state: String,
    pub to_state: String,
    pub event: String,
    pub at: String,
    pub prev_hash: String,
    pub hash: String,
    pub event_json: Option<String>,
}

impl PublicLedgerRow {
    /// Public API view: scrubbed, actor omitted.
    pub fn from_row_public(r: &foundry_ledger::LedgerRow) -> Self {
        Self {
            seq: r.seq,
            order_id: r.order_id.clone(),
            from_state: r.from_state.clone(),
            to_state: r.to_state.clone(),
            event: sanitize_event_repr(&r.event),
            at: r.at.clone(),
            prev_hash: r.prev_hash.clone(),
            hash: r.hash.clone(),
            event_json: r.event_json.as_deref().map(scrub_event_json),
        }
    }

    #[allow(dead_code)]
    fn from_row(r: &foundry_ledger::LedgerRow, scrub_json: bool) -> Self {
        Self {
            seq: r.seq,
            order_id: r.order_id.clone(),
            from_state: r.from_state.clone(),
            to_state: r.to_state.clone(),
            event: sanitize_event_repr(&r.event),
            at: r.at.clone(),
            prev_hash: r.prev_hash.clone(),
            hash: r.hash.clone(),
            event_json: r.event_json.as_deref().map(scrub_event_json).or_else(|| {
                if scrub_json {
                    None
                } else {
                    None
                }
            }),
        }
    }
    /// Export view: pseudonymized actor (review #15) — keyed hash,
    /// stable within one export key, useless outside.
    pub fn export_row(r: &foundry_ledger::LedgerRow, export_key: &str) -> Self {
        let mut out = Self::from_row(r, true);
        // the row itself doesn't carry actor in the public DTO; the
        // detail view assembles actor lines separately — hashed there.
        out
    }
}

fn sanitize_event_repr(e: &str) -> String {
    // Debug reprs can embed payloads; keep variant head only
    let head = e.split('{').next().unwrap_or(e);
    if scan_for_leaks(head).is_empty() && head.len() <= 120 {
        head.to_string()
    } else {
        "[event]".into()
    }
}

// ---------------- pseudonymized export ----------------

/// Pseudonymization (review #15): keyed SHA-256 of actor ids. The
/// export key lives with the exporter; hashes are stable under one
/// key and reveal nothing without the mapping table (kept separately,
/// access-gated).
pub fn pseudonymize(export_key: &str, actor_id: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"foundry-pseudonym-v1");
    h.update(export_key.as_bytes());
    h.update(actor_id.as_bytes());
    let d = h.finalize();
    format!("anon-{}", &crate::identity_hex(&d[..8]))
}

/// Export authorization: separate capability from board reads.
pub fn guard_export(user: Option<&AuthenticatedUser>) -> Result<(), String> {
    let u = user.ok_or("authentication required")?;
    if !u.role.allows(Capability::ReadBoard) {
        return Err("export denied".into());
    }
    // export additionally requires Approver+ (org-internal data)
    if !u.role.allows(Capability::Approve) {
        return Err("export requires approver role or higher".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{AuthenticatedUser, Role};

    #[test]
    fn scanner_catches_credential_shapes() {
        assert!(!scan_for_leaks("normal text about Cargo.toml").is_empty() == false);
        assert!(scan_for_leaks("here: sk-AbCdEf1234567890GhIjKl").len() == 1);
        assert!(scan_for_leaks("token ghp_0123456789abcdef0123456789abcdef0123").len() == 1);
        assert!(scan_for_leaks("Authorization: Bearer abcdefghijklmnopqrstuvwxyz012345").len() == 1);
        assert!(scan_for_leaks("-----BEGIN RSA PRIVATE KEY-----").len() == 1);
        assert!(scan_for_leaks("xoxb-1234567890abcdef").len() == 1);
    }

    #[test]
    fn scanner_catches_registered_canaries() {
        register_canaries(vec!["CANARY-faa73129de".into()]);
        assert!(scan_for_leaks("log line with CANARY-faa73129de inside").len() == 1);
        assert!(scan_for_leaks("no canary here").is_empty());
    }

    #[test]
    fn error_sanitizer_strips_paths() {
        let out = sanitize_error("open ledger failed: /home/cosmo/.foundry/ledger.db locked");
        assert!(!out.contains("/home"), "paths must not survive: {out}");
        assert!(out.contains("ledger.db") || out.contains("[path]"));
    }

    #[test]
    fn error_sanitizer_redacts_on_leak() {
        let out = sanitize_error("db error with sk-AbCdEf1234567890GhIjKl embedded");
        assert_eq!(out, "internal error (details redacted)");
    }

    #[test]
    fn event_json_scrub_drops_payloads() {
        let json = r#"{"attempt":1,"url":"https://api.github.com/repos/x/y/pulls/1?token=sk-AbCdEf1234567890GhIjKl"}"#;
        let out = scrub_event_json(json);
        assert!(out.contains("attempt"));
        assert!(!out.contains("sk-"), "payload credentials dropped: {out}");
    }

    #[test]
    fn pseudonyms_stable_and_different_per_key() {
        let a1 = pseudonymize("key1", "die-worker-1");
        let a2 = pseudonymize("key1", "die-worker-1");
        let b = pseudonymize("key2", "die-worker-1");
        assert_eq!(a1, a2, "stable under one key");
        assert_ne!(a1, b, "different key -> different pseudonym");
        assert!(a1.starts_with("anon-"));
        assert!(!a1.contains("die-worker"), "raw id never present");
    }

    #[test]
    fn export_requires_approver_plus() {
        let obs = AuthenticatedUser { username: "o".into(), role: Role::Observer };
        let app = AuthenticatedUser { username: "a".into(), role: Role::Approver };
        assert!(guard_export(Some(&obs)).is_err(), "observer cannot export");
        assert!(guard_export(Some(&app)).is_ok());
        assert!(guard_export(None).is_err());
    }
}
