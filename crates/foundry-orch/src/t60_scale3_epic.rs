#[cfg(test)]
mod t60_scale3_epic {
    //! T60 — 스케일 3 + 에픽 종단 시나리오 (M7 플랜 v9 H절 T60 확장).
    //! 크레이트 내부 테스트 — 엔진 주도권(실행 경로)이 크레이트 내부이기
    //! 때문(T58 r10 계약). 공개면은 dispatch/Chainer 만 사용해 종단을 증명.

    use foundry_domain::order::{ExternalRef, Order};
    use foundry_domain::{OrderEvent, OrderState};
    use crate::chainer::{Chainer, ChainerError};
    use crate::stream::{Stream, StreamRuntime};
    use crate::{OrchestrationKind, OrchestrationLog};
    use foundry_workflow::{Outcome, WorkflowEngine};
    use std::collections::BTreeSet;

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("foundry-t60-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 스케일 3 프리셋 검증 — bmad-epic-build.
    #[test]
    fn scale3_epic_preset_validates() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../presets/bmad-epic-build.v3.json");
        let doc = foundry_workflow::dsl::load_preset(&path).unwrap();
        assert!(
            foundry_workflow::validator::validate(
                &doc,
                &foundry_domain::workflow_encoding::EmbeddedWorkflow::frozen_v1(),
            )
            .is_ok(),
            "bmad-epic-build must validate"
        );
        // 스케일 3 역할 구성 — 5역할(sm·po·dev·qa + 검토 노드).
        let skills: Vec<_> = doc.skills.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(skills.len(), 4, "scale 3 = role composition sm/po/dev/qa");
    }

    /// 사이징 주문 하나를 FurnaceHold(인간 게이트)까지 진행.
    fn sizing_to_gate(e: &mut WorkflowEngine, exec: &str, oid: &str, journal_path: &std::path::Path) {
        for ev in [
            OrderEvent::Queued,
            OrderEvent::DieStarted,
            OrderEvent::DieSucceeded { attempt: 1 },
            OrderEvent::InspectionPassed,
        ] {
            let att = e.order(oid).unwrap().attempt().max(1);
            let rows = foundry_ledger::SqliteLedger::open(journal_path)
                .unwrap()
                .read_all()
                .unwrap();
            let ctx = crate::test_support::replay_ctx(oid, &rows);
            let ro: &'static mut foundry_ledger::SqliteLedger = Box::leak(Box::new(
                foundry_ledger::SqliteLedger::open(journal_path).unwrap(),
            ));
            e.dispatch_replay_ctx(oid, exec, &ev, att, &ctx, ro).unwrap();
        }
        assert_eq!(e.order(oid).unwrap().state(), OrderState::FurnaceHold);
    }

    /// 주문을 직접 승인으로 Shipped 까지 (T58 run_to_shipped 동일 패턴).
    fn ticket_to_shipped(
        rt: &StreamRuntime,
        stream_id: &str,
        oid: &str,
        grant_id: &str,
        ap_db: &std::path::Path,
    ) {
        let e = rt.stream(stream_id).unwrap();
        {
            let mut eng = e.engine_if_unfenced().unwrap();
            eng.acquire(oid, "t60").unwrap();
            for ev in [
                OrderEvent::Queued,
                OrderEvent::DieStarted,
                OrderEvent::DieSucceeded { attempt: 1 },
                OrderEvent::InspectionPassed,
            ] {
                let att = eng.order(oid).unwrap().attempt().max(1);
                let rows = foundry_ledger::SqliteLedger::open(&e.journal()).unwrap().read_all().unwrap();
                let ctx = crate::test_support::replay_ctx(oid, &rows);
                let ro: &'static mut foundry_ledger::SqliteLedger = Box::leak(Box::new(
                    foundry_ledger::SqliteLedger::open(&e.journal()).unwrap(),
                ));
                eng.dispatch_replay_ctx(oid, "t60", &ev, att, &ctx, ro).unwrap();
            }
        }
        let mut ap = foundry_approval::ApprovalLedger::open(ap_db).unwrap();
        ap.issue_grant(grant_id, &format!("tok-{grant_id}"), &serde_json::json!({
            "order_id": oid, "intent": "ship", "artifact_digest": format!("dig-{oid}"), "root_scope": "",
        }))
        .unwrap();
        let mut hook = foundry_workflow::ApprovalLedgerAdapter(ap);
        {
            let e2 = rt.stream(stream_id).unwrap();
            let mut eng = e2.engine_if_unfenced().unwrap();
            let grant = foundry_workflow::GrantRef { grant_id: grant_id.into() };
            let out = eng
                .dispatch_live(
                    oid,
                    "t60",
                    &OrderEvent::Approved { intent: "ship".into(), artifact_digest: format!("dig-{oid}") },
                    1,
                    Some(&grant),
                    &mut hook,
                )
                .unwrap();
            assert_eq!(out, Outcome::Applied);
            let rows = foundry_ledger::SqliteLedger::open(&e2.journal()).unwrap().read_all().unwrap();
            let ctx = crate::test_support::replay_ctx(oid, &rows);
            let ro: &'static mut foundry_ledger::SqliteLedger = Box::leak(Box::new(
                foundry_ledger::SqliteLedger::open(&e2.journal()).unwrap(),
            ));
            eng.dispatch_replay_ctx(
                oid,
                "t60",
                &OrderEvent::PublishConfirmed { pr_url: "https://pr/t60".into() },
                1,
                &ctx,
                ro,
            )
            .unwrap();
        }
    }

    fn mkorder(id: &str, intake: bool) -> Order {
        let mut ext = ExternalRef::cron("src", "t60");
        if intake {
            ext.external_id = format!("intake:{id}");
        }
        let mut o = Order::new(id.into(), ext, chrono::Utc::now());
        o.force_state_for_test(OrderState::Received, 1);
        o
    }

    /// 종단: 사이징 주문 → 스케일 3 선택 → 4-티켓 에픽 발행 → 전원 Shipped.
    #[test]
    fn scale3_sizing_to_four_ticket_epic_end_to_end() {
        let d = tmpdir("e2e");
        let orch = OrchestrationLog::open(&d.join("orch.db")).unwrap();

        // ── (1) 사이징 주문 — 인테이크 부트스트랩으로 스트림 s-sizing 진입.
        orch.declare_plan(
            "plan-sizing",
            vec![("sz-1".into(), vec![])],
            vec![("sz-1".into(), "s-sizing".into())],
        )
        .unwrap();
        let rt = StreamRuntime::new(orch);
        let orch = rt.orch.clone();
        rt.add_stream(Stream::new("s-sizing", &d).unwrap()).unwrap();
        rt.add_stream(Stream::new("s-epic", &d).unwrap()).unwrap();
        rt.bind_approval_ledger(
            foundry_approval::ApprovalLedger::open(&d.join("ap.db")).unwrap(),
        )
        .unwrap();
        rt.dispatch("sz-1", mkorder("sz-1", true)).unwrap();

        // (T60 astra r1 MINOR) 사이징 주문은 **런타임 소유 엔진**으로 구동 —
        // 메모리 상태·저널이 스트림 소유 그대로다.
        {
            let e = rt.stream("s-sizing").unwrap();
            let mut eng = e.engine_if_unfenced().unwrap();
            eng.acquire("sz-1", "t60-sizing").unwrap();
            sizing_to_gate(&mut eng, "t60-sizing", "sz-1", &d.join("s-sizing.db"));
        }

        // ── (2) 인간 사이징 게이트 — 스케일 3 승인.
        // 그랜트 스코프: 판정 digest + 선택 목적지(root/plan) 인코딩(H절).
        let mut ap = foundry_approval::ApprovalLedger::open(&d.join("ap.db")).unwrap();
        // (T60 astra r1) 목적지 인코딩 digest — 라이브 소비 스코프(4필드)와
        // 양립하는 유일한 인코딩 자리.
        let sizing_digest = crate::chainer::sizing_digest(
            "approved", "bmad-epic-build", "plan-epic-42",
        );
        // root_scope 는 scope_for 기본("")와 일치 — 라이브 소비용.
        let sscope = serde_json::json!({
            "order_id": "sz-1", "intent": "sizing", "artifact_digest": sizing_digest,
            "root_scope": "",
        });
        ap.issue_grant("sizing-sz-1", "tok-sizing-human", &sscope).unwrap();
        // **라이브 인간 게이트** — 사이징 주문 FurnaceHold 에서 dispatch_live
        // 로 승인(그랜트가 정상 경로로 소비된다 — 원장 직접 조작 아님).
        {
            let e2 = rt.stream("s-sizing").unwrap();
            let mut eng = e2.engine_if_unfenced().unwrap();
            let mut hook = foundry_workflow::ApprovalLedgerAdapter(
                foundry_approval::ApprovalLedger::open(&d.join("ap.db")).unwrap(),
            );
            let grant = foundry_workflow::GrantRef { grant_id: "sizing-sz-1".into() };
            let out = eng
                .dispatch_live(
                    "sz-1",
                    "t60-sizing",
                    &OrderEvent::Approved {
                        intent: "sizing".into(),
                        artifact_digest: sizing_digest.clone(),
                    },
                    1,
                    Some(&grant),
                    &mut hook,
                )
                .unwrap();
            assert_eq!(out, Outcome::Applied, "live human sizing approval applies");
        }

        // ── (3) preset_bound — 체이너의 유일 경로(원장 검증+멱등).
        let ch = Chainer::new(&orch, &ap);
        ch.bind_preset_from_ledger("sz-1", "bmad-epic-build", "plan-epic-42").unwrap();

        // ── (4) 4-티켓 에픽 발행 — 의존성: t1→{t2,t3}→t4.
        orch.declare_plan(
            "plan-epic-42",
            vec![
                ("t-1".into(), vec![]),
                ("t-2".into(), vec!["t-1".into()]),
                ("t-3".into(), vec!["t-1".into()]),
                ("t-4".into(), vec!["t-2".into(), "t-3".into()]),
            ],
            vec![
                ("t-1".into(), "s-epic".into()),
                ("t-2".into(), "s-epic".into()),
                ("t-3".into(), "s-epic".into()),
                ("t-4".into(), "s-epic".into()),
            ],
        )
        .unwrap();

        // ── (5) 티켓 dispatch — 사이즈 게이트 통과(preset_bound 존재+digest 일치).
        rt.dispatch("t-1", mkorder("t-1", false)).unwrap();
        // 게이트 ①: unsized — preset_bound 없는 다른 플랜의 발행 거부.
        orch.declare_plan("plan-rogue", vec![("bad-1".into(), vec![])], vec![("bad-1".into(), "s-epic".into())]).unwrap();
        let refused = rt.dispatch("bad-1", mkorder("bad-1", false));
        assert!(refused.is_err(), "gate1: unsized spawn refused");
        // 게이트 ②: digest 불일치 — chainer 직접 확인.
        let r2 = ch.check_spawn_binding(
            "t-2",
            "ext:x",
            Some("sz-1"),
            Some(&crate::chainer::sizing_digest("approved", "bmad-epic-build", "plan-OTHER")),
            "plan-epic-42",
            "bmad-epic-build",
        );
        assert!(
        matches!(r2, Err(ChainerError::DigestMismatch { .. })),
        "gate2: digest (destination) mismatch refused"
    );

        // ── (6) 티켓 완주 — t1 후 t2·t3 병렬, 전부 후 t4.
        ticket_to_shipped(&rt, "s-epic", "t-1", "g-t1", &d.join("ap.db"));
        assert!(rt.ready_to_start("t-2").unwrap());
        assert!(rt.ready_to_start("t-3").unwrap());
        assert!(!rt.ready_to_start("t-4").unwrap(), "t4 waits for t2+t3");
        rt.dispatch("t-2", mkorder("t-2", false)).unwrap();
        rt.dispatch("t-3", mkorder("t-3", false)).unwrap();
        ticket_to_shipped(&rt, "s-epic", "t-2", "g-t2", &d.join("ap.db"));
        ticket_to_shipped(&rt, "s-epic", "t-3", "g-t3", &d.join("ap.db"));
        assert!(rt.ready_to_start("t-4").unwrap());
        rt.dispatch("t-4", mkorder("t-4", false)).unwrap();
        ticket_to_shipped(&rt, "s-epic", "t-4", "g-t4", &d.join("ap.db"));

        // 에픽 스트림 — 4티켓 전원 Shipped.
        for t in ["t-1", "t-2", "t-3", "t-4"] {
            assert!(rt.stream("s-epic").unwrap().order_shipped(t).unwrap(), "{t} shipped");
        }
        // 발행 기록 — 사이징+4티켓 = 5 (rogue 거부는 미발행).
        let rows = orch.read_all().unwrap();
        let spawned: Vec<_> = rows
            .iter()
            .filter(|r| matches!(&r.kind, OrchestrationKind::OrderSpawned { order_id, .. } if order_id != "bad-1"))
            .collect();
        assert_eq!(spawned.len(), 5, "sz-1 + 4 tickets spawned");
    }

    /// (T60 astra r1 MAJOR-②) 스케일 1·2·3 **사이징 선택 경로** 실증 —
    /// 각 스케일로 같은 파이프라인: 그랜트(라이브 소비 스코프) → 체이너
    /// 결속(원장 검증·digest 목적지 일치) → 발행 허용. 스케일 3 종단은
    /// 위 대형 시나리오가 담당.
    #[test]
    fn sizing_selection_paths_for_all_scales() {
        for (scale, root, plan) in [
            ("1", "bmad-quickloop", "plan-scale1"),
            ("2", "bmad-feature-prd", "plan-scale2"),
            ("3", "bmad-epic-build", "plan-scale3"),
        ] {
            let d = tmpdir(&format!("sel{scale}"));
            let orch = OrchestrationLog::open(&d.join("orch.db")).unwrap();
            // 스케일 공통: 사이징 주문을 런타임으로 FurnaceHold 까지 구동 후
            // **라이브 승인**(dispatch_live)으로 그랜트 소비.
            orch.declare_plan(
                &format!("plan-sizing-{scale}"),
                vec![(format!("sz-scale{scale}"), vec![])],
                vec![(format!("sz-scale{scale}"), format!("s-sel{scale}"))],
            )
            .unwrap();
            let rt = StreamRuntime::new(orch);
            rt.add_stream(Stream::new(&format!("s-sel{scale}"), &d).unwrap()).unwrap();
            rt.bind_approval_ledger(
                foundry_approval::ApprovalLedger::open(&d.join("ap.db")).unwrap(),
            )
            .unwrap();
            rt.dispatch(&format!("sz-scale{scale}"), mkorder(&format!("sz-scale{scale}"), true))
                .unwrap_or_else(|e| panic!("scale {scale} dispatch: {e:?}"));
            let sid = format!("sz-scale{scale}");
            {
                let e = rt.stream(&format!("s-sel{scale}")).unwrap();
                let mut eng = e.engine_if_unfenced().unwrap();
                eng.acquire(&sid, "sel").unwrap();
                sizing_to_gate(&mut eng, "sel", &sid, &d.join(format!("s-sel{scale}.db")));
            }
            let mut ap = foundry_approval::ApprovalLedger::open(&d.join("ap.db")).unwrap();
            let digest = crate::chainer::sizing_digest("approved", root, plan);
            let sscope = serde_json::json!({
                "order_id": sid.clone(), "intent": "sizing",
                "artifact_digest": digest.clone(), "root_scope": "",
            });
            ap.issue_grant(&format!("sizing-{sid}"), &format!("tok-{scale}"), &sscope).unwrap();
            // 라이브 인간 승인 — FurnaceHold 에서 dispatch_live.
            {
                let e2 = rt.stream(&format!("s-sel{scale}")).unwrap();
                let mut eng = e2.engine_if_unfenced().unwrap();
                let mut hook = foundry_workflow::ApprovalLedgerAdapter(
                    foundry_approval::ApprovalLedger::open(&d.join("ap.db")).unwrap(),
                );
                let grant = foundry_workflow::GrantRef { grant_id: format!("sizing-{sid}") };
                let out = eng
                    .dispatch_live(
                        &sid,
                        "sel",
                        &OrderEvent::Approved { intent: "sizing".into(), artifact_digest: digest.clone() },
                        1,
                        Some(&grant),
                        &mut hook,
                    )
                    .unwrap_or_else(|e| panic!("scale {scale} live approval: {e:?}"));
                assert_eq!(out, Outcome::Applied);
            }
            // 결속 — 목적지 일치(root/plan).
            let ch = Chainer::new(&rt.orch, &ap);
            ch.bind_preset_from_ledger(&sid, root, plan)
                .unwrap_or_else(|e| panic!("scale {scale} binding failed: {e:?}"));
            // 발행 허용 — digest(=목적지) 일치.
            ch.check_spawn_binding(
                &format!("order-scale{scale}"),
                "ext:x",
                Some(&sid),
                Some(&digest),
                plan,
                root,
            )
            .unwrap_or_else(|e| panic!("scale {scale} spawn gate failed: {e:?}"));
            // 목적지 불일치 — 다른 스케일 root 로 발행 시도 거부.
            let wrong = ch.check_spawn_binding(
                &format!("order-scale{scale}"),
                "ext:x",
                Some(&sid),
                Some(&digest),
                plan,
                "bmad-other-root",
            );
            assert!(
                matches!(wrong, Err(ChainerError::DestinationMismatch { .. })),
                "scale {scale}: cross-scale redirection refused"
            );
        }
    }

    /// 게이트 ③ — 거부된 사이징 후 발행 시도 거부.
    #[test]
    fn rejected_sizing_blocks_epic_spawn() {
        let d = tmpdir("rej");
        let orch = OrchestrationLog::open(&d.join("orch.db")).unwrap();
        let mut ap = foundry_approval::ApprovalLedger::open(&d.join("ap.db")).unwrap();
        // 인간 사이징 거부 — sizing_outcome: rejected.
        ap.issue_grant("sizing-sz-rej", "tok-rej", &serde_json::json!({
            "order_id": "sz-rej", "intent": "sizing",
            "artifact_digest": crate::chainer::sizing_digest("rejected", "bmad-epic-build", "plan-r"),
            "root_scope": "",
        }))
        .unwrap();
        let ch = Chainer::new(&orch, &ap);
        // 결속 시도 — 거부.
        assert!(matches!(
            ch.bind_preset_from_ledger("sz-rej", "bmad-epic-build", "plan-r"),
            Err(ChainerError::SizingRejected { .. })
        ));
        // preset_bound 없음 → 발행도 거부(unsized).
        assert!(matches!(
            ch.check_spawn_binding(
            "t-x",
            "ext:x",
            Some("sz-rej"),
            Some(&crate::chainer::sizing_digest("rejected", "bmad-epic-build", "plan-r")),
            "plan-r",
            "bmad-epic-build",
        ),
            Err(ChainerError::SizingRejected { .. })
        ));
        // 로그에 preset_bound 행 없음 단정.
        let rows = orch.read_all().unwrap();
        assert!(rows.iter().all(|r| !matches!(&r.kind, OrchestrationKind::PresetBound { .. })));
    }

    /// 게이트 ④(보강) — 스케일 3 티켓의 방화벽: 재생 경로 Approved 거부.
    #[test]
    fn scale3_ticket_firewall_holds() {
        let d = tmpdir("fw");
        let orch = OrchestrationLog::open(&d.join("orch.db")).unwrap();
        orch.declare_plan(
            "plan-sizing",
            vec![("sz-f".into(), vec![])],
            vec![("sz-f".into(), "s-f".into())],
        )
        .unwrap();
        let rt = StreamRuntime::new(orch);
        rt.add_stream(Stream::new("s-f", &d).unwrap()).unwrap();
        rt.bind_approval_ledger(
            foundry_approval::ApprovalLedger::open(&d.join("ap.db")).unwrap(),
        )
        .unwrap();
        rt.dispatch("sz-f", mkorder("sz-f", true)).unwrap();
        let e = rt.stream("s-f").unwrap();
        let mut eng = e.engine_if_unfenced().unwrap();
        eng.acquire("sz-f", "fw").unwrap();
        for ev in [
            OrderEvent::Queued,
            OrderEvent::DieStarted,
            OrderEvent::DieSucceeded { attempt: 1 },
            OrderEvent::InspectionPassed,
        ] {
            let att = eng.order("sz-f").unwrap().attempt().max(1);
            let rows = foundry_ledger::SqliteLedger::open(&e.journal()).unwrap().read_all().unwrap();
            let ctx = crate::test_support::replay_ctx("sz-f", &rows);
            let ro: &'static mut foundry_ledger::SqliteLedger = Box::leak(Box::new(
                foundry_ledger::SqliteLedger::open(&e.journal()).unwrap(),
            ));
            eng.dispatch_replay_ctx("sz-f", "fw", &ev, att, &ctx, ro).unwrap();
        }
        // 게이트 도달 — 재생 Approved 거부.
        assert_eq!(eng.order("sz-f").unwrap().state(), OrderState::FurnaceHold);
        let rows = foundry_ledger::SqliteLedger::open(&e.journal()).unwrap().read_all().unwrap();
        let ctx = crate::test_support::replay_ctx("sz-f", &rows);
        let ro: &'static mut foundry_ledger::SqliteLedger = Box::leak(Box::new(
            foundry_ledger::SqliteLedger::open(&e.journal()).unwrap(),
        ));
        let fw = eng.dispatch_replay_ctx(
            "sz-f",
            "fw",
            &OrderEvent::Approved { intent: "ship".into(), artifact_digest: "d".into() },
            1,
            &ctx,
            ro,
        );
        assert!(
            matches!(fw, Err(foundry_workflow::EngineError::AuthorizationRefused { .. })),
            "firewall holds at scale 3"
        );
        let _ = BTreeSet::<String>::new();
    }

}
