//! The navigation-topology harvest — the Flask/Django **Klickweg**.
//!
//! # What this is
//!
//! The Python analog of `ruff_ruby_spo`'s (ruff #62) and `ruff_csharp_spo`'s
//! (ruff #61) `navigates_to` arms: it emits the shared
//! [`Predicate::NavigatesTo`] fact — `(source_screen, navigates_to,
//! target_screen)`, the "which screen opens which" edge — that a plain
//! entity/behaviour transcode (the core-7 predicates in [`crate`]) does NOT
//! capture. The Core consumer is `lance_graph_contract`'s screen-connectivity
//! check: screens = nodes, navigations = edges, and the dead-link / unreachable-
//! screen class of bug is exactly this fact going unmodelled.
//!
//! # Why Python needs only ONE shape (Ruby needs two)
//!
//! In Flask and Django the navigation the user clicks AND the server-side
//! redirect are both authored **code-side**, in `.py`, as calls to `url_for`
//! / `reverse` / `redirect`. So a single AST body-walk over the parsed Python
//! finds every edge. **Rails, by contrast, splits navigation across two
//! artifacts** — the `.erb` view carries the `link_to` *click* edge, the `.rb`
//! controller carries the `redirect_to` edge — so the Ruby arm needs two
//! harvest shapes where Python needs one. (C# `WinForms` is likewise one
//! shape: a screen IS a class, opened by `new Form().Show()`.)
//!
//! # Tier + doctrine (Inferred, AST-based, closed-vocab, honest denominator)
//!
//! Unlike the Ruby/ERB arm — a closed-vocabulary *line* scanner — this arm is
//! **AST-based**, matching [`crate::functions`]: it parses each `.py` with
//! `ruff_python_parser` and walks the tree with the `ruff_python_ast`
//! [`Visitor`], matching [`Expr::Call`] nodes. A call whose terminal callee is
//! one of `url_for` / `reverse` / `redirect` contributes a nav target **only
//! when its first positional argument is a string literal**; that string is
//! resolved to a screen stem (its leading segment before the first `.` or `:`)
//! and matched against a caller-supplied closed vocabulary of known screens
//! ([`NavVocab::screens`], tolerant of the singular/plural pairing
//! `project`↔`projects`). The matched screens become [`PyNavEdge`]s; the raw
//! count of every string-literal nav target seen — vocab member or not — is the
//! honest denominator [`NavScanReport::raw_target_refs`]. This is the same
//! closed-vocab + honest-denominator discipline the Ruby arm uses.
//!
//! # What is NOT captured (by design, not oversight)
//!
//! - **Dynamic / object targets** — `redirect(obj)` / `redirect(url_for(...))`
//!   where the argument to `redirect` is a variable or a nested call, not a
//!   string literal. The string literal is the only closed-vocab handle
//!   without resolving the URL map / router, so a non-literal argument is a
//!   dynamic target and is deliberately skipped (mirroring the Ruby arm's
//!   dynamic-target exclusion). Note this leaves `raw_target_refs` untouched —
//!   a non-literal target is not "a nav target seen", it is a target we cannot
//!   read. (The inner `url_for(...)` of a `redirect(url_for("t"))` IS read on
//!   its own, so no edge is lost.)
//! - **Template-side navigation** — a `{{ url_for(...) }}` in a Jinja template
//!   or a `{% url %}` tag in a Django template. This arm is code-side only;
//!   template scanning is out of scope.

use std::fs;
use std::path::{Path, PathBuf};

use ruff_python_ast::Expr;
use ruff_python_ast::visitor::{Visitor, walk_expr};
use ruff_spo_triplet::{Predicate, Provenance, Triple};

use crate::expr_str;

/// The code-side navigation helpers whose string-literal first argument names
/// a navigation target: Flask's `url_for`, Django's `reverse`, and the
/// `redirect` shortcut common to both (Django resolves a view name; Flask a
/// URL). A call whose terminal callee is one of these is the ONE shape this
/// arm harvests.
const NAV_HELPERS: &[&str] = &["url_for", "reverse", "redirect"];

/// One navigation edge: `source` screen navigates to `target` screen.
///
/// `source`/`target` are screen stems (e.g. `"projects"`, `"work_packages"`);
/// `call` is the raw helper matched (`"url_for"` / `"reverse"` / `"redirect"`)
/// for provenance. [`to_triple`](Self::to_triple) lifts it into the shared
/// `navigates_to` SPO fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyNavEdge {
    /// The screen the edge originates from (the view file's module/dir stem).
    pub source: String,
    /// The screen the edge navigates to (a known [`NavVocab::screens`] stem).
    pub target: String,
    /// Source file path relative to the scanned root (stable id, `/`-joined).
    pub file: String,
    /// The raw helper token matched: `"url_for"` / `"reverse"` / `"redirect"`.
    pub call: String,
}

impl PyNavEdge {
    /// Lift this edge into the shared `navigates_to` SPO triple, namespaced
    /// (`<ns>:<source>` → `<ns>:<target>`), `Inferred` tier — the same fact
    /// shape the Ruby and C# arms emit, and [`Predicate::NavigatesTo`]'s
    /// default provenance.
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

/// The closed vocabulary of known screen stems a nav target must match.
///
/// Mirrors the Ruby arm's `NavVocab`: a resolved target stem must be in
/// `screens` (tolerant of the singular/plural pairing) to become a
/// [`PyNavEdge`]; everything else is counted only in
/// [`NavScanReport::raw_target_refs`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NavVocab {
    /// Known screen stems, e.g. `["projects", "work_packages"]`.
    pub screens: Vec<String>,
}

/// Conservation-ledger totals for a nav scan (the honest-denominator
/// discipline — nothing drops silently).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NavScanReport {
    /// Every `*.py` file found under the scanned root.
    pub py_files: usize,
    /// Files that produced at least one [`PyNavEdge`].
    pub files_with_edges: usize,
    /// Every string-literal nav target seen — the honest denominator,
    /// regardless of whether its stem is a known screen.
    pub raw_target_refs: usize,
}

/// Scan `<root>` for code-side navigation edges and return the known-screen
/// edges. Thin wrapper over [`extract_nav_edges_with_report`].
#[must_use]
pub fn extract_nav_edges(root: &Path, vocab: &NavVocab) -> Vec<PyNavEdge> {
    extract_nav_edges_with_report(root, vocab).0
}

/// Like [`extract_nav_edges`] but also returns the [`NavScanReport`] ledger.
///
/// Walks every `*.py` under `root`, parses each with `ruff_python_parser`
/// (skipping a file that fails to parse — the crate's silent-skip invariant),
/// AST-visits for `url_for` / `reverse` / `redirect` calls with a string-
/// literal first argument, and lifts each known-screen target to an edge whose
/// source is the file's [`source_screen`]. Results are deduped by
/// `(source, target, call)` and sorted deterministically.
#[must_use]
pub fn extract_nav_edges_with_report(
    root: &Path,
    vocab: &NavVocab,
) -> (Vec<PyNavEdge>, NavScanReport) {
    let mut report = NavScanReport::default();
    let mut files = Vec::new();
    collect_py_files(root, &mut files);

    let mut edges: Vec<PyNavEdge> = Vec::new();
    for path in &files {
        report.py_files += 1;
        let Ok(source) = fs::read_to_string(path) else {
            continue;
        };
        let Ok(parsed) = ruff_python_parser::parse_module(&source) else {
            continue;
        };
        let rel = relative_path(root, path);
        let mut walker = NavWalker {
            source: source_screen(&rel),
            file: rel,
            vocab,
            edges: Vec::new(),
            raw_target_refs: 0,
        };
        walker.visit_body(&parsed.syntax().body);
        report.raw_target_refs += walker.raw_target_refs;
        if !walker.edges.is_empty() {
            report.files_with_edges += 1;
        }
        edges.append(&mut walker.edges);
    }

    edges.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then_with(|| a.target.cmp(&b.target))
            .then_with(|| a.call.cmp(&b.call))
            .then_with(|| a.file.cmp(&b.file))
    });
    edges.dedup_by(|a, b| a.source == b.source && a.target == b.target && a.call == b.call);
    (edges, report)
}

/// Walks a parsed module collecting `url_for` / `reverse` / `redirect`
/// navigation edges. One walker per file; `source`/`file` are fixed for the
/// file, `edges`/`raw_target_refs` accumulate as the tree is visited.
struct NavWalker<'a> {
    source: String,
    file: String,
    vocab: &'a NavVocab,
    edges: Vec<PyNavEdge>,
    raw_target_refs: usize,
}

impl<'a> Visitor<'a> for NavWalker<'a> {
    fn visit_expr(&mut self, expr: &'a Expr) {
        // A nav-helper call with a STRING-LITERAL first argument is the only
        // shape that yields a target. `redirect(url_for("t"))` reads only the
        // inner `url_for` (redirect's argument is a call, not a literal), so a
        // nested form is counted exactly once; `redirect(obj)` (a variable /
        // object) is a dynamic target and contributes nothing.
        if let Expr::Call(call) = expr
            && let Some(helper) = terminal_name(&call.func)
            && NAV_HELPERS.contains(&helper)
            && let Some(arg) = call.arguments.args.first()
            && let Some(raw_target) = expr_str(arg)
        {
            self.raw_target_refs += 1;
            let stem = target_stem(&raw_target);
            if let Some(target) = self.vocab.screens.iter().find(|s| screen_matches(stem, s)) {
                self.edges.push(PyNavEdge {
                    source: self.source.clone(),
                    target: target.clone(),
                    file: self.file.clone(),
                    call: helper.to_string(),
                });
            }
        }
        walk_expr(self, expr);
    }
}

/// Recursively collect every `*.py` file under `dir` (sorted for determinism).
fn collect_py_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect_py_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "py") {
            out.push(path);
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

/// The source screen for a view file, by ONE rule: the nearest `views` marker
/// — a `views` directory OR a `views.py` file — groups the file, so the source
/// is the segment right before that marker (`app/projects/views.py` →
/// `projects`; `blog/views.py` → `blog`; `app/projects/views/detail.py` →
/// `projects`). With no `views` marker, the file stem minus a trailing
/// `_views` / `views` (`projects_views.py` → `projects`). This is the
/// module/dir-level altitude the Ruby arm uses — the enclosing function name
/// is deliberately not resolved.
fn source_screen(rel: &str) -> String {
    let segments: Vec<&str> = rel.split('/').collect();
    // The nearest `views` marker: a `views` directory or a `views.py` file.
    let marker = segments
        .iter()
        .rposition(|seg| *seg == "views" || seg.strip_suffix(".py") == Some("views"));
    if let Some(idx) = marker {
        // The grouping segment is the marker's parent dir; a `views` marker at
        // the tree root has no grouping dir.
        if idx >= 1 {
            return segments[idx - 1].to_string();
        }
        return "?".to_string();
    }
    // No `views` marker: the file stem minus a trailing `_views` / `views`.
    let file = segments.last().copied().unwrap_or("");
    let stem = file.strip_suffix(".py").unwrap_or(file);
    let stem = stem
        .strip_suffix("_views")
        .or_else(|| stem.strip_suffix("views"))
        .unwrap_or(stem);
    if stem.is_empty() {
        "?".to_string()
    } else {
        stem.to_string()
    }
}

/// The screen stem of a resolved nav target: the leading segment before the
/// first `.` or `:` (Flask `url_for("projects.index")` → `projects`; Django
/// `reverse("projects:detail")` → `projects`; a plain `"projects"` →
/// `projects`).
fn target_stem(target: &str) -> &str {
    let end = target.find(['.', ':']).unwrap_or(target.len());
    &target[..end]
}

/// Whether a resolved target `stem` names the known `screen`, tolerant of the
/// singular/plural pairing (`project`↔`projects`). NOT a full inflector — only
/// the trailing-`s` case, exactly as the Ruby arm's `screen_matches`.
fn screen_matches(stem: &str, screen: &str) -> bool {
    if stem == screen {
        return true;
    }
    let strip_s = |s: &str| s.strip_suffix('s').unwrap_or(s).to_string();
    strip_s(stem) == strip_s(screen)
}

/// The terminal identifier of a callee expression: `f` for `f(...)`, `attr`
/// for `a.b.attr(...)` (so `shortcuts.redirect(...)` reads as `redirect`).
fn terminal_name(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Name(n) => Some(n.id.as_str()),
        Expr::Attribute(a) => Some(a.attr.id.as_str()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn scratch_dir(case: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("ruff_python_spo_nav_{}_{case}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        root
    }

    fn vocab() -> NavVocab {
        NavVocab {
            screens: vec!["projects".to_string(), "work_packages".to_string()],
        }
    }

    /// Flask `redirect(url_for("projects.index"))` in a blueprint view yields
    /// one edge; the target reads from the inner `url_for`, the source from the
    /// blueprint dir grouping `views.py`.
    #[test]
    fn flask_redirect_url_for_edge() {
        let root = scratch_dir("flask_redirect");
        write_file(
            &root,
            "app/work_packages/views.py",
            "from flask import redirect, url_for\n\
             def create():\n\
             \x20   return redirect(url_for(\"projects.index\"))\n",
        );

        let edges = extract_nav_edges(&root, &vocab());
        assert_eq!(edges.len(), 1, "{edges:?}");
        assert_eq!(edges[0].source, "work_packages");
        assert_eq!(edges[0].target, "projects");
        assert_eq!(edges[0].call, "url_for");

        let _ = fs::remove_dir_all(&root);
    }

    /// Django `HttpResponseRedirect(reverse("work_packages:list"))` yields one
    /// edge via the inner `reverse` (the wrapper is not a nav helper); target
    /// `work_packages`.
    #[test]
    fn django_http_response_redirect_reverse_edge() {
        let root = scratch_dir("django_reverse");
        write_file(
            &root,
            "projects/views.py",
            "from django.http import HttpResponseRedirect\n\
             from django.urls import reverse\n\
             def detail(request):\n\
             \x20   return HttpResponseRedirect(reverse(\"work_packages:list\"))\n",
        );

        let edges = extract_nav_edges(&root, &vocab());
        assert_eq!(edges.len(), 1, "{edges:?}");
        assert_eq!(edges[0].source, "projects");
        assert_eq!(edges[0].target, "work_packages");
        assert_eq!(edges[0].call, "reverse");

        let _ = fs::remove_dir_all(&root);
    }

    /// Bare `url_for("projects")` and `reverse("project")` both resolve to the
    /// `projects` screen (singular/plural tolerance), via their two helpers.
    #[test]
    fn bare_helpers_and_plural_tolerance() {
        let root = scratch_dir("plural");
        write_file(
            &root,
            "projects_views.py",
            "def a():\n\
             \x20   x = url_for(\"projects\")\n\
             \x20   y = reverse(\"project\")\n\
             \x20   return (x, y)\n",
        );

        let edges = extract_nav_edges(&root, &vocab());
        // Both resolve to the `projects` screen; deduped by (source, target,
        // call) so the two distinct helpers stay as two edges.
        let mut seen: Vec<(&str, &str, &str)> = edges
            .iter()
            .map(|e| (e.source.as_str(), e.target.as_str(), e.call.as_str()))
            .collect();
        seen.sort_unstable();
        assert_eq!(
            seen,
            vec![
                ("projects", "projects", "reverse"),
                ("projects", "projects", "url_for"),
            ],
            "{edges:?}"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// A dynamic `redirect(some_obj)` (non-literal argument) is NOT captured,
    /// and — since the argument is not a string literal — does NOT touch
    /// `raw_target_refs`.
    #[test]
    fn dynamic_object_redirect_not_captured() {
        let root = scratch_dir("dynamic");
        write_file(
            &root,
            "app/projects/views.py",
            "def save(obj):\n\
             \x20   return redirect(obj)\n",
        );

        let (edges, report) = extract_nav_edges_with_report(&root, &vocab());
        assert!(edges.is_empty(), "object target is dynamic: {edges:?}");
        assert_eq!(
            report.raw_target_refs, 0,
            "a non-literal target is not a target we can read"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// An unknown target (`url_for("help")`, `help` not in the vocab) is
    /// counted in `raw_target_refs` (the honest denominator) but is NOT an edge.
    #[test]
    fn unknown_target_is_counted_not_edged() {
        let root = scratch_dir("unknown");
        write_file(
            &root,
            "app/projects/views.py",
            "def sidebar():\n\
             \x20   return url_for(\"help\")\n",
        );

        let (edges, report) = extract_nav_edges_with_report(&root, &vocab());
        assert!(edges.is_empty(), "help is not a known screen: {edges:?}");
        assert_eq!(report.raw_target_refs, 1, "but it IS counted raw");

        let _ = fs::remove_dir_all(&root);
    }

    /// The edge lifts into the shared `navigates_to` SPO triple, namespaced,
    /// Inferred tier — byte-identical shape to the Ruby / C# arms.
    #[test]
    fn edge_lifts_to_navigates_to_triple() {
        let edge = PyNavEdge {
            source: "work_packages".to_string(),
            target: "projects".to_string(),
            file: "app/work_packages/views.py".to_string(),
            call: "url_for".to_string(),
        };
        let t = edge.to_triple("openproject");
        assert_eq!(t.s, "openproject:work_packages");
        assert_eq!(t.p, "navigates_to");
        assert_eq!(t.o, "openproject:projects");
        assert_eq!(t.p, Predicate::NavigatesTo.as_str());
        // Inferred tier truth (0.85, 0.75) — matches Predicate::NavigatesTo.
        let (f, c) = Provenance::Inferred.truth();
        assert_eq!((t.f, t.c), (f, c));
    }

    /// A small synthetic multi-file app: one Flask-style file and one
    /// Django-style file are both harvested, and the ledger accounts for every
    /// `.py` file, the edge-bearing subset, and the raw target count.
    #[test]
    fn synthetic_multi_file_app_ledger() {
        let root = scratch_dir("multi");
        // Flask blueprint view — one known edge.
        write_file(
            &root,
            "app/projects/views.py",
            "from flask import redirect, url_for\n\
             def index():\n\
             \x20   return redirect(url_for(\"work_packages.list\"))\n",
        );
        // Django app view — one known edge + one unknown (raw-only) target.
        write_file(
            &root,
            "work_packages/views.py",
            "from django.shortcuts import redirect\n\
             def detail(request):\n\
             \x20   redirect(\"projects\")\n\
             \x20   return redirect(\"help\")\n",
        );
        // A non-view module with no nav calls — counts toward py_files only.
        write_file(&root, "app/models.py", "class Project:\n    pass\n");

        let (edges, report) = extract_nav_edges_with_report(&root, &vocab());

        let mut pairs: Vec<(&str, &str, &str)> = edges
            .iter()
            .map(|e| (e.source.as_str(), e.target.as_str(), e.call.as_str()))
            .collect();
        pairs.sort_unstable();
        assert_eq!(
            pairs,
            vec![
                ("projects", "work_packages", "url_for"),
                ("work_packages", "projects", "redirect"),
            ],
            "{edges:?}"
        );

        assert_eq!(report.py_files, 3, "two views + one model");
        assert_eq!(report.files_with_edges, 2, "only the two view files");
        // Three string-literal targets seen: work_packages.list, projects, help.
        assert_eq!(report.raw_target_refs, 3);

        let _ = fs::remove_dir_all(&root);
    }
}
