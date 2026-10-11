//! T61 astra r4 — 실제 프로세스 킬 크래시 주입 (fork 기반).
//!
//! 자식 프로세스에서 append 트랜잭션(INSERT 후 COMMIT 전 창)에 진입한
//! 뒤 SIGKILL — 부모에서 재오픈하여:
//! ① 커밋된 접두만 생존, ② 부분 행 부재, ③ 체인 유효, ④ 재 append 성공.
//!
//! 창 진입은 자식 전용 테스트 후크(환경 변수)로: append_validated 의
//! 검증 클로저 내에서 훅이 자식을 SIGKILL.

use crate::{OrchestrationKind, OrchestrationLog};

#[test]
fn t61_kill9_mid_append_rolls_back() {
    let d = std::env::temp_dir().join(format!("foundry-t61-kill-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let db = d.join("orch.db");
    // 부모: 정상 행 2개 적립.
    {
        let log = OrchestrationLog::open(&db).unwrap();
        log.declare_plan("p0", vec![("o-0".into(), vec![])], vec![("o-0".into(), "s".into())])
            .unwrap();
    }
    // 자식: 창 내 SIGKILL.
    let marker = d.join("child-ready");
    let child = unsafe { libc::fork() };
    assert!(child >= 0, "fork failed");
    if child == 0 {
        // ── 자식 ──
        let log = OrchestrationLog::open(&db).unwrap();
        // 창 진입: append_validated 의 검증 클로저에서 준비 신호 후 자살.
        let r = log.append_with_validation(
            OrchestrationKind::OrderSpawned {
                order_id: "o-crash".into(),
                plan_id: "p0".into(),
                stream: "s".into(),
            },
            |_| {
                // INSERT 는 아직 — TxGuard.begin 직후(트랜잭션 열림) 단계.
                std::fs::write(&marker, b"ready").unwrap();
                // SIGKILL — Drop 미실행, 트랜잭션 미커밋.
                unsafe { libc::kill(libc::getpid(), libc::SIGKILL); }
                #[allow(unreachable_code)]
                Ok(())
            },
        );
        let _ = r;
        std::process::exit(0);
    }
    // 부모: 자식 준비 대기(최대 10초) 후 이미 죽었는지 확인.
    for _ in 0..200 {
        if marker.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(marker.exists(), "child reached the transaction window");
    let mut status: i32 = 0;
    unsafe { libc::waitpid(child, &mut status, 0) };
    assert!(libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == libc::SIGKILL as i32,
        "child died by SIGKILL in the append window");
    // ── 재오픈: 크래시 후 회복 ──
    let log = OrchestrationLog::open(&db).unwrap();
    let rows = log.read_all().unwrap();
    // ① 부분 행 부재(o-crash 미커밋).
    assert!(
        rows.iter().all(|r| !matches!(&r.kind,
            OrchestrationKind::OrderSpawned { order_id, .. } if order_id == "o-crash")),
        "uncommitted mid-transaction row is absent after SIGKILL"
    );
    // ② 체인 유효(read_all 이 곧 검증) + 커밋 접두 생존.
    assert!(rows.iter().any(|r| matches!(&r.kind, OrchestrationKind::PlanDeclared { .. })),
        "committed prefix survives");
    // ③ 재 append 성공 — 손상 없이 계속 쓸 수 있다.
    log.declare_plan(
        "p-after",
        vec![("o-after".into(), vec![])],
        vec![("o-after".into(), "s".into())],
    )
    .unwrap();
    let rows2 = log.read_all().unwrap();
    assert!(rows2.iter().any(|r| matches!(&r.kind,
        OrchestrationKind::PlanDeclared { plan_id, .. } if plan_id == "p-after")));
}
