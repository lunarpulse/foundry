# EDIT SPEC — r2-MAJOR-3: undeclared reentry refusal must be journaled

## 배경
`Chainer::reenter`의 첫 검사(선언 재검사)는 `DeclarationMissing` 에러만 반환하고
**영구 거부 기록을 남기지 않는다**(재리뷰 r2 MAJOR: "undeclared reentry refusal
remains unrecorded"). 같은 함수의 후속 검사(check_spawn_binding 결과)는 이미
ReentryAdmitted/ReentryRefused 행으로 기록된다. 선언 거부만 누락이다.

## 대상 파일 (이 파일만 수정)
`/home/cosmo/foundry/crates/foundry-orch/src/chainer.rs`

## 정확한 교체 — OLD 블록

```rust
            if !is_declared {
                return Err(ChainerError::DeclarationMissing { order: order_id.into() });
            }
```

## NEW 블록

```rust
            if !is_declared {
                // (r2 MAJOR-3) 미선언 재입장 거부도 영구 기록 — 검증만 하고
                // 흔적 없는 경로 폐지(아래 ReentryRefused 와 동일 규약).
                let _ = crate::refusal::record_refusal(
                    self.log,
                    crate::refusal::RefusalCode::UndeclaredOrder,
                    order_id,
                    None,
                );
                return Err(ChainerError::DeclarationMissing { order: order_id.into() });
            }
```

주의: 이 OLD 블록은 파일에 **정확히 1회** 나와야 한다(확인 후 교체).
2회 이상이면 아무것도 수정하지 말고 중단 보고. 그 외 어떤 것도 수정 금지.

## 인수 명령 (순서대로)

PATH에 cargo가 없다면 먼저 `source $HOME/.cargo/env`를 실행할 것.

1. `cargo build -p foundry-orch` — 에러 0
2. `cargo test -p foundry-orch -- --test-threads=4 --skip t61_kill9` — 48 passed, 0 failed

## 완료 판정
두 명령 모두 통과하면 성공. 통과하지 않으면 수정을 원복하고 실패 내용을 보고.
