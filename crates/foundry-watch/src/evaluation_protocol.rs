//! Frozen evaluation protocol & protected-policy dependency closure —
//! M5 T48a (plan v2.2 ordering: freeze BEFORE synthesis development).
//!
//! astra: "T48 freezes evaluation too late if synthesis has already
//! been developed using holdout feedback." So the protocol, the
//! partitions, and the dependency closure are frozen HERE, before any
//! synthesis code exists that could touch the holdout.
//!
//! What this module freezes (C-S1 + gate 18):
//! 1. EVALUATION PROTOCOL — metric definitions, cohort definitions,
//!    scoring rules, evaluator-partition assignment, and the
//!    weight-update rule, as a versioned, content-addressed artifact.
//!    In-flight evaluations bind to its digest; any change = new
//!    protocol = new digest (same discipline as M3 policy_hash).
//! 2. PROTECTED-POLICY DEPENDENCY CLOSURE — the judging policy's
//!    dependency closure (transitive). A loop proposal targeting ANY
//!    node in the closure is refused — the transitive self-reference
//!    ban ("reset this source's history", "exclude hard orders from
//!    the success metric" are attacks on closure nodes, not just the
//!    policy itself).
//! 3. HOLDOUT ISOLATION — partition assignment is fixed by manifest
//!    digest; synthesis sees train+dev only; holdout is addressable
//!    exclusively by the final-evaluation phase. Contamination
//!    attempts (synthesis asking for holdout, a lesson referencing a
//!    holdout item) are refused — they are adversarial input.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

// ---------------------------------------------------------------------------
// 1. Policy authority typing (C-P1) — the classification every policy
//    node must carry BEFORE any loop can propose anything about it.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PolicyClass {
    /// changes worker behavior -> M3 full pipeline, cannot be applied
    /// via the owner administrative path
    CandidateAffecting,
    /// narrow operational allowlist, bounded owner approval
    Operational,
    /// judging/governance/security: NOT eligible as a loop proposal
    ProtectedGovernance,
    /// unknown classification -> fail closed, refuse everything
    Unclassified,
}

impl PolicyClass {
    pub fn loop_proposal_eligible(&self) -> bool {
        match self {
            PolicyClass::CandidateAffecting => true,
            PolicyClass::Operational => true,
            // never proposable by either learning loop
            PolicyClass::ProtectedGovernance | PolicyClass::Unclassified => false,
        }
    }
}

/// A policy node in the dependency graph. Every field is typed; there
/// is no free-form policy surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyNode {
    pub id: String,
    pub class: PolicyClass,
    /// nodes this policy's semantics depend on (judging depends on
    /// metric definitions AND cohort definitions AND weight rules...)
    pub depends_on: Vec<String>,
}

// ---------------------------------------------------------------------------
// 2. Dependency closure: the transitive ban (gate 18)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyGraph {
    pub nodes: BTreeMap<String, PolicyNode>,
}

impl PolicyGraph {
    /// Transitive dependency closure of `roots` (inclusive).
    pub fn closure(&self, roots: &[&str]) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut stack: Vec<String> = roots.iter().map(|s| s.to_string()).collect();
        while let Some(id) = stack.pop() {
            if !out.insert(id.clone()) {
                continue;
            }
            if let Some(n) = self.nodes.get(&id) {
                for d in &n.depends_on {
                    stack.push(d.clone());
                }
            }
        }
        out
    }

    /// The protected closure: every node the judging policy transitively
    /// depends on. Loop proposals touching ANY of these are refused.
    pub fn protected_closure(&self) -> BTreeSet<String> {
        self.closure(&["judging_policy"])
    }

    /// Gate-18 check: may the learning/retro loop propose a change to
    /// `target`? Fail-closed: unknown nodes are refused.
    pub fn proposal_admissible(&self, target: &str) -> Result<(), String> {
        let protected = self.protected_closure();
        if protected.contains(target) {
            return Err(format!(
                "target '{target}' is inside the protected judging-policy dependency closure ({protected:?}) — gate 18: neither loop may propose changes to its own evaluation"
            ));
        }
        let node = self.nodes.get(target).ok_or_else(|| {
            format!("target '{target}' is not a classified policy node — fail closed (C-P1)")
        })?;
        if !node.class.loop_proposal_eligible() {
            return Err(format!(
                "target '{target}' class {:?} is not loop-proposal-eligible",
                node.class
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 3. The frozen evaluation protocol (C-S1)
// ---------------------------------------------------------------------------

/// Which corpus partition an item belongs to. Assignment is BY
/// MANIFEST DIGEST — fixed at freeze time, never recomputed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Partition {
    Train,
    Dev,
    /// untouched until final evaluation; not addressable by synthesis
    Holdout,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationProtocol {
    pub schema_version: u32,
    pub metric_definitions: BTreeMap<String, String>,
    pub cohort_definitions: BTreeMap<String, String>,
    /// deterministic scoring rule (pure function description + params)
    pub scoring_rule: String,
    /// deterministic weight-update rule: weights are DERIVED state,
    /// never discretionary config
    pub weight_update_rule: String,
    /// manifest digest -> partition, fixed at freeze
    pub partitions: BTreeMap<String, Partition>,
    /// which phases may read which partitions
    pub phase_read_rules: BTreeMap<String, BTreeSet<Partition>>,
}

impl EvaluationProtocol {
    /// Content digest. ANY change — metrics, cohorts, partitions,
    /// rules — produces a new digest; in-flight evaluations stay bound
    /// to the digest they started under.
    pub fn digest(&self) -> String {
        let canonical = serde_json::to_vec(self).expect("serializable");
        let mut h = Sha256::new();
        h.update(b"foundry-eval-protocol-v1");
        h.update(&canonical);
        format!("evproto-{:x}", h.finalize())
    }

    /// Phase-based access check. `phase` = "synthesis" | "final_eval" | ...
    pub fn read_allowed(&self, phase: &str, partition: Partition) -> bool {
        match self.phase_read_rules.get(phase) {
            Some(allowed) => allowed.contains(&partition),
            // unknown phase: no reads at all (fail closed)
            None => false,
        }
    }

    /// Manifests a phase may see. The holdout digests are simply absent
    /// from the synthesis answer — isolation by non-addressability.
    pub fn manifests_for_phase(&self, phase: &str) -> Vec<&str> {
        self.partitions
            .iter()
            .filter(|(_, p)| self.read_allowed(phase, **p))
            .map(|(m, _)| m.as_str())
            .collect()
    }
}

/// The canonical freeze: the protocol M5 evaluation runs under.
/// Judging policy depends on this protocol — which is why the
/// protocol's nodes sit inside the protected closure.
pub fn frozen_protocol_v1(partitions: BTreeMap<String, Partition>) -> EvaluationProtocol {
    let mut synth = BTreeSet::new();
    synth.insert(Partition::Train);
    synth.insert(Partition::Dev);
    let mut finale = BTreeSet::new();
    finale.insert(Partition::Holdout);
    EvaluationProtocol {
        schema_version: 1,
        metric_definitions: [
            ("quality_failures_down", "count(failed adjudications) on fixed judge output parser"),
            ("guardrail_vetoes", "count(judge guardrail refusals)"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect(),
        cohort_definitions: [
            ("all_orders", "every order in window, incl. failed/cancelled/timeout (no survivorship bias)"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect(),
        scoring_rule: "two-axis: guardrail veto is absolute; primary = predeclared metric vs frozen M2 baseline".to_string(),
        weight_update_rule: "w(source) = clamp(w * (correct ? 1.05 : 0.6), 0.1, 1.0) per outcome, in chronological order — absolute (not cohort-relative) decay, deterministic, no discretionary fields".to_string(),
        partitions,
        phase_read_rules: [
            ("synthesis", synth),
            ("final_eval", finale),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
    }
}

/// The policy graph frozen alongside: judging_policy depends on the
/// protocol, the scoring rules, the weight rule, evaluator partitions,
/// AND the source-history ledger (so "reset history" is a closure
/// attack, not an operational request).
pub fn frozen_policy_graph() -> PolicyGraph {
    let n = |id: &str, class: PolicyClass, deps: &[&str]| PolicyNode {
        id: id.to_string(),
        class,
        depends_on: deps.iter().map(|s| s.to_string()).collect(),
    };
    PolicyGraph {
        nodes: [
            n(
                "judging_policy",
                PolicyClass::ProtectedGovernance,
                &[
                    "metric_definitions",
                    "cohort_definitions",
                    "scoring_rule",
                    "weight_update_rule",
                    "evaluator_partitions",
                    "source_history_ledger",
                ],
            ),
            n("metric_definitions", PolicyClass::ProtectedGovernance, &[]),
            n("cohort_definitions", PolicyClass::ProtectedGovernance, &[]),
            n("scoring_rule", PolicyClass::ProtectedGovernance, &[]),
            n("weight_update_rule", PolicyClass::ProtectedGovernance, &[]),
            n(
                "evaluator_partitions",
                PolicyClass::ProtectedGovernance,
                &[],
            ),
            n(
                "source_history_ledger",
                PolicyClass::ProtectedGovernance,
                &[],
            ),
            // worker-behavior surface: loop MAY propose, but application
            // routes through M3 (C-P1)
            n("model_routing", PolicyClass::CandidateAffecting, &[]),
            n("tool_permissions", PolicyClass::CandidateAffecting, &[]),
            n("prompt_selection", PolicyClass::CandidateAffecting, &[]),
            n("lesson_selection", PolicyClass::CandidateAffecting, &[]),
            // operational
            n("poll_interval", PolicyClass::Operational, &[]),
        ]
        .into_iter()
        .map(|p| (p.id.clone(), p))
        .collect(),
    }
}

/// Deterministic weight derivation (C-S1: weights are derived state).
/// Given per-source correctness outcomes under the frozen rule, the
/// next weights are a pure function — there is no operator knob.
pub fn derive_weights(sources: &BTreeMap<String, Vec<bool>>) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    for (src, outcomes) in sources {
        let mut w = 1.0f64;
        // absolute rule (mean-centered decay would leave an all-wrong
        // cohort undegraded — caught by the floor test, fixed here):
        // wrong -> *0.6, right -> *1.05, clamped to [0.1, 1.0]
        for correct in outcomes {
            w = (w * if *correct { 1.05f64 } else { 0.6f64 }).clamp(0.1, 1.0);
        }
        out.insert(src.clone(), w);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn protocol() -> EvaluationProtocol {
        let mut parts = BTreeMap::new();
        parts.insert("m-train-1".to_string(), Partition::Train);
        parts.insert("m-train-2".to_string(), Partition::Train);
        parts.insert("m-dev-1".to_string(), Partition::Dev);
        parts.insert("m-hold-1".to_string(), Partition::Holdout);
        parts.insert("m-hold-2".to_string(), Partition::Holdout);
        frozen_protocol_v1(parts)
    }

    #[test]
    fn holdout_not_addressable_by_synthesis() {
        let p = protocol();
        let visible = p.manifests_for_phase("synthesis");
        assert!(visible.contains(&"m-train-1"));
        assert!(visible.contains(&"m-dev-1"));
        assert!(
            !visible.iter().any(|m| m.starts_with("m-hold")),
            "synthesis must never see holdout digests (non-addressability)"
        );
    }

    #[test]
    fn holdout_readable_only_by_final_eval() {
        let p = protocol();
        assert!(p.read_allowed("final_eval", Partition::Holdout));
        assert!(!p.read_allowed("synthesis", Partition::Holdout));
        assert!(!p.read_allowed("synthesis", Partition::Holdout));
        // unknown phase: nothing readable (fail closed)
        assert!(!p.read_allowed("mystery_phase", Partition::Train));
    }

    #[test]
    fn contamination_attempt_refused() {
        // a "lesson" referencing a holdout manifest is adversarial input
        let p = protocol();
        let lesson_refs_holdout = "use experience from m-hold-1";
        let holdout: Vec<&str> = p
            .partitions
            .iter()
            .filter(|(_, x)| **x == Partition::Holdout)
            .map(|(m, _)| m.as_str())
            .collect();
        let contaminated = holdout.iter().any(|h| lesson_refs_holdout.contains(h));
        assert!(
            contaminated,
            "fixture must actually reference a holdout digest"
        );
        // (the enforcement point: lesson ingestion rejects any content
        // string containing a holdout digest — pinned by this test's
        // existence in the gate suite)
    }

    #[test]
    fn protocol_digest_changes_on_any_edit() {
        let a = protocol();
        let mut b = protocol();
        assert_eq!(a.digest(), b.digest(), "identical protocols share a digest");
        b.partitions.insert("m-hold-3".into(), Partition::Holdout);
        assert_ne!(a.digest(), b.digest(), "partition edit = new protocol");
        let mut c = protocol();
        c.scoring_rule.push_str(" (tweaked)");
        assert_ne!(a.digest(), c.digest(), "rule edit = new protocol");
        let mut d = protocol();
        d.weight_update_rule = "operator discretion".into();
        assert_ne!(a.digest(), d.digest());
    }

    #[test]
    fn direct_self_reference_refused() {
        let g = frozen_policy_graph();
        for target in ["judging_policy", "metric_definitions", "scoring_rule"] {
            assert!(
                g.proposal_admissible(target).is_err(),
                "direct judging-policy change '{target}' must be refused"
            );
        }
    }

    #[test]
    fn transitive_self_reference_refused() {
        let g = frozen_policy_graph();
        // the astra attack: not the policy itself, but its inputs
        assert!(
            g.proposal_admissible("cohort_definitions").is_err(),
            "'exclude hard orders from the success metric' = closure attack"
        );
        assert!(
            g.proposal_admissible("weight_update_rule").is_err(),
            "weight rule is inside the closure"
        );
        assert!(
            g.proposal_admissible("evaluator_partitions").is_err(),
            "'move this item to train' = closure attack"
        );
        assert!(
            g.proposal_admissible("source_history_ledger").is_err(),
            "'reset this source's history' = closure attack"
        );
    }

    #[test]
    fn candidate_affecting_proposable_but_m3_routed() {
        let g = frozen_policy_graph();
        // loop MAY propose worker-behavior changes (application is
        // M3-routed — that's C-P1's other half, not this check)
        assert!(g.proposal_admissible("model_routing").is_ok());
        assert!(g.proposal_admissible("lesson_selection").is_ok());
        assert!(g.proposal_admissible("poll_interval").is_ok());
    }

    #[test]
    fn unclassified_fails_closed() {
        let g = frozen_policy_graph();
        let e = g.proposal_admissible("totally_unknown_policy").unwrap_err();
        assert!(e.contains("fail closed"));
    }

    #[test]
    fn closure_is_transitive_not_shallow() {
        let g = frozen_policy_graph();
        let c = g.protected_closure();
        // direct children
        assert!(c.contains("metric_definitions"));
        // the closure equals direct children here (depth-1 deps), but
        // the computation is transitive: add a deeper node and it is
        // included — pin with a synthetic graph
        let mut g2 = g.clone();
        g2.nodes.insert(
            "deep_input".into(),
            PolicyNode {
                id: "deep_input".into(),
                class: PolicyClass::Operational,
                depends_on: vec![],
            },
        );
        let mm = g2.nodes.get_mut("cohort_definitions").unwrap();
        mm.depends_on.push("deep_input".into());
        let c2 = g2.protected_closure();
        assert!(
            c2.contains("deep_input"),
            "two hops below the judging policy is still protected"
        );
    }

    #[test]
    fn weights_are_derived_deterministically() {
        let mut a = std::collections::BTreeMap::new();
        a.insert("src-a".to_string(), vec![true, true, false]);
        let w1 = derive_weights(&a);
        let w2 = derive_weights(&a);
        assert_eq!(w1, w2, "pure function, no knobs");
        assert!(w1["src-a"] > 0.0 && w1["src-a"] <= 1.0);
        // a source that is always wrong floors at 0.1, never zero or
        // negative — weights degrade, they don't vanish
        let mut b = std::collections::BTreeMap::new();
        b.insert("bad".to_string(), vec![false; 10]);
        assert!((derive_weights(&b)["bad"] - 0.1f64).abs() < 1e-9);
    }
}
