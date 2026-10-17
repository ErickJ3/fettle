use serde::{Deserialize, Serialize};

use crate::ResourceKey;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Create,
    Update,
    Remove,
    Forget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceChange {
    #[serde(flatten)]
    pub key: ResourceKey,
    pub action: Action,
    /// Printable text chosen by the resource, such as `+2 −1 lines`.
    pub detail: Option<String>,
}

pub type Plan = Vec<ResourceChange>;

/// A failed write stops its host, so a report holds at most one failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyReport {
    pub applied: Vec<ResourceChange>,
    pub failed: Option<Failure>,
    pub skipped: Vec<ResourceKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    #[serde(flatten)]
    pub key: ResourceKey,
    pub error: String,
}
