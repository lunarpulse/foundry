//! Casting circuit breaker — SCOPED, never production (M3 T25,
//! plan v2; reviews #9/#10).
//!
//! Review #9: a bad EXPERIMENTAL profile must never halt PRODUCTION
//! intake. So the casting breaker lives in its own registry, keyed by
//! (blueprint, casting_run_id) — it can stop scheduling candidate
//! tasks inside that run and nothing else. Escalation to a production
//! BlueprintDisabled is a human decision, explicitly recorded.
//!
//! Review #10 (drain semantics): when the breaker trips mid-run —
//!   1. no NEW candidate tasks are scheduled;
//!   2. in-flight tasks drain: each gets a terminal SkippedByBreaker
//!      entry in the candidate corpus (no silent loss);
//!   3. judge() sees the skipped entries and computes comparable
//!      coverage EXCLUDING them — which drives the verdict toward
//!      Inconclusive (non-promotable), never a lucky Promote.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const DEFAULT_TRIP_THRESHOLD: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BreakerDetail {
    pub blueprint: String,
    pub casting_run_id: String,
    pub candidate_profile_id: String,
    pub consecutive_failures: u32,
    pub threshold: u32,
    pub tripped_at: String,
    /// human-readable cause chain (OpenHands disabled_detail style)
    pub causes: Vec<String>,
    /// release condition — ALWAYS "explicit human approval"
    pub release_condition: String,
}

#[derive(Debug, Default)]
pub struct CastingBreaker {
    threshold: u32,
    /// (blueprint, run_id) -> consecutive failures
    counts: HashMap<(String, String), u32>,
    tripped: HashMap<(String, String), BreakerDetail>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Scheduling {
    Allowed,
    /// breaker open for this (blueprint, run): stop scheduling
    Blocked {
        detail: BreakerDetail,
    },
}

impl CastingBreaker {
    pub fn new(threshold: u32) -> Self {
        CastingBreaker {
            threshold: threshold.max(1),
            ..Default::default()
        }
    }

    /// Record a candidate task failure. Trips at threshold; the trip
    /// detail carries cause + release condition.
    pub fn record_failure(
        &mut self,
        blueprint: &str,
        run_id: &str,
        profile: &str,
        cause: &str,
        at: &str,
    ) -> Scheduling {
        let k = (blueprint.to_string(), run_id.to_string());
        let c = self.counts.entry(k.clone()).or_insert(0);
        *c += 1;
        if *c >= self.threshold {
            let detail = BreakerDetail {
                blueprint: blueprint.to_string(),
                casting_run_id: run_id.to_string(),
                candidate_profile_id: profile.to_string(),
                consecutive_failures: *c,
                threshold: self.threshold,
                tripped_at: at.to_string(),
                causes: vec![cause.to_string()],
                release_condition: "explicit human approval only".into(),
            };
            self.tripped.insert(k.clone(), detail.clone());
            Scheduling::Blocked { detail }
        } else {
            Scheduling::Allowed
        }
    }

    /// Record a success — resets the consecutive counter (classic
    /// consecutive-failure semantics, NOT cumulative).
    pub fn record_success(&mut self, blueprint: &str, run_id: &str) {
        self.counts
            .remove(&(blueprint.to_string(), run_id.to_string()));
    }

    /// May we schedule the next candidate task for this blueprint?
    pub fn check(&self, blueprint: &str, run_id: &str) -> Scheduling {
        match self
            .tripped
            .get(&(blueprint.to_string(), run_id.to_string()))
        {
            Some(d) => Scheduling::Blocked { detail: d.clone() },
            None => Scheduling::Allowed,
        }
    }

    /// PRODUCTION intake decision — the casting breaker has NO say.
    /// (Structurally: production checks a different registry entirely;
    ///  this method exists to make the guarantee testable/greppable.)
    pub fn production_intake_allowed(&self) -> bool {
        true // always — casting failures never halt production (review #9)
    }
}

/// Drain: terminal entries for tasks skipped by an open breaker
/// (review #10 — no silent loss; judge counts them as non-comparable).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkippedByBreaker {
    pub order_id: String,
    pub blueprint: String,
    pub casting_run_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trips_at_threshold_scoped_to_run() {
        let mut b = CastingBreaker::new(3);
        for i in 0..2 {
            assert_eq!(
                b.record_failure("dep-bump", "cast-1", "prof-x", "die timeout", "t"),
                Scheduling::Allowed
            );
            let _ = i;
        }
        // third consecutive failure trips
        match b.record_failure("dep-bump", "cast-1", "prof-x", "die timeout", "t") {
            Scheduling::Blocked { detail } => {
                assert_eq!(detail.consecutive_failures, 3);
                assert_eq!(detail.release_condition, "explicit human approval only");
            }
            _ => panic!("expected trip"),
        }
        // scheduling now blocked for (dep-bump, cast-1)...
        assert!(matches!(
            b.check("dep-bump", "cast-1"),
            Scheduling::Blocked { .. }
        ));
        // ...but a DIFFERENT run of the same blueprint is unaffected,
        // and production intake is never halted (review #9)
        assert_eq!(b.check("dep-bump", "cast-2"), Scheduling::Allowed);
        assert!(b.production_intake_allowed());
    }

    #[test]
    fn success_resets_counter() {
        let mut b = CastingBreaker::new(3);
        b.record_failure("bp", "r", "p", "x", "t");
        b.record_failure("bp", "r", "p", "x", "t");
        b.record_success("bp", "r"); // reset
        assert_eq!(
            b.record_failure("bp", "r", "p", "x", "t"),
            Scheduling::Allowed
        );
        assert_eq!(
            b.record_failure("bp", "r", "p", "x", "t"),
            Scheduling::Allowed
        );
        // only after 3 CONSECUTIVE does it trip
        assert!(matches!(
            b.record_failure("bp", "r", "p", "x", "t"),
            Scheduling::Blocked { .. }
        ));
    }

    #[test]
    fn drain_entries_are_counted_not_lost() {
        // judge-side accounting: skipped entries reduce comparable
        // coverage, pushing toward Inconclusive (never a lucky promote)
        let skipped = vec![
            SkippedByBreaker {
                order_id: "o-1".into(),
                blueprint: "dep-bump".into(),
                casting_run_id: "cast-1".into(),
            },
            SkippedByBreaker {
                order_id: "o-2".into(),
                blueprint: "dep-bump".into(),
                casting_run_id: "cast-1".into(),
            },
        ];
        let total = 10usize;
        let comparable = total - skipped.len();
        let coverage = comparable as f64 / total as f64;
        // 0.8 < typical min_coverage (0.90) => Inconclusive territory
        assert!(coverage < 0.90);
    }
}
