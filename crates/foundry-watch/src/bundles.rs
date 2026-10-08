//! Immutable content-addressed candidate bundles — M5 T46.
//!
//! The promotion unit of the self-improvement loop. A bundle FREEZES
//! everything that shapes candidate behavior into ONE digest:
//!   profile (model + decoding + params)
//!   prompt template (content-hashed)
//!   lesson SNAPSHOTS (content-addressed copies — NOT live references;
//!     a mutable lesson reference is uninterpretable in production,
//!     M5 review #15)
//!   dependency pins (container image digest, tool binary digests,
//!     model revision when the provider supports it)
//!   repro class (T35: pinning that is impossible is DECLARED, not
//!     claimed — NondeterministicContract says so on the tin)
//!
//! astra #11: consumption-time re-verification — the digest is
//! recomputed at every load; any byte drift = different candidate =
//! re-examination required. Promotion tokens bind to the digest, so
//! a token can never promote "the same name, different contents".
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Content-addressed lesson snapshot: the lesson's material AT SEAL
/// TIME, copied into the bundle. The live LessonStore may revoke the
/// original — the bundle stays byte-identical, only its ELIGIBILITY
/// changes (C-V1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LessonSnapshot {
    pub lesson_id: String,
    pub content_hash: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BundleProfile {
    pub model: String,
    pub temperature: Option<f64>,
    pub seed: Option<u64>,
    pub max_output_tokens: u64,
}

/// Dependency pins. `model_revision` is Option: providers that cannot
/// pin a revision force the repro class to say so — absence of a pin
/// is a DECLARED limit, never a silent gap.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DependencyPins {
    pub image_digest: String,
    pub tool_digests: BTreeMap<String, String>,
    pub model_revision: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReproClass {
    /// byte-identical replay observed (deepseek-style contracts)
    Deterministic,
    /// provider declares same-seed-same-answer but observations vary
    NondeterministicContract,
    /// no decoding control at all
    Undecodable,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BundleError {
    /// a lesson was referenced by ID without a snapshot — mutable
    /// references are the exact hole review #15 closed
    MutableLessonReference { lesson_id: String },
    /// snapshot content does not hash to its declared hash
    SnapshotCorrupt { lesson_id: String, declared: String, computed: String },
    /// digest drift at consumption time — the stored artifact is not
    /// what was reviewed
    DigestDrift { bundle_id: String, stored: String, recomputed: String },
    /// token was issued for a different digest
    TokenDigestMismatch { token: String, bound: String, presented: String },
    /// unpinned provider on a Deterministic claim — the declaration
    /// must be honest (astra #11: declare limits, don't paper over)
    UnpinnedButDeterministic { model: String },
    /// a referenced lesson is revoked at promotion time (T45 gate)
    LessonRevoked { lesson_id: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateBundle {
    pub candidate_id: String,
    pub profile: BundleProfile,
    pub prompt_template: String,
    pub lessons: Vec<LessonSnapshot>,
    pub pins: DependencyPins,
    pub repro_class: ReproClass,
}

fn h(parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for p in parts {
        hasher.update(&(p.len() as u64).to_be_bytes());
        hasher.update(p);
    }
    format!("{:x}", hasher.finalize())
}

impl CandidateBundle {
    /// Canonical serialization: field order fixed by a struct-level
    /// deterministic writer (BTreeMaps everywhere, no HashMaps, no
    /// timestamps inside the digest input).
    fn canonical_bytes(&self) -> Vec<u8> {
        // serde_json with BTreeMap-only content is order-deterministic
        serde_json::to_vec(self).expect("serializable")
    }

    /// The single digest. Every behavior-shaping byte contributes.
    pub fn digest(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"foundry-bundle-v1");
        hasher.update(&self.canonical_bytes());
        format!("bundle-{:x}", hasher.finalize())
    }

    /// Construct, enforcing the snapshot rule: every lesson comes IN,
    /// with content that hashes to its claim. ID-only references are
    /// refused at construction — the hole never opens.
    pub fn build(
        candidate_id: &str,
        profile: BundleProfile,
        prompt_template: &str,
        lessons: Vec<LessonSnapshot>,
        pins: DependencyPins,
    ) -> Result<Self, BundleError> {
        for l in &lessons {
            let computed = h(&[l.content.as_bytes()]);
            if computed != l.content_hash {
                return Err(BundleError::SnapshotCorrupt {
                    lesson_id: l.lesson_id.clone(),
                    declared: l.content_hash.clone(),
                    computed,
                });
            }
        }
        // honesty rule: claiming Deterministic without a pinned model
        // revision is a false declaration
        let repro = profile_pinned_repro(&profile, &pins);
        // honesty rule, stronger form: a SEED claim without a pinned
        // model revision is unenforceable — refusing the build forces
        // the builder to either pin the provider or drop the seed
        // (declare Undecodable). Unenforceable claims never ship.
        if profile.seed.is_some() && pins.model_revision.is_none() {
            return Err(BundleError::UnpinnedButDeterministic { model: profile.model.clone() });
        }
        Ok(CandidateBundle {
            candidate_id: candidate_id.to_string(),
            profile,
            prompt_template: prompt_template.to_string(),
            lessons,
            pins,
            repro_class: repro,
        })
    }
}

fn profile_pinned_repro(profile: &BundleProfile, pins: &DependencyPins) -> ReproClass {
    if pins.model_revision.is_some() && profile.seed.is_some() {
        ReproClass::Deterministic
    } else if profile.seed.is_some() {
        // seed declared but provider unpinnable: the contract is
        // provider-claimed only
        ReproClass::NondeterministicContract
    } else {
        ReproClass::Undecodable
    }
}

/// The bundle store: content-addressed writes, verified reads.
#[derive(Default)]
pub struct BundleStore {
    by_digest: BTreeMap<String, CandidateBundle>,
    /// issued promotion tokens: token -> digest (never candidate_id)
    tokens: BTreeMap<String, String>,
}

impl BundleStore {
    pub fn put(&mut self, b: CandidateBundle) -> String {
        let d = b.digest();
        self.by_digest.insert(d.clone(), b);
        d
    }

    /// Test/ops accessor: number of stored bundles.
    pub fn by_digest_len(&self) -> usize {
        self.by_digest.len()
    }

    /// Consumption-time re-verification (astra #11): recompute from
    /// the stored artifact; drift means the store was tampered with.
    pub fn get_verified(&self, digest: &str) -> Result<&CandidateBundle, BundleError> {
        let b = self
            .by_digest
            .get(digest)
            .ok_or_else(|| BundleError::DigestDrift {
                bundle_id: digest.to_string(),
                stored: digest.to_string(),
                recomputed: "<absent>".into(),
            })?;
        let recomputed = b.digest();
        if recomputed != digest {
            return Err(BundleError::DigestDrift {
                bundle_id: b.candidate_id.clone(),
                stored: digest.to_string(),
                recomputed,
            });
        }
        Ok(b)
    }

    /// Issue a promotion token BOUND TO THE DIGEST.
    pub fn issue_token(&mut self, digest: &str, token: &str) -> Result<(), BundleError> {
        if !self.by_digest.contains_key(digest) {
            return Err(BundleError::DigestDrift {
                bundle_id: digest.into(),
                stored: digest.into(),
                recomputed: "<absent>".into(),
            });
        }
        self.tokens.insert(token.to_string(), digest.to_string());
        Ok(())
    }

    /// Consume a token against a presented digest: the token only
    /// ever promotes exactly what it was issued for.
    pub fn consume_token(&mut self, token: &str, presented_digest: &str) -> Result<(), BundleError> {
        match self.tokens.get(token) {
            None => Err(BundleError::TokenDigestMismatch {
                token: token.into(),
                bound: "<unknown>".into(),
                presented: presented_digest.into(),
            }),
            Some(bound) if bound == presented_digest => {
                self.tokens.remove(token); // single use
                Ok(())
            }
            Some(bound) => Err(BundleError::TokenDigestMismatch {
                token: token.into(),
                bound: bound.clone(),
                presented: presented_digest.into(),
            }),
        }
    }

    /// Promotion eligibility consults the live lesson store (T45):
    /// a bundle whose SNAPSHOTPED lessons have since been revoked is
    /// blocked — immutability of bytes, change of eligibility (C-V1).
    pub fn promotion_check<F>(&self, digest: &str, lesson_active: F) -> Result<(), BundleError>
    where
        F: Fn(&str) -> bool,
    {
        let b = self.get_verified(digest)?;
        for l in &b.lessons {
            if !lesson_active(&l.lesson_id) {
                return Err(BundleError::LessonRevoked { lesson_id: l.lesson_id.clone() });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(id: &str, content: &str) -> LessonSnapshot {
        LessonSnapshot {
            lesson_id: id.into(),
            content_hash: h(&[content.as_bytes()]),
            content: content.into(),
        }
    }

    fn profile() -> BundleProfile {
        BundleProfile { model: "deepseek-v4".into(), temperature: Some(0.0), seed: Some(7), max_output_tokens: 4096 }
    }

    fn pins(revision: Option<&str>) -> DependencyPins {
        DependencyPins {
            image_digest: "sha256:aa".into(),
            tool_digests: [("cargo".to_string(), "sha256:bb".into())].into_iter().collect(),
            model_revision: revision.map(|s| s.into()),
        }
    }

    #[test]
    fn digest_covers_every_behavior_shaping_byte() {
        let a = CandidateBundle::build("cand-1", profile(), "solve {{task}}", vec![snap("L1", "prefer map_err")], pins(Some("rev-9"))).unwrap();
        let a2 = CandidateBundle::build("cand-1", profile(), "solve {{task}}", vec![snap("L1", "prefer map_err")], pins(Some("rev-9"))).unwrap();
        assert_eq!(a.digest(), a2.digest(), "deterministic");

        let mut b = a2.clone();
        b.prompt_template = "solve {{task}} ".into();
        assert_ne!(a.digest(), b.digest(), "prompt edit");

        let c = CandidateBundle::build(
            "cand-1",
            BundleProfile { temperature: Some(0.1), ..profile() },
            "solve {{task}}",
            vec![snap("L1", "prefer map_err")],
            pins(Some("rev-9")),
        )
        .unwrap();
        assert_ne!(a.digest(), c.digest(), "parameter edit");

        let d = CandidateBundle::build("cand-1", profile(), "solve {{task}}", vec![snap("L1", "prefer unwrap")], pins(Some("rev-9"))).unwrap();
        assert_ne!(a.digest(), d.digest(), "lesson content edit");

        let e = CandidateBundle::build("cand-1", profile(), "solve {{task}}", vec![snap("L1", "prefer map_err")], pins(Some("rev-10"))).unwrap();
        assert_ne!(a.digest(), e.digest(), "dependency pin edit");
    }

    #[test]
    fn snapshot_corruption_refused_at_build() {
        let mut bad = snap("L1", "content");
        bad.content_hash = h(&[b"other"]);
        assert!(matches!(
            CandidateBundle::build("c", profile(), "p", vec![bad], pins(Some("r"))),
            Err(BundleError::SnapshotCorrupt { .. })
        ));
    }

    #[test]
    fn mutable_lesson_reference_cannot_be_expressed() {
        // the compile-time form: CandidateBundle.lessons is
        // Vec<LessonSnapshot> — there is no "reference" field to set.
        // Serializing proves no bare lesson-id list leaks in:
        let b = CandidateBundle::build("c", profile(), "p", vec![snap("L1", "x")], pins(Some("r"))).unwrap();
        let j = serde_json::to_string(&b).unwrap();
        assert!(j.contains("content_hash"), "snapshots carry content, not references");
        assert!(serde_json::from_str::<serde_json::Value>(&j).unwrap()["lessons"][0]["content"].is_string());
    }

    #[test]
    fn honest_repro_declarations() {
        // unpinned provider + Deterministic claim = refused
        assert!(matches!(
            CandidateBundle::build("c", profile(), "p", vec![], pins(None)),
            Err(BundleError::UnpinnedButDeterministic { .. })
        ));
        // no decoding control at all: Undecodable
        let mut p2 = profile();
        p2.seed = None;
        let b = CandidateBundle::build("c", p2, "p", vec![], pins(Some("r"))).unwrap();
        assert_eq!(b.repro_class, ReproClass::Undecodable);
        // seed present but no revision pin: the build REFUSES to claim
        // deterministic — the refusal is the honest declaration
        assert!(matches!(
            CandidateBundle::build("c", profile(), "p", vec![], pins(None)),
            Err(BundleError::UnpinnedButDeterministic { .. })
        ));
    }

    #[test]
    fn consumption_time_reverification_catches_drift() {
        let mut store = BundleStore::default();
        let b = CandidateBundle::build("c", profile(), "p", vec![], pins(Some("r"))).unwrap();
        let d = store.put(b);
        assert!(store.get_verified(&d).is_ok());
        // tamper: swap contents under the same digest key
        let tampered = CandidateBundle::build("c", profile(), "p tampered", vec![], pins(Some("r"))).unwrap();
        store.by_digest.insert(d.clone(), tampered);
        assert!(matches!(
            store.get_verified(&d),
            Err(BundleError::DigestDrift { .. })
        ), "byte drift must be caught at load time");
    }

    #[test]
    fn token_binds_to_digest_not_identity() {
        let mut store = BundleStore::default();
        let b = CandidateBundle::build("cand-A", profile(), "p", vec![], pins(Some("r"))).unwrap();
        let d1 = store.put(b);
        // same candidate_id, DIFFERENT contents -> different digest
        let b2 = CandidateBundle::build("cand-A", profile(), "p evolved", vec![], pins(Some("r"))).unwrap();
        let d2 = store.put(b2);
        assert_ne!(d1, d2, "identity is content, not name");

        store.issue_token(&d1, "tok-1").unwrap();
        // the token cannot promote the "same name" bundle with new contents
        assert!(matches!(
            store.consume_token("tok-1", &d2),
            Err(BundleError::TokenDigestMismatch { .. })
        ));
        // it CAN promote exactly what it was issued for
        store.consume_token("tok-1", &d1).unwrap();
        // single use
        assert!(matches!(
            store.consume_token("tok-1", &d1),
            Err(BundleError::TokenDigestMismatch { .. })
        ));
    }

    #[test]
    fn revocation_changes_eligibility_not_bytes() {
        let mut store = BundleStore::default();
        let b = CandidateBundle::build(
            "cand-B",
            profile(),
            "p",
            vec![snap("L9", "lesson nine")],
            pins(Some("r")),
        )
        .unwrap();
        let d = store.put(b);
        // lesson L9 active: promotion OK
        assert!(store.promotion_check(&d, |l| l == "L9").is_ok());
        // revoked: blocked — but the bundle bytes are unchanged
        let err = store.promotion_check(&d, |l| l != "L9").unwrap_err();
        assert!(matches!(err, BundleError::LessonRevoked { lesson_id } if lesson_id == "L9"));
        assert!(store.get_verified(&d).is_ok(), "bytes immutable, eligibility changed (C-V1)");
    }
}
