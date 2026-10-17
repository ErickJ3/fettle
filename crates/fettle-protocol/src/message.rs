use serde::{Deserialize, Serialize};

use crate::{ApplyReport, Catalog, Facts, Plan};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliMessage {
    Catalog(Catalog),
    Apply,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMessage {
    Facts(Facts),
    Plan(Plan),
    Report(ApplyReport),
}
