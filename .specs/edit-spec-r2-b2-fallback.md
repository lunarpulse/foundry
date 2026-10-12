# EDIT SPEC — r2-B2 residual: empty preset_root must fail-closed (no self-compare fallback)

## 배경
스트림 루트 결속 게이트에서 `preset_root`가 빈 스트림(`Stream::new`, 루트 미지정)은
`expected = bound_root` — **자기비교**로 되돌아가 항상 통과한다(재리뷰 r2: "root
fallback still self-compares"). 빈 루트 스트림의 Intake 부트스트랩은 거부되어야
한다(fail-closed). `new_with_root`로 생성된 스트림만 부트스트랩을 호스팅한다.

## 대상 파일 (이 파일만 수정)
`/home/cosmo/foundry/crates/foundry-orch/src/stream.rs`

## 정확한 교체 — OLD 블록

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

## NEW 블록

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

주의: 이 OLD 블록은 파일에 **정확히 1회** 나와야 한다(확인 후 교체).
2회 이상이면 아무것도 수정하지 말고 중단 보고. 그 외 어떤 것도 수정 금지.

## 기대 동작 변화
- `Stream::new`(빈 루트)로 만든 스트림에 Intake 주문 dispatch → 이제 거부
  (기존 통과). 관련 기존 테스트가 이 형태를 쓰고 있으면 **그 테스트가
  실패할 수 있다** — 그 경우 테스트 파일은 수정하지 말고 원복 후 실패
  테스트 이름을 보고한다(테스트 갱신은 별도 스펙으로 진행).

## 인수 명령 (순서대로)

PATH에 cargo가 없다면 먼저 `source $HOME/.cargo/env`를 실행할 것.

1. `cargo build -p foundry-orch` — 에러 0
2. `cargo test -p foundry-orch -- --test-threads=4 --skip t61_kill9` — 결과 보고(통과/실패 테스트 이름 포함)

## 완료 판정
빌드 에러 0 + 기존 48 테스트가 여전히 전부 통과하면 성공. 기존 테스트가
실패하면 원복하고 어떤 테스트가 왜 실패했는지 보고(테스트 갱신은 별도 처리).
