//! Conservation: what the source graph held against what the rows say.
//!
//! The two sides are computed by separate code. [`seen`] reads the
//! [`ModelGraph`] and the import edges and never looks at a row; [`tally`]
//! reads the rows and never looks at the graph. A projection whose sides
//! differ is refused before anything is written.
//!
//! Both sides count distinct facts, because rows are a set: the same
//! `before_save :a` declared twice is two declarations and one row. The
//! event lane is where the repeat is kept.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use ruff_spo_triplet::{AttrKind, ModelGraph};
use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::imports::Edges;
use crate::rows::{Built, Family, Reason, Row};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Balance {
    pub rows: u32,
    pub seen: u32,
}

/// Counts from the source side, by family.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Seen {
    pub associations: u32,
    pub callbacks: u32,
    pub calls: u32,
    pub fields: u32,
    pub functions: u32,
    pub unnamed_callback_targets: u32,
    pub validations: u32,
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Count the distinct declarations of `graph` by family, without a row.
#[must_use]
pub fn seen(graph: &ModelGraph) -> Seen {
    let mut associations = BTreeSet::new();
    let mut callbacks = BTreeSet::new();
    let mut calls = BTreeSet::new();
    let mut fields = BTreeSet::new();
    let mut functions = BTreeSet::new();
    let mut unnamed = BTreeSet::new();
    let mut validations = HashSet::new();
    for model in &graph.models {
        let m = model.name.as_str();
        for f in model.functions.iter().chain(&model.helpers) {
            functions.insert((m, f.name.as_str()));
            for call in &f.calls {
                calls.insert((m, f.name.as_str(), call.as_str()));
            }
        }
        for field in &model.fields {
            fields.insert((m, field.name.as_str()));
        }
        for attribute in model
            .attributes
            .iter()
            .filter(|a| a.kind == AttrKind::Attribute)
        {
            fields.insert((m, attribute.name.as_str()));
        }
        for assoc in &model.associations {
            associations.insert((m, assoc.name.as_str()));
        }
        for validation in &model.validations {
            validations.insert((m, validation.kind, validation.target.as_str()));
        }
        for callback in &model.callbacks {
            callbacks.insert((m, callback.phase.as_str(), callback.target.as_str()));
            let defined = model
                .functions
                .iter()
                .chain(&model.helpers)
                .any(|f| f.name == callback.target);
            if !defined {
                unnamed.insert((m, callback.phase.as_str(), callback.target.as_str()));
            }
        }
    }
    Seen {
        associations: count(associations.len()),
        callbacks: count(callbacks.len()),
        calls: count(calls.len()),
        fields: count(fields.len()),
        functions: count(functions.len()),
        unnamed_callback_targets: count(unnamed.len()),
        validations: count(validations.len()),
    }
}

/// Counts from the row side: rows by family and predicate, residuals by reason.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tally {
    pub associations: u32,
    pub callbacks: u32,
    pub calls: u32,
    pub callee_residuals: u32,
    pub fields: u32,
    pub functions: u32,
    pub imports: u32,
    pub reference_residuals: u32,
    pub unresolved_calls: u32,
    pub validations: u32,
}

#[must_use]
pub fn tally(rows: &[Row], residuals: &[crate::rows::Residual]) -> Tally {
    let mut t = Tally::default();
    for row in rows {
        match (row.family, row.predicate) {
            (Family::Association, "ASSOCIATION") => t.associations += 1,
            (Family::Callback, "CALLBACK") => t.callbacks += 1,
            (Family::Call, "CALLS_NAMED") => t.calls += 1,
            (Family::Call, "UNRESOLVED_CALL") => {
                t.calls += 1;
                t.unresolved_calls += 1;
            }
            (Family::Field, "DEFINES") => t.fields += 1,
            (Family::Function, "DEFINES") => t.functions += 1,
            (Family::Import, "IMPORTS") => t.imports += 1,
            (Family::Validation, "VALIDATES") => t.validations += 1,
            _ => {}
        }
    }
    for residual in residuals {
        match residual.reason {
            Reason::CalleeNotNamed => t.callee_residuals += 1,
            Reason::ReferenceNotNamed => t.reference_residuals += 1,
        }
    }
    t
}

/// Compare the two sides. `graph` is `Some` when the structural lane ran and
/// `edges` is `Some` when the imports lane ran; a lane that did not run is
/// not in the result. Returns every balance, or an error naming every one that
/// does not hold.
pub fn check(
    graph: Option<&ModelGraph>,
    edges: Option<&Edges>,
    built: &Built,
) -> Result<BTreeMap<String, Balance>, Error> {
    let t = tally(&built.rows, &built.residuals);
    let mut balances = BTreeMap::new();
    if let Some(graph) = graph {
        let s = seen(graph);
        for (name, seen, rows) in [
            ("associations", s.associations, t.associations),
            ("callbacks", s.callbacks, t.callbacks),
            ("calls", s.calls, t.calls),
            ("fields", s.fields, t.fields),
            ("functions", s.functions, t.functions),
            ("validations", s.validations, t.validations),
            // Every callback whose target is not a function of its model has a residual.
            (
                "callback_targets_not_named",
                s.unnamed_callback_targets,
                t.reference_residuals,
            ),
            // Every call left unresolved has a residual.
            ("calls_not_named", t.unresolved_calls, t.callee_residuals),
        ] {
            balances.insert(name.to_owned(), Balance { rows, seen });
        }
    }
    if let Some(edges) = edges {
        balances.insert(
            "imports".to_owned(),
            Balance {
                rows: t.imports,
                seen: count(edges.len()),
            },
        );
    }
    let broken: Vec<String> = balances
        .iter()
        .filter(|(_, b)| b.rows != b.seen)
        .map(|(name, b)| format!("{name}: {} seen, {} rows", b.seen, b.rows))
        .collect();
    if broken.is_empty() {
        Ok(balances)
    } else {
        Err(Error::Conservation(broken))
    }
}

/// Declarations the Rails frontend delivers that this projection writes no
/// row for, by family. The loss is stated, not hidden.
#[must_use]
pub fn not_projected(graph: &ModelGraph) -> BTreeMap<String, u32> {
    let mut out = BTreeMap::new();
    let mut add = |name: &str, n: usize| {
        if n > 0 {
            *out.entry(name.to_owned()).or_insert(0) += count(n);
        }
    };
    for model in &graph.models {
        add(
            "attributes",
            model
                .attributes
                .iter()
                .filter(|a| a.kind != AttrKind::Attribute)
                .count(),
        );
        add("enum_forms", model.enum_forms.len());
        add("acts_as", model.acts_as.len());
        add("gem_dsl", model.gem_dsl.len());
        add("dynamic_methods", model.dynamic_methods.len());
        add("refinements", model.refinements.len());
    }
    out
}
