//! Publish Bridge S2 — M8 라이브 경로의 PR 발행·Shipped 확정 실주행.
//!
//! 설계 docs/design/publish-bridge.md v1.6 (r6 APPROVED).
//! 사이클: 이슈 시드 → Die 세그먼트(실 rustain → foundary-demo 커밋) →
//! 인간 게이트 그랜트 → **실PR 발행(GithubPublisher 무변경 재사용)** →
//! Shipped 확정 → 알림(관측 플레인) → 종결 증거.
//!
//! 실행:
//!   새 런:      cargo run -p foundry --example m8_publish -- --issue 141
//!   재개:       cargo run -p foundry --example m8_publish -- --resume <run-dir>
//!   (선택)      --notify-mode console|discord|all
//!
//! v1.6 계약 요점: workdir은 공식 빌더 경로(/tmp/foundry-die-<order>-1),
//! 재실행은 런 접미사(o-141-r2)로 새 주문, 불확실 후 재시도는 원격
//! 재판정 선행(SHA·base·이력), 머지는 인간 소유.

use std::path::{Path, PathBuf};

use foundry_domain::order::{ExternalRef, Order, Source};
use foundry_domain::workdir::workdir_path;
use foundry_domain::{OrderState, PublishOutcome};
use foundry_orch::OrchestrationLog;
use foundry_orch::stream::{Stream, StreamRuntime};

// ─── run.json — ExternalRef 영속화 (v1.6 r3-MAJOR-2) ─────────────

#[derive(Debug, Clone)]
struct RunMeta {
    run_dir: PathBuf,
    issue: u64,
    order_id: String,
    run_suffix: String,
}

impl RunMeta {
    /// 새 런 — rN 접미사로 주문 식별자 분리(D4: non-fast-forward·GC 회피).
    fn new(issue: u64) -> Self {
        let seq = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let run_suffix = format!("r{seq}");
        let order_id = format!("o-{issue}-{run_suffix}");
        let run_dir = scratch_dir().join(format!("m8pub-{issue}-{run_suffix}"));
        Self {
            run_dir,
            issue,
            order_id,
            run_suffix,
        }
    }

    fn save(&self) {
        std::fs::create_dir_all(&self.run_dir).unwrap();
        // ExternalRef 직렬화 — replay가 복원 못 하므로(r3-MAJOR-2) 런
        // 메타데이터가 원본을 소유. 재개 시 퍼블리셔 스냅샷에 복원.
        let body = format!(
            "issue={}\norder_id={}\nrun_suffix={}\ndedup_key=lunarpulse/foundary-demo#{}:{}\nsource=Intake\n",
            self.issue, self.order_id, self.run_suffix, self.issue, self.run_suffix
        );
        std::fs::write(self.run_dir.join("run.json"), body).unwrap();
    }

    /// 재개 — 기존 run.json에서 식별자 복원.
    fn load(run_dir: &Path) -> Result<Self, String> {
        let body = std::fs::read_to_string(run_dir.join("run.json"))
            .map_err(|e| format!("run.json: {e}"))?;
        let mut get = |k: &str| -> Result<String, String> {
            body.lines()
                .find_map(|l| l.strip_prefix(&format!("{k}=")))
                .map(|v| v.to_string())
                .ok_or(format!("run.json missing {k}"))
        };
        Ok(Self {
            run_dir: run_dir.to_path_buf(),
            issue: get("issue")?.parse().map_err(|_| "issue parse")?,
            order_id: get("order_id")?,
            run_suffix: get("run_suffix")?,
        })
    }

    fn external_ref(&self) -> ExternalRef {
        ExternalRef {
            source: Source::Intake,
            external_id: format!("lunarpulse/foundary-demo#{}", self.issue),
            dedup_key: format!(
                "lunarpulse/foundary-demo#{}:{}",
                self.issue, self.run_suffix
            ),
        }
    }
}

fn scratch_dir() -> PathBuf {
    std::env::temp_dir().join("foundry-m8pub")
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/home/cosmo".into()))
}

// ─── 메인 ────────────────────────────────────────────────────────

fn main() {
    let mut issue: Option<u64> = None;
    let mut resume: Option<PathBuf> = None;
    let mut notify_mode = "console".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--issue" => issue = args.next().and_then(|v| v.parse().ok()),
            "--resume" => resume = args.next().map(PathBuf::from),
            "--notify-mode" => notify_mode = args.next().unwrap_or_default(),
            _ => {}
        }
    }

    let meta = match (&issue, &resume) {
        (Some(i), None) => {
            println!("══ M8 PUBLISH BRIDGE — 새 런 (issue #{i}) ══");
            let m = RunMeta::new(*i);
            m.save();
            m
        }
        (None, Some(d)) => {
            println!("══ M8 PUBLISH BRIDGE — 재개 (v1.6 D2-R2) ══");
            RunMeta::load(d).unwrap_or_else(|e| {
                eprintln!("재개 실패: {e}");
                std::process::exit(4);
            })
        }
        _ => {
            eprintln!(
                "사용법: --issue <n> | --resume <run-dir> [--notify-mode console|discord|all]"
            );
            std::process::exit(64);
        }
    };

    println!(
        "order={} run={} dir={}",
        meta.order_id,
        meta.run_suffix,
        meta.run_dir.display()
    );
    let ap_db = meta.run_dir.join("ap.db");
    let orch_db = meta.run_dir.join("orch.db");
    let wd = workdir_path(&meta.order_id, 1); // 공식 빌더 경로 (D1)
    println!("Die workdir(공식): {}", wd.display());

    // ── 셋업 ────────────────────────────────────────────────────
    let rt = StreamRuntime::new(OrchestrationLog::open(&orch_db).unwrap());
    let orch = rt.orch.clone();
    orch.declare_plan(
        "plan-live",
        vec![(meta.order_id.clone(), vec![])],
        vec![(meta.order_id.clone(), "s-live".into())],
    )
    .unwrap();
    rt.add_stream(Stream::new_with_root("s-live", &meta.run_dir, "bmad-quickloop").unwrap())
        .unwrap();
    rt.bind_approval_ledger(foundry_approval::ApprovalLedger::open(&ap_db).unwrap())
        .unwrap();

    // ── 주문 입장 (새 런만 — 재개는 건너뜀) ──────────────────────
    let fresh = issue.is_some();
    if fresh {
        let mut o = Order::new(
            meta.order_id.clone(),
            meta.external_ref(),
            chrono::Utc::now(),
        );
        o.force_state_for_test(OrderState::Received, 1);
        rt.dispatch(&meta.order_id, o).unwrap();
        println!(
            "[1/8] 주문 입장 (Intake, dedup #{}:{})",
            meta.issue, meta.run_suffix
        );

        // ── 실 Die 세그먼트 ─────────────────────────────────────
        let st = rt
            .start_die_segment("s-live", &meta.order_id, "m8pub")
            .unwrap();
        println!("[2/8] Die 세그먼트 시작: {st:?}");
        std::fs::create_dir_all(&wd).unwrap();
        let harness = foundry_harness_cli::CliHarness::new(
            home().join(".foundry/bin/foundry-die"),
            "coding",
            900,
        );
        let order = rt
            .stream("s-live")
            .unwrap()
            .order_snapshot(&meta.order_id)
            .unwrap();
        let die = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                use foundry_domain::ports::Harness as _;
                harness.run(&order, 1, &wd).await
            });
        match die {
            Ok(out) if out.exit_code == 0 => {
                println!(
                    "       Die 완료: {}",
                    &out.result_commit[..12.min(out.result_commit.len())]
                );
                let st = rt
                    .finish_die_segment("s-live", &meta.order_id, "m8pub", &out)
                    .unwrap();
                println!("[3/8] 세그먼트 종결: {st:?}");
            }
            Ok(out) => {
                eprintln!(
                    "[3/8] Die 거부: exit={} — 헤드룸 없음 또는 태스크 파싱 실패",
                    out.exit_code
                );
                std::process::exit(3);
            }
            Err(e) => {
                eprintln!("[3/8] Die 실패: {e:?}");
                std::process::exit(2);
            }
        }

        // ── 인간 게이트 ────────────────────────────────────────
        let digest = rt
            .stream("s-live")
            .unwrap()
            .order_snapshot(&meta.order_id)
            .unwrap()
            .artifact_digest
            .unwrap_or_default();
        let grant = format!("ship-{}", meta.order_id);
        rt.approve_at_gate("s-live", &meta.order_id, "m8pub", &grant, "ship", &digest)
            .unwrap();
        println!(
            "[4/8] 그랜트 {grant} CONSUMED — digest={}",
            &digest[..12.min(digest.len())]
        );
    }

    // ── 발행 (publish_precheck → publisher → confirm_published) ─
    let material = rt.publish_precheck("s-live", &meta.order_id).unwrap();
    println!(
        "[5/8] 발행 precheck 통과: op_id={} attempt={}",
        material.op_id, material.attempt
    );

    // 주문 스냅샷 — ExternalRef는 run.json에서 복원(r3-MAJOR-2:
    // replay 미복원으로 Closes #N 누락 방지).
    let mut order = rt
        .stream("s-live")
        .unwrap()
        .order_snapshot(&meta.order_id)
        .unwrap();
    order.external = meta.external_ref();

    let publisher = foundry_watch::github_publisher::GithubPublisher {
        owner: "lunarpulse".into(),
        repo: "foundary-demo".into(),
        token_path: home().join(".foundry/gh_token"),
        base: "main".into(),
    };
    use foundry_domain::ports::Publisher as _;
    let outcome = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            publisher
                .publish(&order, &material.op_id, &material.digest)
                .await
        });
    let pr_url = match outcome {
        Ok(PublishOutcome::Confirmed { pr_url }) if pr_url.starts_with("https://") => pr_url,
        Ok(PublishOutcome::Confirmed { pr_url }) => {
            // r4-MAJOR: 빈/무효 URL Confirmed — 보존 후 수동 재판정.
            eprintln!("[5/8] 퍼블리셔가 무효 URL({pr_url:?})을 반환 — 보존 후 재개로 재판정");
            std::process::exit(4);
        }
        Ok(other) => {
            eprintln!(
                "[5/8] 발행 결과 불확실({other:?}) — 보존: --resume {}",
                meta.run_dir.display()
            );
            std::process::exit(4);
        }
        Err(e) => {
            eprintln!(
                "[5/8] 발행 실패({e:?}) — 보존: --resume {}",
                meta.run_dir.display()
            );
            std::process::exit(4);
        }
    };
    println!("       PR 생성: {pr_url}");

    // ── Shipped 확정 ───────────────────────────────────────────
    let st = rt
        .confirm_published("s-live", &meta.order_id, "m8pub", &pr_url)
        .unwrap();
    println!("[6/8] Shipped 확정: {st:?}");

    // ── 알림 (관측 플레인 — 실패해도 진행) ──────────────────────
    let mut notifier: Box<dyn foundry_domain::ports::Notifier> = match notify_mode.as_str() {
        "discord" => Box::new(foundry_watch::discord_notifier::DiscordNotifier::from_env()),
        "all" => Box::new(foundry_watch::discord_notifier::FanOutNotifier(vec![
            Box::new(foundry_watch::discord_notifier::DiscordNotifier::from_env()),
            Box::new(foundry_watch::discord_notifier::ConsoleNotifier),
        ])),
        _ => Box::new(foundry_watch::discord_notifier::ConsoleNotifier),
    };
    if let Err(e) = notifier.notify(foundry_domain::ports::NotifyEvent::Shipped {
        order_id: meta.order_id.clone(),
        pr_url: pr_url.clone(),
    }) {
        eprintln!("[7/8] 알림 실패(무시 — 관측 플레인): {e:?}");
    } else {
        println!("[7/8] 알림 전송({notify_mode})");
    }
    notifier.drain();

    // ── 종결 증거 ──────────────────────────────────────────────
    let rows = rt.stream("s-live").unwrap().journal_rows().unwrap();
    let shipped_row = rows
        .iter()
        .find(|r| {
            r.order_id == meta.order_id
                && r.event_json
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                    .is_some_and(|v| {
                        v.get("type").and_then(|t| t.as_str()) == Some("publish_confirmed")
                    })
        })
        .or_else(|| {
            rows.iter()
                .find(|r| r.order_id == meta.order_id && r.to_state == "Shipped")
        })
        .expect("Shipped journal row");
    println!(
        "[8/8] 증거: 저널 {}행, Shipped 행 seq={} pr_url 포함={}",
        rows.len(),
        shipped_row.seq,
        shipped_row
            .event_json
            .as_deref()
            .unwrap_or("")
            .contains(&pr_url),
    );
    println!("run-dir(보존): {}", meta.run_dir.display());
    println!("머지는 인간 리뷰 소유 — PR에서 확인하세요.");
}
