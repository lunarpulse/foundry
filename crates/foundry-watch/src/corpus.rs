//! Corpus manifest — the "inputs were identical" proof material for
//! replay/benchmark (M2 T18, plan v2; reviews #7/#10/#13).
//!
//! The journal is FACT (what happened); the corpus is RE-PRODUCTION
//! MATERIAL (what it took). Every TERMINAL path leaves a manifest —
//! Shipped, Failed, Rejected (incl. no-op), Canceled — so benchmark
//! cohorts can never silently drop the orders that did not ship.
//!
//! Secrets are never stored: fields like tokens record only a hash or
//! are omitted. Unknown values are recorded as `None` — the benchmark
//! reports known/unknown coverage rather than inventing numbers.

use foundry_domain::OrderState;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Per-order cost fields (review #10) — nullable by design; Die log
/// parsing fills what it can, the rest stays `None` (unknown).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CostFields {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub tool_call_count: Option<u64>,
    pub cost_estimate_usd: Option<f64>,
}

impl CostFields {
    pub fn coverage(&self) -> (usize, usize) {
        let known = [
            self.input_tokens.is_some(),
            self.output_tokens.is_some(),
            self.cached_tokens.is_some(),
            self.tool_call_count.is_some(),
            self.cost_estimate_usd.is_some(),
        ]
        .iter()
        .filter(|b| **b)
        .count();
        (known, 5)
    }
}

/// One corpus entry — the minimal fields a fair replay/benchmark needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusManifest {
    pub order_id: String,
    /// Which terminal (or current) lifecycle outcome this entry records.
    pub terminal_state: String,
    /// Git SHA the workdir was based on (input identity).
    pub base_git_sha: Option<String>,
    /// What was asked: "dep-bump: anyhow" style task string.
    pub target: String,
    /// Prompt template id + render hash (or the full prompt if short).
    pub prompt_template_id: Option<String>,
    pub prompt_render_hash: Option<String>,
    /// Model/decoding identity of the Die run.
    pub model_id: Option<String>,
    pub decoding_params: Option<String>,
    pub tool_policy: Option<String>,
    /// foundry binary provenance.
    pub foundry_git_sha: Option<String>,
    /// Journal linkage: rows of this lifecycle.
    pub ledger_row_range: Option<(i64, i64)>,
    /// Archive dir hash (post-GC evidence), if archived.
    pub archive_hash: Option<String>,
    /// Attempt count actually executed.
    pub attempts: u32,
    /// Die wall time per attempt, if known.
    pub die_seconds: Option<f64>,
    /// Cost fields (nullable; benchmark reports coverage honestly).
    #[serde(default)]
    pub cost: CostFields,
    /// Event schema version of the journal rows this entry references.
    pub event_schema_version: u8,
}

impl CorpusManifest {
    /// Write into `<corpus_root>/<order_id>-<lifecycle>.json`.
    pub fn save(&self, corpus_root: &Path) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(corpus_root)?;
        let safe = self.order_id.replace(':', "_");
        let path = corpus_root.join(format!(
            "{safe}-{}.json",
            lifecycle_of(&self.terminal_state)
        ));
        let mut buf = Vec::new();
        let mut ser = serde_json::Serializer::pretty(&mut buf);
        self.serialize(&mut ser)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(&path, buf)?;
        Ok(path)
    }

    /// Load all manifests under a corpus root (benchmark input).
    pub fn load_all(corpus_root: &Path) -> std::io::Result<Vec<CorpusManifest>> {
        let mut out = Vec::new();
        if !corpus_root.exists() {
            return Ok(out);
        }
        for ent in std::fs::read_dir(corpus_root)? {
            let ent = ent?;
            let p = ent.path();
            if p.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let s = std::fs::read_to_string(&p)?;
            if let Ok(m) = serde_json::from_str(&s) {
                out.push(m);
            }
        }
        out.sort_by(|a, b| a.order_id.cmp(&b.order_id));
        Ok(out)
    }
}

fn lifecycle_of(_terminal: &str) -> u32 {
    0 // placeholder until multi-lifecycle corpus entries exist (M3)
}

/// Which states warrant a corpus entry: every terminal path (review #13)
/// plus Publishing (ambiguous — T21 re-adjudicates, then rewrites this
/// entry).
pub fn wants_manifest(state: OrderState) -> bool {
    matches!(
        state,
        OrderState::Shipped
            | OrderState::Failed
            | OrderState::Rejected
            | OrderState::Canceled
            | OrderState::Unknown
            | OrderState::Publishing
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_roundtrips_and_loads() {
        let dir = std::env::temp_dir().join(format!("corpus-t-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let m = CorpusManifest {
            order_id: "o-137:2026-10-07T18:36:27Z".into(),
            terminal_state: "shipped".into(),
            base_git_sha: Some("84a78014d3691068790529dbfeebfa4a614825e4".into()),
            target: "dep-bump: anyhow".into(),
            prompt_template_id: Some("dep-bump-v1".into()),
            prompt_render_hash: Some("9f2c".into()),
            model_id: Some("rustain/local".into()),
            decoding_params: None,
            tool_policy: Some("subprocess".into()),
            foundry_git_sha: Some("723cfae".into()),
            ledger_row_range: Some((151, 157)),
            archive_hash: Some("aa11".into()),
            attempts: 1,
            die_seconds: Some(41.0),
            cost: CostFields {
                input_tokens: Some(21151),
                output_tokens: Some(649),
                ..Default::default()
            },
            event_schema_version: 1,
        };
        let p = m.save(&dir).unwrap();
        assert!(p.exists());
        let all = CorpusManifest::load_all(&dir).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].order_id, "o-137:2026-10-07T18:36:27Z");
        assert_eq!(all[0].cost.coverage(), (2, 5));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_terminal_path_wants_a_manifest() {
        for s in [
            OrderState::Shipped,
            OrderState::Failed,
            OrderState::Rejected,
            OrderState::Canceled,
        ] {
            assert!(wants_manifest(s), "{s:?} must leave a corpus entry");
        }
        assert!(!wants_manifest(OrderState::Working));
        assert!(!wants_manifest(OrderState::FurnaceHold));
    }
}

/// Best-effort attempt number from a journal event repr (shared parser).
pub fn corpus_attempts(event_repr: &str) -> Option<u32> {
    let i = event_repr.find("attempt")? + "attempt".len();
    let rest = &event_repr[i..];
    let digits: String = rest
        .trim_start_matches(|c: char| !c.is_ascii_digit())
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}
