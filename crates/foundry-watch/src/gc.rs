//! Die workdir GC — plan T10 (M0 defect #6 root fix).
//!
//! rustain's `cargo check` builds a ~215MB target dir inside the workdir;
//! /tmp is a 3.9GB tmpfs, so ~18 completed Die runs fill it (measured
//! 2026-10-07: mid-run ENOSPC killed 10 publishes). Policy:
//! - Before deleting anything, PROMOTE minimal reproduction evidence to
//!   `<archive>/<order>/` (review #4: GC must never erase what the ledger
//!   alone cannot reproduce — logs, patch, stderr tail, disk snapshot).
//! - Keep the LATEST attempt dir of every order for debugging; delete
//!   older attempts after evidence promotion.
//! - Terminal Failed/Rejected orders keep their last attempt for
//!   `failed_keep_ticks` sweeps (default 3) before final GC.
//! - Disk guard: if /tmp usage exceeds `guard_pct` (default 85), callers
//!   should hold off starting new Dies (better slow than dead — M0 lesson).
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct GcConfig {
    /// Archive root for promoted evidence.
    pub archive_root: PathBuf,
    /// Failed/Rejected orders keep their last workdir this many sweeps.
    pub failed_keep_ticks: u32,
    /// /tmp usage % above which new Dies should be held (0-100).
    pub guard_pct: u32,
}

impl Default for GcConfig {
    fn default() -> Self {
        Self {
            archive_root: dirs_home().join(".foundry/archive"),
            failed_keep_ticks: 3,
            guard_pct: 85,
        }
    }
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

/// Status of one workdir the sweeper found.
#[derive(Debug, PartialEq)]
pub enum SweepAction {
    /// Deleted after promoting evidence to the archive.
    Collected { order: String, attempt: u32 },
    /// Kept: it is the order's latest attempt.
    KeptLatest { order: String, attempt: u32 },
    /// Kept: terminal-failed order within failed_keep_ticks.
    KeptFailedGrace { order: String, attempt: u32 },
}

/// /tmp filesystem usage in percent, if statfs is available.
#[cfg(target_os = "linux")]
pub fn tmp_usage_pct() -> Option<u32> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(foundry_domain::WORKDIR_ROOT.as_bytes()).ok()?;
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statfs(c_path.as_ptr(), &mut st) };
    if rc != 0 {
        return None;
    }
    let total = st.f_blocks.max(1) as u64;
    let free = st.f_bfree as u64;
    let used = total.saturating_sub(free);
    Some(((used * 100) / total) as u32)
}

#[cfg(not(target_os = "linux"))]
pub fn tmp_usage_pct() -> Option<u32> {
    None
}

/// True when the disk guard says: do not start new Dies right now.
pub fn guard_tripped(cfg: &GcConfig) -> bool {
    tmp_usage_pct().map(|p| p >= cfg.guard_pct).unwrap_or(false)
}

/// Promote minimal reproduction evidence from a workdir into the archive.
/// Returns the archive dir if anything was promoted.
pub fn promote_evidence(
    workdir: &Path,
    order: &str,
    attempt: u32,
    cfg: &GcConfig,
) -> std::io::Result<Option<PathBuf>> {
    let dest = cfg
        .archive_root
        .join(format!("{order}"))
        .join(format!("attempt-{attempt}"));
    let repo = workdir.join("repo");
    let mut promoted = false;

    // attempt logs: attempt-*.log at workdir root
    if let Ok(entries) = fs::read_dir(workdir) {
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("attempt-") && name.ends_with(".log") {
                fs::create_dir_all(&dest)?;
                let _ = fs::copy(e.path(), dest.join(name.as_ref()));
                promoted = true;
            }
        }
    }
    // patch of the Die result vs its base commit (what the Die actually changed)
    if repo.is_dir() {
        let patch = run_git(&repo, &["diff", "HEAD~1", "--stat", "--patch"]);
        if let Some(p) = patch {
            fs::create_dir_all(&dest)?;
            fs::write(dest.join("result.patch"), p)?;
            promoted = true;
        }
    }
    // disk snapshot — the failure context that killed M0 run 3
    if let Some(pct) = tmp_usage_pct() {
        fs::create_dir_all(&dest)?;
        fs::write(dest.join("disk.txt"), format!("tmp_usage_pct={pct}\n"))?;
        promoted = true;
    }
    Ok(promoted.then_some(dest))
}

fn run_git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
        .filter(|s| !s.trim().is_empty())
}

/// One GC sweep over a given root (WORKDIR_ROOT in production).
///
/// `terminal_failed` lists orders currently in a terminal failure state
/// (Failed/Rejected) — their latest attempt is kept for a grace period.
/// Simplicity over cleverness: we scan the dir, group by order, keep
/// the max attempt per order, and delete the rest after evidence promotion.
pub fn sweep(
    cfg: &GcConfig,
    terminal_failed: &[String],
    tick: u32,
    now_ms: u64,
) -> std::io::Result<Vec<SweepAction>> {
    sweep_with_root(
        cfg,
        terminal_failed,
        tick,
        now_ms,
        Path::new(foundry_domain::WORKDIR_ROOT),
    )
}

pub fn sweep_with_root(
    cfg: &GcConfig,
    terminal_failed: &[String],
    _tick: u32,
    now_ms: u64,
    root: &Path,
) -> std::io::Result<Vec<SweepAction>> {
    let mut orders: std::collections::HashMap<String, Vec<(u32, PathBuf, u64)>> =
        std::collections::HashMap::new();
    for e in fs::read_dir(root)?.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        let Some(rest) = name.strip_prefix("foundry-die-") else {
            continue;
        };
        // order ids contain '-' and ':' themselves; split on the LAST '-<n>'
        let Some((order, att)) = rest.rsplit_once('-') else {
            continue;
        };
        let Ok(attempt) = att.parse::<u32>() else {
            continue;
        };
        let mtime = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        orders
            .entry(order.to_string())
            .or_default()
            .push((attempt, e.path(), mtime));
    }

    let mut actions = Vec::new();
    for (order, mut attempts) in orders {
        attempts.sort_by_key(|(a, _, _)| *a);
        let (latest_att, latest_path, latest_mtime) = {
            let (a, p, m) = attempts.last().unwrap();
            (*a, p.clone(), *m)
        };
        let is_failed = terminal_failed.iter().any(|o| o == &order);

        // grace: failed orders keep latest until failed_keep_ticks sweeps
        // have passed since this sweep first saw them (deadline marker file).
        let marker = latest_path.join(".foundry-gc-grace");
        let in_grace = if is_failed {
            if let Ok(deadline) = fs::read_to_string(&marker) {
                now_ms < deadline.trim().parse::<u64>().unwrap_or(0)
            } else {
                // first sweep seeing this failed order: grant grace
                let sweep_interval_ms: u64 = std::env::var("FOUNDRY_GC_INTERVAL_MS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(60_000);
                let deadline = now_ms + cfg.failed_keep_ticks as u64 * sweep_interval_ms;
                let _ = fs::write(&marker, deadline.to_string());
                true
            }
        } else {
            false
        };

        // delete older attempts (after promotion), keep the latest
        for (att, path, _) in attempts[..attempts.len() - 1].iter() {
            let _ = promote_evidence(path, &order, *att, cfg);
            let _ = fs::remove_dir_all(path);
            actions.push(SweepAction::Collected {
                order: order.clone(),
                attempt: *att,
            });
        }
        if in_grace {
            actions.push(SweepAction::KeptFailedGrace {
                order,
                attempt: latest_att,
            });
        } else if is_failed {
            // grace expired: promote + collect the last one too
            let _ = promote_evidence(&latest_path, &order, latest_att, cfg);
            let _ = fs::remove_dir_all(&latest_path);
            actions.push(SweepAction::Collected {
                order,
                attempt: latest_att,
            });
        } else {
            let _ = latest_mtime;
            actions.push(SweepAction::KeptLatest {
                order,
                attempt: latest_att,
            });
        }
    }
    Ok(actions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(order: &str, attempt: u32) -> PathBuf {
        let d = workdir_test_root().join(format!("foundry-die-{order}-{attempt}"));
        fs::create_dir_all(d.join("repo")).unwrap();
        fs::write(d.join(format!("attempt-{attempt}.log")), "die log line\n").unwrap();
        d
    }

    fn workdir_test_root() -> PathBuf {
        // redirect WORKDIR_ROOT for tests via env — see sweep_root override below
        PathBuf::from(std::env::var("FOUNDRY_TEST_WORKDIR_ROOT").unwrap_or_else(|_| "/tmp".into()))
    }

    #[test]
    fn older_attempts_collected_latest_kept() {
        let root = std::env::temp_dir().join(format!("gc-t-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let cfg = GcConfig {
            archive_root: root.join("archive"),
            ..Default::default()
        };
        for att in [1u32, 2] {
            let d = root.join(format!("foundry-die-o-x-{att}"));
            fs::create_dir_all(d.join("repo")).unwrap();
            fs::write(d.join(format!("attempt-{att}.log")), "die log\n").unwrap();
        }
        let actions = sweep_with_root(&cfg, &[], 1, 0, &root).unwrap();
        assert!(actions.contains(&SweepAction::Collected {
            order: "o-x".into(),
            attempt: 1
        }));
        assert!(actions.contains(&SweepAction::KeptLatest {
            order: "o-x".into(),
            attempt: 2
        }));
        assert!(!root.join("foundry-die-o-x-1").exists());
        assert!(root.join("foundry-die-o-x-2").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn failed_evidence_survives_gc() {
        let root = std::env::temp_dir().join(format!("gc-f-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let cfg = GcConfig {
            archive_root: root.join("archive"),
            failed_keep_ticks: 0,
            ..Default::default()
        };
        let d = root.join("foundry-die-o-fail-1");
        fs::create_dir_all(d.join("repo")).unwrap();
        fs::write(d.join("attempt-1.log"), "boom: reason\n").unwrap();
        // grace expired (deadline 0)
        fs::write(d.join(".foundry-gc-grace"), "0").unwrap();
        let actions = sweep_with_root(&cfg, &["o-fail".into()], 1, 1_000_000, &root).unwrap();
        assert!(actions.contains(&SweepAction::Collected {
            order: "o-fail".into(),
            attempt: 1
        }));
        let ev = root.join("archive/o-fail/attempt-1/attempt-1.log");
        assert!(
            ev.exists(),
            "failed evidence must survive GC: {}",
            ev.display()
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn grace_keeps_failed_workdir_for_a_while() {
        let root = std::env::temp_dir().join(format!("gc-g-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let cfg = GcConfig {
            archive_root: root.join("archive"),
            failed_keep_ticks: 3,
            ..Default::default()
        };
        let d = root.join("foundry-die-o-g-1");
        fs::create_dir_all(&d).unwrap();
        // first sweep grants grace (deadline in future relative to now_ms=1000)
        let actions = sweep_with_root(&cfg, &["o-g".into()], 1, 1_000, &root).unwrap();
        assert_eq!(
            actions,
            vec![SweepAction::KeptFailedGrace {
                order: "o-g".into(),
                attempt: 1
            }]
        );
        assert!(d.exists(), "grace must keep the failed workdir");
        let _ = fs::remove_dir_all(&root);
    }
}
