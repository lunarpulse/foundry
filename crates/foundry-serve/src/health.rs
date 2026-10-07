//! /api/status health inputs — secret-free by contract (review #5).
//!
//! Reports existence + permission BITS only. Never paths, never contents,
//! never env values. The UI must not become a secret oracle.

pub fn tmp_usage_pct() -> Option<u32> {
    // statfs on /tmp without libc here: reuse the same syscalls via
    // std-free approach — nix not available; do it via /proc/self/mountinfo?
    // Simplest portable-enough: shell out to `df` is process-spawn per
    // request — no. Keep it honest: reuse foundry-watch's statfs impl.
    foundry_watch::gc::tmp_usage_pct()
}

/// Credential file checks: name → (present, owner_only).
/// owner_only = mode & 0o777 == 0o600-ish (no group/world bits).
pub fn credential_checks() -> serde_json::Value {
    let home = std::env::var("HOME").unwrap_or_default();
    let files = [
        ("gh_token", format!("{home}/.foundry/gh_token")),
        ("die_env", format!("{home}/.foundry/die.env")),
        ("notifier_env", format!("{home}/.foundry/notifier.env")),
    ];
    let mut out = serde_json::Map::new();
    for (name, path) in files {
        let meta = std::fs::metadata(&path).ok();
        let present = meta.is_some();
        let owner_only = meta
            .map(|m| {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    m.permissions().mode() & 0o077 == 0
                }
                #[cfg(not(unix))]
                {
                    true
                }
            })
            .unwrap_or(false);
        out.insert(
            name.to_string(),
            serde_json::json!({ "present": present, "owner_only": owner_only }),
        );
    }
    serde_json::Value::Object(out)
}
