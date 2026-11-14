//! Serialized parse result, shaped exactly like the dictionaries the Python
//! parsers emit (`parsers/base.py`, `model.py`): absent optional keys stay
//! absent, `summary` and `crux` are always present and may be null.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ParseResult {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub refs: Vec<Reference>,
    /// True only when Ruff parses the source cleanly into a Python module
    /// without pre-parse refusals (BOM, NUL, or CPython parser limits).
    #[serde(default)]
    pub syntax_valid: bool,
}

impl ParseResult {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("parse results are plain serializable data")
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Node {
    pub id: String,
    pub kind: String,
    pub name: String,
    /// Absent on `env` nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub language: String,
    pub source_digest: String,
    pub summary: Option<String>,
    pub crux: Option<String>,
    pub metadata: Metadata,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Metadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Edge {
    pub source: String,
    pub target: String,
    pub relation: String,
    pub confidence: String,
    pub evidence: Evidence,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Evidence {
    pub path: String,
    pub source_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
}

/// An unresolved cross-file reference; `kind` is `module`, `name` or `path`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Reference {
    pub from: String,
    pub relation: String,
    pub kind: String,
    pub value: String,
    pub line: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scoped: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method_call: Option<bool>,
}
