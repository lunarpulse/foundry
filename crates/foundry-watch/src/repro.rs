//! Reproducibility environment — T28 (M3, plan v2; reviews #2/#3).
//!
//! Review #2: corpus tasks are not uniformly replayable — preconditions
//! may have vanished (file deleted, API changed, fixture gone).
//! Review #3: fixed seed alone doesn't freeze time or external state.
//!
//! This module classifies each corpus task BEFORE a CastingRun:
//!
//!   ReplayableAtSha — preconditions are fully determined by the base
//!                    git SHA (task = edit files that exist at that SHA)
//!   RequiresSnapshot — preconditions live OUTSIDE the repo (registry
//!                    state, network service); replay needs a captured
//!                    snapshot (recorded responses)
//!   NonReplayable    — cannot be re-executed faithfully (point-in-time
//!                    approvals, irrevocable external actions)
//!
//! And freezes the environment the Dry Die sees:
//!   - deterministic clock (seeded, NOT wall time — M1 from_env_at
//!     precedent: time is an injected dependency)
//!   - seed bound into the CastingRun input_digest (T22)
use crate::corpus::CorpusManifest;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplayClass {
    ReplayableAtSha,
    RequiresSnapshot,
    NonReplayable,
}

/// Classify one corpus entry. Dep-bump tasks whose target crate state
/// must be re-queried from crates.io => RequiresSnapshot (registry is
/// external state). Tasks whose only inputs are repo files => AtSha.
/// Entries with missing provenance => NonReplayable (conservative —
/// an unclassifiable task is never silently compared).
pub fn classify(m: &CorpusManifest) -> ReplayClass {
    let has_sha = m
        .base_git_sha
        .as_deref()
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    let is_dep_bump = m.prompt_template_id.as_deref() == Some("dep-bump-v1");
    match (has_sha, is_dep_bump) {
        (false, _) => ReplayClass::NonReplayable, // no provenance at all
        (true, true) => ReplayClass::RequiresSnapshot, // registry = external state
        (true, false) => ReplayClass::ReplayableAtSha,
    }
}

/// Coverage report per class — judge consumes this (T24 coverage gate).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CoverageReport {
    pub total: usize,
    pub replayable_at_sha: usize,
    pub requires_snapshot: usize,
    pub non_replayable: usize,
    /// (replayable + snapshot-available) / total — comparable fraction
    pub comparable_frac: f64,
}

pub fn coverage(corpus: &[CorpusManifest], snapshots_available: usize) -> CoverageReport {
    let mut r = CoverageReport::default();
    r.total = corpus.len();
    for m in corpus {
        match classify(m) {
            ReplayClass::ReplayableAtSha => r.replayable_at_sha += 1,
            ReplayClass::RequiresSnapshot => r.requires_snapshot += 1,
            ReplayClass::NonReplayable => r.non_replayable += 1,
        }
    }
    let comparable = r.replayable_at_sha + snapshots_available.min(r.requires_snapshot);
    r.comparable_frac = if r.total == 0 {
        0.0
    } else {
        comparable as f64 / r.total as f64
    };
    r
}

/// Deterministic clock for candidate execution (review #3). Time is an
/// injected dependency: same seed => same timestamps => identical
/// artifacts (gate 4). 26-hour-style jumps are impossible by design.
#[derive(Debug, Clone)]
pub struct FrozenClock {
    /// nanoseconds since epoch at run start (from the run seed)
    start_nanos: i64,
    tick_nanos: u64,
}

impl FrozenClock {
    pub fn new(seed: u64) -> Self {
        FrozenClock {
            start_nanos: ((seed % 1_600_000_000u64) as i64 * 1_000_000_000)
                + 1_600_000_000_000_000_000,
            tick_nanos: 0,
        }
    }

    /// Advance one deterministic step; returns RFC3339-ish timestamp.
    pub fn next(&mut self) -> String {
        self.tick_nanos += 1_000_000; // 1ms per call, never wall time
        let secs = self.start_nanos / 1_000_000_000 + (self.tick_nanos / 1_000_000_000) as i64;
        let nanos = (self.start_nanos % 1_000_000_000) + (self.tick_nanos % 1_000_000_000) as i64;
        format!("{secs}.{nanos:09}Z")
    }
}

/// Base-SHA gate (review #1): a CastingRun may only start if the
/// repo's CURRENT head equals the corpus's recorded base SHA. Drift
/// means the comparison would be confounded by code changes.
pub fn base_sha_gate(corpus_base_sha: Option<&str>, current_head: &str) -> Result<(), String> {
    match corpus_base_sha {
        Some(sha) if !sha.is_empty() && sha == current_head => Ok(()),
        Some(sha) => Err(format!(
            "base_git_sha mismatch: corpus recorded {sha:.12}.. but repo head is {current_head:.12}.. — \
             re-execute would compare against a different code state; refusing"
        )),
        None => Err(
            "corpus entries carry no base_git_sha provenance — cannot verify replay preconditions"
                .into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::CostFields;

    fn manifest(sha: Option<&str>, template: Option<&str>) -> CorpusManifest {
        CorpusManifest {
            order_id: "o-1".into(),
            terminal_state: "shipped".into(),
            base_git_sha: sha.map(String::from),
            target: "t".into(),
            prompt_template_id: template.map(String::from),
            prompt_render_hash: None,
            model_id: None,
            decoding_params: None,
            tool_policy: None,
            foundry_git_sha: None,
            ledger_row_range: None,
            archive_hash: None,
            attempts: 1,
            die_seconds: None,
            cost: CostFields::default(),
            event_schema_version: 1,
        }
    }

    #[test]
    fn classification_three_kinds() {
        // dep-bump with provenance => external registry state
        assert_eq!(
            classify(&manifest(Some("abc"), Some("dep-bump-v1"))),
            ReplayClass::RequiresSnapshot
        );
        // non-dep-bump with provenance => fully determined by SHA
        assert_eq!(
            classify(&manifest(Some("abc"), None)),
            ReplayClass::ReplayableAtSha
        );
        // no provenance => conservative non-replayable
        assert_eq!(
            classify(&manifest(None, Some("dep-bump-v1"))),
            ReplayClass::NonReplayable
        );
    }

    #[test]
    fn coverage_counts_snapshots() {
        let corpus = vec![
            manifest(Some("a"), None),
            manifest(Some("a"), Some("dep-bump-v1")),
            manifest(Some("a"), Some("dep-bump-v1")),
        ];
        // 1 replayable + min(2 snapshots, 2 requiring) = 2/3 comparable
        let c = coverage(&corpus, 1);
        assert_eq!(c.replayable_at_sha, 1);
        assert_eq!(c.requires_snapshot, 2);
        assert!((c.comparable_frac - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn base_sha_gate_refuses_drift() {
        assert!(base_sha_gate(Some("aaa"), "aaa").is_ok());
        assert!(base_sha_gate(Some("aaa"), "bbb").is_err());
        assert!(base_sha_gate(None, "aaa").is_err());
    }

    #[test]
    fn frozen_clock_deterministic() {
        let mut c1 = FrozenClock::new(42);
        let mut c2 = FrozenClock::new(42);
        for _ in 0..5 {
            assert_eq!(c1.next(), c2.next());
        }
        // different seed => different epoch base
        let mut c3 = FrozenClock::new(43);
        assert_ne!(c1.next(), c3.next());
    }
}
