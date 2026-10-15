//! CastingRun domain — the A/B adoption lifecycle (M3 T22, plan v2).
//!
//! A CastingRun re-executes a frozen corpus under a CANDIDATE profile
//! (model/prompt/tool-policy) and produces a candidate corpus for
//! judgement. Reviews #1/#14: the run is BOUND to everything the
//! comparison depends on via `input_digest` — profile hash, corpus
//! hash, policy hash, metric version, seed, base git SHA, tool
//! versions. judge/promote RE-VERIFY the binding and refuse on drift.
//!
//! Promotion is enforced IN THE DOMAIN (review #13): the
//! `promoted` terminal state is reachable ONLY through
//! `transition_promote(approval: &ApprovalToken)` — no event, no
//! replay, no CLI path can produce it without a human-issued token.
use crate::ports::ApprovalToken;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CastingState {
    Draft,
    Running,
    Judged {
        verdict: Verdict,
    },
    /// terminal — human approved the promotion
    Promoted,
    /// terminal — rejected or aborted
    Rejected {
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    Promote,
    Reject,
    /// incomparable (coverage gaps, breaker drain) — NOT promotable by
    /// default (review #6: inconclusive must not be a promotion bypass).
    Inconclusive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CastingRun {
    pub id: String,
    pub candidate_profile_id: String,
    pub candidate_profile_hash: String,
    pub corpus_ref: String,
    pub corpus_hash: String,
    pub policy_hash: String,
    pub metric_version: u32,
    pub seed: u64,
    pub base_git_sha: String,
    pub tool_versions: Vec<(String, String)>,
    /// combined binding hash over ALL of the above (review #14)
    pub input_digest: String,
    pub state: CastingState,
}

impl CastingRun {
    /// Everything the fairness of the comparison depends on, digested.
    pub fn compute_input_digest(
        candidate_profile_hash: &str,
        corpus_hash: &str,
        policy_hash: &str,
        metric_version: u32,
        seed: u64,
        base_git_sha: &str,
        tool_versions: &[(String, String)],
    ) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"casting-run-input-v1\n");
        h.update(candidate_profile_hash.as_bytes());
        h.update(b"\n");
        h.update(corpus_hash.as_bytes());
        h.update(b"\n");
        h.update(policy_hash.as_bytes());
        h.update(b"\n");
        h.update(metric_version.to_le_bytes());
        h.update(seed.to_le_bytes());
        h.update(base_git_sha.as_bytes());
        h.update(b"\n");
        for (name, ver) in tool_versions {
            h.update(name.as_bytes());
            h.update(b"=");
            h.update(ver.as_bytes());
            h.update(b";");
        }
        hex(&h.finalize())
    }

    /// Verify the run is still bound to what it started with.
    /// judge()/promote() MUST call this first and refuse on mismatch
    /// (reviews #1/#11: base SHA / policy drift between phases).
    pub fn verify_binding(&self) -> Result<(), String> {
        let d = Self::compute_input_digest(
            &self.candidate_profile_hash,
            &self.corpus_hash,
            &self.policy_hash,
            self.metric_version,
            self.seed,
            &self.base_git_sha,
            &self.tool_versions,
        );
        if d == self.input_digest {
            Ok(())
        } else {
            Err(format!(
                "input_digest drift: bound {:.12}.., recomputed {:.12}.. — \
                 comparison inputs changed after run start",
                self.input_digest, d
            ))
        }
    }

    /// Legal lifecycle transitions. `promoted` has exactly ONE door:
    /// an ApprovalToken (human). Everything else is refuse.
    pub fn transition(
        &mut self,
        next: CastingState,
        approval: Option<&ApprovalToken>,
    ) -> Result<(), String> {
        use CastingState::*;
        let ok = match (&self.state, &next) {
            (Draft, Running) | (Draft, Rejected { .. }) => true,
            (Running, Judged { .. }) | (Running, Rejected { .. }) => true,
            (
                Judged {
                    verdict: Verdict::Promote,
                },
                Promoted,
            ) => {
                // the ONLY path to Promoted, and only with a token
                approval.is_some()
            }
            _ => false,
        };
        if !ok {
            return Err(format!(
                "illegal casting transition {:?} -> {:?} (approval={})",
                self.state,
                next,
                approval.is_some()
            ));
        }
        // Inconclusive can never be promoted (review #6)
        if let Judged {
            verdict: Verdict::Inconclusive,
        } = &self.state
            && matches!(next, Promoted)
        {
            return Err("inconclusive runs are not promotable".into());
        }

        self.state = next;
        Ok(())
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{ActorId, ActorKind};

    fn token() -> ApprovalToken {
        ApprovalToken {
            order_id: "cast-1".into(),
            intent: "promote candidate after 3-axis review".into(),
            artifact_digest: "d".into(),
            actor: ActorId {
                kind: ActorKind::Human,
                source: "test".into(),
                id: "user".into(),
            },
            granted_at: chrono::Utc::now(),
        }
    }

    fn run() -> CastingRun {
        let d = CastingRun::compute_input_digest(
            "p",
            "c",
            "pol",
            1,
            7,
            "sha1",
            &[("die".into(), "1.0".into())],
        );
        CastingRun {
            id: "cast-1".into(),
            candidate_profile_id: "prof-a".into(),
            candidate_profile_hash: "p".into(),
            corpus_ref: "corpus".into(),
            corpus_hash: "c".into(),
            policy_hash: "pol".into(),
            metric_version: 1,
            seed: 7,
            base_git_sha: "sha1".into(),
            tool_versions: vec![("die".into(), "1.0".into())],
            input_digest: d,
            state: CastingState::Draft,
        }
    }

    #[test]
    fn digest_binds_all_inputs() {
        let r = run();
        assert!(r.verify_binding().is_ok());
        // any single input change breaks the binding
        let mut r2 = r.clone();
        r2.base_git_sha = "sha2".into();
        assert!(r2.verify_binding().is_err());
        let mut r3 = r.clone();
        r3.policy_hash = "pol2".into();
        assert!(r3.verify_binding().is_err());
    }

    #[test]
    fn promote_requires_human_token() {
        let mut r = run();
        r.transition(CastingState::Running, None).unwrap();
        r.transition(
            CastingState::Judged {
                verdict: Verdict::Promote,
            },
            None,
        )
        .unwrap();
        // no token -> refused
        assert!(r.transition(CastingState::Promoted, None).is_err());
        // with token -> the only door opens
        assert!(r.transition(CastingState::Promoted, Some(&token())).is_ok());
    }

    #[test]
    fn inconclusive_not_promotable_even_with_token() {
        let mut r = run();
        r.transition(CastingState::Running, None).unwrap();
        r.transition(
            CastingState::Judged {
                verdict: Verdict::Inconclusive,
            },
            None,
        )
        .unwrap();
        assert!(
            r.transition(CastingState::Promoted, Some(&token()))
                .is_err()
        );
    }

    #[test]
    fn no_skip_paths() {
        let mut r = run();
        // Draft -> Promoted directly: illegal
        assert!(
            r.transition(CastingState::Promoted, Some(&token()))
                .is_err()
        );
        // Judged(Promote) -> Running: no backwards
        r.transition(CastingState::Running, None).unwrap();
        r.transition(
            CastingState::Judged {
                verdict: Verdict::Promote,
            },
            None,
        )
        .unwrap();
        assert!(r.transition(CastingState::Running, None).is_err());
    }
}
