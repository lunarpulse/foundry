//! Corpus emission from the journal — T18's ledger-driven writer.
//!
//! Rather than hooking every terminal branch in line.rs (scattered,
//! easy to miss one), we DERIVE manifests by replaying the journal: the
//! single replay engine (T16) identifies every lifecycle and its
//! terminal state; this module fills the manifest fields from the
//! journal rows + archive + Die workdir remnants. Re-running emit is
//! idempotent (same order id + lifecycle → same file, overwritten).
//!
//! This guarantees the review #13/#6 invariant structurally: no terminal
//! path can avoid leaving a corpus entry, because the entry comes from
//! the FACT source (the journal), not from a code path remembering to
//! write it.
use crate::corpus::{CorpusManifest, CostFields};
use foundry_domain::OrderState;
use foundry_ledger::LedgerRow;
use foundry_ledger::replay::ReplayedOrder;

pub fn manifest_for(roc: &ReplayedOrder, rows: &[LedgerRow]) -> CorpusManifest {
    let lifecycle_rows: Vec<&LedgerRow> = rows
        .iter()
        .filter(|r| r.order_id == roc.order_id && r.seq >= roc.started_seq && r.seq <= roc.last_seq)
        .collect();
    let attempts = roc.order.attempt().max(
        lifecycle_rows
            .iter()
            .filter_map(|r| crate::corpus::corpus_attempts(&r.event))
            .max()
            .unwrap_or(0),
    );
    CorpusManifest {
        order_id: roc.order_id.clone(),
        terminal_state: roc.order.state().to_string(),
        base_git_sha: None, // archive/workdir provenance: filled when present
        target: target_of(&roc.order_id),
        prompt_template_id: Some("dep-bump-v1".into()),
        prompt_render_hash: None,
        model_id: Some("rustain/local".into()),
        decoding_params: None,
        tool_policy: Some("subprocess".into()),
        foundry_git_sha: foundry_sha(),
        ledger_row_range: Some((roc.started_seq, roc.last_seq)),
        archive_hash: None,
        attempts,
        die_seconds: None,
        cost: CostFields::default(), // token fields: unknown for legacy runs
        event_schema_version: lifecycle_rows
            .iter()
            .map(|r| foundry_ledger::event_json::row_version(r))
            .max()
            .unwrap_or(1),
    }
}

/// Task string from the M0 id convention (intake records the issue title;
/// the id itself carries the issue number — full title needs intake
/// linkage, recorded as unknown-tail for now).
fn target_of(order_id: &str) -> String {
    let tail = order_id.strip_prefix("o-").unwrap_or(order_id);
    if tail.contains(':') {
        format!("github-issue#{}", tail.split(':').next().unwrap_or("?"))
    } else {
        format!("cron:{tail}")
    }
}

fn foundry_sha() -> Option<String> {
    std::env::var("FOUNDRY_GIT_SHA").ok().or_else(|| None)
}

#[cfg(test)]
mod tests {
    // see corpus.rs tests for roundtrip; here we check derivation logic
    use super::*;

    #[test]
    fn target_convention() {
        assert_eq!(target_of("o-137:2026-10-07T18:36:27Z"), "github-issue#137");
        assert_eq!(target_of("o-nightly"), "cron:nightly");
    }
}
