//! Offline candidate synthesis under enforced limits — M5 T47.
//!
//! The seam where the self-improvement loop ASSEMBLES a new candidate:
//! base profile + prompt + ACTIVE lessons (T45) -> immutable bundle
//! (T46). Synthesis is the phase with contamination incentives, so
//! every earlier contract funnels here:
//!
//!   budget   (T44a): synthesis may not run without an admitted
//!                    reservation under the lineage scope; spend
//!                    commits to actual usage; fenced runs cannot
//!                    synthesize.
//!   lessons  (T45): only Active lessons are readable; a revoked
//!                   lesson is invisible to synthesis (not "present
//!                   but filtered").
//!   bundles  (T46): the OUTPUT must satisfy build() rules —
//!                   snapshots not references, honest seed/pin
//!                   declarations, canonical digest.
//!   protocol (T48a): synthesis sees train+dev manifests ONLY; the
//!                    holdout is not addressable. The synthesized
//!                    prompt content is itself screened for holdout
//!                    digests — a lesson whose content smuggles one
//!                    is already refused at seal time, but the
//!                    ASSEMBLED artifact is re-screened (defense in
//!                    depth: the output is what ships).
//!   governance (T49a): CandidateSynthesize rides the intent/effect
//!                    commit protocol — crash mid-synthesis leaves a
//!                    detectable pending intent.
#![allow(clippy::needless_borrows_for_generic_args)] // W6a clippy debt: preserve explicit generic-call borrow boundaries
#![allow(unused_imports)] // W6a clippy debt: retain the existing feature and test configuration import set

use crate::budget_accounting::{Amount, BudgetError, BudgetEvent, BudgetLedger};
use crate::bundles::{BundleProfile, BundleStore, CandidateBundle, DependencyPins, LessonSnapshot};
use crate::evaluation_protocol::{EvaluationProtocol, Partition};
use crate::governance_events::{GovernanceEvent, OpKind, Phase};
use crate::lessons::{LessonState, LessonStore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

#[derive(Debug, Clone)]
pub enum SynthesisError {
    /// the run is fenced (cancelled) or out of budget
    NotAdmitted { run: String, reason: String },
    /// synthesis would read holdout material — refused, logged
    HoldoutNotAddressable { detail: String },
    /// assembled prompt embeds a holdout digest (defense in depth)
    OutputContaminated { digest: String },
    /// the assembled bundle violates build rules (T46 surfaces why)
    BundleRule(String),
    /// spend exceeded the reservation
    SpendExceedsReservation { used: u64, reserved: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesisRequest {
    pub run: String,
    pub lineage: Vec<String>,
    pub candidate_id: String,
    pub base_profile: BundleProfile,
    pub prompt_template: String,
    /// lessons to incorporate (by id) — must be Active
    pub lesson_ids: Vec<String>,
    /// spend quote for admission (points)
    pub quoted_spend: u64,
}

#[derive(Debug, Clone)]
pub struct SynthesisOutcome {
    pub bundle_digest: String,
    pub actual_spend: u64,
}

pub struct Synthesizer<'a> {
    pub lessons: &'a LessonStore,
    pub protocol: &'a EvaluationProtocol,
    pub budgets: &'a mut BudgetLedger,
    pub bundles: &'a mut BundleStore,
    pub governance: &'a mut Vec<GovernanceEvent>,
    gov_seq: i64,
}

impl<'a> Synthesizer<'a> {
    pub fn new(
        lessons: &'a LessonStore,
        protocol: &'a EvaluationProtocol,
        budgets: &'a mut BudgetLedger,
        bundles: &'a mut BundleStore,
        governance: &'a mut Vec<GovernanceEvent>,
    ) -> Self {
        let gov_seq = governance.last().map(|e| e.seq).unwrap_or(0);
        Synthesizer {
            lessons,
            protocol,
            budgets,
            bundles,
            governance,
            gov_seq,
        }
    }

    fn gov(&mut self, op: OpKind, phase: Phase, op_id: &str, payload: &str) {
        self.gov_seq += 1;
        self.governance.push(GovernanceEvent {
            schema_version: 1,
            seq: self.gov_seq,
            op,
            phase,
            op_id: op_id.to_string(),
            payload_digest: payload.to_string(),
            actor: "synthesis".into(),
            at: "2026-10-08T00:00:00Z".into(),
        });
    }

    fn holdout_digests(&self) -> BTreeSet<String> {
        self.protocol
            .partitions
            .iter()
            .filter(|(_, p)| **p == Partition::Holdout)
            .map(|(m, _)| m.clone())
            .collect()
    }

    /// Snapshot the ACTIVE lessons the request names. Revoked or
    /// unknown lessons are an error (fail loud — a silent skip would
    /// let synthesis ship something other than what was requested).
    fn snapshot_lessons(&self, ids: &[String]) -> Result<Vec<LessonSnapshot>, SynthesisError> {
        let mut out = Vec::new();
        for id in ids {
            let rec = self.lessons.store_get(id).ok_or_else(|| {
                SynthesisError::BundleRule(format!(
                    "lesson {id} unknown — mutable references uninterpretable"
                ))
            })?;
            match &rec.state {
                LessonState::Active => {
                    // T46 canonical hash framing: (len:u64 BE, bytes)
                    let content = rec.input.content.as_bytes();
                    let mut hasher = Sha256::new();
                    hasher.update(&(content.len() as u64).to_be_bytes());
                    hasher.update(content);
                    out.push(LessonSnapshot {
                        lesson_id: id.clone(),
                        content_hash: format!("{:x}", hasher.finalize()),
                        content: rec.input.content.clone(),
                    });
                }
                LessonState::Revoked { .. } => {
                    return Err(SynthesisError::BundleRule(format!(
                        "lesson {id} is revoked — synthesis reads Active lessons only"
                    )));
                }
            }
        }
        Ok(out)
    }

    pub fn synthesize(
        &mut self,
        req: &SynthesisRequest,
        pins: DependencyPins,
        actual_spend: u64,
    ) -> Result<SynthesisOutcome, SynthesisError> {
        let op_id = format!("synth:{}", req.candidate_id);
        let payload = {
            let mut hasher = Sha256::new();
            hasher.update(req.prompt_template.as_bytes());
            hasher.update(req.candidate_id.as_bytes());
            hasher.update(&req.quoted_spend.to_be_bytes());
            format!("synth-{:.16x}", hasher.finalize())
        };

        // 1. BUDGET (T44a): admission before any work
        let reserve_id = format!("{}/synth", req.run);
        let mut scope: Vec<String> = req.lineage.clone();
        scope.push(req.run.clone());
        let scope_refs: Vec<&str> = scope.iter().map(|s| s.as_str()).collect();
        self.budgets
            .try_reserve(&reserve_id, &scope_refs, Amount::pts(req.quoted_spend))
            .map_err(|e| SynthesisError::NotAdmitted {
                run: req.run.clone(),
                reason: format!("{e}"),
            })?;

        // governance intent BEFORE the effect exists
        self.gov(
            OpKind::CandidateSynthesize,
            Phase::Pending,
            &op_id,
            &payload,
        );

        // 2. PROTOCOL (T48a): synthesis must not address the holdout.
        //    The request itself carries no manifests — but we prove
        //    non-addressability by refusing any prompt/lesson text
        //    that contains a holdout digest.
        let holdouts = self.holdout_digests();
        for d in &holdouts {
            if req.prompt_template.contains(d.as_str()) {
                let err = SynthesisError::HoldoutNotAddressable {
                    detail: format!("prompt template references holdout manifest {d}"),
                };
                self.gov(
                    OpKind::CandidateSynthesize,
                    Phase::Aborted,
                    &op_id,
                    &payload,
                );
                return Err(err);
            }
        }

        // 3. LESSONS (T45): active-only snapshots
        let snapshots = self.snapshot_lessons(&req.lesson_ids)?;

        // 4. OUTPUT SCREEN (defense in depth): the assembled prompt is
        //    what ships — re-screen the concatenated content
        let assembled = format!(
            "{}{}",
            req.prompt_template,
            snapshots
                .iter()
                .map(|s| s.content.as_str())
                .collect::<String>()
        );
        for d in &holdouts {
            if assembled.contains(d.as_str()) {
                let err = SynthesisError::OutputContaminated { digest: d.clone() };
                self.gov(
                    OpKind::CandidateSynthesize,
                    Phase::Aborted,
                    &op_id,
                    &payload,
                );
                return Err(err);
            }
        }

        // 5. SPEND SETTLEMENT FIRST (T44a): reject overspend BEFORE the
        //    bundle ships — an aborted synthesis must leave no artifact
        if actual_spend > req.quoted_spend {
            let err = SynthesisError::SpendExceedsReservation {
                used: actual_spend,
                reserved: req.quoted_spend,
            };
            self.gov(
                OpKind::CandidateSynthesize,
                Phase::Aborted,
                &op_id,
                &payload,
            );
            return Err(err);
        }

        // 6. BUNDLE (T46): build rules apply — snapshots verified,
        //    honest seed/pin declarations enforced
        let bundle = CandidateBundle::build(
            &req.candidate_id,
            req.base_profile.clone(),
            &req.prompt_template,
            snapshots,
            pins,
        )
        .map_err(|e| {
            self.gov(
                OpKind::CandidateSynthesize,
                Phase::Aborted,
                &op_id,
                &payload,
            );
            SynthesisError::BundleRule(format!("{e:?}"))
        })?;
        let digest = self.bundles.put(bundle);
        self.budgets
            .try_commit(&reserve_id, Amount::pts(actual_spend))
            .expect("commit <= reservation is structurally admitted");

        // governance effect: the candidate exists
        self.gov(
            OpKind::CandidateSynthesize,
            Phase::Committed,
            &op_id,
            &payload,
        );
        Ok(SynthesisOutcome {
            bundle_digest: digest,
            actual_spend,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation_protocol::frozen_protocol_v1;
    use crate::lessons::{ApprovalRef, LessonSealInput};

    fn protocol() -> EvaluationProtocol {
        let mut parts = std::collections::BTreeMap::new();
        parts.insert("m-train-1".to_string(), Partition::Train);
        parts.insert("m-dev-1".to_string(), Partition::Dev);
        parts.insert("m-hold-1".to_string(), Partition::Holdout);
        frozen_protocol_v1(parts)
    }

    fn lessons() -> LessonStore {
        let mut s = LessonStore::new();
        let mk = |id: &str, content: &str| LessonSealInput {
            lesson_id: id.into(),
            comment_id: 1,
            comment_revisions: 1,
            withdrawn: false,
            content_hash: "h".into(),
            content: content.into(),
            author_account: "human".into(),
            author_is_bot: false,
            pr_head_sha: "sha".into(),
            approval: ApprovalRef {
                order_id: "o".into(),
                journal_seq: 1,
                approved_head_sha: "sha".into(),
                approver: "owner".into(),
            },
            holdout_digests: BTreeSet::new(),
        };
        let _ = mk("L1", "prefer map_err over unwrap");
        s.seal(mk("L1", "prefer map_err over unwrap"), "sha")
            .unwrap();
        s.seal(mk("L2", "keep diffs minimal"), "sha").unwrap();
        s
    }

    fn setup() -> (
        LessonStore,
        EvaluationProtocol,
        BudgetLedger,
        BundleStore,
        Vec<GovernanceEvent>,
    ) {
        let mut b = BudgetLedger::new();
        let e = BudgetEvent::SetLimit {
            scope: vec!["factory".into(), "casting".into(), "lineage-s1".into()],
            amount: Amount::pts(500),
        };
        b.apply(e.clone()).unwrap();
        b.journal.push(e);
        (lessons(), protocol(), b, BundleStore::default(), Vec::new())
    }

    fn profile() -> BundleProfile {
        BundleProfile {
            model: "deepseek-v4".into(),
            temperature: Some(0.0),
            seed: Some(7),
            max_output_tokens: 4096,
        }
    }

    fn pins() -> DependencyPins {
        DependencyPins {
            image_digest: "sha256:aa".into(),
            tool_digests: Default::default(),
            model_revision: Some("rev-9".into()),
        }
    }

    fn req() -> SynthesisRequest {
        SynthesisRequest {
            run: "run-s1".into(),
            lineage: vec!["factory".into(), "casting".into(), "lineage-s1".into()],
            candidate_id: "cand-s1".into(),
            base_profile: profile(),
            prompt_template: "solve {{task}} carefully".into(),
            lesson_ids: vec!["L1".into(), "L2".into()],
            quoted_spend: 100,
        }
    }

    #[test]
    fn happy_synthesis_binds_every_layer() {
        let (l, p, mut b, mut bs, mut g) = setup();
        let mut synth = Synthesizer::new(&l, &p, &mut b, &mut bs, &mut g);
        let out = synth.synthesize(&req(), pins(), 60).unwrap();
        // bundle exists, verifiable, carries both snapshots
        let bundle = bs.get_verified(&out.bundle_digest).unwrap();
        assert_eq!(bundle.lessons.len(), 2);
        // budget settled to ACTUALS: 60 of 100 quoted
        assert_eq!(
            b.available(&["factory", "casting", "lineage-s1"]),
            Some(440)
        );
        // governance: pending + committed with identical payload
        let kinds: Vec<_> = g.iter().map(|e| (e.op, e.phase)).collect();
        assert!(kinds.contains(&(OpKind::CandidateSynthesize, Phase::Pending)));
        assert!(kinds.contains(&(OpKind::CandidateSynthesize, Phase::Committed)));
        assert!(crate::governance_events::verify_stream(&g).is_ok());
    }

    #[test]
    fn no_budget_no_synthesis() {
        let (l, p, mut b, mut bs, mut g) = setup();
        // exhaust the lineage first
        b.try_reserve(
            "other/run",
            &["factory", "casting", "lineage-s1", "run-other"],
            Amount::pts(500),
        )
        .unwrap();
        let mut synth = Synthesizer::new(&l, &p, &mut b, &mut bs, &mut g);
        let mut r = req();
        r.run = "run-s2".into();
        assert!(matches!(
            synth.synthesize(&r, pins(), 10),
            Err(SynthesisError::NotAdmitted { .. })
        ));
    }

    #[test]
    fn revoked_lesson_is_unreadable_not_filtered() {
        let (l, p, mut b, mut bs, mut g) = setup();
        // revoke L2 through the store's own path
        struct NoIndex;
        impl crate::lessons::BundleIndex for NoIndex {
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
        let mut l2 = l;
        l2.revoke("L2", "obsolete", &NoIndex, &[], &[]).unwrap();
        let mut synth = Synthesizer::new(&l2, &p, &mut b, &mut bs, &mut g);
        let err = synth.synthesize(&req(), pins(), 10).unwrap_err();
        let detail = format!("{err:?}");
        assert!(
            matches!(err, SynthesisError::BundleRule(m) if m.contains("revoked")),
            "fail loud, not silent skip: {detail}"
        );
    }

    #[test]
    fn holdout_reference_in_prompt_refused() {
        let (l, p, mut b, mut bs, mut g) = setup();
        let mut synth = Synthesizer::new(&l, &p, &mut b, &mut bs, &mut g);
        let mut r = req();
        r.prompt_template = "solve like we did on m-hold-1".into();
        assert!(matches!(
            synth.synthesize(&r, pins(), 10),
            Err(SynthesisError::HoldoutNotAddressable { .. })
        ));
        // governance shows the ABORT (crash-safe: no dangling pending)
        let last = g.last().unwrap();
        assert_eq!(last.phase, Phase::Aborted);
        assert!(crate::governance_events::verify_stream(&g).is_ok());
    }

    #[test]
    fn dishonest_seed_claim_blocked_at_output() {
        let (l, p, mut b, mut bs, mut g) = setup();
        let mut synth = Synthesizer::new(&l, &p, &mut b, &mut bs, &mut g);
        // seed present, no model pin — T46 refuses; synthesis surfaces it
        assert!(matches!(
            synth.synthesize(&req(), DependencyPins { image_digest: "x".into(), tool_digests: Default::default(), model_revision: None }, 10),
            Err(SynthesisError::BundleRule(m)) if m.contains("UnpinnedButDeterministic")
        ));
    }

    #[test]
    fn overspend_refused_and_aborted() {
        let (l, p, mut b, mut bs, mut g) = setup();
        let mut synth = Synthesizer::new(&l, &p, &mut b, &mut bs, &mut g);
        assert!(matches!(
            synth.synthesize(&req(), pins(), 101),
            Err(SynthesisError::SpendExceedsReservation {
                used: 101,
                reserved: 100
            })
        ));
        // reservation still held by the pending op — recovery (T49a)
        // aborts it; nothing shipped
        assert_eq!(bs.by_digest_len(), 0);
        let last = g.last().unwrap();
        assert_eq!(last.phase, Phase::Aborted);
    }
}
