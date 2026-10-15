use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);
    };
}

macro_rules! number_id {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub u64);
    };
}

string_id!(OrderId);
number_id!(Attempt);
number_id!(RunId);
number_id!(Generation);
number_id!(Epoch);
string_id!(ArtifactLocation);
string_id!(ImmutableCommit);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionIntent {
    pub order: OrderId,
    pub attempt: Attempt,
    pub run: RunId,
    pub activation: u64,
    pub generation: Generation,
    pub epoch: Epoch,
    pub external_ref: String,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivationRef {
    pub node: String,
    pub inputs_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionOutcome {
    pub intent_key: (OrderId, Attempt, RunId),
    pub activation: u64,
    pub cursor: Option<String>,
    pub next_activation: Option<ActivationRef>,
    pub receipts: Vec<Receipt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmitReceipt {
    pub key: ArtifactTupleKey,
    pub event: String,
    pub at: DateTime<Utc>,
}

impl EmitReceipt {
    pub fn digest(&self) -> String {
        receipt_digest(
            "fx-emit-",
            [
                self.key.order.0.as_str(),
                &self.key.attempt.0.to_string(),
                &self.key.run.0.to_string(),
                &self.key.generation.0.to_string(),
                self.event.as_str(),
                &self.at.to_rfc3339(),
            ],
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromotionReceipt {
    pub tuple: ArtifactTupleKey,
    pub promoted_at: DateTime<Utc>,
}

impl PromotionReceipt {
    pub fn digest(&self) -> String {
        receipt_digest(
            "fx-promo-",
            [
                self.tuple.order.0.as_str(),
                &self.tuple.attempt.0.to_string(),
                &self.tuple.run.0.to_string(),
                &self.tuple.generation.0.to_string(),
                &self.promoted_at.to_rfc3339(),
            ],
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionReceipt {
    pub session_id: String,
    pub order: OrderId,
    pub run: RunId,
    pub restored_from: Option<String>,
    pub at: DateTime<Utc>,
}

impl SessionReceipt {
    pub fn digest(&self) -> String {
        receipt_digest(
            "fx-sess-",
            [
                self.session_id.as_str(),
                self.order.0.as_str(),
                &self.run.0.to_string(),
                if self.restored_from.is_some() { "some" } else { "none" },
                self.restored_from.as_deref().unwrap_or(""),
                &self.at.to_rfc3339(),
            ],
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "receipt", rename_all = "snake_case", deny_unknown_fields)]
pub enum Receipt {
    /// Complete durable worker output; this receipt does not choose graph routing.
    Host(crate::host::HostResult),
    Emit(EmitReceipt),
    Promotion(PromotionReceipt),
    Session(SessionReceipt),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactTupleKey {
    pub order: OrderId,
    pub attempt: Attempt,
    pub run: RunId,
    pub generation: Generation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactTuple {
    pub order: OrderId,
    pub attempt: Attempt,
    pub run: RunId,
    pub generation: Generation,
    pub immutable_commit: ImmutableCommit,
    pub artifact_location: ArtifactLocation,
}

impl ArtifactTuple {
    pub fn key(&self) -> ArtifactTupleKey {
        ArtifactTupleKey {
            order: self.order.clone(),
            attempt: self.attempt,
            run: self.run,
            generation: self.generation,
        }
    }

    /// Hashes all fields in order with 64-bit big-endian byte-length prefixes.
    pub fn digest(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(b"foundry-artifact-tuple-v1");
        for field in [
            self.order.0.as_bytes(),
            &self.attempt.0.to_be_bytes(),
            &self.run.0.to_be_bytes(),
            &self.generation.0.to_be_bytes(),
            self.immutable_commit.0.as_bytes(),
            self.artifact_location.0.as_bytes(),
        ] {
            hash.update((field.len() as u64).to_be_bytes());
            hash.update(field);
        }
        format!("{:x}", hash.finalize())
    }
}

fn receipt_digest<'a>(prefix: &str, fields: impl IntoIterator<Item = &'a str>) -> String {
    let mut hash = Sha256::new();
    hash.update(b"foundry-exec-receipt-v1");
    hash.update((prefix.len() as u64).to_be_bytes());
    hash.update(prefix.as_bytes());
    for field in fields {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    format!("{prefix}{:x}", hash.finalize())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FencingToken {
    pub epoch: Epoch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FenceVerdict {
    Current,
    Stale(Epoch),
}

/// Future epochs are treated as Current; this core verdict has no Future variant.
pub fn fence(current: Epoch, presented: Epoch) -> FenceVerdict {
    if presented < current {
        FenceVerdict::Stale(presented)
    } else {
        FenceVerdict::Current
    }
}
