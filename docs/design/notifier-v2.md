# Notifier v2.7 — SOLID 정합성 평가 + 데모용 확장 설계 (검증 r7 반영)

상태: 설계 제안 (미구현). 독립 검증 7회 (r1 PROCEED-WITH-CHANGES,
r2~r7 REWORK — r7 잔여: drain의 Client 명시적 방출·생성 상한(동시수
아님)·Fresh send 거부 모순 — 본판 처리).

## 1. 현행 구조 (사실, r3 측정 확정)

- `Notifier::notify(&mut self, &str, &str)` — DiscordNotifier 1개, dyn 미사용
- 소비부 main.rs 645–706: async 컨텍스트 직접 blocking 호출(spawn_blocking 없음)
- `Line::publish`는 **`Result<OrderState>`만 반환 — pr_url은 내부 소실**,
  저널(event_json)에만 남음 (r3 확인: "returns only OrderState")
- `extract_quoted`는 `pub(crate)` — 크레이트 밖 재조회 불가 (r3)
- `Client::builder().build()` 실패 시 `unwrap_or_else(|_| Client::new())` —
  이 폴백도 실패하면 **panic** (risks에 미기재였음, r3)

## 2. SOLID 정합성 (현행) — v2.2와 동일 (§2 유지)

SRP ✅ / OCP ⚠️(플랫폼 축 열림, 페이로드 암묵 문자열 규약) / LSP ✅ / ISP ✅ / DIP ✅

## 3. 설계 (v2.3)

### 3.1 구조화 이벤트 + URL 노출 (r3-1)

```rust
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum NotifyEvent {
    FurnaceHold { order_id: String, digest: String },
    Failed { order_id: String, reason: String },
    Shipped { order_id: String, pr_url: String },
}

pub trait Notifier: Send {
    fn notify(&mut self, event: NotifyEvent) -> PortResult<()>;
}
```

**pr_url 소싱 확정 (r3-1 해소)**: `Line::publish` 반환 타입을
`Result<OrderState, LineError>` → `Result<PublishReceipt, LineError>`로 확장.

```rust
pub struct PublishReceipt { pub state: OrderState, pub pr_url: Option<String> }
```

- 커밋된 이벤트에서 직접 취득(이미 publisher가 반환한 값 — 새 조회 없음):
  `PublishConfirmed { pr_url }` 커밋 시 값을 receipt에 실음. `Unknown`이면 None.
- 기존 `publish` 호출부(2곳: watch_cmd, CLI publish 505)는 `.state`로 마이그레이션.
  소스 브레이크 범위 컴파일러 전량 포착.
- 리플레이 재조회 경로(재시작 후 복구)는 본 설계 범위 밖 — 필요 시
  `extract_quoted`를 `pub` 승격하는 별도 1행 태스크로 분리 (r3-2: 사전
  승격하지 않고 필요 시점 연기).

### 3.2 렌더링/인코딩/전송 3분리 + 심각도 매핑 (r3-5)

```rust
pub trait EventRenderer: Send { fn render(&self, e: &NotifyEvent) -> Rendered; }
pub struct Rendered { pub title: String, pub body: String, pub severity: Severity }
pub enum Severity { Info, Attention, Critical }
pub trait PlatformBodyEncoder: Send {
    fn encode(&self, r: &Rendered) -> Result<Vec<u8>, String>;
    fn content_type(&self) -> &'static str;
}
```

- 심각도 기본 매핑(이넘 3변종 전부 — Raw 변종은 v2.3에서 제거됨):
  FurnaceHold=Attention, Failed=Critical, Shipped=Info. 렌더러가 재정의
  가능(콘솔 색상 등)하나 기본은 이 표.
- 인코더 주입(r4): `WebhookNotifier<R, E>` 생성자가 `webhook_url`,
  `renderer: R`, `encoder: E`를 전부 인자로 받음 — Discord 조립은
  `WebhookNotifier::new(url, DiscordRenderer, DiscordEncoder)` 팩토리.
  encoder가 `encode(&Rendered) -> Result<Vec<u8>, String>` + `content_type()`
  를 구현하므로 신규 플랫폼=인코더+렌더러만 신규 타입 (OCP 실증점).
- DiscordEncoder: `{"content": title+"\n"+body}` (Content-Type:
  application/json) / SlackEncoder: `{"text": …}`
- DiscordRenderer: 현행 형식 이관 — 형식 회귀 테스트로 고정

### 3.3 전송 라이프사이클 + 패닉 제거 (r5 재설계)

- **클라이언트 상태 머신 (r5 — OnceCell 폐지)**: `WebhookNotifier` 내부

  ```rust
  enum ClientState { Fresh, Ready(Client), Failed(String), Drained }
  ```

  4상태 단일 enum — lazy 생성/실패 캐시/종결을 추가 필드 없이 표현.
  전이: Fresh→Ready|Failed(첫 send 시 빌드 시도), Ready|Failed→Drained(drain),
  **Fresh→Drained도 합법(미사용 종결 — Client 없음, no-op)** (r6).
  **Drained 이후 send만 `Retryable("notifier drained")` — Fresh에서의
  send는 당연히 빌드를 시도한다(lazy 빌드와 모순 없음, r7 정정)**.
  빌드 실패 캐시 후 재시도 없음 — 상태 고정, 패닉 경로 제거.
  **생성 상한 (r7)**: `built: bool` 필드 — 한 인스턴스가 Client를
  **최대 1회만 생성**(Fresh→Ready 전이 1회, Failed 후 재빌드 금지는
  기존 규칙이 유지). 누수 상한은 "동시 인스턴스 수"가 아니라
  "프로세스 수명 동안 생성된 Client 총수 = 인스턴스 수"(각 ≤1) —
  정적 상한, 재시도로 인한 무한 증가 경로 없음.
- **drain은 trait 계약 (r6)**: `Notifier`에 포함 —

  ```rust
  pub trait Notifier: Send {
      fn notify(&mut self, event: NotifyEvent) -> PortResult<()>;
      /// 관측 종결. 구현은 전송 자원을 안전하게 정리한다.
      fn drain(&mut self) {}
  }
  ```

  기본 no-op 구현 — ConsoleNotifier 등 무자원 구현은 무변경 (ISP).
  FanOutNotifier::drain은 벡터 순회 위임.
  **WebhookNotifier::drain 구현 (r7 — 명시적 방출)**:

  ```rust
  fn drain(&mut self) {
      if let ClientState::Ready(_) = self.client {
          // Client를 상태에서 꺼내 명시적으로 방출 — Drained 치환 중
          // Drop이 다시 개입하지 않도록 take 후 forget.
          let ready = std::mem::replace(&mut self.client, ClientState::Drained);
          if let ClientState::Ready(c) = ready { std::mem::forget(c); }
      } else {
          self.client = ClientState::Drained;
      }
  }
  ```

  Drop 구현은 동일 로직의 최후 방어선(이중 방출 안전 — Ready가 이미
  꺼진 뒤면 no-op). 즉 **방출은 drain이 주 경로, Drop은 백업** — 어느
  스레드에서 드롭되든 Ready Client는 forget되고 reqwest teardown은
  비동기 컨텍스트에서 실행되지 않는다.
- **소유권/'static (r5)**: `notify_async`는 `&mut` 차용을 spawn_blocking
  클로저로 이동할 수 없다. 해법: watch_cmd가 notifier를
  `Arc<Mutex<Box<dyn Notifier>>>`로 소유(래핑 비용 1회), notify_async는
  Arc 클론을 spawn_blocking으로 이동 후 lock하여 send.
  `Box<dyn Notifier>: Send` 성립(trait Send 바운드). 락 홀드는 전송
  최대 8s로 제한 — 관측 플레인이라 알림 동시성 요구 없음(수용).
- **취소 안전 — 의도적 누수로 근본 해소 (r6 재설계)**: r5 안(종료 직전
  drain)은 watch_cmd 자체가 취소되면 우회된다 — Arc<Mutex<Ready(Client)>>
  가 async 스레드에서 드롭되어 reqwest blocking 런타임 티어다운이 async
  컨텍스트에서 실행되는 경로가 남는다. 근본 해법: **Client를 절대
  Drop시키지 않는다** — `WebhookNotifier`의 `Drop` 구현이
  `ClientState::Ready(c)`를 `std::mem::forget`로 방출(의도적·문서화된
  유계 누수). 그러면 어떤 스레드에서 드롭되든 reqwest 내부 정리가
  실행되지 않으므로 취소 경로 전부가 안전해진다. 누수 크기: 프로세스당
  notifier 인스턴스 수(1~2개)× Client 1개(내부 런타임 스레드) —
  프로세스 수명과 동일한 유계. drain()은 상태를 Drained로 전환해 이후
  알림 거부(계약 명확화)가 본분이고, 스레드 회수는 프로세스 종료가
  담당한다(관측 플레인 — 데몬은 상시 주재).
- 소비부: watch_cmd 클로저 전부 notify_async 헬퍼 경유로 교체
- 라이프사이클 테스트: ①async 컨텍스트 notify_async → 런타임 경고 없음
  ②동일 notifier 3회 연속 → 클라이언트 1회 생성(시임 카운트) ③빌드 실패
  주입(시임) → 이후 전 알림 Retryable, 패닉 없음 ④drain 후 send →
  Retryable ⑤await 취소(drop(timeout(notify_async))) → 이후 notifier
  재사용 가능 ⑥**소유자 취소**: watch_cmd future를 임의 시점에 drop해
  Ready(Client) notifier가 async 스레드에서 드롭되어도 런타임 경고 없음
  (mem::forget 경로 실증 — r6)

### 3.4 팬아웃 + 콘솔 (r3-3)

```rust
pub struct FanOutNotifier(Vec<Box<dyn Notifier>>); // Notifier: Send
pub struct ConsoleNotifier; // println! 관측 — 데모용
```

- 개별 실패 eprintln 로그 후 계속.
- CLI 선택 의미론 (r4·r5): `--notify <mode>` — `discord`(기본, 단독),
  `console`(단독), `all`(FanOut[discord, console]). 미지정=discord —
  기존 동작 100% 보존. 파싱은 clap 기존 인자 패턴 준수.
  배선 테스트(r5): ①미지정 → Discord 단독 ②`--notify all` → FanOut
  2요소 ③`--notify console` → 콘솔 단독 (파싱→구성 단정)

### 3.5 구현 착수 안 (r5 순서 확정)

1. foundry-line: PublishReceipt 반환 확장 + 호출부 2곳 마이그레이션 (직접 —
   반환 타입 설계 포함)
2. ports.rs: NotifyEvent + Severity + trait(`: Send`) 교체 (기계적 — 위임)
3. discord_notifier.rs 재구성: EventRenderer/PlatformBodyEncoder/
   WebhookNotifier(ClientState 상태 머신)/DiscordRenderer + 형식 회귀
   테스트 (직접)
4. main.rs: NotifyEvent 조립(pr_url은 receipt에서) + notify_async 헬퍼
   (Arc<Mutex<Box<dyn Notifier>>>) 배선 (기계적)
5. ClientState 머신 + notify_async + drain 배선 + 라이프사이클 테스트
   6종(3.3 ①~⑥) + drain 이중 호출 no-op 단정 + 인스턴스당 Client 생성
   ≤1 상한 단정 (직접 — 병렬·수명 설계 포함)
6. ConsoleNotifier + FanOutNotifier + 실패 격리 테스트 (직접)
7. `--notify` CLI 배선 + 구성 단정 테스트 3종 (기계적 — 파서 인자 추가)
8. PublishReceipt URL 테스트: Confirmed → Some, Unknown → None (r4)
9. 인수: 전 워크스페이스 green + 형식 회귀 + CLI 호환(미지정 동일 동작)

### 3.6 명시적 비목표 (유지)

재시도/데드레터, 템플릿 엔진, 백프레셔, JSONL mirror(T14), 리플레이 pr_url
재조회(필요 시 extract_quoted pub 승격 별도 태스크).

## 4. 리스크 (r3 갱신)

- 소스 브레이크: publish 반환 2곳 + notify 소비부 1파일 — 전부 워크스페이스 내
- **런타임(현행)**: async 내 직접 blocking send — 이식 전까지 잔존, v2.3 3.3으로 제거
- **패닉(현행→제거)**: Client 빌드 실패 폴백 `Client::new()` panic 가능 —
  lazy Err 보관으로 제거
- **빌더 패닉 잔존 경로 (r4)**: lazy 생성이라도 `Client::builder().build()`
  실패 폴백 자체가 없어지는 것은 아님 — Err 캐시로 전환하므로 패닉은
  제거되나, 최초 1회 빌드 시도는 spawn_blocking 안에서 수행해 async 내
  패닉 전파도 차단
- **테스트 호출부 마이그레이션 (r4)**: 기존 DiscordNotifier 테스트
  (notify 문자열 3종)도 NotifyEvent 기반으로 전부 재작성 — 형식 회귀는
  렌더러 테스트로 이관되므로 커버리지 유지
- **Arc<Mutex> 직렬화 (r5)**: 관측 플레인 특성상 알림 동시성 요구 없음 —
  락 홀드 최대 8s 수용. 데몬 틱 주기와 합의 불필요
- `#[non_exhaustive]` 이벤트 확장 비파괴(워크스페이스 내 와일드카드)
- FanOut Send 성립: trait Send 바운드로 전파
