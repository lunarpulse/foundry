//! Discord Notifier port impl — plan T13 (M1).
//!
//! Observation-plane only: a failed webhook must NEVER block the pipeline
//! (returns Ok(NotifyOutcome::Failed) — logged, retried next event; the
//! gate proves this with an unreachable webhook). Credentials live in
//! ~/.foundry/notifier.env (chmod 600, §11 isolation); absent file = no-op
//! notifier, not an error.

use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, PartialEq)]
pub enum NotifyOutcome {
    Sent,
    /// Webhook unreachable/failed — pipeline continues (contract: obs plane).
    Failed(String),
    /// No webhook configured — no-op.
    Disabled,
}

pub struct DiscordNotifier {
    webhook_url: Option<String>,
    client: reqwest::blocking::Client,
}

#[derive(serde::Serialize)]
struct WebhookBody {
    content: String,
}

impl DiscordNotifier {
    /// Load from `<home>/.foundry/notifier.env` (FOUNDRY_DISCORD_WEBHOOK=...).
    pub fn from_env() -> Self {
        let home = std::env::var("HOME").unwrap_or_default();
        Self::from_env_at(PathBuf::from(home).join(".foundry/notifier.env"))
    }

    /// Test seam: explicit env-file path.
    pub fn from_env_at(path: PathBuf) -> Self {
        let url = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| {
                s.lines().find_map(|l| {
                    let (k, v) = l.split_once('=')?;
                    (k.trim() == "FOUNDRY_DISCORD_WEBHOOK").then(|| v.trim().to_string())
                })
            })
            .filter(|u| u.starts_with("https://"));
        Self {
            webhook_url: url,
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(8))
                .build()
                .unwrap_or_else(|_| reqwest::blocking::Client::new()),
        }
    }

    /// Fire-and-observe send. Never panics, never blocks long (8s timeout).
    pub fn send(&self, content: &str) -> NotifyOutcome {
        let Some(url) = &self.webhook_url else {
            return NotifyOutcome::Disabled;
        };
        match self
            .client
            .post(url)
            .json(&WebhookBody {
                content: content.to_string(),
            })
            .header("Content-Type", "application/json")
            .send()
        {
            Ok(resp) if resp.status().is_success() => NotifyOutcome::Sent,
            Ok(resp) => NotifyOutcome::Failed(format!("webhook status {}", resp.status())),
            Err(e) => NotifyOutcome::Failed(format!("webhook unreachable: {e}")),
        }
    }
}

impl foundry_domain::ports::Notifier for DiscordNotifier {
    /// Domain port contract: one dispatch method. Rich payloads ride the
    /// event string (M1 pragmatism; M2 introduces structured events).
    /// Blocking send is wrapped in spawn_blocking by the caller (publisher
    /// pattern, T9 lesson #3).
    fn notify(&mut self, event: &str, order_id: &str) -> foundry_domain::ports::PortResult<()> {
        let msg = match event {
            e if e.starts_with("furnace_hold") => furnace_msg(e, order_id),
            e if e.starts_with("shipped") => format!("📦 **Shipped** `{order_id}`\n{e}"),
            e if e.starts_with("failed") => format!("💥 **Failed** `{order_id}`\n{e}"),
            other => format!("▸ `{order_id}` {other}"),
        };
        match self.send(&msg) {
            NotifyOutcome::Sent => Ok(()),
            // review #3 / gate ③: observation-plane failure never blocks —
            // but surface it in the error channel for logging; PortError is
            // Retryable so callers may log-and-continue without dying.
            NotifyOutcome::Failed(e) => Err(foundry_domain::ports::PortError::Retryable(e)),
            NotifyOutcome::Disabled => Ok(()),
        }
    }
}

/// FurnaceHold message is self-contained (review #3): a human approving
/// from this message alone knows WHAT they sign.
fn furnace_msg(_event: &str, order_id: &str) -> String {
    format!(
        "🔥 **FurnaceHold — 승인 대기**\norder: `{order_id}`\n승인: `foundry approve --order {order_id} --intent \"<사유>\"`"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use foundry_domain::ports::Notifier as _;

    #[test]
    fn absent_env_is_disabled_not_error() {
        let tmp = std::env::temp_dir().join(format!("nt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let mut n = DiscordNotifier::from_env_at(tmp.join("notifier.env"));
        assert_eq!(n.send("hi"), NotifyOutcome::Disabled);
        // and the Notifier trait dispatch must not panic (pipeline contract)
        n.notify("furnace_hold digest=abc123", "o-1").ok();
        n.notify("shipped https://x/pr/1", "o-1").ok();
        n.notify("failed boom", "o-1").ok();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn configured_env_is_loaded() {
        let tmp = std::env::temp_dir().join(format!("nt2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("notifier.env"),
            "FOUNDRY_DISCORD_WEBHOOK=https://discord.com/api/webhooks/1/x\n",
        )
        .unwrap();
        let n = DiscordNotifier::from_env_at(tmp.join("notifier.env"));
        assert_eq!(
            n.webhook_url.as_deref(),
            Some("https://discord.com/api/webhooks/1/x")
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn unreachable_webhook_fails_without_panicking() {
        // gate scenario ③ in miniature: dead webhook never blocks/panics
        let n = DiscordNotifier {
            webhook_url: Some("http://127.0.0.1:1/nothing".into()), // closed port
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_millis(500))
                .build()
                .unwrap(),
        };
        match n.send("x") {
            NotifyOutcome::Failed(_) => {} // expected
            other => panic!("expected Failed, got {other:?}"),
        }
    }
}
