//! Durable GitHub poll records for ProductAtomicV1.
//!
//! These values contain validated API response snapshots. They are not
//! authority: ProductStore checks the configured connector and repository
//! again when the snapshot is admitted.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductDelivery {
    pub delivery_id: String,
    pub connector_id: String,
    pub repository_id: String,
    pub issue_id: String,
    pub observed_updated_at: String,
    pub version_digest: String,
    pub task_kind: String,
    pub payload: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueDelivery {
    pub delivery: ProductDelivery,
    pub state: String,
    pub retry_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductPollCursor {
    pub connector_id: String,
    pub query_fingerprint: String,
    pub sweep_id: String,
    pub next_page: u32,
    pub page_ceiling: u32,
    pub updated_at: String,
}
