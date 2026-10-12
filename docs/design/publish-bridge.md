# Publish Bridge — M8 라이브 경로의 PR 발행·Shipped 확정 설계

- **버전**: v1.6 (2026-10-12) — r6 APPROVED-FOR-IMPLEMENTATION, 비차단 MINOR 2 문구 정리
- **상태**: 재검증 대기
- **범위**: m8_live_die 데모가 인간 게이트 통과 후 **실PR 발행 → Shipped
  확정**까지 닫히게 하는 브리지.
- **비범위**: PR 머지 자동화(인간 리뷰 소유), 워치 데몬 상시화, notify_async.

## 0. 측정된 현행 사실

### 0.1 이미 존재하는 부품
- `GithubPublisher`(github_publisher.rs, M0 게이트 레일): 토큰 로드→
  workdir HEAD == approved digest 재검증→`foundry/<order>` 브랜치 push→
  PR 생성. 멱등 조회는 push **이후** 동일 head의 open PR만 확인(성공
  재시도에만 멱등 — push 실패를 건너뛰지 않음). PR body `Closes #N`
  (dedup_key에서 추출, base 브랜치 머지 시에만 자동 종료).
- 오류 분류(실측, r1-MINOR-1 반영):
  - git push: 오류 문자열에 `Could not resolve`/`timed out` 포함 시에만
    Retryable, 그 외 Permanent
  - PR 조회 HTTP: 4xx 포함 전부 Retryable
  - PR 생성 응답 JSON 파싱 실패: 상태 무관 Permanent
  - PR 생성 파싱 성공 후 실패: 5xx Retryable, 나머지 Permanent
  - 토큰 스크럽은 지정 오류 문자열 내 원문 토큰 치환 범위
- `StreamRuntime::confirm_publish`(stream.rs 337–365): PublishConfirmed
  디스패치 → Publishing→Shipped(order.rs 316–317). **단 Outcome을 버리고
  Ok(()) 반환** — 성공 사후조건 강화 필요(§D2-4).
- `Line::publish`(line.rs 365–407): M1 경로 — `PublishUnknown` 기록
  포함. 데모가 쓰면 상태 이중 관리(기각안 유지).
- Notifier v2.7: `Shipped { order_id, pr_url }` — pr_url 실림.

### 0.2 경로 계약 (workdir.rs — 단일 진실원)
```
workdir_path(order, attempt) = /tmp/foundry-die-<order>-<attempt>
repo_path(order, attempt)    = <workdir>/repo
```
퍼블리셔는 이 공식 경로만 읽는다(매개변수 없음). m8_live_die의 현재
스크래치 경로와 어긋남 — 이 설계가 정렬한다.

### 0.3 갭 (라이브 런 실측)
- 데모는 Publishing에서 멈춤. #140 산출 커밋 `e9491ce`는 로컬 workdir에
  존재(HEAD 실측 `e9491cec3bf8…`). 원격 브랜치 부재는 발행 시점에 조회로
  확인한다(사전 주장 없음).
- Publishing→Shipped 전이를 밟는 M8 호출자 부재.

## 1. 설계 결정

### D1 — 퍼블리셔 무변경 재사용 (경로 관례 정렬)
`GithubPublisher` 불변. 데모의 Die workdir을 `workdir_path(order_id,
attempt)`에 만든다. 실행 전제(전부 데모가 보장):
- `op_id = "{order_id}:{attempt}"` — 퍼블리셔가 마지막 `:` 뒤를 attempt로
  파싱(rsplit, 런 접미사가 포함돼도 `o-140-r2:1`→1 정상).
- precheck가 반환하는 digest는 승인된 `artifact_digest` **원문 그대로**
  (축약 해시 금지, HEAD 재계산 금지) — 퍼블리셔 HEAD 비교와 동일 형식.
- 토큰 파일 실존 + Contents/PR 쓰기 스코프, base 브랜치 존재,
  push 가능 권한, base 대비 실제 변경 존재.
- `git config user`는 커밋용으로 foundry-die 래퍼가 이미 설정(37행).

### D2 — 발행 정문: **제네릭 + 비동기 소유 분할** (r1-BLOCKER 반영)
`&dyn Publisher`는 컴파일 불가(RPITIT E0038, astra 실측). 또한 정문이
publisher 실행을 내장하면 foundry-orch에 tokio 의존이 생기고, 엔진
가드를 든 채 confirm_publish를 호출하면 **같은 mutex 재획득 교착**
(stream.rs 345)이 발생한다. 따라서 정문을 둘로 쪼갠다:

```rust
/// 게이트 1: 발행 전 상태·재료 검사 — 엔진 가드를 취득 후 해제하고 반환.
pub fn publish_precheck(
    &self, stream_id: &str, order_id: &str,
) -> Result<PublishMaterial, String>;
// PublishMaterial { digest, attempt, op_id } — 상태 != Publishing이면 거부

/// 게이트 2: 발행 결과 확정 — 별도 가드 취득, 기존 confirm_publish 강화판.
pub fn confirm_published(
    &self, stream_id: &str, order_id: &str, exec: &str, pr_url: &str,
) -> Result<OrderState, String>;
// 멱등 3분기 (r2-MAJOR-2 — append 후 재진입은 NoEdge가 되므로 매번
// Applied를 요구하면 재확정이 영원히 실패한다):
// (a) 최초 전이: dispatch Outcome::Applied + 상태 Shipped + 저널
//     PublishConfirmed{pr_url} 행 일치 → Ok(Shipped)
// (b) 이미 Shipped + 저널 pr_url 일치 → 재디스패치 없이 Ok(Shipped)
// (c) Shipped인데 pr_url 불일치 → Err(다른 PR에 대한 재확정 시도)
// 저널 append 실패로 exec가 Infrastructure 래치(lib.rs 776)에 걸린
// 경우: 같은 exec 재호출은 래치에서 재차 실패(lib.rs 580)하므로
// **새 exec로 재시도**한다(재구축 후) — 회귀 테스트에 (a)(b) 및
// append-실패-새-exec 경로 포함.
```

**확정 필수조건에 pr_url 유효성 포함** (r4-MAJOR): 퍼블리셔는
`Confirmed`의 `html_url` 누락·타입 오류를 **빈 문자열로 성공 반환**한다
(github_publisher.rs 166·203 — 2xx+`{}` 응답이 빈 URL Confirmed가 되는
코드상 허용 경로). 도메인도 URL을 검사하지 않는다(order.rs 316). 따라서
confirm_published는 **URL이 비었거나 대상 저장소 PR URL 형식이 아니면
Err**로 거부하고, 호출부도 Confirmed 반환 즉시 같은 검증으로 선행
거부한다(Publisher 무변경 원칙 유지). 빈 URL Confirmed 시 Publishing·
증거 보존 후 재판정(D2-R). 회귀 테스트: 빈 URL·필드 누락→거부 2종.

호출부(데모) 순서 — 토크 런타임 소유도 데모(m8_live_die.rs 92 기존
current-thread runtime 패턴):
```
precheck → (가드 해제됨) → 데모의 Tokio runtime에서 publisher.publish(
  &order_snapshot, op_id, digest).await → Ok(Confirmed{pr_url})이면
  confirm_published(pr_url) → Shipped 검증
```
- Publisher는 제네릭 필요 없음: 데모가 `GithubPublisher` 구체 타입을
  직접 호출(포트 트레이트 경유, 정적 디스패치). foundry-orch는
  Publisher를 전혀 모른다 — DIP 방향 유지 + 의존 추가 0.
- TOCTOU: precheck와 발행 사이 창은 존재하나 본 데모는 단일 프로세스
  순차 실행으로 닫힌다. **동시 실행 보장으로 확대 해석 금지** — 상시
  운영(데몬화) 시 별도 설계.

### D2-R — 불확실 발행의 재판정 (r1-MAJOR-1 반영)
`GithubPublisher`는 Unknown을 반환하지 않는다. 불확실 창은 2종
(r2-MINOR 정확화): (i) `.send()` 실패 → Retryable, (ii) **본문 수신/JSON
파싱 실패 → Permanent로 분류되지만 원격 생성 여부는 불확실**. 둘 다
재판정 대상이며, 재판정 책임은 **데모 호출부**다:
- **공통 선행 절차** (r5-MAJOR-1): 불확실(Retryable·본문 상실·빈
  URL) 후의 **어떤 재시도든** — 동일 프로세스든 재개든 — 먼저 D2-R2의
  원격 재판정(SHA·base·닫힘/머지 이력)을 수행한다. 근거: 퍼블리셔는
  open-PR 부재 시 push 성공 후 곧바로 생성 POST로 진행하므로(132·156·
  181행) 검증 없는 재시도는 "이미 존재하지만 닫힌 PR" 위에 중복 생성
  한다. 분기: 검증된 open PR → confirm만 / **미생성 확인**(전 이력
  조회로 생성 부재 실증) → 동일 op_id publish 재시도 / 그 외(닫힘·
  머지·SHA 불일치·조회 실패) → 수동 판정, 재시도 금지.
- **publisher 재호출 분기만 push부터 수행**한다. 검증된 open PR은
  publisher를 호출하지 않고 confirm만 한다. 자동 멱등 성립 조건은
  "원격 브랜치 head 불변 + 검증된 open PR"으로 한정.
- PR 생성엔 성공했는데 confirm 실패 시: 동일 경로 재실행으로
  confirm_published만 재적용.
- Domain의 `PublishUnknown` 상태는 M1 라인 경로가 소유 — 본 데모는
  Publishing 유지 + 재시도로 처리(상태 전이 불필요).

### D2-R2 — 프로세스 재시작 후 재개 (`--resume <run-dir>`) (r2-MAJOR-1)
보존만으로는 재개가 안 된다(r2 실측: `Stream::new`는 빈 엔진 생성,
journal 재생 없음 — stream.rs 71). 재개는 데모 CLI의 명시적 경로다:
- 대상(2개 소재로 분리 — r4-MINOR): ① 런 디렉터리(orch.db·스트림
  저널·승인 원장 db·run.json), ② **외부 공식 Die workdir**(/tmp/
  foundry-die-<order>-1 — workdir.rs 경로 계약상 런 디렉터리 밖).
  ②의 존재 검사는 상태 분기 **이후 publisher 재호출이 필요한 경우에만**
  (r5-MAJOR-2). 부재면 D4 소실 정책(새 런).
- 절차: **저널 재생·상태 판정이 먼저** (r5-MAJOR-2) — workdir 존재
  검사는 그 이후, **실제 publisher 재호출에만** 적용한다(Shipped
  확정·검증된 PR의 confirm은 workdir 불요 — 확정 경로는 workdir을
  읽지 않는다, stream.rs 344). → 엔진 재구축 → 스트림 등록 → 분기: 재조회는 **브랜치 라벨 조회로는 승인되지 않은 커밋을 확정할
  수 있다**(r3-MAJOR-1 — 퍼블리셔 head 조회는 라벨 조건, SHA 아님,
  github_publisher.rs 154)는 사실에서 출발: 원격 head ref·base·
  **`head.sha == approved digest`**·닫힘/머지 이력을 전부 조회해야
  한다. SHA 불일치·닫힘·머지·조회 실패 = 보존 후 수동 판정.
  **"미생성 확인"된 경우만** 동일 op_id 재시도 허용(닫힘/머지와
  미생성을 open-PR 부재만으로 구별 금지).
- 상태 분기:
  - **Publishing**: 승인/Die 생략. 원격 검증(SHA 일치 open PR 존재)
    → pr_url 확보 → confirm_published(workdir 불요). **미생성 확인 →
    publisher 재시도(이때만 workdir 존재 필수)** — 부재면 D4 소실
    정책(새 런). 불확실 → 수동 판정 안내 후 종료(코드 4).
  - **Shipped** (r3-MINOR — confirm append 성공 후 출력 전 종료):
    발행 없이 저널 해당 주문 `PublishConfirmed.pr_url` 검증 후 종결
    증거만 출력하는 성공 분기 — **workdir 부재와 무관**. precheck의
    Publishing 요구는 신규 발행에만 적용.
- 실행 잠금은 재획득(기존 exec 래치는 Infrastructure 상태였다면 새 exec).
- **ExternalRef 영속화** (r3-MAJOR-2): replay는 원래 ExternalRef를
  복원하지 않는다(ledger/replay.rs 155 — `o-140-r2`는
  `ExternalRef::cron("replay","140-r2")`로 추정되고 dedup_key에 `#140`이
  없음). 퍼블리셔는 dedup_key에서만 이슈 번호를 추출하므로(35행)
  종료→재개→최초 PR 생성 경로에서 `Closes #140`이 누락된다. 해결:
  **런 메타데이터 파일(run.json: order_id→ExternalRef 직렬화)을 런
  시작 시 저장**, 재개 시 이를 order_id와 대조해 퍼블리셔에 전달할
  주문 스냅샷에 복원. 회귀 테스트: 종료→재개→PR 생성 시 PR body에
  `Closes #<N>` 존재 검증.
- 새 런(기본)과 재개는 상호 배타 — 재개 중 같은 주문의 새 Die 금지.

### D3 — m8_live_die 시나리오 확장 (8→10단계)
- (8) 발행: precheck → GithubPublisher(`owner: lunarpulse, repo:
  foundary-demo, token_path: ~/.foundry/gh_token, base: main`) →
  confirm_published → PR URL 출력.
- (9) 알림: `Shipped { order_id, pr_url }` — `--notify-mode console`
  기본, notifier.env 있으면 Discord. **알림 실패는 발행·증거를 절대
  방해하지 않는다**(관측 플레인 — `?`/unwrap 전파 금지).
- (10) 종결 증거: 저널 해당 주문 to_state=Shipped 행 + pr_url.
- 기존 종료 문구 "PR 생성 → 머지 → Shipped" 수정: **"PR 생성 시
  Shipped 확정, 머지는 인간 리뷰 소유"** (r1 D5 지적 반영).

### D4 — workdir 수명과 배타 (r1-MAJOR-2/3 반영)
- 안전 순서 고정: **클린 → Die → finish → 승인 → 발행 확정 → (선택)
  정리**. 발행 확정 전에는 workdir 절대 삭제 불가.
- 같은 이슈 재실행 충돌: `o-140` 재사용 시 (a) PID별 승인 원장이라
  이전 CONSUMED가 재승인을 못 막고 (b) 새 클론 커밋의 기존 원격 브랜치
  push가 non-fast-forward로 실패한다. 해결: **재실행은 주문 식별자에
  런 접미사**(`o-140-r2`) — 새 workdir·새 브랜치·새 그랜트. "동일
  산출물 발행 재시도"만 D2-R의 보존·재시도 경로.
- GC(gc.rs 223): 같은 주문의 최대 attempt 외 즉시 삭제 — 데모는 attempt
  1 단일 유지 + 런 접미사로 주문이 매번 달라 교집합 없음. /tmp 소실
  시: workdir이 없으면 발행 불가(복구 정책: Die 재실행 = 새 런, 이전
  원격 브랜치는 인간 정리) — 명시된 복구 불가 정책.

### D5 — 머지는 인간 소유
PR 생성까지만 자동화. `Closes #N`은 base(main) 머지 시 이슈 자동 종료
— 데모 종료 문구와 일치하도록 수정(D3).

### D6 — 운영 정리
- 이슈 #139(rand, 헤드룸 없음 정직 거부): "no headroom" 코멘트 후
  클로즈.
- 재실행 이슈 시드: crates.io 헤드룸 사전 검증 후 발행.

## 2. 리스크

| 리스크 | 완화 |
|---|---|
| /tmp workdir 소실(GC·리부트) | 발행은 Die 직후 순차. 소실 시 **publisher 재호출 불가**(검증된 PR의 confirm·Shipped 증거 출력은 가능), 재발행은 새 런 |
| 토큰 노출 | 퍼블리셔 스크럽 내장; 데모는 token_path만 전달 |
| PR 생성 4xx | Permanent 즉시 중단 — confirm 불가 → Publishing 유지 |
| 발행 후 confirm 실패 | D2-R 보존+재시도(동일 head PR 멱등) |
| 같은 주문 재실행 충돌 | 런 접미사로 새 주문(D4) |
| 알림 실패 | 관측 플레인 — 증거 출력까지 계속(D3) |

## 3. 구현 분업

- **S1 (위임)**: `publish_precheck`/`confirm_published` 정문 + 회귀
  테스트 5종(Publishing 아님→거부 / 정상 최초→Shipped+저널 pr_url /
  이미 Shipped+URL 일치→멱등 Ok / Shipped+URL 불일치→Err /
  append-실패 Infrastructure 래치→새 exec로 확정).
- **S2 (직접)**: m8_live_die — workdir 경로 이동·런 접미사·8~10단계·
  `--notify-mode`·문구 수정.
- **S3 (직접)**: 라이브 e2e(새 이슈 시드) — PR URL·저널 Shipped·
  (env 있으면) Discord 도착 검증.
- **S4 (운영)**: #139 클로즈, 커밋·푸시.
