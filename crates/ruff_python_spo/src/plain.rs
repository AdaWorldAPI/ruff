//! Plain-Python arm — the non-Odoo sibling of the top-level `extract*`
//! family.
//!
//! `extract()`/`extract_with()` (see `lib.rs`) gate every class behind
//! `walk::is_model_base` / `_name` / `_inherit` — Odoo markers. A codebase
//! with none of those (the forcing case: `monarch-initiative/dismech`, 586
//! classes / 878 module-level defs / 201 dataclasses, zero Odoo markers)
//! makes `extract()` provably return an empty graph. [`extract_plain`] /
//! [`extract_plain_from_source`] drop the Odoo gate entirely: every
//! module-level `class X:` becomes a [`Model`], no base-class or
//! `_name`/`_inherit` check required.
//!
//! # What it extracts
//!
//! | Plain Python construct                  | IR field           |
//! | ---                                     | ---                |
//! | `class X:` (any bases, any decorators)  | one [`Model`], named `<module>_<X>` |
//! | `class X(Base):`                        | `Base` → [`Model::inherits`] (only `Name`/`Attribute` bases resolve; `Generic[T]`, `metaclass=` keywords, etc. do not) |
//! | `def m(self): ...` in the class body    | [`Function`] via the **same** [`crate::functions::analyze_method`] the Odoo arm uses — it is already language-generic (it only ever looks at `self`, `for`/`if`/`raise`/`Assign` statements, and a closed ORM-mutator-name list; none of that is Odoo-specific) |
//! | `def helper(): ...` at module level      | attached to a synthetic per-module [`Model`] named `<module>` (only emitted when the module has at least one top-level def) |
//! | `x: T` / `x: T = ...` (`AnnAssign`)     | a [`Field`] is ALWAYS recorded; `field_type` is the annotation's simple name (a bare `Name`, or the head of a `Subscript` — `list[str]` → `"list"`) lowercased, or `None` for a more complex annotation (union types, etc. — never guessed) |
//! | `x = <literal>` (bare `Assign`)          | a [`Field`] is recorded ONLY when the literal kind is trivially inferable (str/int/float/bool/list/dict/set/tuple); anything else (a call, a name, a binop, …) is skipped entirely — no field is invented for an assignment whose type can't be read off the RHS |
//!
//! Model names are normalised the same way the Odoo arm normalises
//! `account.move` → `account_move`: the module's dotted path (`export.
//! sepio_export`, computed from the file's path relative to the extraction
//! root) has its dots replaced with underscores before being joined to the
//! class name with `_`.
//!
//! No new [`ruff_spo_triplet::Predicate`] variants are introduced — the enum
//! is closed. Everything here reuses the existing core-7 predicate surface
//! (`rdf:type`, `has_function`, `has_field`, `field_type`, `reads_field`,
//! `writes_field`, `writes_if_blank`, `raises`, `traverses_relation`,
//! `calls`, `inherits_from`) via [`crate::to_ndjson`] / `ruff_spo_triplet::expand`
//! unchanged.

use std::fs;
use std::path::Path;

use ruff_python_ast::{Expr, Number, Stmt, StmtAssign, StmtClassDef};
use ruff_python_parser::parse_module;
use ruff_spo_triplet::{Field, Function, Model, ModelGraph};

use crate::functions::analyze_method;
use crate::name_id;

/// The IRI namespace prefix [`extract_plain_from_source`] stamps on the
/// [`ModelGraph`] it returns before [`extract_plain`] overwrites it with the
/// caller-supplied namespace.
const PLAIN_NAMESPACE: &str = "py";

/// Extract a [`ModelGraph`] from a single Python source string, treating
/// `module` as its dotted module path (e.g. `"export.sepio_export"` for
/// `export/sepio_export.py`). Every top-level `class`/`def` becomes a
/// [`Model`]/[`Function`] — see the module doc for the exact mapping.
///
/// A source that fails to parse contributes nothing (returns an empty
/// graph), mirroring the Odoo extractor's silent-skip invariant.
#[must_use]
pub fn extract_plain_from_source(source: &str, module: &str) -> ModelGraph {
    let module_prefix = module.replace('.', "_");
    let Ok(parsed) = parse_module(source) else {
        return ModelGraph::new(PLAIN_NAMESPACE);
    };

    let mut models = Vec::new();
    let mut module_functions = Vec::new();
    for stmt in &parsed.syntax().body {
        match stmt {
            Stmt::ClassDef(class) => models.push(walk_plain_class(class, &module_prefix)),
            Stmt::FunctionDef(func) => module_functions.push(analyze_method(func)),
            _ => {}
        }
    }
    if !module_functions.is_empty() {
        models.push(Model {
            name: module_prefix,
            functions: module_functions
                .into_iter()
                .map(function_from_raw)
                .collect(),
            ..Model::default()
        });
    }

    ModelGraph {
        namespace: PLAIN_NAMESPACE.to_string(),
        models,
    }
}

/// Extract a [`ModelGraph`] from a source tree (recursively reads `*.py`),
/// under the given `namespace`. Module names are derived from each file's
/// path relative to `root` (`graph.py` → `"graph"`, `export/sepio_export.py`
/// → `"export.sepio_export"`); a file whose path isn't valid UTF-8 is
/// skipped (same silent-skip posture as an unparseable file).
#[must_use]
pub fn extract_plain(root: &Path, namespace: &str) -> ModelGraph {
    let mut models = Vec::new();
    collect_plain(root, root, &mut models);
    ModelGraph {
        namespace: namespace.to_string(),
        models,
    }
}

/// Recursively collect every `*.py` file's models under `dir`.
fn collect_plain(root: &Path, dir: &Path, out: &mut Vec<Model>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_plain(root, &path, out);
        } else if path.extension().is_some_and(|e| e == "py")
            && let Ok(src) = fs::read_to_string(&path)
            && let Some(module) = module_name(root, &path)
        {
            out.extend(extract_plain_from_source(&src, &module).models);
        }
    }
}

/// The dotted module name for `file`, relative to `root`: strip the `.py`
/// extension, then join the remaining path components with `.`.
fn module_name(root: &Path, file: &Path) -> Option<String> {
    let rel = file.strip_prefix(root).ok()?.with_extension("");
    let parts: Vec<&str> = rel
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    (!parts.is_empty()).then(|| parts.join("."))
}

/// Walk one top-level `class X(Base1, Base2):` into a [`Model`].
fn walk_plain_class(class: &StmtClassDef, module_prefix: &str) -> Model {
    let mut fields = Vec::new();
    let mut functions = Vec::new();
    for stmt in &class.body {
        match stmt {
            Stmt::AnnAssign(ann) => {
                if let Some(name) = name_id(&ann.target) {
                    fields.push(Field {
                        name: name.to_string(),
                        field_type: field_type_from_annotation(&ann.annotation),
                        ..Field::default()
                    });
                }
            }
            Stmt::Assign(assign) => {
                if let Some(name) = single_name_target(assign)
                    && let Some(field_type) = field_type_from_literal(&assign.value)
                {
                    fields.push(Field {
                        name: name.to_string(),
                        field_type: Some(field_type),
                        ..Field::default()
                    });
                }
            }
            Stmt::FunctionDef(func) => functions.push(function_from_raw(analyze_method(func))),
            _ => {}
        }
    }

    let inherits = class
        .arguments
        .as_deref()
        .map(|args| args.args.iter().filter_map(base_name).collect())
        .unwrap_or_default();

    Model {
        name: format!("{module_prefix}_{}", class.name.id),
        fields,
        functions,
        inherits,
        ..Model::default()
    }
}

/// The single LHS identifier of `x = ...`, or `None` for tuple/chained
/// (`a = b = ...`) targets. Local copy of `walk::single_name_target` — that
/// one is private to its module and this arm has no Odoo-specific coupling
/// to reuse it through.
fn single_name_target(assign: &StmtAssign) -> Option<&str> {
    match assign.targets.as_slice() {
        [target] => name_id(target),
        _ => None,
    }
}

/// A base-class expression's terminal name: `Base` for `class X(Base):`,
/// `Enum` for `class X(enum.Enum):`. `None` for anything else
/// (`Generic[T]`, a call expression, …) — never guessed.
fn base_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Name(n) => Some(n.id.to_string()),
        Expr::Attribute(a) => Some(a.attr.id.to_string()),
        _ => None,
    }
}

/// The lowercased simple name of an `AnnAssign` annotation: a bare `Name`
/// (`int`, `str`), an `Attribute`'s terminal name (`dataclasses.KW_ONLY` →
/// `"kw_only"`), or a `Subscript`'s head (`list[str]` → `"list"`,
/// `typing.Optional[int]` → `"optional"`). `None` for anything more complex
/// (union types `str | None`, `Callable[[int], str]`'s parameter list, …).
fn field_type_from_annotation(annotation: &Expr) -> Option<String> {
    let head = match annotation {
        Expr::Name(n) => n.id.as_str(),
        Expr::Attribute(a) => a.attr.id.as_str(),
        Expr::Subscript(sub) => match &*sub.value {
            Expr::Name(n) => n.id.as_str(),
            Expr::Attribute(a) => a.attr.id.as_str(),
            _ => return None,
        },
        _ => return None,
    };
    Some(head.to_lowercase())
}

/// The trivially-inferable literal kind of a bare `x = <value>` RHS, or
/// `None` when the RHS isn't a literal (a call, a name, a binop, …) — in
/// which case [`walk_plain_class`] skips the field entirely rather than
/// guessing.
fn field_type_from_literal(value: &Expr) -> Option<String> {
    match value {
        Expr::StringLiteral(_) => Some("str".to_string()),
        Expr::BooleanLiteral(_) => Some("bool".to_string()),
        Expr::NumberLiteral(n) => Some(
            match n.value {
                Number::Int(_) => "int",
                Number::Float(_) => "float",
                Number::Complex { .. } => "complex",
            }
            .to_string(),
        ),
        Expr::List(_) => Some("list".to_string()),
        Expr::Dict(_) => Some("dict".to_string()),
        Expr::Set(_) => Some("set".to_string()),
        Expr::Tuple(_) => Some("tuple".to_string()),
        _ => None,
    }
}

/// Lift a `crate::RawMethod` into the frontend-agnostic [`Function`] IR —
/// the same field-for-field mapping `lib.rs::build_graph` uses for the Odoo
/// arm (this arm has no compute→depends join, so every field maps directly).
fn function_from_raw(m: crate::RawMethod) -> Function {
    Function {
        name: m.name,
        reads: m.reads,
        raises: m.raises,
        traverses: m.traverses,
        writes: m.writes,
        guarded_writes: m.guarded_writes,
        calls: m.calls,
        constrains: m.constrains,
        onchange: m.onchange,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruff_spo_triplet::expand;

    fn model<'a>(graph: &'a ModelGraph, name: &str) -> &'a Model {
        graph
            .models
            .iter()
            .find(|m| m.name == name)
            .unwrap_or_else(|| panic!("missing model {name}"))
    }

    // A dataclass with annotated fields — no Odoo markers at all.
    const DATACLASS: &str = r#"
from dataclasses import dataclass


@dataclass
class Point:
    x: int
    y: int = 0
    label: str = "origin"
"#;

    #[test]
    fn dataclass_annotated_fields_become_typed_fields() {
        let graph = extract_plain_from_source(DATACLASS, "mod");
        assert_eq!(graph.models.len(), 1);
        let m = model(&graph, "mod_Point");
        assert_eq!(m.fields.len(), 3);
        let get = |name: &str| m.fields.iter().find(|f| f.name == name).unwrap();
        assert_eq!(get("x").field_type.as_deref(), Some("int"));
        assert_eq!(get("y").field_type.as_deref(), Some("int"));
        assert_eq!(get("label").field_type.as_deref(), Some("str"));
        assert!(m.inherits.is_empty());
        assert!(m.functions.is_empty());

        // The core-7 predicates still expand unchanged — no new predicate
        // strings, no Odoo namespace leaking into a plain-Python namespace.
        let graph_ns = ModelGraph {
            namespace: "py".to_string(),
            ..graph
        };
        let t = expand(&graph_ns);
        assert!(
            t.iter().any(|tr| tr.s == "py:mod_Point"
                && tr.p == "rdf:type"
                && tr.o == "ogit:ObjectType")
        );
        assert!(
            t.iter()
                .any(|tr| tr.s == "py:mod_Point.x" && tr.p == "field_type" && tr.o == "int")
        );
    }

    // A plain (non-dataclass) class with a base and a method exercising
    // reads / writes / raises / calls.
    const PLAIN_WITH_BASE: &str = r#"
class Thing(Base):
    def process(self):
        if self.value:
            self.data = self.value
        self.data.update({"a": 1})
        raise ValueError("bad")
"#;

    #[test]
    fn plain_class_with_base_and_method_facts() {
        let graph = extract_plain_from_source(PLAIN_WITH_BASE, "mod");
        assert_eq!(graph.models.len(), 1);
        let m = model(&graph, "mod_Thing");
        assert_eq!(m.inherits, vec!["Base".to_string()]);
        assert_eq!(m.functions.len(), 1);
        let f = &m.functions[0];
        assert_eq!(f.name, "process");
        assert!(f.reads.contains(&"value".to_string()));
        assert_eq!(f.writes, vec!["data".to_string()]);
        assert_eq!(f.raises, vec!["ValueError".to_string()]);
        assert_eq!(f.calls, vec!["data.update".to_string()]);
    }

    // A module-level def with no enclosing class.
    const MODULE_LEVEL_DEF: &str = r#"
def helper(x):
    return x + 1
"#;

    #[test]
    fn module_level_def_attaches_to_synthetic_module_model() {
        let graph = extract_plain_from_source(MODULE_LEVEL_DEF, "mod");
        assert_eq!(graph.models.len(), 1);
        let m = model(&graph, "mod");
        assert_eq!(m.functions.len(), 1);
        assert_eq!(m.functions[0].name, "helper");
        assert!(m.fields.is_empty());
    }

    #[test]
    fn module_with_no_classes_or_defs_yields_empty_graph() {
        let graph = extract_plain_from_source("x = 1\n", "mod");
        assert!(graph.models.is_empty());
    }

    #[test]
    fn unparsable_source_yields_empty_graph() {
        let graph = extract_plain_from_source("class Broken(:  # not valid python\n", "mod");
        assert!(graph.models.is_empty());
    }

    #[test]
    fn bare_literal_assignment_only_recorded_when_trivially_inferable() {
        let src = r#"
class Config:
    DEBUG = True
    NAME = "x"
    COUNT = 3
    RATIO = 1.5
    TAGS = ["a"]
    OPTS = {"a": 1}
    UNIQUE = {1, 2}
    PAIR = (1, 2)
    computed = some_call()
    aliased = OtherClass
"#;
        let graph = extract_plain_from_source(src, "mod");
        let m = model(&graph, "mod_Config");
        assert_eq!(m.fields.len(), 8);
        let get = |name: &str| m.fields.iter().find(|f| f.name == name).unwrap();
        assert_eq!(get("DEBUG").field_type.as_deref(), Some("bool"));
        assert_eq!(get("NAME").field_type.as_deref(), Some("str"));
        assert_eq!(get("COUNT").field_type.as_deref(), Some("int"));
        assert_eq!(get("RATIO").field_type.as_deref(), Some("float"));
        assert_eq!(get("TAGS").field_type.as_deref(), Some("list"));
        assert_eq!(get("OPTS").field_type.as_deref(), Some("dict"));
        assert_eq!(get("UNIQUE").field_type.as_deref(), Some("set"));
        assert_eq!(get("PAIR").field_type.as_deref(), Some("tuple"));
        assert!(m.fields.iter().all(|f| f.name != "computed"));
        assert!(m.fields.iter().all(|f| f.name != "aliased"));
    }

    #[test]
    fn subscript_annotation_uses_head_and_complex_annotation_is_none() {
        let src = r#"
class Row:
    items: list[str]
    meta: "dict[str, int]"
    weird: str | None
"#;
        let graph = extract_plain_from_source(src, "mod");
        let m = model(&graph, "mod_Row");
        // Every AnnAssign gets a Field, regardless of annotation complexity.
        assert_eq!(m.fields.len(), 3);
        let get = |name: &str| m.fields.iter().find(|f| f.name == name).unwrap();
        assert_eq!(get("items").field_type.as_deref(), Some("list"));
        // A string-literal forward-reference annotation is a StringLiteral
        // expr, not a Subscript — correctly not resolved (never guessed).
        assert_eq!(get("meta").field_type, None);
        // `str | None` is a BinOp, not a Name/Attribute/Subscript head.
        assert_eq!(get("weird").field_type, None);
    }

    #[test]
    fn module_path_normalises_dots_to_underscores_in_model_name() {
        let graph = extract_plain_from_source("class Foo:\n    pass\n", "export.sepio_export");
        assert_eq!(graph.models.len(), 1);
        assert_eq!(graph.models[0].name, "export_sepio_export_Foo");
    }

    #[test]
    fn extract_plain_walks_a_tree_and_derives_module_names_from_paths() {
        let dir =
            std::env::temp_dir().join(format!("ruff_python_spo_plain_test_{}", std::process::id()));
        let sub = dir.join("export");
        std::fs::create_dir_all(&sub).expect("create tmp tree");
        std::fs::write(dir.join("graph.py"), "class Graph:\n    pass\n").expect("write graph.py");
        std::fs::write(sub.join("sepio_export.py"), "class Exporter:\n    pass\n")
            .expect("write sepio_export.py");

        let graph = extract_plain(&dir, "py");
        let names: Vec<&str> = graph.models.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"graph_Graph"));
        assert!(names.contains(&"export_sepio_export_Exporter"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
