//! Notifier v2.7 — 관측 플레인 다중 플랫폼 알림 (설계 docs/design/notifier-v2.md).
//!
//! 구조: EventRenderer(형식) × PlatformBodyEncoder(부호화) ×
//! WebhookNotifier(전송). Discord는 렌더러+인코더의 한 조합일 뿐 —
//! 신규 플랫폼은 두 trait의 신규 구현 타입만으로 추가된다(OCP 실증).
//!
//! 관측 플레인 불변(M1 T13 승계): 어떤 전송 실패도 파이프라인을 막지
//! 않는다. 웹훅 미설정=Disabled(no-op). ClientState 머신(r5~r7):
//! 인스턴스당 Client 최대 1회 생성, 빌드 실패 캐시 후 재시도 없음,
//! drain은 명시적 방출, Drop은 백업 — 취소 경로 전부에서 reqwest
//! teardown이 async 컨텍스트에서 실행되지 않는다(의도적 유계 누수).

#![allow(unused_imports)] // W6a clippy debt: retain the existing feature and test configuration import set
#![allow(clippy::useless_format)] // W6a clippy debt: preserve the current formatting call site and output construction

use std::path::PathBuf;
use std::time::Duration;

use foundry_domain::ports::{Notifier, NotifyEvent, PortError, PortResult};

// ─── 전송 결과 ────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
pub enum NotifyOutcome {
    Sent,
    /// Webhook unreachable/failed — pipeline continues (contract: obs plane).
    Failed(String),
    /// No webhook configured — no-op.
    Disabled,
}

// ─── 렌더링 (형식) ────────────────────────────────────────────

/// 이벤트 → 플랫폼 독립 텍스트. 형식 회귀 테스트가 이 계층을 고정한다.
pub trait EventRenderer: Send {
    fn render(&self, e: &NotifyEvent) -> Rendered;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub title: String,
    pub body: String,
    pub severity: Severity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Attention,
    Critical,
}

impl Severity {
    /// 설계 §3.2 기본 매핑 — 렌더러가 재정의할 수 있다.
    pub fn of(e: &NotifyEvent) -> Self {
        match e {
            NotifyEvent::FurnaceHold { .. } => Severity::Attention,
            NotifyEvent::Failed { .. } => Severity::Critical,
            NotifyEvent::Shipped { .. } => Severity::Info,
            _ => Severity::Info, // non_exhaustive 와일드카드
        }
    }
}

/// 현행 Discord 형식(M1 T13) 이관 — 변경 금지(형식 회귀 대상).
#[derive(Debug, Clone, Copy, Default)]
pub struct DiscordRenderer;

impl EventRenderer for DiscordRenderer {
    fn render(&self, e: &NotifyEvent) -> Rendered {
        match e {
            NotifyEvent::FurnaceHold { order_id, digest } => Rendered {
                title: format!("🔥 **FurnaceHold — 승인 대기**"),
                body: format!(
                    "order: `{order_id}`\ndigest: `{digest}`\n승인: `foundry approve --order {order_id} --intent \"<사유>\"`"
                ),
                severity: Severity::of(e),
            },
            NotifyEvent::Shipped { order_id, pr_url } => Rendered {
                title: format!("📦 **Shipped** `{order_id}`"),
                body: format!("{pr_url}"),
                severity: Severity::of(e),
            },
            NotifyEvent::Failed { order_id, reason } => Rendered {
                title: format!("💥 **Failed** `{order_id}`"),
                body: format!("{reason}"),
                severity: Severity::of(e),
            },
            _ => Rendered {
                title: format!("▸ `{e:?}`"),
                body: String::new(),
                severity: Severity::Info,
            },
        }
    }
}

// ─── 부호화 (플랫폼 바디) ─────────────────────────────────────

/// Rendered → 플랫폼 HTTP 바디. 플랫폼별 부호화의 유일한 소유지.
pub trait PlatformBodyEncoder: Send {
    fn encode(&self, r: &Rendered) -> Result<Vec<u8>, String>;
    fn content_type(&self) -> &'static str;
}

/// Discord 웹훅: `{"content": "title\nbody"}`.
#[derive(Debug, Clone, Copy, Default)]
pub struct DiscordEncoder;

impl PlatformBodyEncoder for DiscordEncoder {
    fn encode(&self, r: &Rendered) -> Result<Vec<u8>, String> {
        #[derive(serde::Serialize)]
        struct WebhookBody {
            content: String,
        }
        serde_json::to_vec(&WebhookBody {
            content: format!("{}\n{}", r.title, r.body),
        })
        .map_err(|e| format!("encode: {e}"))
    }

    fn content_type(&self) -> &'static str {
        "application/json"
    }
}

/// 슬랙 계열 웹훅: `{"text": ...}` — 신규 플랫폼 추가 예시(비용 실증).
#[derive(Debug, Clone, Copy, Default)]
pub struct SlackEncoder;

impl PlatformBodyEncoder for SlackEncoder {
    fn encode(&self, r: &Rendered) -> Result<Vec<u8>, String> {
        #[derive(serde::Serialize)]
        struct SlackBody {
            text: String,
        }
        serde_json::to_vec(&SlackBody {
            text: format!("{}\n{}", r.title, r.body),
        })
        .map_err(|e| format!("encode: {e}"))
    }

    fn content_type(&self) -> &'static str {
        "application/json"
    }
}

// ─── 전송 (웹훅 골격) ─────────────────────────────────────────

/// 클라이언트 상태 머신(r5~r7) — Fresh/Ready/Failed/Drained.
/// 인스턴스당 Client 생성 ≤1(빌드 상한), 실패 캐시 후 재시도 없음.
enum ClientState {
    Fresh,
    Ready(reqwest::blocking::Client),
    Failed(String),
    Drained,
}

/// 범용 웹훅 전송 골격. 렌더러·인코더를 주입받는다(설계 §3.2).
pub struct WebhookNotifier<R: EventRenderer, E: PlatformBodyEncoder> {
    webhook_url: Option<String>,
    renderer: R,
    encoder: E,
    client: ClientState,
}

impl<R: EventRenderer, E: PlatformBodyEncoder> WebhookNotifier<R, E> {
    pub fn new(webhook_url: Option<String>, renderer: R, encoder: E) -> Self {
        Self {
            webhook_url,
            renderer,
            encoder,
            client: ClientState::Fresh,
        }
    }

    /// 테스트 시임: 이미 만들어진 Client 주입(생성 카운트 검증용).
    pub fn with_client(
        webhook_url: Option<String>,
        renderer: R,
        encoder: E,
        c: reqwest::blocking::Client,
    ) -> Self {
        Self {
            webhook_url,
            renderer,
            encoder,
            client: ClientState::Ready(c),
        }
    }

    /// 상태 노출(테스트용 — 전이 단정).
    pub fn client_state_tag(&self) -> &'static str {
        match self.client {
            ClientState::Fresh => "fresh",
            ClientState::Ready(_) => "ready",
            ClientState::Failed(_) => "failed",
            ClientState::Drained => "drained",
        }
    }

    /// Fire-and-observe send. Never panics, never blocks long (8s timeout).
    pub fn send(&mut self, content: &str) -> NotifyOutcome {
        let Some(url) = &self.webhook_url else {
            return NotifyOutcome::Disabled;
        };
        // lazy 빌드(≤1회) — 실패는 캐시, 패닉 경로 없음.
        match &mut self.client {
            ClientState::Fresh => {
                match reqwest::blocking::Client::builder()
                    .timeout(Duration::from_secs(8))
                    .build()
                {
                    Ok(c) => self.client = ClientState::Ready(c),
                    Err(e) => {
                        let msg = format!("http client build: {e}");
                        self.client = ClientState::Failed(msg.clone());
                        return NotifyOutcome::Failed(msg);
                    }
                }
            }
            ClientState::Failed(msg) => return NotifyOutcome::Failed(msg.clone()),
            ClientState::Drained => {
                return NotifyOutcome::Failed("notifier drained".into());
            }
            ClientState::Ready(_) => {}
        }
        let ClientState::Ready(client) = &self.client else {
            return NotifyOutcome::Failed("client state race".into());
        };
        let body = (self.encoder.encode(&Rendered {
            title: String::new(),
            body: content.to_string(),
            severity: Severity::Info,
        }))
        .unwrap_or_else(|_| Vec::new()); // send() 레거시 경로 — 항상 텍스트
        match client
            .post(url)
            .header("Content-Type", self.encoder.content_type())
            .body(body)
            .send()
        {
            Ok(resp) if resp.status().is_success() => NotifyOutcome::Sent,
            Ok(resp) => NotifyOutcome::Failed(format!("webhook status {}", resp.status())),
            Err(e) => NotifyOutcome::Failed(format!("webhook unreachable: {e}")),
        }
    }

    /// 구조화 경로 전송(레거시 send와 동일 골격, 인코더 경유).
    fn deliver(&mut self, r: &Rendered) -> NotifyOutcome {
        // Drained 거부가 Disabled보다 우선(계약: 종결 후 알림은 에러).
        if matches!(self.client, ClientState::Drained) {
            return NotifyOutcome::Failed("notifier drained".into());
        }
        let Some(url) = &self.webhook_url else {
            return NotifyOutcome::Disabled;
        };
        match &mut self.client {
            ClientState::Fresh => {
                match reqwest::blocking::Client::builder()
                    .timeout(Duration::from_secs(8))
                    .build()
                {
                    Ok(c) => self.client = ClientState::Ready(c),
                    Err(e) => {
                        let msg = format!("http client build: {e}");
                        self.client = ClientState::Failed(msg.clone());
                        return NotifyOutcome::Failed(msg);
                    }
                }
            }
            ClientState::Failed(msg) => return NotifyOutcome::Failed(msg.clone()),
            ClientState::Drained => return NotifyOutcome::Failed("notifier drained".into()),
            ClientState::Ready(_) => {}
        }
        let Ok(body) = self.encoder.encode(r) else {
            return NotifyOutcome::Failed("encode failure".into());
        };
        let ClientState::Ready(client) = &self.client else {
            return NotifyOutcome::Failed("client state race".into());
        };
        match client
            .post(url)
            .header("Content-Type", self.encoder.content_type())
            .body(body)
            .send()
        {
            Ok(resp) if resp.status().is_success() => NotifyOutcome::Sent,
            Ok(resp) => NotifyOutcome::Failed(format!("webhook status {}", resp.status())),
            Err(e) => NotifyOutcome::Failed(format!("webhook unreachable: {e}")),
        }
    }
}

impl<R: EventRenderer, E: PlatformBodyEncoder> Notifier for WebhookNotifier<R, E> {
    fn notify(&mut self, event: NotifyEvent) -> PortResult<()> {
        let rendered = self.renderer.render(&event);
        match self.deliver(&rendered) {
            NotifyOutcome::Sent => Ok(()),
            // review #3 / gate ③: observation-plane failure never blocks —
            // PortError::Retryable so callers log-and-continue without dying.
            NotifyOutcome::Failed(e) => Err(PortError::Retryable(e)),
            NotifyOutcome::Disabled => Ok(()),
        }
    }

    fn drain(&mut self) {
        // r7: 명시적 방출 — Drained 치환 중 Drop이 개입하지 않게 take 후 forget.
        let prev = std::mem::replace(&mut self.client, ClientState::Drained);
        if let ClientState::Ready(c) = prev {
            std::mem::forget(c);
        }
    }
}

impl<R: EventRenderer, E: PlatformBodyEncoder> Drop for WebhookNotifier<R, E> {
    fn drop(&mut self) {
        // 백업 방어선: drain을 거치지 않고 드롭되어도 Ready Client는
        // forget — async 컨텍스트 드롭에서 reqwest teardown 실행 차단.
        if let ClientState::Ready(c) = std::mem::replace(&mut self.client, ClientState::Drained) {
            std::mem::forget(c);
        }
    }
}

// ─── Discord 조립(현행 호환 진입점) ───────────────────────────

/// M1 T13 호환 래퍼 — `WebhookNotifier<DiscordRenderer, DiscordEncoder>`.
pub type DiscordNotifier = WebhookNotifier<DiscordRenderer, DiscordEncoder>;

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
        Self::new(url, DiscordRenderer, DiscordEncoder)
    }

    /// 테스트 접근자(기존 webhook_url 단정 호환).
    pub fn webhook_url(&self) -> Option<&str> {
        self.webhook_url.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use foundry_domain::ports::Notifier as _;

    fn tmpdir(tag: &str) -> PathBuf {
        let tmp = std::env::temp_dir().join(format!("nt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        tmp
    }

    #[test]
    fn absent_env_is_disabled_not_error() {
        let tmp = tmpdir("v2");
        let mut n = DiscordNotifier::from_env_at(tmp.join("notifier.env"));
        assert_eq!(n.send("hi"), NotifyOutcome::Disabled);
        n.notify(NotifyEvent::FurnaceHold {
            order_id: "o-1".into(),
            digest: "abc123".into(),
        })
        .ok();
        n.notify(NotifyEvent::Shipped {
            order_id: "o-1".into(),
            pr_url: "https://x/pr/1".into(),
        })
        .ok();
        n.notify(NotifyEvent::Failed {
            order_id: "o-1".into(),
            reason: "boom".into(),
        })
        .ok();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn configured_env_is_loaded() {
        let tmp = tmpdir("v2b");
        std::fs::write(
            tmp.join("notifier.env"),
            "FOUNDRY_DISCORD_WEBHOOK=https://discord.com/api/webhooks/1/x\n",
        )
        .unwrap();
        let n = DiscordNotifier::from_env_at(tmp.join("notifier.env"));
        assert_eq!(
            n.webhook_url(),
            Some("https://discord.com/api/webhooks/1/x")
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn unreachable_webhook_fails_without_panicking() {
        let c = reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .unwrap();
        let mut n = DiscordNotifier::with_client(
            Some("http://127.0.0.1:1/nothing".into()),
            DiscordRenderer,
            DiscordEncoder,
            c,
        );
        match n.send("x") {
            NotifyOutcome::Failed(_) => {}
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    // ─── 형식 회귀(현행 M1 형식 보존) ──────────────────────

    #[test]
    fn furnace_format_regression() {
        let r = DiscordRenderer.render(&NotifyEvent::FurnaceHold {
            order_id: "o-9".into(),
            digest: "abc123".into(),
        });
        assert_eq!(r.title, "🔥 **FurnaceHold — 승인 대기**");
        assert!(r.body.contains("order: `o-9`"));
        assert!(r.body.contains("digest: `abc123`"));
        assert!(r.body.contains("foundry approve --order o-9 --intent"));
        assert_eq!(r.severity, Severity::Attention);
    }

    #[test]
    fn shipped_failed_format_regression() {
        let s = DiscordRenderer.render(&NotifyEvent::Shipped {
            order_id: "o-1".into(),
            pr_url: "https://x/pr/1".into(),
        });
        assert_eq!(s.title, "📦 **Shipped** `o-1`");
        assert_eq!(s.body, "https://x/pr/1");
        assert_eq!(s.severity, Severity::Info);
        let f = DiscordRenderer.render(&NotifyEvent::Failed {
            order_id: "o-2".into(),
            reason: "die".into(),
        });
        assert_eq!(f.title, "💥 **Failed** `o-2`");
        assert_eq!(f.severity, Severity::Critical);
    }

    #[test]
    fn encoders_platform_split() {
        let r = DiscordRenderer.render(&NotifyEvent::Shipped {
            order_id: "o-1".into(),
            pr_url: "u".into(),
        });
        let d = DiscordEncoder.encode(&r).unwrap();
        assert!(String::from_utf8(d).unwrap().contains("\"content\""));
        let s = SlackEncoder.encode(&r).unwrap();
        assert!(String::from_utf8(s).unwrap().contains("\"text\""));
        assert_eq!(DiscordEncoder.content_type(), "application/json");
    }

    // ─── ClientState 머신 라이프사이클(설계 §3.3 ①~⑥) ────

    #[test]
    fn lifecycle_client_built_once() {
        // ②동일 notifier 3회 연속 알림 → 클라이언트 1회 생성(주입 Client
        // 경유 — Ready 상태 재사용, Fresh→Ready 전이 없음을 tag로 단정).
        let c = reqwest::blocking::Client::builder().build().unwrap();
        let mut n = DiscordNotifier::with_client(None, DiscordRenderer, DiscordEncoder, c);
        assert_eq!(n.client_state_tag(), "ready");
        for _ in 0..3 {
            n.notify(NotifyEvent::Shipped {
                order_id: "o".into(),
                pr_url: "u".into(),
            })
            .ok();
        }
        assert_eq!(n.client_state_tag(), "ready");
    }

    #[test]
    fn lifecycle_drained_rejects_then_double_drain_noop() {
        let c = reqwest::blocking::Client::builder().build().unwrap();
        let mut n = DiscordNotifier::with_client(None, DiscordRenderer, DiscordEncoder, c);
        n.drain();
        assert_eq!(n.client_state_tag(), "drained");
        let e = n.notify(NotifyEvent::Shipped {
            order_id: "o".into(),
            pr_url: "u".into(),
        });
        assert!(e.is_err());
        n.drain(); // 이중 drain no-op
        assert_eq!(n.client_state_tag(), "drained");
    }

    #[test]
    fn lifecycle_fresh_drain_is_noop() {
        let mut n = DiscordNotifier::new(None, DiscordRenderer, DiscordEncoder);
        assert_eq!(n.client_state_tag(), "fresh");
        n.drain();
        assert_eq!(n.client_state_tag(), "drained");
    }

    #[test]
    fn lifecycle_disabled_never_builds_client() {
        // Fresh + 웹훅 None → Disabled 반환, Client 생성 없음(상태 Fresh 유지).
        let mut n = DiscordNotifier::new(None, DiscordRenderer, DiscordEncoder);
        n.notify(NotifyEvent::Shipped {
            order_id: "o".into(),
            pr_url: "u".into(),
        })
        .unwrap();
        assert_eq!(n.client_state_tag(), "fresh");
    }
}

// ─── 콘솔 관측 + 팬아웃(설계 §3.4) ────────────────────────────

/// 콘솔 관측 notifier — 데모용 println!. 무전송자원.
pub struct ConsoleNotifier;

impl Notifier for ConsoleNotifier {
    fn notify(&mut self, event: NotifyEvent) -> PortResult<()> {
        let r = DiscordRenderer.render(&event);
        println!("[notify:{}] {}", severity_tag(r.severity), r.title);
        if !r.body.is_empty() {
            println!("            {}", r.body.replace('\n', "\n            "));
        }
        Ok(())
    }
}

fn severity_tag(s: Severity) -> &'static str {
    match s {
        Severity::Info => "info",
        Severity::Attention => "ATTN",
        Severity::Critical => "CRIT",
    }
}

/// 팬아웃 — 동일 이벤트를 복수 관측자에게. 개별 실패는 로그만(관측 플레인).
pub struct FanOutNotifier(pub Vec<Box<dyn Notifier>>);

impl Notifier for FanOutNotifier {
    fn notify(&mut self, event: NotifyEvent) -> PortResult<()> {
        let mut first_err = None;
        for n in &mut self.0 {
            if let Err(e) = n.notify(event.clone()) {
                eprintln!("fanout branch failed (non-fatal): {e:?}");
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn drain(&mut self) {
        for n in &mut self.0 {
            n.drain();
        }
    }
}

#[cfg(test)]
mod fanout_tests {
    use super::*;

    struct RecordingNotifier {
        pub events: std::sync::mpsc::Sender<String>,
    }

    impl Notifier for RecordingNotifier {
        fn notify(&mut self, event: NotifyEvent) -> PortResult<()> {
            self.events.send(format!("{event:?}")).ok();
            Ok(())
        }
    }

    struct FailingNotifier;
    impl Notifier for FailingNotifier {
        fn notify(&mut self, _event: NotifyEvent) -> PortResult<()> {
            Err(PortError::Retryable("always fails".into()))
        }
    }

    #[test]
    fn fanout_delivers_to_all_and_isolates_failure() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut fan = FanOutNotifier(vec![
            Box::new(FailingNotifier),
            Box::new(RecordingNotifier { events: tx }),
        ]);
        // 실패 브랜치가 있어도 Ok 브랜치로 전달 — 하지만 첫 에러 전파
        let res = fan.notify(NotifyEvent::Shipped {
            order_id: "o-1".into(),
            pr_url: "u".into(),
        });
        assert!(res.is_err()); // 관측 플레인: 호출부는 로그만 하므로 무해
        assert!(rx.recv().unwrap().contains("Shipped"));
    }

    #[test]
    fn console_notifier_renders_without_resources() {
        let mut c = ConsoleNotifier;
        c.notify(NotifyEvent::FurnaceHold {
            order_id: "o".into(),
            digest: "d".into(),
        })
        .unwrap();
        c.drain(); // no-op
    }
}
