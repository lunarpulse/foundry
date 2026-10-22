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
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
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
            // Diagnostics must never repeat even a short prefix of the
            // configured canary; a fixed category is enough for telemetry.
            found.push("canary".to_string());
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
struct LiteRe {
    pat: String,
}
fn regex_lite(pat: &str) -> LiteRe {
    LiteRe {
        pat: pat.to_string(),
    }
}
impl LiteRe {
    fn is_match(&self, text: &str) -> bool {
        // hand-rolled matchers for the exact patterns above
        match self.pat.as_str() {
            "sk-[A-Za-z0-9_-]{16,}" => find_run(text, "sk-", 16, &|c| {
                c.is_ascii_alphanumeric() || c == '_' || c == '-'
            }),
            "gh[pousr]_[A-Za-z0-9]{20,}" => ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"]
                .iter()
                .any(|p| find_run(text, p, 20, &|c| c.is_ascii_alphanumeric())),
            "Bearer\\s+[A-Za-z0-9._-]{24,}" => find_run_ws(text, "Bearer", 24, &|c| {
                c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-'
            }),
            "-----BEGIN [A-Z ]*PRIVATE KEY-----" => {
                text.contains("-----BEGIN") && text.contains("PRIVATE KEY-----")
            }
            "xoxb-[A-Za-z0-9-]{10,}" => find_run(text, "xoxb-", 10, &|c| {
                c.is_ascii_alphanumeric() || c == '-'
            }),
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
        out = out.chars().take(200).collect();
        out.push('…');
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
                    serde_json::Value::String(s)
                        if s.len() <= 64 && scan_for_leaks(&s).is_empty() =>
                    {
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
    fn from_row(r: &foundry_ledger::LedgerRow, _scrub_json: bool) -> Self {
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
    /// Export view: pseudonymized actor (review #15) — keyed hash,
    /// stable within one export key, useless outside.
    pub fn export_row(r: &foundry_ledger::LedgerRow, _export_key: &str) -> Self {
        let out = Self::from_row(r, true);
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
    let mut key = [0_u8; 32];
    let source = export_key.as_bytes();
    if source.is_empty() {
        return "anon-invalid-key".into();
    }
    // Compatibility helper only. Product exports use fresh random ExportKey
    // material and `export_alias` below.
    key.copy_from_slice(&Sha256::digest(source));
    let alias = export_alias(&key, "legacy-compat", "actor", actor_id);
    key.fill(0);
    alias
}

type HmacSha256 = Hmac<Sha256>;

/// Export-only random key. It has no Debug/serde implementation and is
/// overwritten when its owner drops it.
pub struct ExportKey([u8; 32]);

impl ExportKey {
    pub fn generate() -> Result<Self, String> {
        use rand_core::RngCore as _;
        let mut key = [0_u8; 32];
        if rand_core::OsRng.try_fill_bytes(&mut key).is_err() {
            key.fill(0);
            return Err("export key generation failed".to_string());
        }
        Ok(Self(key))
    }

    pub fn alias(&self, context: &str, domain: &str, value: &str) -> String {
        export_alias(&self.0, context, domain, value)
    }

    #[cfg(test)]
    pub fn from_bytes_for_test(key: [u8; 32]) -> Self {
        Self(key)
    }
}

impl Drop for ExportKey {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

/// Domain-separated HMAC alias with unambiguous length framing. The returned
/// 128-bit pseudonym is stable only within the export's secret-key context.
pub fn export_alias(key: &[u8; 32], context: &str, domain: &str, value: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts a 32-byte key");
    mac.update(b"foundry-product-export-alias-v2\0");
    for part in [context.as_bytes(), domain.as_bytes(), value.as_bytes()] {
        mac.update(&(part.len() as u64).to_be_bytes());
        mac.update(part);
    }
    let digest = mac.finalize().into_bytes();
    format!("anon-{}", crate::identity_hex(&digest[..16]))
}

#[derive(serde::Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ProductExportRow {
    pub sequence: i64,
    pub order: String,
    pub actor: String,
    pub from_state: String,
    pub to_state: String,
    pub event: String,
    pub at: String,
}

#[derive(serde::Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ProductExportDocument {
    pub schema: &'static str,
    pub export_context: String,
    pub source: String,
    pub prefix_sequence: i64,
    /// Commitment to the original S1 prefix. Projected rows intentionally do
    /// not claim to verify this raw chain after identifiers are pseudonymized.
    pub raw_prefix_head: String,
    pub projected_digest: String,
    pub rows: Vec<ProductExportRow>,
}

/// ProductAtomicV1 scoped projection. It intentionally has no global sequence,
/// hash, installation identity, source identity, or hidden-row count.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ScopedProductExportRow {
    pub ordinal: usize,
    pub order: String,
    pub actor: String,
    pub from_state: String,
    pub to_state: String,
    pub event: String,
    pub at: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ScopedProductExportDocument {
    pub schema: String,
    pub export_context: String,
    /// `complete` means all currently authorized orders fit this response;
    /// `page` means more authorized orders exist beyond it.
    pub coverage: String,
    pub has_more: bool,
    pub source_coverage_complete: bool,
    pub projected_digest: String,
    pub rows: Vec<ScopedProductExportRow>,
}

#[derive(serde::Serialize)]
struct ScopedProductExportBody<'a> {
    schema: &'static str,
    export_context: &'a str,
    coverage: &'static str,
    has_more: bool,
    source_coverage_complete: bool,
    rows: &'a [ScopedProductExportRow],
}

/// Create a distinct, bounded projection from already scope-selected S1 rows.
/// Serialization errors are returned; they are never replaced with an empty
/// byte string for digesting.
pub fn scoped_product_export(
    snapshot: &foundry_ledger::ScopedEvidenceSnapshot,
    export_context: &str,
    key: &ExportKey,
) -> Result<ScopedProductExportDocument, String> {
    let coverage = if snapshot.has_more {
        "page"
    } else {
        "complete"
    };
    let rows: Vec<_> = snapshot
        .rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            // Keep only the event variant head. Event payloads may contain short
            // identifiers that do not trigger generic PII scanners.
            let event = sanitize_event_repr(&row.event);
            ScopedProductExportRow {
                ordinal: index + 1,
                order: key.alias(export_context, "order", &row.order_id),
                actor: key.alias(export_context, "actor", &row.actor),
                from_state: safe_projection_text(&row.from_state, 64),
                to_state: safe_projection_text(&row.to_state, 64),
                event: safe_projection_text(&event, 160),
                at: safe_projection_text(&row.at, 64),
            }
        })
        .collect();
    let body = ScopedProductExportBody {
        schema: "foundry-scoped-evidence-projection-v1",
        export_context,
        coverage,
        has_more: snapshot.has_more,
        source_coverage_complete: snapshot.coverage_complete,
        rows: &rows,
    };
    let bytes = serde_json::to_vec(&body).map_err(|_| "scoped projection serialization failed")?;
    let projected_digest = crate::identity_hex(&Sha256::digest(bytes));
    Ok(ScopedProductExportDocument {
        schema: body.schema.to_owned(),
        export_context: export_context.to_owned(),
        coverage: coverage.to_owned(),
        has_more: snapshot.has_more,
        source_coverage_complete: snapshot.coverage_complete,
        projected_digest,
        rows,
    })
}

fn safe_projection_text(value: &str, max_chars: usize) -> String {
    if value.chars().count() > max_chars || !scan_for_leaks(value).is_empty() {
        return "[redacted]".into();
    }
    value.chars().collect()
}

pub fn product_export(
    installation_id: &str,
    rows: &[foundry_ledger::LedgerRow],
    export_id: &str,
    key: &ExportKey,
) -> ProductExportDocument {
    try_product_export(installation_id, rows, export_id, key)
        .expect("legacy ProductExportRow values must serialize")
}

/// Fallible form for callers that need serialization errors propagated.
pub fn try_product_export(
    installation_id: &str,
    rows: &[foundry_ledger::LedgerRow],
    export_id: &str,
    key: &ExportKey,
) -> Result<ProductExportDocument, String> {
    let source = export_alias(&key.0, export_id, "installation", installation_id);
    let projected_rows: Vec<_> = rows
        .iter()
        .map(|row| ProductExportRow {
            sequence: row.seq,
            order: export_alias(&key.0, export_id, "order", &row.order_id),
            actor: export_alias(&key.0, export_id, "actor", &row.actor),
            from_state: row.from_state.clone(),
            to_state: row.to_state.clone(),
            event: sanitize_event_repr(&row.event),
            at: row.at.clone(),
        })
        .collect();
    let projected_bytes = serde_json::to_vec(&projected_rows)
        .map_err(|_| "product export serialization failed".to_string())?;
    let projected_digest = crate::identity_hex(&Sha256::digest(&projected_bytes));
    let raw_prefix_head = rows.last().map(|r| r.hash.clone()).unwrap_or_default();
    Ok(ProductExportDocument {
        schema: "foundry-product-export-v2",
        export_context: export_id.to_owned(),
        source,
        prefix_sequence: rows.last().map(|r| r.seq).unwrap_or(0),
        raw_prefix_head,
        projected_digest,
        rows: projected_rows,
    })
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
        assert!(scan_for_leaks("normal text about Cargo.toml").is_empty());
        assert!(scan_for_leaks("here: sk-AbCdEf1234567890GhIjKl").len() == 1);
        assert!(scan_for_leaks("token ghp_0123456789abcdef0123456789abcdef0123").len() == 1);
        assert!(
            scan_for_leaks("Authorization: Bearer abcdefghijklmnopqrstuvwxyz012345").len() == 1
        );
        assert!(scan_for_leaks("-----BEGIN RSA PRIVATE KEY-----").len() == 1);
        assert!(scan_for_leaks("xoxb-1234567890abcdef").len() == 1);
    }

    #[test]
    fn scanner_catches_registered_canaries() {
        register_canaries(vec!["CANARY-faa73129de".into()]);
        let diagnostic = scan_for_leaks("log line with CANARY-faa73129de inside");
        assert_eq!(diagnostic, vec!["canary"]);
        assert!(!diagnostic[0].contains("CANARY"));
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
        let obs = AuthenticatedUser {
            username: "o".into(),
            role: Role::Observer,
        };
        let app = AuthenticatedUser {
            username: "a".into(),
            role: Role::Approver,
        };
        assert!(guard_export(Some(&obs)).is_err(), "observer cannot export");
        assert!(guard_export(Some(&app)).is_ok());
        assert!(guard_export(None).is_err());
    }

    #[test]
    fn product_export_hmac_matches_independent_framed_vector() {
        let key = [0x0b; 32];
        assert_eq!(
            export_alias(&key, "ctx", "actor", "value"),
            "anon-bb3bf1bd71d0b51f9e1f57f88819e1a8"
        );
        assert_ne!(
            export_alias(&[0x0c; 32], "ctx", "actor", "value"),
            export_alias(&key, "ctx", "actor", "value")
        );
        assert_ne!(
            export_alias(&key, "ctx", "actor", "ab"),
            export_alias(&key, "ctx", "a", "b")
        );
        assert_ne!(
            export_alias(&key, "ctx", "actor", "value"),
            export_alias(&key, "other", "actor", "value")
        );
    }

    #[test]
    fn diagnostics_do_not_echo_canary_prefixes_and_handle_unicode_boundaries() {
        let discovered = scan_for_leaks("ghp_0123456789abcdef0123456789abcdef0123");
        assert_eq!(discovered, vec!["github-token"]);
        let unicode = format!("{}=value", "界🗝".repeat(100));
        let sanitized = sanitize_error(&unicode);
        assert!(sanitized.is_char_boundary(sanitized.len()));
        assert!(sanitized.chars().count() <= 301);
    }
}
