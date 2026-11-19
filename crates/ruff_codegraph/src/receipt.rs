//! `<ns>.receipt.json`: what a projection wrote and what it accounts for.
//!
//! Deterministic: keys are sorted, there is no timestamp and no absolute path.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::account::Balance;

pub const VERSION: u32 = 1;

/// The only direction this crate knows. Nothing here reads a store back into a
/// `ModelGraph`.
pub const DIRECTION: &str = "ruff->store";

pub const PRESENT: &str = "present";
pub const ABSENT: &str = "absent";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cardinality {
    /// Event rows per lane; a lane that did not run is not listed.
    pub events: BTreeMap<String, u32>,
    /// Residual rows per reason; both reasons this crate writes are always listed.
    pub residuals: BTreeMap<String, u32>,
    /// Relation rows per predicate.
    pub rows: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// FNV-1a 64 over the sorted `path<TAB>digest<LF>` lines of `files.tsv`.
    pub digest: String,
    pub files: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub cardinality: Cardinality,
    pub conservation: BTreeMap<String, Balance>,
    pub direction: String,
    /// The format version of each file written.
    pub formats: BTreeMap<String, u32>,
    /// FNV-1a 64 of the imports JSON when that lane ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imports_fnv64: Option<String>,
    /// `present` or `absent` for `structural`, `order` and `imports`.
    pub lanes: BTreeMap<String, String>,
    pub namespace: String,
    /// Declarations the frontend delivered that have no row, by family.
    pub not_projected: BTreeMap<String, u32>,
    pub snapshot: Snapshot,
}

impl Receipt {
    #[must_use]
    pub fn render(&self) -> String {
        // Serializing plain structs and string-keyed maps cannot fail.
        let mut text = serde_json::to_string_pretty(self).unwrap_or_default();
        text.push('\n');
        text
    }
}

#[must_use]
pub fn lane(present: bool) -> String {
    (if present { PRESENT } else { ABSENT }).to_owned()
}
