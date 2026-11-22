//! The order lane: `<ns>.events.tsv`.
//!
//! The structural rows are a sorted set and cannot say which of two callbacks
//! runs first. This lane keeps what the Ruby frontend's ordered walk
//! (`ruff_ruby_spo::events`) produced, grouped by scope and, inside a scope,
//! in the order the walk produced it. It is never sorted by value and never
//! de-duplicated: the same event twice is two rows.
//!
//! `DECL` rows are the declarations of a class body, with the class as scope
//! (`ns::Name`), or of a module body and its `included do` block, with the
//! module as scope (`ns::module:Name`).
//! `BODY` rows are the events of a method body, with the method as scope.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use ruff_ruby_spo::Declaration;
use ruff_ruby_spo::events::{ClassOre, OreKind, Symbols, source_ore};

use crate::cell;
use crate::error::Error;
use crate::rows::{
    assoc_macro, attr_macro, concern_macro, gem_macro, scope_macro, validation_macro,
};

pub const VERSION: u32 = 1;
pub const SCHEMA: &str = "scope\tseq\tlane\tkind\tsubject\tobject\tanchor";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    Decl,
    Body,
}

impl Lane {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Decl => "DECL",
            Self::Body => "BODY",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRow {
    pub seq: u32,
    pub lane: Lane,
    pub kind: String,
    pub subject: String,
    pub object: String,
    pub anchor: u32,
}

/// The events of every scope, scopes sorted, each scope's rows in the order
/// the walk produced them.
#[derive(Debug, Default)]
pub struct Events {
    scopes: BTreeMap<String, Vec<EventRow>>,
}

impl Events {
    /// Walk one Ruby source and add its classes. A scope met twice (a class
    /// reopened in a later file) keeps the earlier rows first.
    pub fn add_source(&mut self, namespace: &str, source: &str) -> Result<(), Error> {
        let mut symbols = Symbols::default();
        let classes = source_ore(source, namespace, &mut symbols);
        let names: BTreeMap<String, String> = symbols
            .rows()
            .map(|(id, _, _, name, _)| (id, name.to_owned()))
            .collect();
        for class in &classes {
            self.add_class(namespace, class, &names)?;
        }
        Ok(())
    }

    fn add_class(
        &mut self,
        namespace: &str,
        class: &ClassOre,
        names: &BTreeMap<String, String>,
    ) -> Result<(), Error> {
        // A class keeps `ns::Name`. A module is `ns::module:Name`, the same
        // shape as the structural lane's `ns::file:<path>`, so a module never
        // shares a scope string with a class of the same name.
        let scope = match class.kind {
            OreKind::Class => format!("{namespace}::{}", cell::name("class", &class.name)?),
            OreKind::Module => {
                format!("{namespace}::module:{}", cell::name("module", &class.name)?)
            }
        };
        let resolve = |id: Option<&String>| -> String {
            id.map_or_else(
                || "-".to_owned(),
                |id| cell::text(names.get(id).map_or(id.as_str(), String::as_str)),
            )
        };
        for event in &class.declarations {
            let (subject, object) = decl_cells(&event.decl);
            self.push(
                &scope,
                EventRow {
                    seq: event.seq,
                    lane: Lane::Decl,
                    kind: event.kind.as_str().to_owned(),
                    subject: cell::text(&subject),
                    object: cell::text(&object),
                    anchor: event.anchor,
                },
            );
        }
        for method in &class.methods {
            let scope = format!("{scope}::{}", cell::name("method", &method.name)?);
            for event in &method.events {
                self.push(
                    &scope,
                    EventRow {
                        seq: event.seq,
                        lane: Lane::Body,
                        kind: event.kind.as_str().to_owned(),
                        subject: resolve(event.subj.as_ref()),
                        object: resolve(event.obj.as_ref()),
                        anchor: event.anchor,
                    },
                );
            }
        }
        Ok(())
    }

    fn push(&mut self, scope: &str, row: EventRow) {
        self.scopes.entry(scope.to_owned()).or_default().push(row);
    }

    /// Rows per lane.
    #[must_use]
    pub fn counts(&self) -> BTreeMap<String, u32> {
        let mut counts = BTreeMap::new();
        for row in self.scopes.values().flatten() {
            let n = counts.entry(row.lane.as_str().to_owned()).or_insert(0u32);
            *n += 1;
        }
        counts
    }

    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!("#version {VERSION}\n#schema {SCHEMA}\n");
        for (scope, rows) in &self.scopes {
            for r in rows {
                let _ = writeln!(
                    out,
                    "{scope}\t{}\t{}\t{}\t{}\t{}\t{}",
                    r.seq,
                    r.lane.as_str(),
                    r.kind,
                    r.subject,
                    r.object,
                    r.anchor
                );
            }
        }
        out
    }
}

/// A short spelling of a declaration: the macro (or phase) and its target.
fn decl_cells(decl: &Declaration) -> (String, String) {
    match decl {
        Declaration::Association(a) => (assoc_macro(a.kind).to_owned(), a.name.clone()),
        Declaration::Validation(v) => (validation_macro(v.kind).to_owned(), v.target.clone()),
        Declaration::Callback(c) => (c.phase.clone(), c.target.clone()),
        Declaration::Concern(c) => (concern_macro(c.kind).to_owned(), c.module.clone()),
        Declaration::Attribute(a) => (attr_macro(a.kind).to_owned(), a.name.clone()),
        Declaration::EnumForm(e) => ("enum".to_owned(), e.column.clone()),
        Declaration::Delegation(d) => (
            "delegate".to_owned(),
            format!("{}:{}", d.methods.join(","), d.to),
        ),
        Declaration::Scope(s) => (scope_macro(s.kind).to_owned(), s.name.clone()),
        Declaration::ActsAs(a) => ("acts_as".to_owned(), a.variant.clone()),
        Declaration::DslCall(d) => (d.name.clone(), d.args.clone()),
        Declaration::GemDsl(g) => (gem_macro(g.gem).to_owned(), g.args.clone()),
        Declaration::DynamicMethod(d) => ("define_method".to_owned(), d.name_expr.clone()),
        Declaration::Using(u) => ("using".to_owned(), u.refinement_module.clone()),
        Declaration::Sti(s) => (
            "sti".to_owned(),
            s.inherits_from.clone().unwrap_or_default(),
        ),
    }
}
