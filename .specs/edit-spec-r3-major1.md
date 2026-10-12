# EDIT SPEC — r3-MAJOR-1: dispatch_handoff CONSUMED-idempotent retry

## 배경
재리뷰 r3 신규 MAJOR: `dispatch_handoff`이 검증 후 무조건 reserve→finalize를
수행한다. ①정상 소비 후 재시도 ②finalize와 confirm 사이 크래시 후 재시도 —
두 경우 모두 그랜트가 이미 CONSUMED라 `reserve`가 `AlreadyConsumed`로
실패해 정문이 영구 막힌다. CONSUMED 그랜트는 멱등하게 confirm만 재수행해야
한다.

## 대상 파일 (이 파일만 수정)
`/home/cosmo/foundry/crates/foundry-orch/src/stream.rs`

## 정확한 교체 — OLD 블록

```rust
        // ② 소비 — 예약→적용 개시→확정(CONSUMED). fence 는 원장 TxGuard.
        let exec = format!("handoff-{source_order}-{dest_order}");
        let sr = ap
            .reserve_grant(grant_id, &scope_val, &exec, 1)
            .map_err(|e| format!("reserve: {e:?}"))?;
        ap.begin_apply(grant_id, &exec, 1, sr.fence_token)
            .map_err(|e| format!("begin_apply: {e:?}"))?;
        ap.finalize_grant(grant_id, &exec, 1, sr.fence_token)
            .map_err(|e| format!("finalize: {e:?}"))?;
```

## NEW 블록

```rust
        // ② 소비 — 예약→적용 개시→확정(CONSUMED). fence 는 원장 TxGuard.
        // (r3 MAJOR-1) 이미 CONSUMED 면 소비를 건너뛰고 확정만 재수행
        // (멱등 재시도: 정상 소비 후 재호출, finalize↔confirm 사이
        // 크래시 후 재호출 모두 정문으로 회복).
        let already_consumed = ap
            .get_grant(grant_id)
            .map_err(|e| format!("grant lookup: {e:?}"))?
            .state
            == foundry_approval::GrantState::Consumed;
        if !already_consumed {
            let exec = format!("handoff-{source_order}-{dest_order}");
            let sr = ap
                .reserve_grant(grant_id, &scope_val, &exec, 1)
                .map_err(|e| format!("reserve: {e:?}"))?;
            ap.begin_apply(grant_id, &exec, 1, sr.fence_token)
                .map_err(|e| format!("begin_apply: {e:?}"))?;
            ap.finalize_grant(grant_id, &exec, 1, sr.fence_token)
                .map_err(|e| format!("finalize: {e:?}"))?;
        }
```

주의: 이 OLD 블록은 파일에 **정확히 1회** 나와야 한다(확인 후 교체).
2회 이상이면 아무것도 수정하지 말고 중단 보고. 그 외 어떤 것도 수정 금지.

## 2단계 — 회귀 테스트 추가

`crates/foundry/foundry-orch/src/t61_end_to_end.rs` 파일 끝에 아래 테스트를
**그대로 append** 한다(기존 내용 수정 금지):

```rust

/// (r3 MAJOR-1) 소비 후 재시도 멱등 — CONSUMED 그랜트로 정문 재호출이
/// AlreadyConsumed 에러 없이 성공(confirm 재수행).
#[test]
fn t61_handoff_retry_after_consumption_idempotent() {
    let d = tmpdir("horetry");
    let orch = OrchestrationLog::open(&d.join("orch.db")).unwrap();
    orch.declare_plan(
        "plan-hr",
        vec![("t-r1".into(), vec![]), ("t-r2".into(), vec![])],
        vec![("t-r1".into(), "s-hr".into()), ("t-r2".into(), "s-hr".into())],
    )
    .unwrap();
    let mut ap = foundry_approval::ApprovalLedger::open(&d.join("ap.db")).unwrap();
    {
        let digest = crate::chainer::sizing_digest("approved", "bmad-epic-build", "plan-hr");
        ap.issue_grant("sizing-sz-hr", "tok-hr", &serde_json::json!({
            "order_id": "sz-hr", "intent": "sizing", "artifact_digest": digest, "root_scope": "",
        }))
        .unwrap();
        let sr = ap.reserve_grant("sizing-sz-hr", &serde_json::json!({
            "order_id": "sz-hr", "intent": "sizing", "artifact_digest": digest, "root_scope": "",
        }), "hr", 1).unwrap();
        ap.begin_apply("sizing-sz-hr", "hr", 1, sr.fence_token).unwrap();
        ap.finalize_grant("sizing-sz-hr", "hr", 1, sr.fence_token).unwrap();
        let ch = Chainer::new(&orch, &ap);
        ch.bind_preset_from_ledger("sz-hr", "bmad-epic-build", "plan-hr").unwrap();
    }
    let rt = StreamRuntime::new(OrchestrationLog::open(&d.join("orch.db")).unwrap());
    rt.add_stream(Stream::new_with_root("s-hr", &d, "bmad-epic-build").unwrap()).unwrap();
    rt.bind_approval_ledger(
        foundry_approval::ApprovalLedger::open(&d.join("ap.db")).unwrap(),
    )
    .unwrap();
    rt.dispatch("t-r1", mkorder("t-r1", false)).unwrap();
    rt.dispatch("t-r2", mkorder("t-r2", false)).unwrap();
    let (manifest, _enc) = rt.prepare_handoff("t-r1", "t-r2", "art-hr").unwrap();
    let mut ap2 = foundry_approval::ApprovalLedger::open(&d.join("ap.db")).unwrap();
    ap2.issue_grant("ho-hr", "tok-ho2", &serde_json::json!({
        "order_id": "t-r1", "dest_order_id": "t-r2", "intent": "handoff",
        "artifact_digest": "art-hr", "handoff_manifest": manifest, "root_scope": "",
    }))
    .unwrap();
    // 1차 소비.
    rt.dispatch_handoff("t-r1", "t-r2", "art-hr", "ho-hr").unwrap();
    // 재시도 — CONSUMED 면 멱등 성공이어야 한다(AlreadyConsumed 아님).
    rt.dispatch_handoff("t-r1", "t-r2", "art-hr", "ho-hr")
        .unwrap_or_else(|e| panic!("retry after consumption must be idempotent: {e}"));
    let ap_check = foundry_approval::ApprovalLedger::open(&d.join("ap.db")).unwrap();
    assert!(ap_check.get_grant("ho-hr").unwrap().state
        == foundry_approval::GrantState::Consumed);
}
```

(append 전에 `fn t61_handoff_retry_after_consumption_idempotent`가 이미
파일에 없는지 확인할 것. 있으면 중단 보고.)

## 인수 명령 (순서대로)

PATH에 cargo가 없다면 먼저 `source $HOME/.cargo/env`를 실행할 것.

1. `cargo build -p foundry-orch` — 에러 0
2. `cargo test -p foundry-orch -- --test-threads=4 --skip t61_kill9` — 49 passed, 0 failed

## 완료 판정
전부 통과하면 성공. 실패하면 이 스펙의 수정만 원복(git checkout 대상:
stream.rs, t61_end_to_end.rs)하고 실패 테스트 이름+패닉 메시지를 보고.
