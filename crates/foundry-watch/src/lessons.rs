//! Lesson provenance sealing & revocation semantics — M5 T45.
//!
//! C-L1 (lesson eligibility): a lesson is human review feedback sealed
//! to an APPROVAL TRANSITION — the comment that becomes a lesson must
//! (a) be authored by a human (bot output is not feedback),
//! (b) be unedited (revision 1 — edited history is not what was
//!     approved),
//! (c) be unwithdrawn,
//! (d) reference the exact PR head SHA that the approval transition
//!     was granted against (a post-approval push voids the binding —
//!     "승인 전이 SHA ↔ 발행 head 정확 대응 필수"),
//! (e) carry no holdout-manifest references (evaluator contamination
//!     is adversarial input, T48a),
//! (f) carry no credential-shaped content (M4 privacy pipeline).
//!
//! C-V1 (validity != immutability): a sealed lesson may be REVOKED
//! without rewriting it. Revocation's effects are explicit:
//!   - future selection: excluded from eligible sets,
//!   - pending candidates whose bundle referenced it: ineligible for
//!     promotion (bundles stay immutable, eligibility changes),
//!   - unconsumed promotion tokens bound to such bundles: invalidated,
//!   - active deployments: FLAGGED for human re-evaluation, never
//!     auto-rolled-back.
//!
//! Sealing emits T49a governance events (LessonSeal / LessonRevoke
//! intent+effect), so the whole lesson lifecycle rides the crash-safe
//! commit protocol.
use crate::governance_events::{GovernanceEvent, OpKind, Phase};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRef {
    pub order_id: String,
    pub journal_seq: i64,
    pub approved_head_sha: String,
    pub approver: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SealError {
    BotAuthor { comment_id: u64 },
    EditedComment { comment_id: u64, revisions: u32 },
    WithdrawnComment { comment_id: u64 },
    UnboundApproval { comment_id: u64 },
    HeadShaDrift { comment_id: u64, sealed: String, current: String },
    HoldoutReference { comment_id: u64, digest: String },
    PrivacyLeak { comment_id: u64 },
    AlreadySealed { lesson_id: String },
    UnknownApproval { order_id: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LessonSealInput {
    pub lesson_id: String,
    pub comment_id: u64,
    pub comment_revisions: u32,
    pub withdrawn: bool,
    pub content_hash: String,
    pub content: String,
    pub author_account: String,
    pub author_is_bot: bool,
    pub pr_head_sha: String,
    pub approval: ApprovalRef,
    pub holdout_digests: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LessonState {
    Active,
    Revoked { reason: String, at: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LessonRecord {
    pub input: LessonSealInput,
    pub state: LessonState,
}

/// Effect matrix of one revocation (C-V1) — computed, not hand-waved.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RevocationEffects {
    pub excluded_from_selection: Vec<String>,
    pub candidates_made_ineligible: Vec<String>,
    pub promotion_tokens_invalidated: Vec<String>,
    pub active_deployments_flagged: Vec<String>,
}

/// Which bundles reference which lessons (the reverse index the
/// effect computation needs; in production derived from T46 bundles).
pub trait BundleIndex {
    /// candidate id -> lessons its (immutable) bundle references
    fn lessons_of_candidate(&self, candidate: &str) -> Vec<String>;
    /// promotion token -> the bundle digest / candidate it is bound to
    fn token_binding(&self, token: &str) -> Option<String>;
    /// currently-active (promoted & deployed) candidate ids
    fn active_candidates(&self) -> Vec<String>;
}

#[derive(Default)]
pub struct LessonStore {
    lessons: BTreeMap<String, LessonRecord>,
    pub governance_journal: Vec<GovernanceEvent>,
    seq: i64,
}

const CREDENTIAL_SHAPES: [&str; 6] = ["sk-", "ghp_", "gho_", "xoxb-", "Bearer ", "PRIVATE KEY"];

impl LessonStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn gov(&mut self, op: OpKind, phase: Phase, op_id: &str, payload: &str) -> GovernanceEvent {
        self.seq += 1;
        GovernanceEvent {
            schema_version: 1,
            seq: self.seq,
            op,
            phase,
            op_id: op_id.to_string(),
            payload_digest: payload.to_string(),
            actor: "lesson-pipeline".into(),
            at: "2026-10-08T00:00:00Z".into(),
        }
    }

    /// Seal a lesson. `current_head` is the PR's head NOW (the seal
    /// moment); it must equal the approval transition's head.
    pub fn seal(&mut self, input: LessonSealInput, current_head: &str) -> Result<(), SealError> {
        if self.lessons.contains_key(&input.lesson_id) {
            return Err(SealError::AlreadySealed { lesson_id: input.lesson_id });
        }
        // (a) human authorship — bot output is machine output, not
        // human feedback; a bot spoofing a human account is exactly
        // why the flag is trusted only from the connector layer
        if input.author_is_bot {
            return Err(SealError::BotAuthor { comment_id: input.comment_id });
        }
        // (b) unedited
        if input.comment_revisions != 1 {
            return Err(SealError::EditedComment { comment_id: input.comment_id, revisions: input.comment_revisions });
        }
        // (c) unwithdrawn
        if input.withdrawn {
            return Err(SealError::WithdrawnComment { comment_id: input.comment_id });
        }
        // (d) approval-transition binding: head drift voids the seal
        if input.pr_head_sha != input.approval.approved_head_sha || input.pr_head_sha != current_head {
            return Err(SealError::HeadShaDrift {
                comment_id: input.comment_id,
                sealed: input.approval.approved_head_sha.clone(),
                current: current_head.to_string(),
            });
        }
        // (e) holdout contamination is adversarial input
        for d in &input.holdout_digests {
            if input.content.contains(d.as_str()) {
                return Err(SealError::HoldoutReference { comment_id: input.comment_id, digest: d.clone() });
            }
        }
        // (f) privacy screen
        if CREDENTIAL_SHAPES.iter().any(|p| input.content.contains(p)) {
            return Err(SealError::PrivacyLeak { comment_id: input.comment_id });
        }

        // governance ride-along: intent + effect with the SAME digest.
        // op_id namespaces the OPERATION INSTANCE: sealing and later
        // revoking the same lesson are two distinct op instances
        // (T49a: one op_id = one intent/effect pair).
        let seal_op = format!("seal:{}", input.lesson_id);
        let e1 = self.gov(OpKind::LessonSeal, Phase::Pending, &seal_op, &input.content_hash);
        let e2 = self.gov(OpKind::LessonSeal, Phase::Committed, &seal_op, &input.content_hash);
        self.governance_journal.push(e1);
        self.governance_journal.push(e2);

        self.lessons.insert(
            input.lesson_id.clone(),
            LessonRecord { input, state: LessonState::Active },
        );
        Ok(())
    }

    /// Eligible lessons for candidate synthesis (Active only).
    pub fn eligible(&self) -> Vec<&LessonRecord> {
        self.lessons
            .values()
            .filter(|l| l.state == LessonState::Active)
            .collect()
    }

    /// Revoke a lesson — C-V1: validity change, not a rewrite. The
    /// record stays byte-identical; only its state flips, and the
    /// effect matrix is COMPUTED against the bundle index.
    pub fn revoke(
        &mut self,
        lesson_id: &str,
        reason: &str,
        bundles: &dyn BundleIndex,
        known_candidates: &[String],
        known_tokens: &[String],
    ) -> Result<RevocationEffects, String> {
        let content_hash = {
            let rec = self
                .lessons
                .get(lesson_id)
                .ok_or_else(|| format!("unknown lesson {lesson_id}"))?;
            if let LessonState::Revoked { .. } = rec.state {
                return Err(format!("lesson {lesson_id} already revoked"));
            }
            rec.input.content_hash.clone()
        };
        let revoke_op = format!("revoke:{lesson_id}");
        let e1 = self.gov(OpKind::LessonRevoke, Phase::Pending, &revoke_op, &content_hash);
        let e2 = self.gov(OpKind::LessonRevoke, Phase::Committed, &revoke_op, &content_hash);
        self.governance_journal.push(e1);
        self.governance_journal.push(e2);
        // state flip AFTER the governance pair (intent precedes effect)
        let rec = self.lessons.get_mut(lesson_id).unwrap();
        rec.state = LessonState::Revoked { reason: reason.into(), at: "2026-10-08T00:00:00Z".into() };

        // effect matrix over the bundle index
        let mut fx = RevocationEffects { excluded_from_selection: vec![lesson_id.to_string()], ..Default::default() };
        let active = bundles.active_candidates();
        for cand in known_candidates {
            if bundles.lessons_of_candidate(cand).iter().any(|l| l == lesson_id) {
                if active.contains(cand) {
                    // already live: flag for human re-evaluation, never auto-rollback
                    fx.active_deployments_flagged.push(cand.clone());
                } else {
                    fx.candidates_made_ineligible.push(cand.clone());
                }
            }
        }
        for t in known_tokens {
            if let Some(binding) = bundles.token_binding(t) {
                if bundles.lessons_of_candidate(&binding).iter().any(|l| l == lesson_id) {
                    fx.promotion_tokens_invalidated.push(t.clone());
                }
            }
        }
        Ok(fx)
    }

    /// A candidate may only be promoted if every lesson its bundle
    /// references is still Active — the promotion gate consults this.
    pub fn promotion_eligible(&self, lessons_referenced: &[String]) -> Result<(), String> {
        for l in lessons_referenced {
            match self.lessons.get(l) {
                None => return Err(format!("candidate references unknown lesson {l} — mutable lesson references are not interpretable in production (M5 #15)")),
                Some(r) if r.state == LessonState::Active => {}
                Some(r) => return Err(format!("candidate references revoked lesson {l}: {:?}", r.state)),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FlatIndex {
        bundles: BTreeMap<String, Vec<String>>,
        tokens: BTreeMap<String, String>,
        active: Vec<String>,
    }
    impl BundleIndex for FlatIndex {
        fn lessons_of_candidate(&self, c: &str) -> Vec<String> {
            self.bundles.get(c).cloned().unwrap_or_default()
        }
        fn token_binding(&self, t: &str) -> Option<String> {
            self.tokens.get(t).cloned()
        }
        fn active_candidates(&self) -> Vec<String> {
            self.active.clone()
        }
    }

    fn input(lesson: &str) -> LessonSealInput {
        LessonSealInput {
            lesson_id: lesson.into(),
            comment_id: 9001,
            comment_revisions: 1,
            withdrawn: false,
            content_hash: "h-abc".into(),
            content: "prefer map_err over unwrap in intake paths".into(),
            author_account: "human-reviewer".into(),
            author_is_bot: false,
            pr_head_sha: "sha-1".into(),
            approval: ApprovalRef {
                order_id: "o-137".into(),
                journal_seq: 42,
                approved_head_sha: "sha-1".into(),
                approver: "owner".into(),
            },
            holdout_digests: BTreeSet::new(),
        }
    }

    #[test]
    fn happy_seal_and_eligibility() {
        let mut s = LessonStore::new();
        s.seal(input("L1"), "sha-1").unwrap();
        assert_eq!(s.eligible().len(), 1);
        assert!(s.promotion_eligible(&["L1".into()]).is_ok());
    }

    #[test]
    fn bot_authorship_refused() {
        let mut s = LessonStore::new();
        let mut i = input("L2");
        i.author_is_bot = true;
        assert!(matches!(s.seal(i, "sha-1"), Err(SealError::BotAuthor { .. })));
    }

    #[test]
    fn edited_comment_refused() {
        let mut s = LessonStore::new();
        let mut i = input("L3");
        i.comment_revisions = 2; // the comment was edited after the fact
        assert!(matches!(s.seal(i, "sha-1"), Err(SealError::EditedComment { revisions: 2, .. })));
    }

    #[test]
    fn withdrawn_comment_refused() {
        let mut s = LessonStore::new();
        let mut i = input("L4");
        i.withdrawn = true;
        assert!(matches!(s.seal(i, "sha-1"), Err(SealError::WithdrawnComment { .. })));
    }

    #[test]
    fn post_approval_push_voids_binding() {
        let mut s = LessonStore::new();
        // force-push after approval: current head drifted
        assert!(matches!(s.seal(input("L5"), "sha-2"), Err(SealError::HeadShaDrift { .. })));
        // and a seal claiming a different head than the approval is
        // refused even if the CURRENT head matches the claim
        let mut i = input("L5b");
        i.pr_head_sha = "sha-2".into();
        assert!(matches!(s.seal(i, "sha-2"), Err(SealError::HeadShaDrift { .. })));
    }

    #[test]
    fn holdout_reference_is_adversarial_input() {
        let mut s = LessonStore::new();
        let mut i = input("L6");
        i.holdout_digests.insert("m-hold-9".into());
        i.content = "remember the fix pattern from m-hold-9".into();
        assert!(matches!(s.seal(i, "sha-1"), Err(SealError::HoldoutReference { digest, .. }) if digest == "m-hold-9"));
    }

    #[test]
    fn credential_shaped_content_refused() {
        let mut s = LessonStore::new();
        let mut i = input("L7");
        i.content = "use this key instead: sk-CANARY-1234".into();
        assert!(matches!(s.seal(i, "sha-1"), Err(SealError::PrivacyLeak { .. })));
    }

    #[test]
    fn revocation_effects_matrix() {
        let mut s = LessonStore::new();
        s.seal(input("L10"), "sha-1").unwrap();
        s.seal(input("L11"), "sha-1").unwrap();
        let idx = FlatIndex {
            bundles: [
                ("cand-pending".to_string(), vec!["L10".to_string(), "L11".to_string()]),
                ("cand-clean".to_string(), vec!["L11".to_string()]),
                ("cand-live".to_string(), vec!["L10".to_string()]),
            ]
            .into_iter()
            .collect(),
            tokens: [("tok-1".to_string(), "cand-pending".to_string())].into_iter().collect(),
            active: vec!["cand-live".to_string()],
        };
        let fx = s
            .revoke(
                "L10",
                "author requested deletion",
                &idx,
                &["cand-pending".into(), "cand-clean".into(), "cand-live".into()],
                &["tok-1".into()],
            )
            .unwrap();
        assert_eq!(fx.excluded_from_selection, vec!["L10"]);
        assert_eq!(fx.candidates_made_ineligible, vec!["cand-pending"]);
        assert_eq!(fx.promotion_tokens_invalidated, vec!["tok-1"]);
        assert_eq!(fx.active_deployments_flagged, vec!["cand-live"], "live: flagged for human re-eval, NOT auto-rollback");
        // clean candidate untouched
        assert!(!fx.candidates_made_ineligible.contains(&"cand-clean".to_string()));

        // eligibility follows immediately
        assert_eq!(s.eligible().iter().map(|l| l.input.lesson_id.clone()).collect::<Vec<_>>(), vec!["L11"]);
        // promotion gate: pending candidate is now blocked
        assert!(s.promotion_eligible(&["L10".into(), "L11".into()]).is_err());
        assert!(s.promotion_eligible(&["L11".into()]).is_ok());
        // double revoke refused
        assert!(s.revoke("L10", "again", &idx, &[], &[]).is_err());
        // unknown lesson in a bundle: refused outright (mutable refs)
        assert!(s.promotion_eligible(&["L-nope".into()]).is_err());
    }

    #[test]
    fn lesson_lifecycle_rides_the_governance_protocol() {
        let mut s = LessonStore::new();
        s.seal(input("L20"), "sha-1").unwrap();
        let idx = FlatIndex { bundles: BTreeMap::new(), tokens: BTreeMap::new(), active: vec![] };
        s.revoke("L20", "test", &idx, &[], &[]).unwrap();
        // the journal is a valid intent/effect stream (T49a verifier)
        let ok = crate::governance_events::verify_stream(&s.governance_journal);
        assert!(ok.is_ok(), "seal+revoke must verify as paired governance events");
        // and the record itself was never rewritten: state flipped only
        let r = s.lessons.get("L20").unwrap();
        assert_eq!(r.input.content_hash, "h-abc");
        assert!(matches!(r.state, LessonState::Revoked { .. }));
    }
}
