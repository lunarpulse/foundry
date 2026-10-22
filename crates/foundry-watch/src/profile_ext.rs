//! Candidate profile extensions — M4 T35 (plan v2; reviews #17/#18).
//!
//! M3-prep real-API measurements distilled into profile schema:
//!   1. reasoning_budget — gpt-6-luna spent 56% of output tokens on
//!      reasoning; a profile without a budget cannot be cost-compared.
//!   2. DecodingSupport — providers differ: temperature unsupported
//!      (luna), max_tokens vs max_completion_tokens (openai family).
//!      A profile declares what it CAN pin; the harness honors it.
//!   3. ReproClass (T28's 4th class, review #2 follow-up) —
//!      temp-unpinnable profiles have NO same-seed->same-answer
//!      contract; they are NondeterministicContract and can never be
//!      silently treated as deterministic or inconclusive-by-default.
//!
//! COMPATIBILITY CONTRACT (review #17): these fields are ADDITIVE and
//! do NOT participate in casting eligibility — policy hashing,
//! judging, or promotion paths are untouched (test-proven). A
//! profile that predates T35 hashes identically to before.
use serde::{Deserialize, Serialize};

/// What a provider's API lets us PIN. Anything not pinnable is
/// honestly declared — the reproducibility contract follows from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DecodingSupport {
    /// temperature param accepted and honored (deepseek: yes at 0.0)
    pub temperature: bool,
    /// sampling seed param accepted (deterministic engines)
    pub seed: bool,
    /// openai-style max_completion_tokens (vs max_tokens)
    pub max_completion_tokens: bool,
}

/// The four reproducibility classes (T28 3 + T35 4th).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReproClass {
    /// same seed -> byte-identical (our Dry Die, deepseek at temp=0)
    Deterministic,
    /// registry/network snapshot required to replay (dep-bump class)
    RequiresSnapshot,
    /// no provenance to even locate the inputs — conservative
    NonReplayable,
    /// T35: the provider contract itself cannot pin sampling
    /// (gpt-6-luna forbids temperature). "Same seed -> same answer"
    /// is NOT a contract here. Measured M3-prep: byte-identical
    /// 6/8 tasks by luck, 2/8 diverged in phrasing.
    NondeterministicContract,
}

/// Cost dimensions the profile declares up front (M3-prep finding:
/// reasoning tokens were invisible until measured — 56% of output).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CostShape {
    /// hard cap on reasoning tokens per call; None = provider default
    pub reasoning_budget: Option<u32>,
    /// whether reasoning tokens are billed as output tokens
    pub reasoning_billed_as_output: bool,
}

/// T35 additive extensions. Old profiles construct with
/// ProfileExtensions::legacy() and hash exactly as M3 did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileExtensions {
    pub decoding: DecodingSupport,
    pub repro_class: ReproClass,
    pub cost: CostShape,
}

impl ProfileExtensions {
    /// The M3-era shape: nothing pinned, no budget, snapshot-free.
    /// Hashing a legacy profile must be byte-identical to M3.
    pub fn legacy() -> Self {
        Self {
            decoding: DecodingSupport::default(),
            repro_class: ReproClass::NonReplayable,
            cost: CostShape::default(),
        }
    }

    pub fn deepseek_v4() -> Self {
        Self {
            decoding: DecodingSupport {
                temperature: true,
                seed: false,
                max_completion_tokens: false,
            },
            repro_class: ReproClass::Deterministic,
            cost: CostShape {
                reasoning_budget: None,
                reasoning_billed_as_output: false,
            },
        }
    }

    pub fn glm_5_3() -> Self {
        Self {
            decoding: DecodingSupport {
                temperature: true,
                seed: false,
                max_completion_tokens: false,
            },
            // measured M3-prep: temp=0 declared but phrasing still
            // varied 8/8 — honest class, not the optimistic one
            repro_class: ReproClass::NondeterministicContract,
            cost: CostShape {
                reasoning_budget: None,
                reasoning_billed_as_output: true,
            },
        }
    }

    pub fn gpt_6_luna() -> Self {
        Self {
            decoding: DecodingSupport {
                temperature: false,
                seed: false,
                max_completion_tokens: true,
            },
            repro_class: ReproClass::NondeterministicContract,
            cost: CostShape {
                reasoning_budget: Some(1024),
                reasoning_billed_as_output: true,
            },
        }
    }

    /// EXTENSION hash — deliberately SEPARATE from the M3 profile
    /// content hash: judging/policy/eligibility read only the M3
    /// hash; cost/repro tooling reads this one. (review #17:
    /// additive fields must not disturb promotion qualification.)
    pub fn extension_hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"t35-ext-v1");
        h.update([
            self.decoding.temperature as u8,
            self.decoding.seed as u8,
            self.decoding.max_completion_tokens as u8,
        ]);
        let rc = match self.repro_class {
            ReproClass::Deterministic => 1u8,
            ReproClass::RequiresSnapshot => 2,
            ReproClass::NonReplayable => 3,
            ReproClass::NondeterministicContract => 4,
        };
        h.update([rc]);
        h.update(
            self.cost
                .reasoning_budget
                .map(|x| x.to_le_bytes())
                .unwrap_or([0; 4]),
        );
        h.update([self.cost.reasoning_billed_as_output as u8]);
        hex(h.finalize().as_slice())
    }

    /// Does a byte-identical rerun CONTRACT exist for this profile?
    /// NondeterministicContract: no — comparing two runs for byte
    /// equality is not evidence of anything (M3-prep: 2/8 diverged).
    pub fn has_repro_contract(&self) -> bool {
        matches!(self.repro_class, ReproClass::Deterministic)
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_extension_hashes_stably() {
        let a = ProfileExtensions::legacy();
        let b = ProfileExtensions::legacy();
        assert_eq!(a.extension_hash(), b.extension_hash());
    }

    #[test]
    fn measured_profiles_distinct() {
        let d = ProfileExtensions::deepseek_v4();
        let g = ProfileExtensions::glm_5_3();
        let l = ProfileExtensions::gpt_6_luna();
        let hs = [d.extension_hash(), g.extension_hash(), l.extension_hash()];
        assert!(hs[0] != hs[1] && hs[1] != hs[2] && hs[0] != hs[2]);
    }

    #[test]
    fn repro_contract_only_for_deterministic() {
        assert!(ProfileExtensions::deepseek_v4().has_repro_contract());
        assert!(!ProfileExtensions::gpt_6_luna().has_repro_contract());
        assert!(!ProfileExtensions::glm_5_3().has_repro_contract());
        assert!(!ProfileExtensions::legacy().has_repro_contract());
    }

    #[test]
    fn luna_budget_reflects_measurement() {
        // M3-prep: reasoning was 56% of output — the profile carries
        // an explicit budget so cost comparison is meaningful
        let l = ProfileExtensions::gpt_6_luna();
        assert_eq!(l.cost.reasoning_budget, Some(1024));
        assert!(l.cost.reasoning_billed_as_output);
        // deepseek measured 0 reasoning tokens
        let d = ProfileExtensions::deepseek_v4();
        assert!(!d.cost.reasoning_billed_as_output);
    }

    #[test]
    fn serialization_roundtrip() {
        let p = ProfileExtensions::gpt_6_luna();
        let j = serde_json::to_string(&p).unwrap();
        let back: ProfileExtensions = serde_json::from_str(&j).unwrap();
        assert_eq!(p, back);
        assert_eq!(p.extension_hash(), back.extension_hash());
    }
}
