# EDIT SPEC — r2-MAJOR-2: fail-closed drain cross-check (컴파일 픽스)

## 배경
`crates/foundry-orch/src/stream.rs`의 RecoveryCoordinator::recover 내 클로저가
`?` 연산자를 쓰는데 반환 타입이 `bool`로 추론되어 컴파일 에러(E0277 2건,
E0308, E0599). `pending_reservations()`는 이미 `Result<Vec<...>, String>`로
변경됨(정상). stream.rs의 호출부 클로저만 고치면 됨.

## 대상 파일
`/home/cosmo/foundry/crates/foundry-orch/src/stream.rs`

## 정확한 교체 — OLD 블록 (현재 708~714 라인 부근)

```rust
                let drained_in_ledger = (|| -> bool {
                    let ap = foundry_approval::ApprovalLedger::open(approval_db).ok()?;
                    // (r2 MAJOR-2) 읽기 실패는 drain 아님(fail-closed).
                    let pending = ap.pending_reservations().ok()?;
                    Some(want.iter().all(|k| !pending.iter().any(|(g, _, _)| g == k)))
                })()
                .unwrap_or(false);
```

## NEW 블록

```rust
                let drained_in_ledger = (|| -> Option<bool> {
                    let ap = foundry_approval::ApprovalLedger::open(approval_db).ok()?;
                    // (r2 MAJOR-2) 읽기 실패는 drain 아님(fail-closed).
                    let pending = ap.pending_reservations().ok()?;
                    Some(want.iter().all(|k| !pending.iter().any(|(g, _, _)| g == k)))
                })()
                .unwrap_or(false);
```

변경은 단 한 곳: `(|| -> bool` → `(|| -> Option<bool>`.
그 외 어떤 것도 수정 금지. 주석·공백 포함 OLD 블록과 정확히 일치하는
부분만 교체한다. 일치하는 블록이 없으면 아무것도 수정하지 말고 중단.

## 인수 명령 (순서대로, 전부 통과해야 함)

PATH에 cargo가 없다면 먼저 `source $HOME/.cargo/env`를 실행할 것.

1. `cargo build -p foundry-orch` — 에러 0
2. `cargo test -p foundry-orch -- --test-threads=4 --skip t61_kill9` — 49 passed, 0 failed

(참고: `t61_kill9` 테스트는 워크스페이스 동시 실행 시 교착되는 기존 특성이
있어 스킵한다. 그 테스트는 별도 검증한다.)

## 완료 판정
두 명령 모두 통과하면 성공. 통과하지 않으면 수정 내용을 원복(git checkout)
하고 무엇이 실패했는지 보고.
