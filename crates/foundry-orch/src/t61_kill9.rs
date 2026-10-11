//! T61 astra r4/r5 — 실제 프로세스 킬 크래시 주입 (fork 기반).
//!
//! 자식 프로세스에서 append 가 **INSERT 후 COMMIT 전 창**(테스트 전용
//! 훅 — cfg(test))에 진입한 순간 SIGKILL. 부모에서 재오픈하여:
//! ① 커밋된 접두만 생존, ② 창 내 미커밋 행 부재, ③ 체인 유효(read_all),
//! ④ 재 append 성공.

use crate::{OrchestrationKind, OrchestrationLog};

#[test]
fn t61_kill9_mid_append_rolls_back() {
    let d = std::env::temp_dir().join(format!("foundry-t61-kill-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let db = d.join("orch.db");
    // 부모: 정상 행 적립.
    {
        let log = OrchestrationLog::open(&db).unwrap();
        log.declare_plan("p0", vec![("o-0".into(), vec![])], vec![("o-0".into(), "s".into())])
            .unwrap();
    }
    // 자식: 훅 설정 후 append — INSERT 직후 SIGKILL. 부모는 플래그 클리어.
    crate::TEST_CRASH_NOW.store(false, std::sync::atomic::Ordering::SeqCst);
    let child = unsafe { libc::fork() };
    assert!(child >= 0, "fork failed");
    if child == 0 {
        // (astra r6) atomic 플래그 — fork 자식에서 락 없이 안전.
        crate::TEST_CRASH_NOW.store(true, std::sync::atomic::Ordering::SeqCst);
        let log = OrchestrationLog::open(&db).unwrap();
        let _ = log.declare_plan(
            "p-crash",
            vec![("o-crash".into(), vec![])],
            vec![("o-crash".into(), "s".into())],
        );
        std::process::exit(0);
    }
    let mut status: i32 = 0;
    unsafe { libc::waitpid(child, &mut status, 0) };
    assert!(
        libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == libc::SIGKILL as i32,
        "child died by SIGKILL inside the post-INSERT pre-COMMIT window"
    );
    // ── 재오픈: 크래시 후 회복 ──
    let log = OrchestrationLog::open(&db).unwrap();
    let rows = log.read_all().unwrap();
    // ① 창 내 미커밋 행 부재.
    assert!(
        rows.iter().all(|r| !matches!(&r.kind,
            OrchestrationKind::PlanDeclared { plan_id, .. } if plan_id == "p-crash")),
        "uncommitted mid-transaction row is absent after SIGKILL"
    );
    // ② 커밋 접두 생존 + 체인 유효(read_all 이 곧 검증).
    assert!(
        rows.iter().any(|r| matches!(&r.kind, OrchestrationKind::PlanDeclared { plan_id, .. } if plan_id == "p0")),
        "committed prefix survives"
    );
    // ③ 재 append 성공.
    log.declare_plan(
        "p-after",
        vec![("o-after".into(), vec![])],
        vec![("o-after".into(), "s".into())],
    )
    .unwrap();
    assert!(log.read_all().unwrap().iter().any(|r| matches!(&r.kind,
        OrchestrationKind::PlanDeclared { plan_id, .. } if plan_id == "p-after")));
}
