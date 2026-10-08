//! Casting orchestration — T26 (M3). Wires T22-T25 into the CLI flow:
//!
//!   run     — base_sha_gate -> classify corpus -> execute Dry Die in
//!             sandbox per task -> candidate corpus + casting journal
//!   judge   — verify_binding -> compare -> JudgeInput (coverage from
//!             T28 classes, regressions, serious failures) -> verdict
//!   promote — verify_binding + domain transition with ApprovalToken
//!             (the ONLY door; T22)
use crate::breaker::{CastingBreaker, Scheduling};
use crate::corpus::CorpusManifest;
use crate::dry_die::{DryDie, Sandbox};
use crate::judging::{self, JudgeInput, JudgingPolicy};
use crate::repro::{self, ReplayClass};
use foundry_domain::ApprovalToken;
use foundry_domain::casting::{CastingRun, CastingState, Verdict};
use foundry_domain::ports::Harness as _;
use std::path::Path;

pub struct CastingOutcome {
    pub run: CastingRun,
    pub executed: usize,
    pub skipped_breaker: usize,
    pub non_replayable: usize,
}

/// Full dry-casting run. `corpus_dir` holds baseline manifests;
/// `sandbox_root` gets per-order isolated workspaces; results go to
/// `candidate_dir` (same manifest schema — comparable by T20).
pub async fn run_casting(
    mut run: CastingRun,
    corpus_dir: &Path,
    sandbox_root: &Path,
    candidate_dir: &Path,
    journal: &foundry_ledger::casting_journal::CastingLedger,
) -> Result<CastingOutcome, String> {
    // gate: base SHA (review #1) — refuse before any execution
    let corpus = CorpusManifest::load_all(corpus_dir).map_err(|e| e.to_string())?;
    let base = corpus.first().and_then(|c| c.base_git_sha.clone());
    let head = current_head()?;
    repro::base_sha_gate(base.as_deref(), &head)?;

    // lifecycle: draft -> running (journal FIRST, then work)
    run.transition(CastingState::Running, None)
        .map_err(|e| e.to_string())?;
    journal
        .append(
            &run.id,
            &CastingState::Draft,
            &CastingState::Running,
            "run-start",
        )
        .map_err(|e| e.to_string())?;

    std::fs::create_dir_all(candidate_dir).map_err(|e| e.to_string())?;
    let mut breaker = CastingBreaker::new(3);
    let mut executed = 0;
    let mut skipped = 0;
    let mut non_replayable = 0;

    for m in &corpus {
        // T28: classify — non-replayable tasks are recorded, never run
        match repro::classify(m) {
            ReplayClass::NonReplayable => {
                non_replayable += 1;
                continue;
            }
            // dry casting: both AtSha and RequiresSnapshot execute via the
            // deterministic DryDie (snapshots = the frozen seed/clock env)
            _ => {}
        }
        // T25: breaker check before scheduling
        if let Scheduling::Blocked { .. } = breaker.check("dep-bump", &run.id) {
            skipped += 1;
            continue;
        }
        // execute candidate in sandbox (T23)
        let profile = crate::dry_die::CandidateProfile {
            id: run.candidate_profile_id.clone(),
            model_id: "mock/deterministic".into(),
            prompt_template: "dep-bump-v1".into(),
            tool_allowlist: vec!["cargo".into()],
            env_allowlist: vec!["PATH".into()],
            seed: run.seed,
        };
        let die = DryDie::new(profile, sandbox_root);
        let order = foundry_domain::Order::new(
            m.order_id.clone(),
            foundry_domain::ExternalRef::cron("casting", "casting"),
            chrono::Utc::now(),
        );
        match die.run(&order, 1, Path::new("wd")).await {
            Ok(out) => {
                if out.exit_code == 0
                    && !out.result_commit.is_empty()
                    && out.result_commit != out.base_commit
                {
                    breaker.record_success("dep-bump", &run.id);
                    executed += 1;
                    // candidate manifest — same schema (comparable)
                    let mut cm = m.clone();
                    cm.attempts = 1;
                    cm.event_schema_version = 2;
                    cm.save(candidate_dir).map_err(|e| e.to_string())?;
                } else {
                    if let Scheduling::Blocked { .. } = breaker.record_failure(
                        "dep-bump",
                        &run.id,
                        &run.candidate_profile_id,
                        "no-op/failed dry die",
                        "t",
                    ) {
                        skipped += 1;
                    }
                }
            }
            Err(e) => {
                if let Scheduling::Blocked { .. } = breaker.record_failure(
                    "dep-bump",
                    &run.id,
                    &run.candidate_profile_id,
                    &format!("die error: {e:?}"),
                    "t",
                ) {
                    skipped += 1;
                }
            }
        }
    }

    Ok(CastingOutcome {
        run,
        executed,
        skipped_breaker: skipped,
        non_replayable,
    })
}

/// Judge: verify binding, compare corpora, apply the policy.
pub fn judge_casting(
    run: &CastingRun,
    baseline_dir: &Path,
    candidate_dir: &Path,
    policy: &JudgingPolicy,
    skipped_by_breaker: usize,
) -> Result<(Verdict, Vec<String>), String> {
    // review #7: policy hash bound at run time must still match
    if judging::policy_hash(policy) != run.policy_hash {
        return Err(format!(
            "judging policy changed since run start (bound {:.12}.., current {:.12}..) — refusing to judge",
            run.policy_hash,
            judging::policy_hash(policy)
        ));
    }
    run.verify_binding()?;
    let baseline = CorpusManifest::load_all(baseline_dir).map_err(|e| e.to_string())?;
    let candidate = CorpusManifest::load_all(candidate_dir).map_err(|e| e.to_string())?;
    let report =
        crate::benchmark::compare(&candidate, baseline_dir).map_err(|e| format!("{e:?}"))?;

    // coverage: comparable = executed candidate manifests / baseline total
    let comparable = candidate.len() as f64 / baseline.len().max(1) as f64;
    let _ = skipped_by_breaker; // already excluded from candidate corpus

    // regressions: baseline-shipped orders that candidate failed
    let shipped_ids: std::collections::HashSet<&str> = baseline
        .iter()
        .filter(|c| c.terminal_state == "shipped")
        .map(|c| c.order_id.as_str())
        .collect();
    let regressions = candidate
        .iter()
        .filter(|c| shipped_ids.contains(c.order_id.as_str()) && c.terminal_state == "failed")
        .count();

    // new serious failure classes: candidate has failures baseline didn't
    let base_failed = baseline
        .iter()
        .filter(|c| c.terminal_state == "failed")
        .count();
    let cand_failed = candidate
        .iter()
        .filter(|c| c.terminal_state == "failed")
        .count();
    let new_classes = if cand_failed > base_failed && regressions > 0 {
        vec!["shipped-to-failed regression".to_string()]
    } else {
        vec![]
    };

    let input = JudgeInput {
        report,
        comparable_coverage: comparable,
        new_serious_failure_classes: new_classes,
        shipped_to_failed_regressions: regressions,
    };
    let outcome = judging::judge(policy, &input);
    Ok((outcome.verdict, outcome.reasons))
}

/// Promote: the ONLY door is a human ApprovalToken (T22 domain).
pub fn promote_casting(
    run: &mut CastingRun,
    verdict: Verdict,
    token: Option<&ApprovalToken>,
    journal: &foundry_ledger::casting_journal::CastingLedger,
) -> Result<(), String> {
    run.verify_binding()?;
    run.transition(CastingState::Judged { verdict }, None)
        .map_err(|e| e.to_string())?;
    journal
        .append(
            &run.id,
            &CastingState::Running,
            &CastingState::Judged { verdict },
            "judged",
        )
        .map_err(|e| e.to_string())?;
    // the door: refuses without a token, refuses Inconclusive (domain)
    run.transition(CastingState::Promoted, token)
        .map_err(|e| format!("PROMOTION REFUSED: {e}"))?;
    journal
        .append(
            &run.id,
            &CastingState::Judged { verdict },
            &CastingState::Promoted,
            "promoted",
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn current_head() -> Result<String, String> {
    std::fs::read_to_string(".git/HEAD")
        .ok()
        .and_then(|h| {
            let h = h.trim();
            if let Some(r) = h.strip_prefix("ref: ") {
                std::fs::read_to_string(format!(".git/{r}")).ok()
            } else {
                Some(h.to_string())
            }
        })
        .map(|s| s.trim().to_string())
        .filter(|s| s.len() >= 7)
        .ok_or_else(|| "cannot read repo HEAD".to_string())
}

/// Sandbox for the whole casting run (per-order subdirs inside).
pub fn run_sandbox(
    root: &Path,
    profile: &crate::dry_die::CandidateProfile,
) -> Result<Sandbox, String> {
    Sandbox::new(root, profile).map_err(|e| e.to_string())
}
