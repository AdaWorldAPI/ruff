//! `ruff_charon` — the Rust intake arm of D-CODEGRAPH-0 (lance-graph
//! `.claude/plans/d-codegraph-0-resident-codegraph-v1.md`).
//!
//! Charon translates a crate to LLBC; this crate reads the LLBC through
//! `charon_lib` and reports what Charon witnessed, in three forms:
//!
//! - [`calls_text`]: one line per local function, byte-identical to
//!   `lance-graph/crates/sag-ra-probe/tools/llbc_calls.py`. That script is the
//!   reference this crate is checked against.
//! - [`relations`]: relation rows in the crai-rs vocabulary (`DEFINES`,
//!   `CALLS_RESOLVED`, `MAY_DISPATCH`, `UNRESOLVED_CALL`), each with its grade
//!   and source span, plus [`Residual`]s and a [`Receipt`] whose conservation
//!   check is `harvested == represented + residual`.
//! - [`kythe_entries`]: Kythe entries (`defines`, `ref/call`, `childof`) in the
//!   JSON shape `kythe_entries.py` writes and `entrystream` reads.
//!
//! # What a call is, here
//!
//! A call is a `Call` statement in a structured (LLBC) body. Its callee is
//! classified by how Charon resolved it:
//!
//! | Charon | Meaning | Relation |
//! |---|---|---|
//! | `FnPtrKind::Fun(id)` | a resolved function | `CALLS_RESOLVED` |
//! | `FnPtrKind::Trait` with `TraitRefKind::TraitImpl` | a known impl | `CALLS_RESOLVED` |
//! | `FnPtrKind::Trait`, any other trait-ref kind (`Clause`, `Dyn`, ...) | the impl is chosen later | `MAY_DISPATCH` |
//! | `FnOperand::Dynamic` | a function pointer or vtable slot | `UNRESOLVED_CALL` |
//!
//! A `MAY_DISPATCH` row is never a confirmed caller (crai-rs RS-F07). Nothing
//! is inferred from names.

use std::fmt::Write as _;
use std::path::Path;

use charon_lib::ast::llbc_ast::{Statement, StatementKind};
use charon_lib::ast::visitor::AstVisitable;
use charon_lib::ast::*;
use charon_lib::export::CrateData;
use charon_lib::formatter::IntoFormatter;
use charon_lib::options::SerializationFormat;
use charon_lib::pretty::FmtWithCtx;

/// A deserialized LLBC file.
pub struct Llbc {
    /// Charon reported errors: the crate description is partial.
    pub has_errors: bool,
    pub krate: TranslatedCrate,
}

/// Read an LLBC file written by `charon ... --dest-file`.
pub fn load(path: &Path) -> anyhow::Result<Llbc> {
    let data = CrateData::deserialize_from_file(path, SerializationFormat::Json)?;
    Ok(Llbc {
        has_errors: data.has_errors,
        krate: data.translated,
    })
}

/// The name of a Rust enum variant, from its `Debug` form: `Clause(..)` →
/// `Clause`, `SelfId` → `SelfId`. Used where the reference script prints the
/// variant's serialized tag.
fn variant(debug: String) -> String {
    debug
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .next()
        .unwrap_or("")
        .to_owned()
}

/// A path the way the reference script prints it: identifiers as written,
/// every other path element as its variant tag (`Impl`, `Builtin`, ...).
/// Two impl methods can print the same; [`qualified_name`] tells them apart.
pub fn path_name(name: &Name) -> String {
    name.name
        .iter()
        .map(|e| match e {
            PathElem::Ident(s, _) => s.clone(),
            other => variant(format!("{other:?}")),
        })
        .collect::<Vec<_>>()
        .join("::")
}

/// Charon's own rendering of a name, which spells impl blocks out
/// (`{impl Shape for Square}`), so distinct items print distinctly.
pub fn qualified_name(krate: &TranslatedCrate, name: &Name) -> String {
    name.to_string_with_ctx(&krate.into_fmt())
}

/// Where a callee is, as Charon resolved it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Callee {
    /// `FnPtrKind::Fun`: the function's id.
    Fun(FunDeclId),
    /// `FnPtrKind::Trait`: the trait reference's kind and the method's trait.
    Trait {
        kind: String,
        trait_id: Option<TraitDeclId>,
    },
    /// `FnOperand::Dynamic`.
    Dynamic,
}

/// A byte range in a source file, as Charon recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    pub file: String,
    pub start: usize,
    pub end: usize,
}

/// One call statement.
#[derive(Debug, Clone)]
pub struct CallSite {
    pub callee: Callee,
    pub site: Option<Site>,
}

/// What one function's body witnesses.
#[derive(Debug, Clone)]
pub struct FunFacts {
    pub id: FunDeclId,
    pub name: String,
    pub qualified: String,
    pub site: Option<Site>,
    /// `None`: the body is not structured LLBC (opaque, extern, unstructured).
    pub calls: Option<Vec<CallSite>>,
    /// Writes through a dereference rooted in an argument (caller memory; for
    /// a closure, its captured state is argument 1).
    pub deref_write_arg: usize,
    /// Writes through a dereference rooted in a local (may still alias).
    pub deref_write_local: usize,
    /// `Assert` statements: overflow, division and bounds checks that can panic.
    pub asserts: usize,
}

fn file_name(krate: &TranslatedCrate, id: FileId) -> String {
    match &krate.files[id].name {
        FileName::Virtual(p) | FileName::Local(p) => p.display().to_string(),
        FileName::NotReal(s) => s.clone(),
    }
}

/// The byte range of a span, when Charon kept the file's contents.
pub fn site(krate: &TranslatedCrate, span: Span) -> Option<Site> {
    let data = span.data();
    let source = krate.files[data.file_id].contents.as_ref()?;
    let range = data.to_byte_range(source);
    Some(Site {
        file: file_name(krate, data.file_id),
        start: range.start,
        end: range.end,
    })
}

/// The local a place is rooted in, through every projection.
fn root_local(place: &Place) -> Option<LocalId> {
    match &place.kind {
        PlaceKind::Local(id) => Some(*id),
        PlaceKind::Projection(inner, _) => root_local(inner),
        _ => None,
    }
}

fn has_deref(place: &Place) -> bool {
    match &place.kind {
        PlaceKind::Projection(inner, elem) => {
            matches!(elem, ProjectionElem::Deref) || has_deref(inner)
        }
        _ => false,
    }
}

fn callee(call: &Call) -> Callee {
    match &call.func {
        FnOperand::Regular(ptr) => match ptr.kind.as_ref() {
            FnPtrKind::Fun(id) => Callee::Fun(*id),
            FnPtrKind::Trait(tr, _) => Callee::Trait {
                kind: variant(format!("{:?}", tr.kind)),
                trait_id: Some(tr.trait_decl_ref.id),
            },
        },
        FnOperand::Dynamic(..) => Callee::Dynamic,
    }
}

/// The facts of every function whose path starts with `crate_name::`, in
/// Charon's declaration order.
pub fn fun_facts(krate: &TranslatedCrate, crate_name: &str) -> Vec<FunFacts> {
    let prefix = format!("{crate_name}::");
    let mut out = Vec::new();
    for f in krate.fun_decls.iter() {
        let name = path_name(&f.item_meta.name);
        if !name.starts_with(&prefix) {
            continue;
        }
        let mut facts = FunFacts {
            id: f.def_id,
            qualified: qualified_name(krate, &f.item_meta.name),
            name,
            site: site(krate, f.item_meta.span),
            calls: None,
            deref_write_arg: 0,
            deref_write_local: 0,
            asserts: 0,
        };
        if let Body::Structured(body) = &f.body {
            let args = body.locals.arg_count;
            let mut calls = Vec::new();
            body.body.dyn_visit(|st: &Statement| match &st.kind {
                StatementKind::Call { call, .. } => calls.push(CallSite {
                    callee: callee(call),
                    site: site(krate, st.span),
                }),
                StatementKind::Assert { .. } => facts.asserts += 1,
                StatementKind::Assign(place, _) if has_deref(place) => match root_local(place) {
                    Some(l) if l.index() <= args => facts.deref_write_arg += 1,
                    _ => facts.deref_write_local += 1,
                },
                _ => {}
            });
            facts.calls = Some(calls);
        }
        out.push(facts);
    }
    out
}

/// The output of `llbc_calls.py <file> <crate_name>`, byte for byte.
pub fn calls_text(llbc: &Llbc, crate_name: &str) -> String {
    let krate = &llbc.krate;
    let mut out = format!(
        "has_errors\t{}\n",
        if llbc.has_errors { "True" } else { "False" }
    );
    for f in fun_facts(krate, crate_name) {
        let calls: Vec<String> = f
            .calls
            .unwrap_or_default()
            .iter()
            .map(|c| match &c.callee {
                Callee::Fun(id) => {
                    let target = krate
                        .fun_decls
                        .get(*id)
                        .map(|t| path_name(&t.item_meta.name))
                        .unwrap_or_else(|| id.index().to_string());
                    format!("'static:{target}@{}'", id.index())
                }
                Callee::Trait { kind, .. } => format!("'trait:{kind}'"),
                Callee::Dynamic => "'dynamic'".to_owned(),
            })
            .collect();
        let _ = writeln!(
            out,
            "{}\tid={}\tcalls=[{}]\tderef_write_arg={}\tderef_write_local={}\tassert={}",
            f.name,
            f.id.index(),
            calls.join(", "),
            f.deref_write_arg,
            f.deref_write_local,
            f.asserts
        );
    }
    out
}

/// The witness grade of a relation, crai-rs `CR-H0-REVIEW-SEMANTICS` §2.
/// Everything this crate emits is read from compiler output.
pub const GRADE: &str = "COMPILER";

/// One relation row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    pub predicate: &'static str,
    pub subject: String,
    pub object: String,
    pub site: Option<Site>,
}

/// Something Charon gave that no relation represents, with the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Residual {
    pub reason: &'static str,
    pub subject: String,
}

/// Counts of what was read and what became of it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Receipt {
    /// Local functions plus call statements read from their bodies.
    pub harvested: usize,
    pub represented: usize,
    pub residual: usize,
}

impl Receipt {
    /// `harvested == represented + residual`: nothing was dropped silently.
    pub fn conserved(&self) -> bool {
        self.harvested == self.represented + self.residual
    }
}

/// Relation rows, residuals and the receipt for every local function.
pub fn relations(
    krate: &TranslatedCrate,
    crate_name: &str,
) -> (Vec<Relation>, Vec<Residual>, Receipt) {
    let mut rows = Vec::new();
    let mut residual = Vec::new();
    let mut receipt = Receipt::default();
    for f in fun_facts(krate, crate_name) {
        receipt.harvested += 1;
        rows.push(Relation {
            predicate: "DEFINES",
            subject: f.qualified.clone(),
            object: f.qualified.clone(),
            site: f.site.clone(),
        });
        let Some(calls) = f.calls else {
            residual.push(Residual {
                reason: "BODY_NOT_STRUCTURED",
                subject: f.qualified.clone(),
            });
            continue;
        };
        for c in calls {
            receipt.harvested += 1;
            let (predicate, object) = match &c.callee {
                Callee::Fun(id) => match krate.fun_decls.get(*id) {
                    Some(t) => ("CALLS_RESOLVED", qualified_name(krate, &t.item_meta.name)),
                    None => match krate.item_names.get(&ItemId::Fun(*id)) {
                        Some(n) => ("CALLS_RESOLVED", qualified_name(krate, n)),
                        None => {
                            residual.push(Residual {
                                reason: "CALLEE_NOT_NAMED",
                                subject: f.qualified.clone(),
                            });
                            continue;
                        }
                    },
                },
                Callee::Trait { kind, trait_id } => {
                    let tr = trait_id
                        .and_then(|t| krate.item_names.get(&ItemId::TraitDecl(t)))
                        .map(|n| qualified_name(krate, n))
                        .unwrap_or_else(|| "?".to_owned());
                    let p = if kind == "TraitImpl" {
                        "CALLS_RESOLVED"
                    } else {
                        "MAY_DISPATCH"
                    };
                    (p, format!("{tr} via {kind}"))
                }
                Callee::Dynamic => ("UNRESOLVED_CALL", "dynamic".to_owned()),
            };
            rows.push(Relation {
                predicate,
                subject: f.qualified.clone(),
                object,
                site: c.site,
            });
        }
    }
    receipt.represented = rows.len();
    receipt.residual = residual.len();
    (rows, residual, receipt)
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

fn vname(corpus: &str, signature: &str, path: &str, language: &str) -> serde_json::Value {
    serde_json::json!({"signature": signature, "corpus": corpus, "root": "", "path": path, "language": language})
}

/// Kythe entries, one JSON object per line, in the shape `kythe_entries.py`
/// writes. Per local function: a `function` node and a `defines` anchor over
/// its item span. Per call: a `ref/call` anchor over the call statement's span
/// to the callee's node, `childof` the caller. A `MAY_DISPATCH` call points
/// at the trait (Kythe's own reading of a virtual call: the declaration, not
/// an implementation). `strip` is removed from the front of every file path.
pub fn kythe_entries(
    krate: &TranslatedCrate,
    crate_name: &str,
    corpus: &str,
    strip: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut emit = |v: serde_json::Value| {
        let s = v.to_string();
        if seen.insert(s.clone()) {
            out.push(s);
        }
    };
    let fact = |source: serde_json::Value, name: &str, value: &[u8]| serde_json::json!({"source": source, "fact_name": name, "fact_value": b64(value)});
    let path_of = |s: &Site| s.file.strip_prefix(strip).unwrap_or(&s.file).to_owned();
    let anchor = |s: &Site| {
        vname(
            corpus,
            &format!("@{}:{}", s.start, s.end),
            &path_of(s),
            "rust",
        )
    };
    // One `file` node per source file, with its text: Kythe places anchors
    // through the file's text, and returns none for an anchor whose file it
    // does not hold.
    let mut files = std::collections::HashSet::new();
    let (rows, _, _) = relations(krate, crate_name);
    for r in rows {
        let Some(s) = &r.site else { continue };
        if files.insert(s.file.clone())
            && let Some(f) = krate.files.iter().find(|f| match &f.name {
                FileName::Virtual(p) | FileName::Local(p) => p.display().to_string() == s.file,
                FileName::NotReal(n) => *n == s.file,
            })
            && let Some(text) = &f.contents
        {
            let node = vname(corpus, "", &path_of(s), "");
            emit(fact(node.clone(), "/kythe/node/kind", b"file"));
            emit(fact(node, "/kythe/text", text.as_bytes()));
        }
        let a = anchor(s);
        emit(fact(a.clone(), "/kythe/node/kind", b"anchor"));
        emit(fact(
            a.clone(),
            "/kythe/loc/start",
            s.start.to_string().as_bytes(),
        ));
        emit(fact(
            a.clone(),
            "/kythe/loc/end",
            s.end.to_string().as_bytes(),
        ));
        let subject = vname(corpus, &r.subject, "", "rust");
        match r.predicate {
            "DEFINES" => {
                emit(fact(subject.clone(), "/kythe/node/kind", b"function"));
                emit(
                    serde_json::json!({"source": a, "edge_kind": "/kythe/edge/defines", "target": subject, "fact_name": "/"}),
                );
            }
            "CALLS_RESOLVED" | "MAY_DISPATCH" => {
                let target = r.object.split(" via ").next().unwrap_or(&r.object);
                let callee = vname(corpus, target, "", "rust");
                emit(
                    serde_json::json!({"source": a.clone(), "edge_kind": "/kythe/edge/ref/call", "target": callee, "fact_name": "/"}),
                );
                emit(
                    serde_json::json!({"source": a, "edge_kind": "/kythe/edge/childof", "target": subject, "fact_name": "/"}),
                );
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_takes_the_tag_only() {
        assert_eq!(variant("Clause(ClauseDbVar(0), [])".into()), "Clause");
        assert_eq!(variant("SelfId".into()), "SelfId");
        assert_eq!(variant("ItemClause { a: 1 }".into()), "ItemClause");
    }

    #[test]
    fn base64_matches_the_standard_alphabet() {
        assert_eq!(b64(b""), "");
        assert_eq!(b64(b"f"), "Zg==");
        assert_eq!(b64(b"fo"), "Zm8=");
        assert_eq!(b64(b"foo"), "Zm9v");
        assert_eq!(b64(b"function"), "ZnVuY3Rpb24=");
    }

    #[test]
    fn receipt_conservation_is_exact() {
        let ok = Receipt {
            harvested: 5,
            represented: 4,
            residual: 1,
        };
        let lost = Receipt {
            harvested: 5,
            represented: 3,
            residual: 1,
        };
        assert!(ok.conserved());
        assert!(!lost.conserved());
    }
}
