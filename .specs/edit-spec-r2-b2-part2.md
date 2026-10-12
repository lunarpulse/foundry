# EDIT SPEC — r2-B2 residual, part 2: bootstrap-hosting tests migrate to new_with_root

## 배경
이전 스펙(r2-B2)으로 stream.rs의 빈 preset_root 폴백이 fail-closed로 바뀌는
**코드 수정은 이미 검증된 설계**다. 하지만 부트스트랩(Intake) 주문을 호스팅하는
기존 테스트 7개가 `Stream::new`(빈 루트)를 써서 실패한다. 이 스펙은
**코드 수정(동일 NEW 블록) + 실패 테스트의 스트림 생성 마이그레이션**을 함께
수행한다.

## 1단계 — 코드 수정 (stream.rs)

### 대상 파일
`/home/cosmo/foundry/crates/foundry-orch/src/stream.rs`

### OLD 블록 (정확히 1회 출현 — 확인 후 교체)

```rust
                if is_bootstrap {
                    let bound_root = sizing
                        .as_deref()
                        .and_then(|so| self.root_of_sizing(so));
                    let expected = if stream.preset_root.is_empty() {
                        bound_root.clone()
                    } else {
                        Some(stream.preset_root.clone())
                    };
                    if bound_root != expected {
```

### NEW 블록

```rust
                if is_bootstrap {
                    let bound_root = sizing
                        .as_deref()
                        .and_then(|so| self.root_of_sizing(so));
                    // (r2 B2) 빈 preset_root 스트림은 부트스트랩 거부 —
                    // 자기비교 폴백 폐지(fail-closed).
                    if stream.preset_root.is_empty() {
                        let _ = record_refusal(
                            &self.orch,
                            RefusalCode::UnsizedSpawn,
                            order_id,
                            None,
                        );
                        return Err(format!(
                            "order '{order_id}' bootstrap refused — stream '{}' has no bound preset root (use new_with_root)",
                            stream.id()
                        ));
                    }
                    let expected = Some(stream.preset_root.clone());
                    if bound_root != expected {
```

## 2단계 — 실패 테스트의 스트림 생성 마이그레이션

코드 수정 직후 `cargo test -p foundry-orch -- --test-threads=4 --skip t61_kill9`를
실행해 실패 테스트 목록을 확정한다. 예상 실패(부트스트랩 호스팅 테스트):

1. `crates/foundry-orch/src/stream.rs` —
   `coordinator_blocks_dispatch_and_rebuilds`(927 부근),
   `cross_stream_both_orders_run`(977 부근),
   `stream_hosts_two_orders_full_cycle_with_dependency`(1010 부근),
   그 외 이 테스트 실행으로 실패가 확인된 stream.rs 내 테스트
2. `crates/foundry-orch/src/t60_scale3_epic.rs` — 실패 확인된 3개
   (`scale3_sizing_to_four_ticket_epic_end_to_end`,
   `scale3_ticket_firewall_holds`, `sizing_selection_paths_for_all_scales`)
3. `crates/foundry-orch/src/t61_end_to_end.rs` —
   `t61_end_to_end_reentry_bypass_crash_rebuild`(116-117 부근의
   `Stream::new("s-sizing", …)` / `Stream::new("s-epic", …)`)

### 마이그레이션 규칙
- 실패한 테스트 함수 내부의 `Stream::new("<id>", &d)`를
  `Stream::new_with_root("<id>", &d, "<preset>")`로 바꾼다.
- `<preset>`은 그 테스트가 사용하는 프리셋 이름과 동일해야 한다. 해당
  테스트에서 `bind_preset_from_ledger`/`sizing_digest(...)` 호출에 쓰인
  프리셋 문자열(예: "bmad-quickloop", "bmad-epic-build",
  "bmad-intake-pool")을 함수 본문에서 찾아 그 값을 사용한다.
- **부트스트랩(Intake) 주문을 dispatch하지 않는** 테스트(순수
  등록/중복거부 테스트 등)는 수정하지 않는다. 실패 목록에 없으면 손대지
  않는다.
- 테스트 논리(단정·순서)는 일절 수정하지 않는다 — 스트림 생성자 호출만.

## 3단계 — 인수 (순서대로)

PATH에 cargo가 없다면 먼저 `source $HOME/.cargo/env`를 실행할 것.

1. `cargo build -p foundry-orch` — 에러 0
2. `cargo test -p foundry-orch -- --test-threads=4 --skip t61_kill9` —
   **49 passed, 0 failed** 이상(신규 거부 경로 테스트 추가는 없으므로
   49여야 한다), 0 failed 필수

## 완료 판정
전부 통과하면 성공. 어떤 테스트가 계속 실패하면: 코드 수정은 유지하고
원복하지 말 것 — 실패 테스트 이름+패닉 메시지를 그대로 보고한다.
