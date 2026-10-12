# EDIT SPEC — r2-B2 part 3: rooted stream + unbound bootstrap allowed (finalize)

## 배경
이전 스펙(r2-B2-part2)까지 적용된 상태: stream.rs의 빈 preset_root 스트림은
부트스트랩을 거부한다(fail-closed). 하지만 게이트가 **루트 결속 스트림의
미결속(bound=None) 부트스트랩까지 거부**해 7개 테스트가 실패 중이다.

## 설계 확정 (이 스펙의 규칙)
부트스트랩 게이트의 목적은 "임의 스트림이 남의 프리셋 결속으로 부트스트랩"을
차단하는 것이다. 사이징 이전 입장(프리셋 미결속)은 루트 결속 스트림이면
합법(T60/T61 원 규약 — 사이징 주문이 먼저 결속을 만든다). 규칙:

- 스트림 preset_root 빈 경우 → 거부 (이미 구현됨, 유지)
- preset_root 있음 + bound_root = None(미결속) → **허용**
- preset_root 있음 + bound_root = Some → 일치 검사 (이미 구현됨, 유지)

## 대상 파일 (이 파일만 수정)
`/home/cosmo/foundry/crates/foundry-orch/src/stream.rs`

## 정확한 교체 — OLD 블록

```rust
                    let expected = Some(stream.preset_root.clone());
                    if bound_root != expected {
```

## NEW 블록

```rust
                    let expected = Some(stream.preset_root.clone());
                    // (r2 B2 part3) 미결속(None) 부트스트랩은 루트 결속
                    // 스트림이면 허용 — 사이징 이전 입장(결속 생성 전).
                    // 결속이 존재하면 일치해야 한다.
                    if bound_root.is_some() && bound_root != expected {
```

주의: 이 OLD 블록은 파일에 **정확히 1회** 나와야 한다(확인 후 교체).
2회 이상이면 아무것도 수정하지 말고 중단 보고. 그 외 어떤 것도 수정 금지.
**테스트 파일은 이 스펙에서 일절 수정하지 않는다**(part2의 마이그레이션은
이미 적용된 상태를 그대로 둔다).

## 인수 명령 (순서대로)

PATH에 cargo가 없다면 먼저 `source $HOME/.cargo/env`를 실행할 것.

1. `cargo build -p foundry-orch` — 에러 0
2. `cargo test -p foundry-orch -- --test-threads=4 --skip t61_kill9` — 48 passed 이상, 0 failed

## 완료 판정
전부 통과하면 성공. 실패하면 이 스펙의 수정만 원복하고 실패 테스트
이름+패닉 메시지를 보고.
