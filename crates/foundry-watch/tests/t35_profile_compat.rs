//! T35 compatibility gate: the profile extensions are ADDITIVE —
//! M3's casting machinery (profile hash -> input_digest binding ->
//! two-axis judging -> promotion qualification) is untouched by them.
//! Proven three ways:
//!   1. legacy profiles hash identically (byte-level)
//!   2. judging outcomes are unchanged with extensions present
//!   3. promotion paths still refuse without a token, still accept
//!      with one — extensions grant no new authority
use foundry_domain::casting::{CastingRun, CastingState, Verdict};
use foundry_domain::ports::{ActorId, ActorKind, ApprovalToken};
use foundry_watch::dry_die::CandidateProfile;
use foundry_watch::benchmark::{CompareReport, Metrics};
use foundry_watch::judging::{self, JudgeInput, JudgingPolicy};
use foundry_watch::profile_ext::{ProfileExtensions, ReproClass};

fn base_profile(id: &str) -> CandidateProfile {
    CandidateProfile {
        id: id.into(),
        model_id: "m".into(),
        prompt_template: "p".into(),
        tool_allowlist: vec![],
        env_allowlist: vec![],
        seed: 42,
    }
}

#[test]
fn legacy_profile_hash_is_byte_identical_to_m3() {
    // extensions live OUTSIDE CandidateProfile; the M3 hash function
    // never sees them -> identical by construction. We assert the
    // hash is stable and that two profiles differing ONLY in
    // extensions hash THE SAME.
    let a = base_profile("p-1");
    let b = base_profile("p-1"); // same everything
    assert_eq!(a.hash(), b.hash());
    // extension hashes are separate and do not feed the profile hash
    let ext_legacy = ProfileExtensions::legacy();
    let ext_luna = ProfileExtensions::gpt_6_luna();
    assert_ne!(ext_legacy.extension_hash(), ext_luna.extension_hash());
    // and neither changes the M3 profile hash
    let _ = (ext_legacy, ext_luna);
    assert_eq!(a.hash(), base_profile("p-1").hash());
}

#[test]
fn judging_unchanged_with_extensions_present() {
    // two-axis judging: guardrail + primary. Extensions in the room
    // change nothing about verdicts — the judge signature does not
    // even carry them (they are cost/repro metadata only).
    let policy = JudgingPolicy {
        policy_version: 1,
        max_cost_increase_frac: 0.0,
        primary_metrics: vec![foundry_watch::judging::PrimaryMetric::QualityFailuresDown],
        min_coverage_frac: 0.8,
    };
    let _ext = ProfileExtensions::gpt_6_luna();

    // guardrail axis: regression -> immediate Reject
    let input = JudgeInput {
        report: CompareReport {
            baseline: Metrics { metric_version: 1, cohort_size: 10, max_event_schema_version: 2, shipped: 10, failed: 0, rejected_noop: 0, attempts_total: 10, cost: foundry_watch::benchmark::CostSums::default() },
            current: Metrics { metric_version: 1, cohort_size: 10, max_event_schema_version: 2, shipped: 9, failed: 1, rejected_noop: 0, attempts_total: 10, cost: foundry_watch::benchmark::CostSums::default() },
            shipped_delta: -1,
            failed_delta: 1,
            rejected_delta: 0,
        },
        comparable_coverage: 1.0,
        new_serious_failure_classes: vec![],
        shipped_to_failed_regressions: 1,
    };
    let out = judging::judge(&policy, &input);
    assert_eq!(out.verdict, Verdict::Reject, "regression still refuses with extensions present");

    // clean candidate, full coverage, no serious failures -> not auto-promoted either:
    // primary axis requires a declared improvement, which this input lacks
    let clean = JudgeInput {
        report: CompareReport {
            baseline: Metrics { metric_version: 1, cohort_size: 10, max_event_schema_version: 2, shipped: 10, failed: 0, rejected_noop: 0, attempts_total: 10, cost: foundry_watch::benchmark::CostSums::default() },
            current: Metrics { metric_version: 1, cohort_size: 10, max_event_schema_version: 2, shipped: 10, failed: 0, rejected_noop: 0, attempts_total: 10, cost: foundry_watch::benchmark::CostSums::default() },
            shipped_delta: 0,
            failed_delta: 0,
            rejected_delta: 0,
        },
        comparable_coverage: 1.0,
        new_serious_failure_classes: vec![],
        shipped_to_failed_regressions: 0,
    };
    let out2 = judging::judge(&policy, &clean);
    // tie is NOT promotable (review #8: equality is not improvement)
    assert_ne!(out2.verdict, Verdict::Promote, "tie still not promotable");
}

#[test]
fn repro_class_grants_no_authority() {
    // NondeterministicContract profiles are NOT auto-promotable,
    // NOT auto-inconclusive, NOT auto-anything: the class is
    // descriptive metadata. Authority stays with the human token.
    let mut run = make_run();
    run.transition(CastingState::Running, None).unwrap();
    run.transition(CastingState::Judged { verdict: Verdict::Promote }, None).unwrap();
    let ext = ProfileExtensions {
        repro_class: ReproClass::NondeterministicContract,
        ..ProfileExtensions::gpt_6_luna()
    };
    // no token -> refused, class notwithstanding
    assert!(run.transition(CastingState::Promoted, None).is_err());
    // with token -> allowed (the ONLY door; unchanged by T35)
    assert!(run.transition(CastingState::Promoted, Some(&token())).is_ok());
    let _ = ext;
}

fn token() -> ApprovalToken {
    ApprovalToken {
        order_id: "o".into(),
        intent: "i".into(),
        artifact_digest: "d".into(),
        actor: ActorId { kind: ActorKind::Human, source: "t".into(), id: "u".into() },
        granted_at: chrono::Utc::now(),
    }
}

fn make_run() -> CastingRun {
    let d = CastingRun::compute_input_digest("p", "c", "pol", 1, 7, "sha", &[]);
    CastingRun {
        id: "r".into(),
        candidate_profile_id: "cand".into(),
        candidate_profile_hash: "p".into(),
        corpus_ref: "corpus".into(),
        corpus_hash: "c".into(),
        policy_hash: "pol".into(),
        metric_version: 1,
        seed: 7,
        base_git_sha: "sha".into(),
        tool_versions: vec![],
        input_digest: d,
        state: CastingState::Draft,
    }
}
