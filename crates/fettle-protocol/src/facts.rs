use serde::{Deserialize, Serialize};

/// Facts the agent reads from its host, shaped as the host callback reads them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Facts {
    pub os: Os,
    pub arch: String,
    pub hostname: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Os {
    pub family: String,
    pub id: String,
    /// `None` for distributions that report no version, such as Debian testing.
    pub version: Option<String>,
}
