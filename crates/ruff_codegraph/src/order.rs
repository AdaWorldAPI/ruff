//! The order question, answered from the order lane alone.

use std::path::Path;

use crate::error::Error;
use crate::events::{SCHEMA, VERSION};
use crate::fsio;
use crate::receipt::{PRESENT, Receipt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    /// Every occurrence of `a` comes before every occurrence of `b`.
    Before,
    /// Every occurrence of `a` comes after every occurrence of `b`.
    After,
    /// The store cannot say.
    Unknown,
}

impl Relation {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::After => "after",
            Self::Unknown => "unknown",
        }
    }
}

/// Does `a` come before `b` inside `scope`?
///
/// A name matches an event when it equals the event's subject or object cell.
/// The answer is `Unknown` when the receipt says the order lane was not
/// written, when either name matches no event of the scope, when the two names
/// are the same, and when the occurrences interleave. The structural rows are
/// never consulted: they hold no order, so a guess from them would be a guess.
///
/// # Errors
/// The receipt or the events file is missing or is not what this crate writes.
pub fn relation(
    store_dir: &Path,
    namespace: &str,
    scope: &str,
    a: &str,
    b: &str,
) -> Result<Relation, Error> {
    if namespace.is_empty() || namespace.contains(['/', '\\']) {
        return Err(Error::Store {
            path: store_dir.display().to_string(),
            message: format!("namespace {namespace:?} is not a file stem"),
        });
    }
    let receipt_path = store_dir.join(format!("{namespace}.receipt.json"));
    let receipt: Receipt =
        serde_json::from_str(&fsio::read_text(&receipt_path)?).map_err(|e| Error::Store {
            path: receipt_path.display().to_string(),
            message: e.to_string(),
        })?;
    if receipt.lanes.get("order").map(String::as_str) != Some(PRESENT) {
        return Ok(Relation::Unknown);
    }

    let events_path = store_dir.join(format!("{namespace}.events.tsv"));
    let text = fsio::read_text(&events_path)?;
    let bad = |message: String| Error::Store {
        path: events_path.display().to_string(),
        message,
    };
    let mut lines = text.lines();
    if lines.next() != Some(format!("#version {VERSION}").as_str()) {
        return Err(bad(format!("first line is not `#version {VERSION}`")));
    }
    if lines.next() != Some(format!("#schema {SCHEMA}").as_str()) {
        return Err(bad("second line is not the events schema".to_owned()));
    }
    let (mut at_a, mut at_b) = (Vec::new(), Vec::new());
    let mut position = 0usize;
    for (i, line) in lines.enumerate() {
        let cells: Vec<&str> = line.split('\t').collect();
        let [row_scope, _seq, _lane, _kind, subject, object, _anchor] = cells[..] else {
            return Err(bad(format!("line {}: expected 7 cells", i + 3)));
        };
        if row_scope != scope {
            continue;
        }
        if subject == a || object == a {
            at_a.push(position);
        }
        if subject == b || object == b {
            at_b.push(position);
        }
        position += 1;
    }
    let (Some(&first_a), Some(&last_a), Some(&first_b), Some(&last_b)) =
        (at_a.first(), at_a.last(), at_b.first(), at_b.last())
    else {
        return Ok(Relation::Unknown);
    };
    Ok(if a == b {
        Relation::Unknown
    } else if last_a < first_b {
        Relation::Before
    } else if last_b < first_a {
        Relation::After
    } else {
        Relation::Unknown
    })
}
