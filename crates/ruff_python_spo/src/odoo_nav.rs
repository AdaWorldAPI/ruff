//! The navigation-topology harvest — the **Odoo Klickweg** (TWO shapes).
//!
//! # What this is
//!
//! The Odoo analog of the Flask/Django ([`crate::navigation`], ruff #63),
//! Rails (`ruff_ruby_spo`, ruff #62) and `WinForms` (`ruff_csharp_spo`, ruff
//! #61) `navigates_to` arms: it emits the shared [`Predicate::NavigatesTo`]
//! fact — `(source_screen, navigates_to, target_screen)` — feeding the same
//! `lance_graph_contract` screen-connectivity check
//! (`nav_is_fully_connected`).
//!
//! # Why Odoo needs its OWN shapes (the Flask arm is structurally blind here)
//!
//! Odoo's web client is a single-page app: business code never calls
//! `url_for` / `reverse` / `redirect` (measured on the real `account` addon:
//! 2 stray hits, all noise). Navigation is expressed as **window actions**
//! (`ir.actions.act_window`), and — like Rails, unlike Flask — it splits
//! across two artifacts:
//!
//! - **Shape A (code-side, `.py`)** — an `action_*`-style method returns a
//!   dict literal `{'type': 'ir.actions.act_window', 'res_model': '<target>',
//!   …}`. The edge is *(defining model → `res_model`)* — "a button on this
//!   model's screen opens that model's screen". 42 such dicts in the real
//!   `account` addon.
//! - **Shape B (data-side, `.xml`)** — a `<record model=
//!   "ir.actions.act_window">` declares `res_model`, and a `<menuitem
//!   action="…"/>` wires it into the menu. The edge is *(`menu` →
//!   `res_model`)* — the synthetic `menu` root screen, giving the
//!   connectivity check its entry points (18 action-carrying view files +
//!   52 menuitems in `account`).
//!
//! # Tier + doctrine (Inferred, closed-vocab, honest denominator)
//!
//! Shape A is **AST-based** like [`crate::navigation`] (parse with
//! `ruff_python_parser`, walk with the [`Visitor`]); Shape B is a stateful
//! line scanner in the `ruff_ruby_spo` ERB style (the XML surface is flat
//! `<record>/<field>/<menuitem>` markup — no XML dependency warranted). Both
//! shapes resolve targets against a caller-supplied closed vocabulary
//! ([`NavVocab::screens`]); every `act_window` occurrence seen — vocab member
//! or not — counts in the honest denominator
//! ([`OdooNavScanReport::raw_act_window_refs`]).
//!
//! Model names are normalized `.` → `_` (`account.move` → `account_move`),
//! matching the corpus IRI convention (`odoo:account_move`). Matching is
//! EXACT (no singular/plural tolerance): Odoo model names are canonical
//! identifiers, not route stems — `account_move` vs `account_moves` would be
//! two different models, so the Flask arm's plural tolerance would be a
//! false-positive source here, not a convenience.
//!
//! # What is NOT captured (by design, not oversight)
//!
//! - **Dynamic `res_model`** — `{'res_model': self._name}` or any non-literal
//!   value: skipped (the dict still counts in the denominator), mirroring
//!   every sibling arm's dynamic-target exclusion.
//! - **XML actions not wired to a menuitem** — a window action referenced only
//!   from a `%(xml_id)d` button or returned indirectly is not a *menu* entry
//!   point; its code-side counterpart is Shape A's job. Unwired records still
//!   count in the denominator.
//! - **`ir.actions.server` / client actions / URL actions** — different
//!   action families with different semantics; adding them is a deliberate
//!   vocabulary extension, not a default.

use std::fs;
use std::path::{Path, PathBuf};

use ruff_python_ast::visitor::{Visitor, walk_expr, walk_stmt};
use ruff_python_ast::{Expr, Stmt};
use ruff_spo_triplet::{Predicate, Provenance, Triple};

use crate::expr_str;
use crate::navigation::NavVocab;

/// One Odoo navigation edge: `source` screen navigates to `target` screen.
///
/// `source`/`target` are normalized model names (`account_move`), or the
/// synthetic `"menu"` root for Shape-B menu entry points. `via` records the
/// harvest shape for provenance: `"act_window_return"` (code-side dict
/// return) or `"menuitem"` (data-side XML join).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OdooNavEdge {
    /// The screen the edge originates from (normalized model name, or `menu`).
    pub source: String,
    /// The screen the edge opens (normalized `res_model`).
    pub target: String,
    /// Source file path relative to the scanned root (stable id, `/`-joined).
    pub file: String,
    /// The harvest shape: `"act_window_return"` / `"menuitem"`.
    pub via: String,
}

impl OdooNavEdge {
    /// Lift this edge into the shared `navigates_to` SPO triple, namespaced
    /// (`<ns>:<source>` → `<ns>:<target>`), `Inferred` tier — the same fact
    /// shape the Flask, Rails and C# arms emit.
    #[must_use]
    pub fn to_triple(&self, namespace: &str) -> Triple {
        Triple::new(
            format!("{namespace}:{}", self.source),
            Predicate::NavigatesTo,
            format!("{namespace}:{}", self.target),
            Provenance::Inferred,
        )
    }
}

/// Conservation-ledger totals for an Odoo nav scan (the honest-denominator
/// discipline — nothing drops silently).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OdooNavScanReport {
    /// Every `*.py` file found under the scanned root.
    pub py_files: usize,
    /// Every `*.xml` file found under the scanned root.
    pub xml_files: usize,
    /// Every `act_window` occurrence seen — code-side dict literals plus
    /// data-side `<record>` declarations — regardless of whether it produced
    /// an edge. The honest denominator.
    pub raw_act_window_refs: usize,
    /// Files (py or xml) that produced at least one [`OdooNavEdge`].
    pub files_with_edges: usize,
}

/// Scan `<root>` for Odoo navigation edges (both shapes) and return the
/// known-screen edges. Thin wrapper over [`extract_odoo_nav_edges_with_report`].
#[must_use]
pub fn extract_odoo_nav_edges(root: &Path, vocab: &NavVocab) -> Vec<OdooNavEdge> {
    extract_odoo_nav_edges_with_report(root, vocab).0
}

/// Like [`extract_odoo_nav_edges`] but also returns the
/// [`OdooNavScanReport`] ledger.
///
/// Shape A walks every `*.py` under `root` (parse-failure files silently
/// skipped — the crate invariant); Shape B scans every `*.xml`. Results are
/// deduped by `(source, target, via)` and sorted deterministically.
#[must_use]
pub fn extract_odoo_nav_edges_with_report(
    root: &Path,
    vocab: &NavVocab,
) -> (Vec<OdooNavEdge>, OdooNavScanReport) {
    let mut report = OdooNavScanReport::default();
    let mut edges: Vec<OdooNavEdge> = Vec::new();

    let mut py_files = Vec::new();
    let mut xml_files = Vec::new();
    collect_files(root, &mut py_files, &mut xml_files);

    // ── Shape A: code-side act_window dict returns ──
    for path in &py_files {
        report.py_files += 1;
        let Ok(source) = fs::read_to_string(path) else {
            continue;
        };
        let Ok(parsed) = ruff_python_parser::parse_module(&source) else {
            continue;
        };
        let rel = relative_path(root, path);
        let mut walker = OdooNavWalker {
            file: rel,
            vocab,
            class_model: None,
            edges: Vec::new(),
            raw_act_window_refs: 0,
        };
        walker.visit_body(&parsed.syntax().body);
        report.raw_act_window_refs += walker.raw_act_window_refs;
        if !walker.edges.is_empty() {
            report.files_with_edges += 1;
        }
        edges.append(&mut walker.edges);
    }

    // ── Shape B: data-side XML act_window records + menuitem wiring ──
    //
    // TWO passes over the whole XML set, because real addons split the
    // artifacts across files: the action `<record>`s live in per-model view
    // files while the `<menuitem>`s live in a central menu file
    // (`account_menuitem.xml` in the real account addon). A per-file join
    // would find zero menu edges — measured before this cross-file fix.
    let mut xml_contents: Vec<(String, String)> = Vec::new();
    for path in &xml_files {
        report.xml_files += 1;
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        xml_contents.push((relative_path(root, path), content));
    }
    // Pass 1 — every act_window record across all files: (xml_id → res_model).
    let mut actions: Vec<(String, Option<String>)> = Vec::new();
    for (_, content) in &xml_contents {
        collect_act_window_records(content, &mut actions);
    }
    report.raw_act_window_refs += actions.len();
    // Pass 2 — menuitems join against the global action map.
    for (rel, content) in &xml_contents {
        let mut file_edges = scan_menuitems(content, rel, &actions, vocab);
        if !file_edges.is_empty() {
            report.files_with_edges += 1;
        }
        edges.append(&mut file_edges);
    }

    edges.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then_with(|| a.target.cmp(&b.target))
            .then_with(|| a.via.cmp(&b.via))
            .then_with(|| a.file.cmp(&b.file))
    });
    edges.dedup_by(|a, b| a.source == b.source && a.target == b.target && a.via == b.via);
    (edges, report)
}

/// Normalize a dotted Odoo model name to the corpus IRI convention:
/// `account.move` → `account_move`.
fn normalize_model(name: &str) -> String {
    name.replace('.', "_")
}

/// Whether a normalized target names a known screen — EXACT match (see the
/// module docs for why the Flask arm's plural tolerance is wrong for Odoo).
fn known_screen(target: &str, vocab: &NavVocab) -> bool {
    vocab.screens.iter().any(|s| s == target)
}

/// Walks a parsed module collecting Shape-A edges. Tracks the enclosing
/// class's model identity (`_name`, else first `_inherit` — the same
/// resolution rule as [`crate`]'s `resolve_name`) so a dict return inside a
/// method attributes its edge to the right source screen.
struct OdooNavWalker<'a> {
    file: String,
    vocab: &'a NavVocab,
    class_model: Option<String>,
    edges: Vec<OdooNavEdge>,
    raw_act_window_refs: usize,
}

impl<'a> Visitor<'a> for OdooNavWalker<'a> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        if let Stmt::ClassDef(class) = stmt {
            // Resolve this class's model identity from its body assigns.
            let saved = self.class_model.take();
            self.class_model = class_model_name(&class.body);
            walk_stmt(self, stmt);
            self.class_model = saved;
        } else {
            walk_stmt(self, stmt);
        }
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        // An act_window dict literal is the ONE code-side shape: a Dict whose
        // 'type' key is the string 'ir.actions.act_window'. Its 'res_model'
        // string literal names the target; a non-literal res_model is a
        // dynamic target (counted, no edge).
        if let Expr::Dict(dict) = expr
            && dict_str_value(dict, "type").as_deref() == Some("ir.actions.act_window")
        {
            self.raw_act_window_refs += 1;
            if let Some(raw_target) = dict_str_value(dict, "res_model")
                && let Some(source) = &self.class_model
            {
                let target = normalize_model(&raw_target);
                if known_screen(&target, self.vocab) {
                    self.edges.push(OdooNavEdge {
                        source: source.clone(),
                        target,
                        file: self.file.clone(),
                        via: "act_window_return".to_string(),
                    });
                }
            }
        }
        walk_expr(self, expr);
    }
}

/// The model identity of a class body: the `_name = '<dotted>'` string if
/// declared, else the first `_inherit` string (an `_inherit`-only extension
/// class takes its identity FROM the inherit — the [`crate`] `resolve_name`
/// rule). Normalized. `None` for a class with neither (not an Odoo model).
fn class_model_name(body: &[Stmt]) -> Option<String> {
    let mut inherit: Option<String> = None;
    for stmt in body {
        let Stmt::Assign(assign) = stmt else {
            continue;
        };
        let Some(Expr::Name(name)) = assign.targets.first() else {
            continue;
        };
        match name.id.as_str() {
            "_name" => {
                if let Some(v) = expr_str(&assign.value) {
                    return Some(normalize_model(&v));
                }
            }
            "_inherit" => {
                if inherit.is_none() {
                    inherit = match assign.value.as_ref() {
                        Expr::List(list) => list.iter().next().and_then(expr_str),
                        other => expr_str(other),
                    };
                }
            }
            _ => {}
        }
    }
    inherit.map(|v| normalize_model(&v))
}

/// The string value of a dict literal's `'<key>'` entry, if both the key and
/// the value are string literals.
fn dict_str_value(dict: &ruff_python_ast::ExprDict, key: &str) -> Option<String> {
    dict.items.iter().find_map(|item| {
        let k = item.key.as_ref()?;
        (expr_str(k)? == key).then(|| expr_str(&item.value))?
    })
}

/// Shape B, pass 1: append every `<record id="X" model=
/// "ir.actions.act_window">` block's `(xml_id → normalized res_model)` from
/// one file into the ADDON-WIDE `actions` map. Cross-file by design: real
/// addons declare actions in per-model view files and reference them from a
/// central menu file.
fn collect_act_window_records(content: &str, actions: &mut Vec<(String, Option<String>)>) {
    let mut in_record: Option<usize> = None; // index into `actions`
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("<record")
            && t.contains("model=\"ir.actions.act_window\"")
            && let Some(id) = attr_value(t, "id")
        {
            actions.push((id, None));
            in_record = Some(actions.len() - 1);
        } else if t.starts_with("<record") {
            // A different record kind closes any open act_window context.
            in_record = None;
        } else if t.starts_with("</record>") {
            in_record = None;
        } else if let Some(idx) = in_record
            && t.starts_with("<field")
            && attr_value(t, "name").as_deref() == Some("res_model")
            && let Some(model) = tag_text(t)
        {
            actions[idx].1 = Some(normalize_model(&model));
        }
    }
}

/// Shape B, pass 2: `<menuitem … action="X"…/>` lines in one file join
/// against the addon-wide action map (a module-qualified `action="mod.X"`
/// joins on its local part — within-addon convention). Only menu-wired
/// actions whose `res_model` is a known screen become edges (source = the
/// synthetic `menu` root).
fn scan_menuitems(
    content: &str,
    file: &str,
    actions: &[(String, Option<String>)],
    vocab: &NavVocab,
) -> Vec<OdooNavEdge> {
    let mut edges = Vec::new();
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("<menuitem")
            && let Some(action_ref) = attr_value(t, "action")
        {
            let local = action_ref.rsplit('.').next().unwrap_or(&action_ref);
            if let Some((_, Some(target))) = actions
                .iter()
                .find(|(id, _)| id == local || id == &action_ref)
                && known_screen(target, vocab)
            {
                edges.push(OdooNavEdge {
                    source: "menu".to_string(),
                    target: target.clone(),
                    file: file.to_string(),
                    via: "menuitem".to_string(),
                });
            }
        }
    }
    edges
}

/// The value of `key="…"` inside a single-line XML tag, if present.
fn attr_value(line: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=\"");
    let start = line.find(&pat)? + pat.len();
    let end = line[start..].find('"')? + start;
    Some(line[start..end].to_string())
}

/// The text content of a one-line `<tag …>text</tag>` element, if present.
fn tag_text(line: &str) -> Option<String> {
    let start = line.find('>')? + 1;
    let end = line[start..].find('<')? + start;
    let text = line[start..end].trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Recursively collect every `*.py` and `*.xml` file under `dir` (sorted for
/// determinism).
fn collect_files(dir: &Path, py: &mut Vec<PathBuf>, xml: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect_files(&path, py, xml);
        } else if path.extension().is_some_and(|e| e == "py") {
            py.push(path);
        } else if path.extension().is_some_and(|e| e == "xml") {
            xml.push(path);
        }
    }
}

/// `path` relative to `root`, `/`-joined (a stable id, not reopened).
fn relative_path(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn vocab(screens: &[&str]) -> NavVocab {
        NavVocab {
            screens: screens.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    const PY_ACTION: &str = r"
from odoo import models

class AccountMove(models.Model):
    _name = 'account.move'

    def action_open_lines(self):
        return {
            'type': 'ir.actions.act_window',
            'res_model': 'account.move.line',
            'view_mode': 'list',
        }

    def action_open_dynamic(self):
        return {
            'type': 'ir.actions.act_window',
            'res_model': self._name,
        }
";

    /// Shape A: the `act_window` dict return yields (defining model ->
    /// `res_model`); the dynamic-res_model dict is counted, edge-free.
    #[test]
    fn act_window_return_yields_edge_and_dynamic_is_counted_only() {
        let dir = tempdir();
        write(&dir, "models/account_move.py", PY_ACTION);
        let v = vocab(&["account_move_line"]);
        let (edges, report) = extract_odoo_nav_edges_with_report(&dir, &v);
        assert_eq!(
            edges.len(),
            1,
            "exactly the literal-res_model edge: {edges:?}"
        );
        assert_eq!(edges[0].source, "account_move");
        assert_eq!(edges[0].target, "account_move_line");
        assert_eq!(edges[0].via, "act_window_return");
        // BOTH dicts count in the honest denominator.
        assert_eq!(report.raw_act_window_refs, 2);
        cleanup(dir);
    }

    /// The edge lifts into the shared `navigates_to` SPO triple, namespaced,
    /// Inferred tier — byte-compatible with the Flask/Rails/C# arms.
    #[test]
    fn edge_lifts_to_navigates_to_triple() {
        let edge = OdooNavEdge {
            source: "account_move".into(),
            target: "account_move_line".into(),
            file: "models/account_move.py".into(),
            via: "act_window_return".into(),
        };
        let t = edge.to_triple("odoo");
        assert_eq!(t.s, "odoo:account_move");
        assert_eq!(t.p, Predicate::NavigatesTo.as_str());
        assert_eq!(t.o, "odoo:account_move_line");
        let (f, c) = Provenance::Inferred.truth();
        assert!((t.f - f).abs() < f32::EPSILON && (t.c - c).abs() < f32::EPSILON);
    }

    /// An `_inherit`-only extension class takes its source identity from the
    /// inherit (the crate's `resolve_name` rule).
    #[test]
    fn inherit_only_class_resolves_source_from_inherit() {
        let dir = tempdir();
        write(
            &dir,
            "models/ext.py",
            r"
from odoo import models

class MoveExt(models.Model):
    _inherit = 'account.move'

    def action_partners(self):
        return {'type': 'ir.actions.act_window', 'res_model': 'res.partner'}
",
        );
        let v = vocab(&["res_partner"]);
        let edges = extract_odoo_nav_edges(&dir, &v);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].source, "account_move");
        assert_eq!(edges[0].target, "res_partner");
        cleanup(dir);
    }

    /// A `res_model` outside the closed vocabulary yields no edge but still
    /// counts in the denominator. Matching is EXACT — no plural tolerance.
    #[test]
    fn unknown_res_model_counts_but_yields_no_edge() {
        let dir = tempdir();
        write(&dir, "models/m.py", PY_ACTION);
        // `account_move_lines` (plural) deliberately does NOT match.
        let v = vocab(&["account_move_lines"]);
        let (edges, report) = extract_odoo_nav_edges_with_report(&dir, &v);
        assert!(
            edges.is_empty(),
            "exact matching must reject the plural: {edges:?}"
        );
        assert_eq!(report.raw_act_window_refs, 2);
        cleanup(dir);
    }

    const XML_ACTION: &str = r#"<?xml version="1.0"?>
<odoo>
    <record id="action_move_out_invoice" model="ir.actions.act_window">
        <field name="name">Invoices</field>
        <field name="res_model">account.move</field>
        <field name="view_mode">list,form</field>
    </record>
    <record id="view_move_form" model="ir.ui.view">
        <field name="model">account.move</field>
    </record>
    <menuitem id="menu_invoices" action="action_move_out_invoice" parent="menu_finance"/>
    <menuitem id="menu_orphan" action="action_not_declared_here"/>
</odoo>
"#;

    /// Shape B: the menuitem joins its `act_window` record -> (menu ->
    /// `res_model`). The ir.ui.view record does NOT leak into the `act_window`
    /// context; the orphan menuitem (unknown action id) yields nothing.
    #[test]
    fn xml_menuitem_joins_act_window_record() {
        let dir = tempdir();
        write(&dir, "views/account_move_views.xml", XML_ACTION);
        let v = vocab(&["account_move"]);
        let (edges, report) = extract_odoo_nav_edges_with_report(&dir, &v);
        assert_eq!(edges.len(), 1, "{edges:?}");
        assert_eq!(edges[0].source, "menu");
        assert_eq!(edges[0].target, "account_move");
        assert_eq!(edges[0].via, "menuitem");
        // Exactly ONE act_window record in the denominator (the ir.ui.view
        // record is a different kind).
        assert_eq!(report.raw_act_window_refs, 1);
        assert_eq!(report.xml_files, 1);
        cleanup(dir);
    }

    /// The menuitem→record join is CROSS-FILE: real addons declare actions in
    /// per-model view files and the menuitems in a central menu file (the
    /// account addon's `account_menuitem.xml`). A per-file join measured ZERO
    /// menu edges on the real addon — this pins the cross-file fix.
    #[test]
    fn menuitem_joins_record_declared_in_another_file() {
        let dir = tempdir();
        write(
            &dir,
            "views/account_move_views.xml",
            r#"<odoo>
    <record id="action_move_journal_line" model="ir.actions.act_window">
        <field name="res_model">account.move</field>
    </record>
</odoo>
"#,
        );
        write(
            &dir,
            "views/account_menuitem.xml",
            r#"<odoo>
    <menuitem id="menu_action_move" action="action_move_journal_line"/>
</odoo>
"#,
        );
        let v = vocab(&["account_move"]);
        let (edges, report) = extract_odoo_nav_edges_with_report(&dir, &v);
        assert_eq!(edges.len(), 1, "{edges:?}");
        assert_eq!(edges[0].source, "menu");
        assert_eq!(edges[0].target, "account_move");
        assert_eq!(edges[0].via, "menuitem");
        assert_eq!(report.xml_files, 2);
        cleanup(dir);
    }

    /// A module-qualified menuitem action ref (`action="account.X"`) joins on
    /// its local part.
    #[test]
    fn module_qualified_action_ref_joins_on_local_part() {
        let dir = tempdir();
        write(
            &dir,
            "views/v.xml",
            r#"<odoo>
    <record id="action_x" model="ir.actions.act_window">
        <field name="res_model">res.partner</field>
    </record>
    <menuitem id="m" action="account.action_x"/>
</odoo>
"#,
        );
        let v = vocab(&["res_partner"]);
        let edges = extract_odoo_nav_edges(&dir, &v);
        assert_eq!(edges.len(), 1, "{edges:?}");
        assert_eq!(edges[0].target, "res_partner");
        cleanup(dir);
    }

    // ── tiny tempdir helpers (no dev-dep; mirrors sibling test style) ──
    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "odoo_nav_test_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: PathBuf) {
        let _ = fs::remove_dir_all(dir);
    }
}
