//! T49b — end-to-end audit & crash-recovery verification of the M5
//! governance surface. This is the integration test that proves the
//! five layers are ONE auditable system, not five journals:
//!
//!   full lifecycle: budget admission -> lesson sealing -> synthesis
//!   -> final evaluation -> retro insight -> owner application ->
//!   prediction scoring, ALL riding governance events.
//!
//! Then three adversarial audit properties (astra #17):
//!   A. MERGE: every layer's stream merges into one journal that
//!      verifies under the T49a rules.
//!   B. CRASH: at EVERY prefix of the journal (crash at any instant),
//!      the stream is either clean or has DETECTABLE pending intents
//!      with deterministic recovery — never a silent gap.
//!   C. TAMPER: single-byte payload mutation anywhere is detected.
use std::collections::BTreeMap;

use foundry_watch::budget_accounting::{Amount, BudgetEvent, BudgetLedger};
use foundry_watch::bundles::{BundleProfile, BundleStore, CandidateBundle, DependencyPins};
use foundry_watch::evaluation_protocol::{Partition, frozen_policy_graph, frozen_protocol_v1};
use foundry_watch::governance_events::{GovernanceEvent, OpKind, Phase, recover, verify_stream};
use foundry_watch::integration_eval::{FinalEvaluationInput, IntegrationGate, ItemResult};
use foundry_watch::lessons::{ApprovalRef, BundleIndex, LessonSealInput, LessonStore};
use foundry_watch::policy_apply::{ApplyCommand, PolicyApplicationLayer, Role};
use foundry_watch::retro::{
    EvidenceEnvelope, InsightPipeline, InsightRecord, PolicyDiff, Prediction, QueryCatalog,
};
use foundry_watch::synthesis::{SynthesisRequest, Synthesizer};

// ---- fixtures -------------------------------------------------------------

fn protocol() -> foundry_watch::evaluation_protocol::EvaluationProtocol {
    let mut parts = BTreeMap::new();
    for m in ["m-train-1", "m-train-2", "m-dev-1"] {
        parts.insert(m.to_string(), Partition::Train);
    }
    for m in ["m-hold-1", "m-hold-2", "m-hold-3"] {
        parts.insert(m.to_string(), Partition::Holdout);
    }
    frozen_protocol_v1(parts)
}

fn sealed_lessons() -> LessonStore {
    let mut s = LessonStore::new();
    for (id, content) in [
        ("L1", "prefer map_err over unwrap"),
        ("L2", "keep diffs minimal"),
    ] {
        let _ = s.seal(
            LessonSealInput {
                lesson_id: id.into(),
                comment_id: 1,
                comment_revisions: 1,
                withdrawn: false,
                content_hash: "h".into(),
                content: content.into(),
                author_account: "human-reviewer".into(),
                author_is_bot: false,
                pr_head_sha: "sha-1".into(),
                approval: ApprovalRef {
                    order_id: "o-137".into(),
                    journal_seq: 42,
                    approved_head_sha: "sha-1".into(),
                    approver: "owner".into(),
                },
                holdout_digests: Default::default(),
            },
            "sha-1",
        );
    }
    s
}

struct NoBundles;
impl BundleIndex for NoBundles {
    fn lessons_of_candidate(&self, _: &str) -> Vec<String> {
        vec![]
    }
    fn token_binding(&self, _: &str) -> Option<String> {
        None
    }
    fn active_candidates(&self) -> Vec<String> {
        vec![]
    }
}

fn pins() -> DependencyPins {
    DependencyPins {
        image_digest: "sha256:aa".into(),
        tool_digests: Default::default(),
        model_revision: Some("rev-9".into()),
    }
}

/// Merge layer streams into one journal with contiguous re-sequencing
/// (stable, deterministic order: lessons, synthesis, evaluation, policy).
fn merged_journal(
    lessons: &LessonStore,
    synth: &[GovernanceEvent],
    evals: &[GovernanceEvent],
    policy: &[GovernanceEvent],
) -> Vec<GovernanceEvent> {
    let mut all: Vec<GovernanceEvent> = lessons
        .governance_journal
        .iter()
        .chain(synth)
        .chain(evals)
        .chain(policy)
        .cloned()
        .collect();
    for (i, e) in all.iter_mut().enumerate() {
        e.seq = (i + 1) as i64;
    }
    all
}

/// The full M5 lifecycle, returning the merged journal.
fn run_lifecycle() -> Vec<GovernanceEvent> {
    // 1. budget + lesson sealing
    let mut budgets = BudgetLedger::new();
    let lim = BudgetEvent::SetLimit {
        scope: vec!["factory".into(), "casting".into(), "lineage-e2e".into()],
        amount: Amount::pts(1000),
    };
    budgets.apply(lim.clone()).unwrap();
    budgets.journal.push(lim);
    let lessons = sealed_lessons();

    // 2. synthesis of a candidate from those lessons
    let p = protocol();
    let mut bundles = BundleStore::default();
    let mut synth_journal = Vec::new();
    {
        let mut synth =
            Synthesizer::new(&lessons, &p, &mut budgets, &mut bundles, &mut synth_journal);
        synth
            .synthesize(
                &SynthesisRequest {
                    run: "run-e2e".into(),
                    lineage: vec!["factory".into(), "casting".into(), "lineage-e2e".into()],
                    candidate_id: "cand-e2e".into(),
                    base_profile: BundleProfile {
                        model: "deepseek-v4".into(),
                        temperature: Some(0.0),
                        seed: Some(7),
                        max_output_tokens: 4096,
                    },
                    prompt_template: "solve {{task}}".into(),
                    lesson_ids: vec!["L1".into(), "L2".into()],
                    quoted_spend: 100,
                },
                pins(),
                60,
            )
            .unwrap();
    }

    // 3. final evaluation over the holdout — promotable
    let digest = bundles.by_digest_keys().into_iter().next().unwrap();
    let mut eval_journal = Vec::new();
    {
        let mut gate = IntegrationGate::new(&p, &bundles, &mut eval_journal);
        let out = gate
            .evaluate(&FinalEvaluationInput {
                bundle_digest: digest.clone(),
                items: ["m-hold-1", "m-hold-2", "m-hold-3"]
                    .iter()
                    .map(|m| ItemResult {
                        manifest: (*m).into(),
                        passed: true,
                        output_excerpt: format!("out {m}"),
                    })
                    .collect(),
                new_serious_failure_classes: vec![],
                shipped_to_failed_regressions: 0,
                holdout_coverage: 1.0,
                min_coverage: 0.8,
            })
            .unwrap();
        assert_eq!(
            out.verdict,
            foundry_watch::integration_eval::IntegrationVerdict::Promotable
        );
    }

    // 4. retro: a Supported insight on an Operational target, applied by owner
    let mut pipeline = InsightPipeline::new(QueryCatalog::frozen_v1(), frozen_policy_graph());
    let mut env = EvidenceEnvelope {
        ledger_prefix_len: 157,
        ledger_prefix_head_hash: "abc".into(),
        corpus_snapshot_digest: "c".into(),
        query_id: "retry_rate_by_task_class".into(),
        query_params: BTreeMap::new(),
        cohort: "all orders".into(),
        exclusions: vec![],
        denominator: "attempts".into(),
        missing_data_policy: "excluded both".into(),
        policy_version: 1,
        protocol_digest: "x".into(),
        result_payload: r#"{"dep_bump":0.41}"#.into(),
        result_digest: String::new(),
    };
    env.result_digest = env.compute_result_digest();
    pipeline
        .ingest(InsightRecord {
            insight_id: "I-e2e".into(),
            claim: "dep-bump retries hot".into(),
            evidence: env,
            proposal: Some(PolicyDiff {
                target: "poll_interval".into(),
                changes: [("max_retries".to_string(), "2".to_string())]
                    .into_iter()
                    .collect(),
            }),
            prediction: Some(Prediction {
                metric: "retry_rate_by_task_class".into(),
                direction: "down".into(),
                threshold: "dep_bump < 0.25".into(),
                window: "next-2-retro-windows".into(),
            }),
            n: 20,
        })
        .unwrap();

    let mut layer = PolicyApplicationLayer::new();
    let prop = pipeline.proposals()[0];
    let pd = format!("{}|{}", prop.insight_id, prop.evidence.result_digest); // digest proxy consistent with digests? use simple marker via sha? keep explicit:
    let _ = pd;
    // digest computed exactly as the layer does
    use sha2::{Digest as _, Sha256};
    let mut h = Sha256::new();
    let a = prop.insight_id.as_str();
    let b = prop.evidence.result_digest.as_str();
    for part in [a, b] {
        h.update(&(part.len() as u64).to_be_bytes());
        h.update(part.as_bytes());
    }
    let proposal_digest = format!("pa-{:.16x}", h.finalize());
    let _ = layer.apply(
        &ApplyCommand {
            op_id: "op-e2e".into(),
            approver: "owner-1".into(),
            role: Role::Owner,
            proposal_digest,
            prediction: Prediction {
                metric: "retry_rate_by_task_class".into(),
                direction: "down".into(),
                threshold: "dep_bump < 0.25".into(),
                window: "next-2-retro-windows".into(),
            },
            base_policy_version: 0,
            issued_at: 100,
            expires_at: 9_000,
        },
        &pipeline,
        &frozen_policy_graph(),
        "agent-e2e",
        200,
    );

    // 5. scoring round: window due, hit
    let scored = layer.score_due(800_000, |_| Some(true));
    assert_eq!(scored.len(), 1);
    assert_eq!(layer.proposer_weights()["agent-e2e"], 1.0);

    merged_journal(&lessons, &synth_journal, &eval_journal, &layer.governance)
}

// ---- the audit properties ---------------------------------------------------

#[test]
fn a_full_lifecycle_merges_into_one_valid_journal() {
    let j = run_lifecycle();
    assert!(
        j.len() >= 10,
        "a full lifecycle produces a real journal: {} events",
        j.len()
    );
    let states = verify_stream(&j).expect("the merged M5 journal verifies");
    // nothing left pending at the end of a clean run
    assert!(
        states
            .iter()
            .all(|s| !matches!(s, foundry_watch::governance_events::OpState::Pending { .. })),
        "clean run leaves no pending intents"
    );
    // every op kind the lifecycle touched appears
    let ops: Vec<OpKind> = j.iter().map(|e| e.op).collect();
    for expected in [
        OpKind::LessonSeal,
        OpKind::CandidateSynthesize,
        OpKind::CandidateActivate,
        OpKind::PolicyApplication,
        OpKind::PredictionScore,
    ] {
        assert!(ops.contains(&expected), "missing {expected:?}");
    }
}

#[test]
fn b_crash_at_every_prefix_is_detectable_and_recoverable() {
    let j = run_lifecycle();
    // crash AFTER every event i (prefix = j[..=i]); also the empty crash
    let mut prefixes_checked = 0;
    for cut in 0..=j.len() {
        let prefix = &j[..cut];
        // a crashed system replays what it has; the verifier tells us
        // whether the state is interpretable and what is pending
        match verify_stream(prefix) {
            Ok(states) => {
                // every pending op must have a DETERMINISTIC recovery
                // decision under the frozen recovery policy
                let pending: Vec<&GovernanceEvent> = prefix
                    .iter()
                    .filter(|e| e.phase == Phase::Pending)
                    .filter(|e| {
                        // pending = its op_id never saw an effect
                        !prefix
                            .iter()
                            .any(|f| f.op_id == e.op_id && f.phase != Phase::Pending)
                    })
                    .collect();
                for pe in pending {
                    let r = recover(pe, (prefix.len() + 1) as i64);
                    match pe.op {
                        // human-bound effects NEVER auto-apply
                        OpKind::PolicyApplication
                        | OpKind::CandidateActivate
                        | OpKind::LessonSeal
                        | OpKind::LessonRevoke => {
                            assert_eq!(
                                r.phase,
                                Phase::Aborted,
                                "crash-cut {cut}: {} must AbortAndSurface",
                                pe.op_id
                            );
                        }
                        // idempotent effects roll forward deterministically
                        OpKind::PredictionScore => {
                            assert_eq!(
                                r.phase,
                                Phase::Committed,
                                "crash-cut {cut}: {} rolls forward",
                                pe.op_id
                            );
                        }
                        _ => {}
                    }
                }
                let _ = states;
            }
            Err(errs) => {
                // a prefix crash NEVER produces effect-without-intent or
                // payload mismatch — those are tamper signals, not crash
                // states. Only "trailing pending" is acceptable, which is
                // the Ok arm; any Err here means the stream itself is
                // malformed, which must not happen at ANY cut.
                panic!("crash-cut {cut}: prefix must never be structurally invalid: {errs:?}");
            }
        }
        prefixes_checked += 1;
    }
    assert!(prefixes_checked > 10);
}

#[test]
fn c_single_byte_tamper_anywhere_is_detected() {
    let j = run_lifecycle();
    for i in 0..j.len() {
        let mut t = j.clone();
        // mutate the payload digest's first char (a "same event, edited
        // content" attack); skip events whose payload is too short to
        // mutate meaningfully
        let old = t[i].payload_digest.clone();
        let mut chars: Vec<char> = old.chars().collect();
        if chars.is_empty() {
            continue;
        }
        chars[0] = if chars[0] == 'f' { '0' } else { 'f' };
        t[i].payload_digest = chars.into_iter().collect();

        // the mutated stream must FAIL verification OR surface a state
        // difference; a payload swap on an intent is caught when the
        // effect arrives, on an effect immediately via mismatch...
        let res = verify_stream(&t);
        let differs = match &res {
            Err(_) => true,
            Ok(states) => {
                // even if structurally valid, the recovered state must
                // differ from the honest replay — same-input-same-state
                // is the determinism contract; different input, same
                // state would mean the payload is not load-bearing
                let honest = verify_stream(&j).unwrap();
                honest != *states
            }
        };
        assert!(
            differs,
            "tamper at event {i} (payload) must not verify identically"
        );
    }
    // and dropping an event (the completeness attack) is caught by seq
    let mut dropped = j.clone();
    dropped.remove(3);
    assert!(
        verify_stream(&dropped).is_err(),
        "dropped event detected via seq gap"
    );
}

#[test]
fn d_budget_rehydration_matches_live_state() {
    // the budget layer is its own journal; rehydrate binds identically
    let mut b = BudgetLedger::new();
    let lim = BudgetEvent::SetLimit {
        scope: vec!["factory".into()],
        amount: Amount::pts(100),
    };
    b.apply(lim.clone()).unwrap();
    b.journal.push(lim);
    b.try_reserve("r1/op", &["factory", "run-r1"], Amount::pts(70))
        .unwrap();
    let live = b.available(&["factory"]);
    let re = BudgetLedger::rehydrate(&b.journal).unwrap();
    assert_eq!(
        live,
        re.available(&["factory"]),
        "restart = same reservations"
    );
}
