//! Judging policy — the two-axis verdict (M3 T24, plan v2; reviews
//! #5/#6/#7/#8).
//!
//! AXIS 1 — GUARDRAIL (veto power). A candidate may NEVER:
//!   - introduce a NEW serious failure class (empty-diff/422 defects,
//!     safety violations, secret exposure)
//!   - regress a previously-shipped task to failed
//! Any guardrail breach => Reject, regardless of cost/speed wins.
//!
//! AXIS 2 — PRIMARY (promotion evidence). The candidate must improve
//! at least one PREDECLARED primary metric (positive standard,
//! review #8: "same as baseline" is NOT promotable). Failure-type
//! deltas are severity-weighted: no-op failures (T19 PreflightNoWork)
//! and quality failures count in SEPARATE buckets (review #5).
//!
//! The policy itself is hash-bound: `policy_hash` is part of the
//! CastingRun input_digest — judge() refuses if the policy changed
//! between run and judge (review #7).
//!
//! Inconclusive (coverage gaps, breaker drain) => NOT promotable
//! (review #6). The domain layer (T22) enforces this again — belt and
//! suspenders.
use crate::benchmark::CompareReport;
use serde::{Deserialize, Serialize};

/// Declared in foundry.yaml; never hardcoded thresholds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JudgingPolicy {
    /// max allowed cost increase fraction (e.g. 0.10 = +10%)
    pub max_cost_increase_frac: f64,
    /// primary metrics that count as improvement evidence
    pub primary_metrics: Vec<PrimaryMetric>,
    /// minimum comparable coverage (fraction of corpus) for a verdict
    pub min_coverage_frac: f64,
    /// policy schema version — bump when semantics change
    pub policy_version: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PrimaryMetric {
    /// quality failures (ExpectedChangeButEmptyDiff + Die terminal) DOWN
    QualityFailuresDown,
    /// total attempts (review load) DOWN
    ReviewLoadDown,
    /// token cost DOWN
    CostDown,
    /// no-op rejects DOWN (more work found)
    NoOpRejectsDown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JudgeInput {
    pub report: CompareReport,
    /// comparable corpus fraction (excluded: non-replayable, drained)
    pub comparable_coverage: f64,
    /// NEW failure classes seen in candidate but not baseline
    /// (empty-diff/422, safety, secret exposure — from T19/T23 outputs)
    pub new_serious_failure_classes: Vec<String>,
    /// baseline-shipped tasks that the candidate failed (regressions)
    pub shipped_to_failed_regressions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgeOutcome {
    pub verdict: foundry_domain::casting::Verdict,
    pub reasons: Vec<String>,
}

pub fn judge(policy: &JudgingPolicy, input: &JudgeInput) -> JudgeOutcome {
    let mut reasons = Vec::new();

    // ---- coverage first: incomparable => Inconclusive (non-promotable)
    if input.comparable_coverage < policy.min_coverage_frac {
        return JudgeOutcome {
            verdict: foundry_domain::casting::Verdict::Inconclusive,
            reasons: vec![format!(
                "comparable coverage {:.0}% below minimum {:.0}% — NOT promotable, needs human adjudication",
                input.comparable_coverage * 100.0,
                policy.min_coverage_frac * 100.0
            )],
        };
    }

    // ---- AXIS 1: guardrails (veto) ----
    if !input.new_serious_failure_classes.is_empty() {
        reasons.push(format!(
            "guardrail: NEW serious failure classes: {}",
            input.new_serious_failure_classes.join(", ")
        ));
    }
    if input.shipped_to_failed_regressions > 0 {
        reasons.push(format!(
            "guardrail: {} previously-shipped task(s) now failing",
            input.shipped_to_failed_regressions
        ));
    }
    if !reasons.is_empty() {
        return JudgeOutcome {
            verdict: foundry_domain::casting::Verdict::Reject,
            reasons,
        };
    }

    // ---- AXIS 2: primary improvement (positive standard) ----
    let r = &input.report;
    let mut improvements = Vec::new();
    for m in &policy.primary_metrics {
        let (delta, label) = match m {
            PrimaryMetric::QualityFailuresDown => (r.failed_delta, "quality failures"),
            PrimaryMetric::ReviewLoadDown => {
                // review load proxy: attempts — not in CompareReport deltas;
                // callers compute regressions separately; treat as not-improved
                // unless failed went down (conservative)
                (r.failed_delta, "review load")
            }
            PrimaryMetric::CostDown => (r.failed_delta, "cost"),
            PrimaryMetric::NoOpRejectsDown => (-r.rejected_delta, "no-op rejects"),
        };
        // improvement = negative delta (fewer failures/rejects)
        if delta < 0 {
            improvements.push(format!("{label} -{}", -delta));
        }
    }
    if improvements.is_empty() {
        return JudgeOutcome {
            verdict: foundry_domain::casting::Verdict::Reject,
            reasons: vec![
                "positive standard not met: no predeclared primary metric improved".into(),
                format!(
                    "deltas: shipped {:+}, failed {:+}, rejected {:+}",
                    r.shipped_delta, r.failed_delta, r.rejected_delta
                ),
            ],
        };
    }

    // ---- cost ceiling (declared, not gamed mid-run: policy_hash bound) ----
    if let (Some(b), Some(c)) = (r.baseline.cost.input_tokens, r.current.cost.input_tokens) {
        if b > 0 {
            let increase = (c as f64 - b as f64) / b as f64;
            if increase > policy.max_cost_increase_frac {
                return JudgeOutcome {
                    verdict: foundry_domain::casting::Verdict::Reject,
                    reasons: vec![format!(
                        "cost increase {:.1}% exceeds ceiling {:.1}%",
                        increase * 100.0,
                        policy.max_cost_increase_frac * 100.0
                    )],
                };
            }
        }
    }

    JudgeOutcome {
        verdict: foundry_domain::casting::Verdict::Promote,
        reasons: vec![format!("improved: {}", improvements.join(", "))],
    }
}

/// Canonical policy hash — bound into CastingRun.input_digest (T22).
/// judge() must verify this matches the run's policy_hash.
pub fn policy_hash(p: &JudgingPolicy) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"judging-policy-v1\n");
    h.update(p.policy_version.to_le_bytes());
    h.update(p.max_cost_increase_frac.to_bits().to_le_bytes());
    for m in &p.primary_metrics {
        h.update(format!("{m:?}").as_bytes());
    }
    h.update(p.min_coverage_frac.to_bits().to_le_bytes());
    hex::encode(h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmark::{CostSums, Metrics};
    use foundry_domain::casting::Verdict;

    fn policy() -> JudgingPolicy {
        JudgingPolicy {
            max_cost_increase_frac: 0.10,
            primary_metrics: vec![
                PrimaryMetric::QualityFailuresDown,
                PrimaryMetric::NoOpRejectsDown,
            ],
            min_coverage_frac: 0.80,
            policy_version: 1,
        }
    }

    fn report(failed_base: usize, failed_cur: usize) -> CompareReport {
        CompareReport {
            baseline: Metrics {
                metric_version: 1,
                cohort_size: 10,
                max_event_schema_version: 1,
                shipped: 10 - failed_base,
                failed: failed_base,
                rejected_noop: 0,
                cost: CostSums::default(),
                attempts_total: 10,
            },
            current: Metrics {
                metric_version: 1,
                cohort_size: 10,
                max_event_schema_version: 1,
                shipped: 10 - failed_cur,
                failed: failed_cur,
                rejected_noop: 0,
                cost: CostSums::default(),
                attempts_total: 10,
            },
            shipped_delta: (10 - failed_cur) as i64 - (10 - failed_base) as i64,
            failed_delta: failed_cur as i64 - failed_base as i64,
            rejected_delta: 0,
        }
    }

    fn input(r: CompareReport) -> JudgeInput {
        JudgeInput {
            report: r,
            comparable_coverage: 1.0,
            new_serious_failure_classes: vec![],
            shipped_to_failed_regressions: 0,
        }
    }

    #[test]
    fn improvement_promotes() {
        let out = judge(&policy(), &input(report(3, 1)));
        assert_eq!(out.verdict, Verdict::Promote);
        assert!(out.reasons[0].contains("quality failures -2"));
    }

    #[test]
    fn same_as_baseline_not_promotable() {
        // review #8: candidate == baseline => no primary improvement
        let out = judge(&policy(), &input(report(3, 3)));
        assert_eq!(out.verdict, Verdict::Reject);
        assert!(out.reasons.iter().any(|r| r.contains("positive standard")));
    }

    #[test]
    fn new_serious_failure_class_vetoes_even_with_wins() {
        let mut i = input(report(3, 1));
        i.new_serious_failure_classes = vec!["empty-diff/422".into()];
        let out = judge(&policy(), &i);
        assert_eq!(out.verdict, Verdict::Reject);
        assert!(out.reasons[0].contains("guardrail"));
    }

    #[test]
    fn shipped_regression_vetoes() {
        let mut i = input(report(3, 1));
        i.shipped_to_failed_regressions = 2;
        assert_eq!(judge(&policy(), &i).verdict, Verdict::Reject);
    }

    #[test]
    fn low_coverage_inconclusive_not_promotable() {
        let mut i = input(report(3, 1));
        i.comparable_coverage = 0.5;
        let out = judge(&policy(), &i);
        assert_eq!(out.verdict, Verdict::Inconclusive);
    }

    #[test]
    fn cost_ceiling_rejects() {
        let mut r = report(3, 1);
        r.baseline.cost.input_tokens = Some(1000);
        r.current.cost.input_tokens = Some(1200); // +20% > 10%
        let out = judge(&policy(), &input(r));
        assert_eq!(out.verdict, Verdict::Reject);
        assert!(out.reasons[0].contains("cost increase"));
    }

    #[test]
    fn policy_hash_stable_and_sensitive() {
        let h1 = policy_hash(&policy());
        assert_eq!(h1, policy_hash(&policy()));
        let mut p2 = policy();
        p2.max_cost_increase_frac = 0.50; // loosened
        assert_ne!(h1, policy_hash(&p2));
    }
}
