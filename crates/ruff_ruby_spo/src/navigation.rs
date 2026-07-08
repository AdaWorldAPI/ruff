//! The navigation-topology harvest — the Rails **Klickweg**.
//!
//! # What this is
//!
//! The Ruby analog of `ruff_csharp_spo`'s `WinForms` `navigates_to` arm
//! (ruff #61): it emits the shared [`Predicate::NavigatesTo`] fact —
//! `(source_screen, navigates_to, target_screen)` — the "which screen
//! opens which" edge that a plain entity/behaviour transcode (`ClassViews` +
//! DTOs) does NOT capture. The Core consumer is
//! `lance_graph_contract::class_view::{screens_reachable_from,
//! nav_is_fully_connected}` (lance-graph #670/#673): screens = nodes,
//! clicks = edges, and the level-editor connectivity check runs over
//! exactly these triples.
//!
//! # Why Ruby needs TWO shapes (C# and Python need one)
//!
//! In C# `WinForms` a screen IS a class, so one body-walk shape
//! (`new Form().Show()`) finds every edge. In Flask/Django the link and
//! the redirect are both code-side (`redirect(url_for("t"))`), again one
//! shape. **Rails splits navigation across two artifacts**, so a single
//! body-walk would miss half the graph:
//!
//! - **Shape A — the ERB *click* edge** (`.erb`): the navigation the user
//!   actually clicks is authored in the view — `link_to`, `button_to`,
//!   `form_with url:`. This is the sibling of [`crate::views`]'s ERB
//!   field-set scanner, and it is the shape that matters for a klickweg.
//! - **Shape B — the controller *redirect* edge** (`.rb`): server-side
//!   post-action navigation — `redirect_to <t>_path`. Sibling of
//!   [`crate::functions`]'s controller body walk.
//!
//! Both emit the same `navigates_to` predicate (Inferred tier) — one fact,
//! two harvest shapes.
//!
//! # Tier + doctrine (Inferred, closed-vocab line scanner)
//!
//! Like [`crate::views`], this is a closed-vocabulary line scanner — **no
//! Ruby/ERB parser**. A route helper (`<stem>_path` / `<stem>_url`) is
//! only read as a nav target when it sits on a line that also carries a
//! navigation DSL verb (shape A: `link_to`/`button_to`/`form_with`/… ;
//! shape B: `redirect_to`/`redirect_back`) — this bounds false positives
//! (a `project_path` passed to a non-nav helper is not a click). The
//! target `<stem>` is matched against a caller-supplied closed vocabulary
//! of known screens ([`NavVocab::screens`], tolerant of the singular /
//! plural pairing `project`↔`projects`), exactly the honest-denominator
//! discipline `views.rs` uses: [`RubyNavEdge`]s are the known subset,
//! [`NavScanReport::raw_target_refs`] is the raw count.
//!
//! # What is NOT captured (by design, not oversight)
//!
//! - **Dynamic targets** — `redirect_to @project` / `link_to t, url` where
//!   the target is an object/variable, not a `*_path`/`*_url` helper. The
//!   route helper is the only closed-vocab handle without a Rails router.
//! - **Nested partials / deep namespacing** — the source screen is the
//!   view's controller-dir segment (`app/views/<screen>/…`) or the
//!   `<screen>_controller.rb` stem; namespaced (`admin/…`) controllers
//!   collapse to their leaf resource. Rails convention makes the common
//!   case exact; the tail is Inferred by nature.

use std::fs;
use std::path::{Path, PathBuf};

use ruff_spo_triplet::{Predicate, Provenance, Triple};

/// Which harvest shape produced a [`RubyNavEdge`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NavShape {
    /// Shape A — an ERB view *click* edge (`link_to`/`button_to`/`form_with`).
    ErbClick,
    /// Shape B — a controller *redirect* edge (`redirect_to`/`redirect_back`).
    ControllerRedirect,
}

/// One navigation edge: `source` screen navigates to `target` screen.
///
/// `source`/`target` are screen stems (e.g. `"work_packages"`,
/// `"projects"`); `helper` is the raw route helper matched (e.g.
/// `"project_path"`) for provenance. [`to_triple`](Self::to_triple) lifts
/// it into the shared `navigates_to` SPO fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RubyNavEdge {
    /// The screen the edge originates from (a controller/view resource stem).
    pub source: String,
    /// The screen the edge navigates to (a known [`NavVocab::screens`] stem).
    pub target: String,
    /// Which of the two harvest shapes produced this edge.
    pub shape: NavShape,
    /// Source file path relative to the app root (stable id, `/`-joined).
    pub file: String,
    /// The raw route helper token matched (e.g. `"edit_project_path"`).
    pub helper: String,
}

impl RubyNavEdge {
    /// Lift this edge into the shared `navigates_to` SPO triple, namespaced
    /// (`<ns>:<source>` → `<ns>:<target>`). `Inferred` tier, matching the
    /// C# arm and [`Predicate::NavigatesTo`]'s default provenance.
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
/// Mirrors [`crate::views::ViewTarget`]'s closed-vocab discipline: a route
/// helper's resolved resource stem must be in `screens` (tolerant of the
/// singular/plural pairing) to become a [`RubyNavEdge`]; everything else is
/// counted only in [`NavScanReport::raw_target_refs`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NavVocab {
    /// Known screen stems, e.g. `["projects", "work_packages"]`.
    pub screens: Vec<String>,
}

/// Conservation-ledger totals for a nav scan (same discipline as
/// [`crate::views::ViewScanReport`] — nothing drops silently).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NavScanReport {
    /// Every `*.erb` file found (shape A candidates).
    pub erb_files: usize,
    /// Every `*_controller.rb` file found (shape B candidates).
    pub controller_files: usize,
    /// Files that produced at least one [`RubyNavEdge`].
    pub files_with_edges: usize,
    /// Every route-helper reference seen on a nav-verb line — the honest
    /// denominator, regardless of whether its stem is a known screen.
    pub raw_target_refs: usize,
}

/// Shape-A navigation DSL verbs — an ERB line must carry one for a route
/// helper on it to count as a click edge.
const ERB_NAV_VERBS: &[&str] = &[
    "link_to",
    "link_to_if",
    "link_to_unless",
    "button_to",
    "form_with",
    "form_for",
    "form_tag",
];

/// Shape-B navigation DSL verbs — a controller line must carry one for a
/// route helper on it to count as a redirect edge.
const CONTROLLER_NAV_VERBS: &[&str] = &["redirect_to", "redirect_back"];

/// Scan `<app_root>` for both nav shapes and return the known-screen edges.
/// Thin wrapper over [`extract_nav_edges_with_report`].
#[must_use]
pub fn extract_nav_edges(app_root: &Path, vocab: &NavVocab) -> Vec<RubyNavEdge> {
    extract_nav_edges_with_report(app_root, vocab).0
}

/// Like [`extract_nav_edges`] but also returns the [`NavScanReport`] ledger.
///
/// Shape A walks every `*.erb` under `app_root`; shape B walks every
/// `*_controller.rb`. Results are deduped by `(source, target, shape)` and
/// sorted (source, target, file) for determinism.
#[must_use]
pub fn extract_nav_edges_with_report(
    app_root: &Path,
    vocab: &NavVocab,
) -> (Vec<RubyNavEdge>, NavScanReport) {
    let mut report = NavScanReport::default();
    let mut files = Vec::new();
    collect_source_files(app_root, &mut files);

    let mut edges: Vec<RubyNavEdge> = Vec::new();
    for path in &files {
        let is_erb = path.extension().and_then(|e| e.to_str()) == Some("erb");
        let is_controller = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with("_controller.rb"));
        if is_erb {
            report.erb_files += 1;
        } else if is_controller {
            report.controller_files += 1;
        } else {
            continue;
        }

        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        let rel = relative_path(app_root, path);
        let (shape, verbs, source) = if is_erb {
            (NavShape::ErbClick, ERB_NAV_VERBS, erb_source_screen(&rel))
        } else {
            (
                NavShape::ControllerRedirect,
                CONTROLLER_NAV_VERBS,
                controller_source_screen(path),
            )
        };

        let before = edges.len();
        for line in content.lines() {
            if !verbs.iter().any(|v| contains_word(line, v)) {
                continue;
            }
            for helper in route_helpers(line) {
                report.raw_target_refs += 1;
                let stem = resource_stem(&helper);
                if let Some(target) = vocab.screens.iter().find(|s| screen_matches(&stem, s)) {
                    edges.push(RubyNavEdge {
                        source: source.clone(),
                        target: target.clone(),
                        shape,
                        file: rel.clone(),
                        helper,
                    });
                }
            }
        }
        if edges.len() > before {
            report.files_with_edges += 1;
        }
    }

    edges.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then_with(|| a.target.cmp(&b.target))
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.helper.cmp(&b.helper))
    });
    edges.dedup_by(|a, b| {
        a.source == b.source && a.target == b.target && a.shape == b.shape && a.helper == b.helper
    });
    (edges, report)
}

/// Walk `dir` recursively, appending every file (sorted for determinism).
/// Shape discrimination happens at the call site by extension / suffix.
fn collect_source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect_source_files(&path, out);
        } else {
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

/// The source screen for a shape-A view: the path segment right after the
/// last `views` component (`app/views/<screen>/show.html.erb` → `<screen>`),
/// falling back to the immediate parent directory name.
fn erb_source_screen(rel: &str) -> String {
    let segments: Vec<&str> = rel.split('/').collect();
    if let Some(views_idx) = segments.iter().rposition(|s| *s == "views") {
        if let Some(screen) = segments.get(views_idx + 1) {
            // Only use it if it is a directory segment (not the file itself).
            if views_idx + 2 < segments.len() {
                return (*screen).to_string();
            }
        }
    }
    // Fallback: the parent directory of the file.
    if segments.len() >= 2 {
        return segments[segments.len() - 2].to_string();
    }
    "?".to_string()
}

/// The source screen for a shape-B controller: the filename stem minus the
/// `_controller` suffix (`work_packages_controller.rb` → `work_packages`).
fn controller_source_screen(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix("_controller.rb"))
        .unwrap_or("?")
        .to_string()
}

/// Every route-helper token (`<stem>_path` / `<stem>_url`) on `line`.
/// `<stem>` is a lowercase Ruby identifier run; the helper is the whole
/// `<stem>_path`/`<stem>_url` token, on a word boundary.
fn route_helpers(line: &str) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        // Start of an identifier run (not mid-token).
        if is_ident_start(&chars, i) {
            let start = i;
            while i < chars.len() && is_ident_char(chars[i]) {
                i += 1;
            }
            let token: String = chars[start..i].iter().collect();
            if (token.ends_with("_path") || token.ends_with("_url")) && token.len() > 5 {
                out.push(token);
            }
        } else {
            i += 1;
        }
    }
    out
}

/// The resource stem of a route helper: drop the trailing `_path`/`_url`
/// and any leading `new_`/`edit_` action prefix. `edit_project_path` →
/// `project`; `projects_path` → `projects`.
fn resource_stem(helper: &str) -> String {
    let base = helper
        .strip_suffix("_path")
        .or_else(|| helper.strip_suffix("_url"))
        .unwrap_or(helper);
    let base = base.strip_prefix("new_").unwrap_or(base);
    let base = base.strip_prefix("edit_").unwrap_or(base);
    base.to_string()
}

/// Whether a route helper's resource `stem` names the known `screen`,
/// tolerant of the Rails singular/plural pairing (`project`↔`projects`,
/// `work_package`↔`work_packages`). NOT a full inflector — only the
/// trailing-`s` case, which covers the overwhelming majority of Rails
/// resource routes; irregular plurals are out of scope (documented).
fn screen_matches(stem: &str, screen: &str) -> bool {
    if stem == screen {
        return true;
    }
    let strip_s = |s: &str| s.strip_suffix('s').unwrap_or(s).to_string();
    strip_s(stem) == strip_s(screen)
}

/// Whether `word` appears in `line` on both-side word boundaries.
fn contains_word(line: &str, word: &str) -> bool {
    let chars: Vec<char> = line.chars().collect();
    let w: Vec<char> = word.chars().collect();
    if w.is_empty() || chars.len() < w.len() {
        return false;
    }
    for start in 0..=(chars.len() - w.len()) {
        if chars[start..start + w.len()] != w[..] {
            continue;
        }
        if start > 0 && is_ident_char(chars[start - 1]) {
            continue;
        }
        let end = start + w.len();
        if end < chars.len() && is_ident_char(chars[end]) {
            continue;
        }
        return true;
    }
    false
}

/// An identifier run starts at `i` when `chars[i]` is an identifier char and
/// the preceding char (if any) is not — so we read whole tokens, never a
/// suffix of `edit_project_path` starting mid-word.
fn is_ident_start(chars: &[char], i: usize) -> bool {
    is_ident_char(chars[i]) && (i == 0 || !is_ident_char(chars[i - 1]))
}

/// Identifier-forming characters (route helpers are `[a-z0-9_]` runs).
fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
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
            std::env::temp_dir().join(format!("ruff_ruby_spo_nav_{}_{case}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        root
    }

    fn vocab() -> NavVocab {
        NavVocab {
            screens: vec!["projects".to_string(), "work_packages".to_string()],
        }
    }

    /// The synthetic Rails app (no proprietary source): a 2-screen klickweg
    /// wired both ways, one edge per shape per direction.
    fn write_synthetic_app(root: &Path) {
        // Shape A — ERB click edges.
        write_file(
            root,
            "app/views/work_packages/index.html.erb",
            "<h1>Work packages</h1>\n<%= link_to \"Back to project\", project_path(@project) %>\n",
        );
        write_file(
            root,
            "app/views/projects/show.html.erb",
            "<h1><%= @project.name %></h1>\n\
             <%= link_to \"Work packages\", work_packages_path(project_id: @project.id) %>\n",
        );
        // Shape B — controller redirect edges.
        write_file(
            root,
            "app/controllers/work_packages_controller.rb",
            "class WorkPackagesController < ApplicationController\n\
             \x20 def create\n\
             \x20   redirect_to projects_path\n\
             \x20 end\n\
             end\n",
        );
    }

    /// Shape A end-to-end: a single `link_to <t>_path` in an ERB view yields
    /// exactly one click edge, source = the view's controller dir.
    #[test]
    fn shape_a_erb_click_edge() {
        let root = scratch_dir("shape_a");
        write_file(
            &root,
            "app/views/work_packages/index.html.erb",
            "<%= link_to \"Project\", project_path(@p) %>\n",
        );

        let edges = extract_nav_edges(&root, &vocab());
        assert_eq!(edges.len(), 1, "{edges:?}");
        assert_eq!(edges[0].source, "work_packages");
        assert_eq!(edges[0].target, "projects");
        assert_eq!(edges[0].shape, NavShape::ErbClick);
        assert_eq!(edges[0].helper, "project_path");

        let _ = fs::remove_dir_all(&root);
    }

    /// Shape B end-to-end: a `redirect_to <t>_path` in a controller body
    /// yields exactly one redirect edge, source = the controller resource.
    #[test]
    fn shape_b_controller_redirect_edge() {
        let root = scratch_dir("shape_b");
        write_file(
            &root,
            "app/controllers/work_packages_controller.rb",
            "class WorkPackagesController\n  def create\n    redirect_to projects_path\n  end\nend\n",
        );

        let edges = extract_nav_edges(&root, &vocab());
        assert_eq!(edges.len(), 1, "{edges:?}");
        assert_eq!(edges[0].source, "work_packages");
        assert_eq!(edges[0].target, "projects");
        assert_eq!(edges[0].shape, NavShape::ControllerRedirect);
        assert_eq!(edges[0].helper, "projects_path");

        let _ = fs::remove_dir_all(&root);
    }

    /// Both shapes over the synthetic app: exactly the three wired edges,
    /// two `ErbClick` + one `ControllerRedirect`, and the ledger accounts for
    /// every file.
    #[test]
    fn both_shapes_over_synthetic_app() {
        let root = scratch_dir("both");
        write_synthetic_app(&root);

        let (edges, report) = extract_nav_edges_with_report(&root, &vocab());
        let mut pairs: Vec<(&str, &str, NavShape)> = edges
            .iter()
            .map(|e| (e.source.as_str(), e.target.as_str(), e.shape))
            .collect();
        pairs.sort();
        assert_eq!(
            pairs,
            vec![
                ("projects", "work_packages", NavShape::ErbClick),
                ("work_packages", "projects", NavShape::ErbClick),
                ("work_packages", "projects", NavShape::ControllerRedirect),
            ],
            "{edges:?}"
        );
        assert_eq!(report.erb_files, 2);
        assert_eq!(report.controller_files, 1);
        assert_eq!(report.files_with_edges, 3);

        let _ = fs::remove_dir_all(&root);
    }

    /// A route helper NOT on a nav-verb line is not a nav edge (a bare
    /// `project_path` in an asset/href context is excluded).
    #[test]
    fn route_helper_without_nav_verb_is_ignored() {
        let root = scratch_dir("no_verb");
        write_file(
            &root,
            "app/views/work_packages/index.html.erb",
            "<meta data-url=\"<%= project_path(@p) %>\">\n",
        );

        let (edges, report) = extract_nav_edges_with_report(&root, &vocab());
        assert!(edges.is_empty(), "no nav verb → no edge: {edges:?}");
        assert_eq!(report.raw_target_refs, 0);

        let _ = fs::remove_dir_all(&root);
    }

    /// A nav target whose stem is NOT a known screen is counted in
    /// `raw_target_refs` (the honest denominator) but is not an edge.
    #[test]
    fn unknown_target_is_counted_not_edged() {
        let root = scratch_dir("unknown_target");
        write_file(
            &root,
            "app/views/work_packages/index.html.erb",
            "<%= link_to \"Help\", help_path %>\n",
        );

        let (edges, report) = extract_nav_edges_with_report(&root, &vocab());
        assert!(edges.is_empty(), "help is not a known screen: {edges:?}");
        assert_eq!(report.raw_target_refs, 1, "but it IS counted raw");

        let _ = fs::remove_dir_all(&root);
    }

    /// `edit_`/`new_` action-prefixed helpers resolve to the same resource
    /// screen, and the singular/plural pairing is tolerated.
    #[test]
    fn action_prefixes_and_plural_resolve_to_the_screen() {
        let root = scratch_dir("prefixes");
        write_file(
            &root,
            "app/views/projects/index.html.erb",
            "<%= link_to \"Edit\", edit_project_path(@p) %>\n\
             <%= link_to \"New WP\", new_work_package_path %>\n",
        );

        let edges = extract_nav_edges(&root, &vocab());
        let targets: Vec<&str> = edges.iter().map(|e| e.target.as_str()).collect();
        assert!(
            targets.contains(&"projects"),
            "edit_project → projects: {edges:?}"
        );
        assert!(
            targets.contains(&"work_packages"),
            "new_work_package → work_packages: {edges:?}"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// The edge lifts into the shared `navigates_to` SPO triple, namespaced,
    /// Inferred tier — byte-identical shape to the C# arm.
    #[test]
    fn edge_lifts_to_navigates_to_triple() {
        let edge = RubyNavEdge {
            source: "work_packages".to_string(),
            target: "projects".to_string(),
            shape: NavShape::ErbClick,
            file: "app/views/work_packages/index.html.erb".to_string(),
            helper: "project_path".to_string(),
        };
        let t = edge.to_triple("openproject");
        assert_eq!(t.s, "openproject:work_packages");
        assert_eq!(t.p, "navigates_to");
        assert_eq!(t.o, "openproject:projects");
        // Inferred tier truth (0.85, 0.75) — matches Predicate::NavigatesTo.
        assert_eq!(t.p, Predicate::NavigatesTo.as_str());
        let (f, c) = Provenance::Inferred.truth();
        assert_eq!((t.f, t.c), (f, c));
    }

    /// Word-boundary: a `redirect_to_helper` custom method must not match the
    /// `redirect_to` nav verb (it is a different identifier).
    #[test]
    fn word_boundary_rejects_verb_substring() {
        let root = scratch_dir("verb_boundary");
        write_file(
            &root,
            "app/controllers/things_controller.rb",
            "  my_redirect_to_helper projects_path\n",
        );

        let edges = extract_nav_edges(&root, &vocab());
        assert!(
            edges.is_empty(),
            "`my_redirect_to_helper` is not the `redirect_to` verb: {edges:?}"
        );

        let _ = fs::remove_dir_all(&root);
    }
}
