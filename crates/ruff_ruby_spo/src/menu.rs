//! The Rails **menu-DSL** harvest — the side-nav registration shape.
//!
//! # What this is
//!
//! The third `navigates_to` production shape (alongside [`crate::navigation`]'s
//! shape A/B), and the Rails twin of the Odoo `<menuitem>` arm (ruff #66): it
//! scans `Redmine::MenuManager`-style menu registrations —
//!
//! ```ruby
//! Redmine::MenuManager.map :application_menu do |menu|
//!   menu.push :work_packages, { controller: 'work_packages', action: 'index' }
//!   menu.push :overview, :project_path
//! end
//! ```
//!
//! — and emits the shared [`Predicate::NavigatesTo`] fact
//! `("menu", navigates_to, <target_screen>)`, source fixed to the literal
//! `"menu"` root. This is exactly the edge shape op-nexgen's
//! `nav::MENU_NAV_EDGES` currently hand-authors from `nav::menu()`
//! (`&[("Board", "/"), ("Projects", "/projects")]`) — once wired, the
//! side-nav becomes harvest-driven instead of a hardcoded tab list, closing
//! the last `navigates_to` production gap (fields/buttons/widgets/nav are
//! all harvested; the side-menu was the one hand-rolled piece).
//!
//! Reuses [`crate::navigation::RubyNavEdge`] directly — no new edge type —
//! tagged with the new [`crate::navigation::NavShape::MenuItem`] shape, so
//! every downstream consumer of `RubyNavEdge`/`extract_nav_edges` already
//! knows how to fold a menu edge into the same klickweg graph.
//!
//! # Tier + doctrine (Inferred, closed-vocab line scanner)
//!
//! Same discipline as [`crate::navigation`] / [`crate::actions`] /
//! [`crate::widgets`]: no Ruby parser. A line counts as a menu registration
//! only when it calls `.push` on a receiver identifier ending in `menu`
//! (`menu`, `main_menu`, `project_menu`, `admin_menu`, …  — the
//! `Redmine::MenuManager.map do |menu| … end` block-param convention),
//! bounding false positives against unrelated `.push` calls
//! (`array.push`, `stack.push`). The target is resolved from either a
//! `controller: 'x'` kwarg or a `*_path`/`*_url` route helper on the same
//! line, matched against the caller-supplied closed vocabulary
//! ([`NavVocab::screens`]) exactly like [`crate::navigation`]'s shapes.
//! Honest denominator: every matched `.push` call is counted in
//! [`MenuScanReport::raw_menu_pushes`], whether or not its target resolved.
//!
//! # What is NOT captured (by design, not oversight)
//!
//! - **The legacy hash-rocket kwarg form** (`:controller => 'x'`) — only
//!   the modern `controller: 'x'` syntax is matched.
//! - **Dynamic / interpolated targets** — same closed-vocab-target
//!   discipline as [`crate::navigation`]: a `controller: @dynamic` or a
//!   `link_to`-free variable target is not a resolvable handle without a
//!   Rails router.
//! - **Nested/conditional menu items** (`if: ->(p) { … }` guards, submenu
//!   `parent:` nesting) — the presence of the edge is captured; visibility
//!   conditions and menu hierarchy are not.

use std::fs;
use std::path::{Path, PathBuf};

use crate::navigation::{
    NavShape, NavVocab, RubyNavEdge, collect_source_files, relative_path, resource_stem,
    route_helpers, screen_matches,
};

/// The synthetic source screen every harvested menu edge originates from —
/// the block-param root of `Redmine::MenuManager.map do |menu| … end`,
/// mirroring Odoo's `<menuitem>` root (ruff #66) and op-nexgen's
/// `nav::MENU_ROOT`. Crate-private: consumers read it off the harvested
/// [`RubyNavEdge::source`] rather than depending on the literal.
pub(crate) const MENU_SOURCE: &str = "menu";

/// Conservation-ledger totals for a menu scan (same discipline as
/// [`crate::navigation::NavScanReport`] — nothing drops silently).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MenuScanReport {
    /// Every `*.rb` file found (menu DSL is not confined to a fixed path —
    /// unlike controllers/views, it lives in `lib/`, `config/initializers/`,
    /// or engine `init.rb` files).
    pub rb_files: usize,
    /// Files that produced at least one menu [`RubyNavEdge`].
    pub files_with_menu_items: usize,
    /// Every `<menu-receiver>.push` call seen — the honest denominator,
    /// regardless of whether its target resolved to a known screen.
    pub raw_menu_pushes: usize,
}

/// Scan `<app_root>` for menu-DSL registrations and return the known-screen
/// edges (shape [`NavShape::MenuItem`]). Thin wrapper over
/// [`extract_menu_edges_with_report`].
#[must_use]
pub fn extract_menu_edges(app_root: &Path, vocab: &NavVocab) -> Vec<RubyNavEdge> {
    extract_menu_edges_with_report(app_root, vocab).0
}

/// Like [`extract_menu_edges`] but also returns the [`MenuScanReport`]
/// ledger. Walks every `*.rb` file under `app_root`; results are deduped by
/// `(target, file, helper)` and sorted for determinism.
#[must_use]
pub fn extract_menu_edges_with_report(
    app_root: &Path,
    vocab: &NavVocab,
) -> (Vec<RubyNavEdge>, MenuScanReport) {
    let mut report = MenuScanReport::default();
    let mut all_files = Vec::new();
    collect_source_files(app_root, &mut all_files);
    let rb_files: Vec<PathBuf> = all_files
        .into_iter()
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("rb"))
        .collect();
    report.rb_files = rb_files.len();

    let mut edges: Vec<RubyNavEdge> = Vec::new();
    for path in &rb_files {
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        let rel = relative_path(app_root, path);
        let before = edges.len();
        for line in content.lines() {
            let Some(args_start) = menu_push_call(line) else {
                continue;
            };
            report.raw_menu_pushes += 1;
            let args = &line[args_start..];
            // Require a `:label` symbol argument — the shape marker that
            // this `.push` call is the `menu.push :label, target, …` DSL,
            // not some other menu-receiver method.
            if first_symbol(args).is_none() {
                continue;
            }
            let Some(stem) = controller_kwarg(args).or_else(|| {
                route_helpers(line)
                    .into_iter()
                    .map(|h| resource_stem(&h))
                    .next()
            }) else {
                continue;
            };
            if let Some(screen) = vocab.screens.iter().find(|s| screen_matches(&stem, s)) {
                edges.push(RubyNavEdge {
                    source: MENU_SOURCE.to_string(),
                    target: screen.clone(),
                    shape: NavShape::MenuItem,
                    file: rel.clone(),
                    helper: stem.clone(),
                });
            }
        }
        if edges.len() > before {
            report.files_with_menu_items += 1;
        }
    }

    edges.sort_by(|a, b| {
        a.target
            .cmp(&b.target)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.helper.cmp(&b.helper))
    });
    edges.dedup_by(|a, b| a.target == b.target && a.file == b.file && a.helper == b.helper);
    (edges, report)
}

/// The byte offset right after a `.push` call on a menu-DSL receiver, or
/// `None`. The receiver identifier immediately before the `.` must END with
/// `menu` (word-bounded on its own start, e.g. `menu`/`main_menu`, not a
/// suffix match inside a longer unrelated identifier) — the closed
/// heuristic bounding this scanner to `Redmine::MenuManager`-style call
/// sites and away from unrelated `.push` calls (`array.push`, `stack.push`).
fn menu_push_call(line: &str) -> Option<usize> {
    let chars: Vec<char> = line.chars().collect();
    let word: Vec<char> = "push".chars().collect();
    let mut i = 0;
    while i + word.len() <= chars.len() {
        if chars[i..i + word.len()] == word[..] {
            let end_ok = i + word.len() == chars.len() || !is_ident_char(chars[i + word.len()]);
            if i > 0 && chars[i - 1] == '.' && end_ok {
                let recv_end = i - 1; // index of the '.'
                let mut recv_start = recv_end;
                while recv_start > 0 && is_ident_char(chars[recv_start - 1]) {
                    recv_start -= 1;
                }
                let receiver: String = chars[recv_start..recv_end].iter().collect();
                let boundary_ok = recv_start == 0 || !is_ident_char(chars[recv_start - 1]);
                if boundary_ok && !receiver.is_empty() && receiver.ends_with("menu") {
                    let byte_pos = line
                        .char_indices()
                        .nth(i + word.len())
                        .map_or(line.len(), |(b, _)| b);
                    return Some(byte_pos);
                }
            }
        }
        i += 1;
    }
    None
}

/// The quoted value of a `controller: 'x'` / `controller: "x"` kwarg in `s`,
/// if present. Modern Ruby kwarg syntax only — the legacy hash-rocket form
/// (`:controller => 'x'`) is a documented non-capture (see module docs).
fn controller_kwarg(s: &str) -> Option<String> {
    let chars: Vec<char> = s.chars().collect();
    let key: Vec<char> = "controller".chars().collect();
    let mut i = 0;
    while i + key.len() < chars.len() {
        let word_start_ok = i == 0 || !is_ident_char(chars[i - 1]);
        if word_start_ok && chars[i..i + key.len()] == key[..] {
            let mut j = i + key.len();
            if j < chars.len() && chars[j] == ':' {
                j += 1;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                if let Some(&quote) = chars.get(j) {
                    if quote == '\'' || quote == '"' {
                        let start = j + 1;
                        let mut end = start;
                        while end < chars.len() && chars[end] != quote {
                            end += 1;
                        }
                        if end < chars.len() {
                            return Some(chars[start..end].iter().collect());
                        }
                    }
                }
            }
        }
        i += 1;
    }
    None
}

/// The first `:symbol` (Ruby symbol literal) in `s` — a `:` followed by an
/// identifier run, not part of a `::` namespace. Used only as a shape gate
/// (see [`extract_menu_edges_with_report`]); the symbol's text itself is not
/// retained — a consumer's own menu-label registry (e.g. op-server's
/// `nav::menu()`) is a separate concern from the harvested edge.
fn first_symbol(s: &str) -> Option<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ':'
            && chars.get(i + 1).is_some_and(|c| is_ident_start(*c))
            && (i == 0 || chars[i - 1] != ':')
        {
            let start = i + 1;
            let mut end = start;
            while end < chars.len() && is_ident_char(chars[end]) {
                end += 1;
            }
            return Some(chars[start..end].iter().collect());
        }
        i += 1;
    }
    None
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

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
            std::env::temp_dir().join(format!("ruff_ruby_spo_menu_{}_{case}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        root
    }

    fn vocab() -> NavVocab {
        NavVocab {
            screens: vec!["projects".to_string(), "work_packages".to_string()],
        }
    }

    /// `controller:` kwarg form — the common `Redmine::MenuManager` shape.
    #[test]
    fn controller_kwarg_form_resolves_to_screen() {
        let root = scratch_dir("controller_kwarg");
        write_file(
            &root,
            "lib/redmine/default_menu.rb",
            "Redmine::MenuManager.map :application_menu do |menu|\n\
             \x20 menu.push :work_packages, { controller: 'work_packages', action: 'index' }\n\
             end\n",
        );

        let edges = extract_menu_edges(&root, &vocab());
        assert_eq!(edges.len(), 1, "{edges:?}");
        assert_eq!(edges[0].source, "menu");
        assert_eq!(edges[0].target, "work_packages");
        assert_eq!(edges[0].shape, NavShape::MenuItem);

        let _ = fs::remove_dir_all(&root);
    }

    /// Route-helper fallback form — `menu.push :label, :project_path`.
    #[test]
    fn route_helper_form_resolves_to_screen() {
        let root = scratch_dir("route_helper");
        write_file(
            &root,
            "lib/redmine/default_menu.rb",
            "Redmine::MenuManager.map :application_menu do |menu|\n\
             \x20 menu.push :overview, :project_path\n\
             end\n",
        );

        let edges = extract_menu_edges(&root, &vocab());
        assert_eq!(edges.len(), 1, "{edges:?}");
        assert_eq!(edges[0].target, "projects");

        let _ = fs::remove_dir_all(&root);
    }

    /// Word boundary + receiver suffix: an unrelated `.push` (on `array`,
    /// or on a receiver that merely CONTAINS `menu` mid-word rather than
    /// ending in it) must not match.
    #[test]
    fn unrelated_push_calls_are_ignored() {
        let root = scratch_dir("unrelated");
        write_file(
            &root,
            "lib/some_helper.rb",
            "array.push :work_packages\n\
             menuitem_registry.push :projects\n",
        );

        let (edges, report) = extract_menu_edges_with_report(&root, &vocab());
        assert!(
            edges.is_empty(),
            "neither receiver ends in `menu`: {edges:?}"
        );
        assert_eq!(report.raw_menu_pushes, 0);

        let _ = fs::remove_dir_all(&root);
    }

    /// A `<x>menu.push` call whose target is NOT a known screen is counted
    /// in `raw_menu_pushes` (the honest denominator) but produces no edge.
    #[test]
    fn unknown_target_is_counted_not_edged() {
        let root = scratch_dir("unknown_target");
        write_file(
            &root,
            "lib/redmine/default_menu.rb",
            "menu.push :help, { controller: 'help' }\n",
        );

        let (edges, report) = extract_menu_edges_with_report(&root, &vocab());
        assert!(edges.is_empty(), "help is not a known screen: {edges:?}");
        assert_eq!(report.raw_menu_pushes, 1, "but it IS counted raw");

        let _ = fs::remove_dir_all(&root);
    }

    /// A `.push` call missing the leading `:label` symbol is not treated as
    /// the menu DSL shape (still counted raw).
    #[test]
    fn push_without_label_symbol_is_not_an_edge() {
        let root = scratch_dir("no_label");
        write_file(
            &root,
            "lib/redmine/default_menu.rb",
            "menu.push(project_variable)\n",
        );

        let (edges, report) = extract_menu_edges_with_report(&root, &vocab());
        assert!(edges.is_empty(), "{edges:?}");
        assert_eq!(report.raw_menu_pushes, 1);

        let _ = fs::remove_dir_all(&root);
    }

    /// The edge lifts into the shared `navigates_to` SPO triple via
    /// [`RubyNavEdge::to_triple`] — no new lift method needed, source fixed
    /// to `"menu"`.
    #[test]
    fn edge_lifts_to_navigates_to_triple_via_shared_type() {
        let edge = RubyNavEdge {
            source: MENU_SOURCE.to_string(),
            target: "projects".to_string(),
            shape: NavShape::MenuItem,
            file: "lib/redmine/default_menu.rb".to_string(),
            helper: "projects".to_string(),
        };
        let t = edge.to_triple("openproject");
        assert_eq!(t.s, "openproject:menu");
        assert_eq!(t.p, "navigates_to");
        assert_eq!(t.o, "openproject:projects");
    }

    /// Ledger totals across a two-item menu block, one resolvable and one
    /// carrying an unknown target.
    #[test]
    fn ledger_counts_files_and_pushes() {
        let root = scratch_dir("ledger");
        write_file(
            &root,
            "lib/redmine/default_menu.rb",
            "Redmine::MenuManager.map :application_menu do |menu|\n\
             \x20 menu.push :work_packages, { controller: 'work_packages' }\n\
             \x20 menu.push :help, { controller: 'help' }\n\
             end\n",
        );
        write_file(&root, "lib/unrelated.rb", "puts 'no menu here'\n");

        let (edges, report) = extract_menu_edges_with_report(&root, &vocab());
        assert_eq!(edges.len(), 1, "{edges:?}");
        assert_eq!(report.rb_files, 2);
        assert_eq!(report.files_with_menu_items, 1);
        assert_eq!(report.raw_menu_pushes, 2);

        let _ = fs::remove_dir_all(&root);
    }
}
