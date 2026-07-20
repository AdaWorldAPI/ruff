//! The ERB **action-button** harvest — the mutating-affordance shape.
//!
//! # What this is (and why it is distinct from the nav arm)
//!
//! [`crate::navigation`] harvests `navigates_to` — plain GET links between
//! screens (the Klickweg). This arm harvests [`Predicate::InvokesAction`]:
//! the **buttons that mutate**, which a plain nav scan cannot see and codegen
//! must emit *as a button, not a link*. In Rails ERB a mutating action is one
//! of three affordances, all with a **non-GET HTTP verb**:
//!
//! - `button_to "Label", <t>_path(..)[, method: :verb]` — a form-backed
//!   button; defaults to **POST** when no `method:` is given.
//! - `link_to "Label", <t>_path(..), method: :patch|:put|:delete|:post` — a
//!   link that Rails' UJS turns into a non-GET request. A `link_to` **without**
//!   a non-GET `method:` is navigation, not an action, and is left to
//!   [`crate::navigation`] (a `method: :get` link is also navigation).
//! - a form submit (`<button type="submit">` / `f.submit`) — the form's own
//!   verb; captured as the enclosing form's action (see non-captures).
//!
//! The emitted fact is `(source_screen, invokes_action, resource#member)` —
//! the object names the ACTION (the route helper's stem, e.g.
//! `complete_work_package`), never a screen. This is the same discipline
//! `selects_view` (ruff #64) uses to keep `navigates_to` a pure screen→screen
//! graph: an action is not a screen, so it gets its own plane. The HTTP verb
//! rides the harvested [`RubyActionEdge`] (frontend detail for codegen: "emit
//! a `<button>` with method X"), not the SPO object.
//!
//! # Tier + doctrine (Inferred, closed-vocab line scanner)
//!
//! Like [`crate::navigation`] / [`crate::views`], this is a closed-vocabulary
//! ERB line scanner — **no Ruby parser**. The closed vocab here is the set of
//! mutating HTTP verbs (`post`/`put`/`patch`/`delete`); the affordance verb
//! (`button_to` / `link_to`) gates the line, the `method:` kwarg (or the
//! `button_to` POST default) supplies the HTTP verb, and the `<t>_path` helper
//! supplies the action target. Honest denominator: every `button_to` and every
//! non-GET `link_to` seen is counted in [`ActionScanReport::raw_action_refs`],
//! whether or not a target was parseable.
//!
//! # What is NOT captured (by design)
//!
//! - **Form submits inside `<%= form_with %>` blocks** — the submit button is
//!   a separate line from the form's `url:`/`method:`, so a single-line scan
//!   cannot join them; that is a cross-line join (the Odoo-menu #66 lesson) and
//!   is out of scope for this line scanner. `button_to` (self-contained) is the
//!   covered form-backed affordance.
//! - **Dynamic verbs / targets** — `method: some_var`, or a target that is an
//!   object/variable rather than a `<t>_path`/`<t>_url` helper.

use std::fs;
use std::path::{Path, PathBuf};

use ruff_spo_triplet::{Predicate, Provenance, Triple};

/// The mutating HTTP verb an action affordance carries. GET is deliberately
/// absent — a GET affordance is navigation ([`crate::navigation`]), not an
/// action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ActionVerb {
    Post,
    Put,
    Patch,
    Delete,
}

impl ActionVerb {
    /// Parse a Rails `method:` value (`:patch`, `"patch"`, `patch`) into a
    /// mutating verb. `None` for `get` (navigation) or anything unknown.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw
            .trim()
            .trim_start_matches(':')
            .trim_matches('"')
            .trim_matches('\'')
        {
            "post" => Some(Self::Post),
            "put" => Some(Self::Put),
            "patch" => Some(Self::Patch),
            "delete" => Some(Self::Delete),
            _ => None,
        }
    }

    /// The lowercase HTTP verb (what codegen stamps on the emitted button).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Post => "post",
            Self::Put => "put",
            Self::Patch => "patch",
            Self::Delete => "delete",
        }
    }
}

/// One harvested action affordance: a button/link on `source` that invokes a
/// mutating action `target` via `verb`, labelled `label`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RubyActionEdge {
    /// The screen the affordance lives on (the view's controller-dir segment).
    pub source: String,
    /// The action target — the route helper stem (resource + member action,
    /// e.g. `complete_work_package`).
    pub target: String,
    /// The mutating HTTP verb the affordance carries.
    pub verb: ActionVerb,
    /// The affordance's visible label (first string literal), best-effort.
    pub label: String,
    /// Source file path relative to the views root (`/`-joined).
    pub file: String,
}

impl RubyActionEdge {
    /// Lift into the shared `invokes_action` SPO triple, namespaced. The
    /// object names the ACTION (`<ns>:<target>`); the verb stays on the edge
    /// (codegen reads it to emit the right `<button method>`). Inferred tier.
    #[must_use]
    pub fn to_triple(&self, namespace: &str) -> Triple {
        Triple::new(
            format!("{namespace}:{}", self.source),
            Predicate::InvokesAction,
            format!("{namespace}:{}", self.target),
            Provenance::Inferred,
        )
    }
}

/// Conservation-ledger totals for an action scan (same discipline as
/// [`crate::navigation::NavScanReport`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ActionScanReport {
    /// Every `*.erb` file found.
    pub erb_files: usize,
    /// Files that produced at least one [`RubyActionEdge`].
    pub files_with_actions: usize,
    /// Every mutating affordance seen (a `button_to`, or a non-GET
    /// `link_to`) — the honest denominator, whether or not a target parsed.
    pub raw_action_refs: usize,
}

/// Scan `<views_root>` for `*.erb` action affordances. Thin wrapper over
/// [`extract_action_edges_with_report`].
#[must_use]
pub fn extract_action_edges(views_root: &Path) -> Vec<RubyActionEdge> {
    extract_action_edges_with_report(views_root).0
}

/// Like [`extract_action_edges`] but also returns the [`ActionScanReport`].
/// Results are deduped by `(source, target, verb)` and sorted for determinism.
#[must_use]
pub fn extract_action_edges_with_report(
    views_root: &Path,
) -> (Vec<RubyActionEdge>, ActionScanReport) {
    let mut report = ActionScanReport::default();
    let mut files = Vec::new();
    collect_erb_files(views_root, &mut files);
    report.erb_files = files.len();

    let mut edges: Vec<RubyActionEdge> = Vec::new();
    for path in &files {
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        let rel = relative_path(views_root, path);
        let source = erb_source_screen(&rel);
        let before = edges.len();
        for line in content.lines() {
            if let Some((verb, is_affordance)) = affordance_verb(line) {
                if is_affordance {
                    report.raw_action_refs += 1;
                }
                if let Some(target) = action_target(line) {
                    edges.push(RubyActionEdge {
                        source: source.clone(),
                        target,
                        verb,
                        label: first_string_literal(line).unwrap_or_default(),
                        file: rel.clone(),
                    });
                }
            }
        }
        if edges.len() > before {
            report.files_with_actions += 1;
        }
    }

    edges.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then_with(|| a.target.cmp(&b.target))
            .then_with(|| a.verb.cmp(&b.verb))
            .then_with(|| a.file.cmp(&b.file))
    });
    edges.dedup_by(|a, b| a.source == b.source && a.target == b.target && a.verb == b.verb);
    (edges, report)
}

/// The mutating verb of an action affordance on `line`, plus whether the line
/// IS an affordance (for the honest denominator). Returns `None` for a line
/// that is neither a `button_to` nor a non-GET `link_to`.
///
/// - `button_to` → the `method:` verb, or POST by default (Rails' default).
/// - `link_to` → ONLY when it carries a non-GET `method:` (else it is a plain
///   navigation link, left to [`crate::navigation`]).
fn affordance_verb(line: &str) -> Option<(ActionVerb, bool)> {
    let method = method_kwarg(line);
    if contains_word(line, "button_to") {
        // button_to defaults to POST when no explicit method is given.
        let verb = method
            .and_then(|m| ActionVerb::parse(&m))
            .unwrap_or(ActionVerb::Post);
        return Some((verb, true));
    }
    if contains_word(line, "link_to") {
        if let Some(verb) = method.as_deref().and_then(ActionVerb::parse) {
            return Some((verb, true));
        }
    }
    None
}

/// The value of a `method:` kwarg on the line (`method: :patch` → `":patch"`,
/// `method: "delete"` → `"\"delete\""`), if present. Best-effort single-line
/// read — the token after `method:` up to the next `,`/`)`/whitespace.
fn method_kwarg(line: &str) -> Option<String> {
    let idx = line.find("method:")?;
    let rest = line[idx + "method:".len()..].trim_start();
    let end = rest
        .find(|c: char| c == ',' || c == ')' || c == '}' || c.is_whitespace())
        .unwrap_or(rest.len());
    let tok = rest[..end].trim();
    if tok.is_empty() {
        None
    } else {
        Some(tok.to_string())
    }
}

/// The action target of an affordance line — the FIRST `<stem>_path`/
/// `<stem>_url` route helper, stem = the helper minus the `_path`/`_url`
/// suffix (the `new_`/`edit_` prefix is NOT stripped here: unlike navigation,
/// an action's member prefix — `complete_`, `close_` — IS the action name).
fn action_target(line: &str) -> Option<String> {
    route_helpers(line).into_iter().next().map(|h| {
        h.strip_suffix("_path")
            .or_else(|| h.strip_suffix("_url"))
            .unwrap_or(&h)
            .to_string()
    })
}

/// Every `<stem>_path`/`<stem>_url` route-helper token on the line.
fn route_helpers(line: &str) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if is_ident_char(chars[i]) && (i == 0 || !is_ident_char(chars[i - 1])) {
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

/// The first double-quoted string literal on the line (an affordance's label
/// is conventionally its first argument). Best-effort.
fn first_string_literal(line: &str) -> Option<String> {
    let start = line.find('"')?;
    let rest = &line[start + 1..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// The source screen for a view: the segment after the last `views`
/// component (`app/views/<screen>/index.html.erb` → `<screen>`), else the
/// file's parent directory.
fn erb_source_screen(rel: &str) -> String {
    let segments: Vec<&str> = rel.split('/').collect();
    if let Some(views_idx) = segments.iter().rposition(|s| *s == "views") {
        if let Some(screen) = segments.get(views_idx + 1) {
            if views_idx + 2 < segments.len() {
                return (*screen).to_string();
            }
        }
    }
    if segments.len() >= 2 {
        return segments[segments.len() - 2].to_string();
    }
    "?".to_string()
}

fn collect_erb_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect_erb_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("erb") {
            out.push(path);
        }
    }
}

fn relative_path(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

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

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_view(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn scratch_dir(case: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "ruff_ruby_spo_actions_{}_{case}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }

    /// A `button_to` with an explicit `method:` is an action with that verb.
    #[test]
    fn button_to_with_method_is_an_action() {
        let root = scratch_dir("button_to");
        write_view(
            &root,
            "app/views/work_packages/show.html.erb",
            "<%= button_to \"Complete\", complete_work_package_path(@wp), method: :patch %>\n",
        );
        let edges = extract_action_edges(&root);
        assert_eq!(edges.len(), 1, "{edges:?}");
        assert_eq!(edges[0].source, "work_packages");
        assert_eq!(edges[0].target, "complete_work_package");
        assert_eq!(edges[0].verb, ActionVerb::Patch);
        assert_eq!(edges[0].label, "Complete");
        let _ = fs::remove_dir_all(&root);
    }

    /// A bare `button_to` (no `method:`) defaults to POST — Rails' default.
    #[test]
    fn bare_button_to_defaults_to_post() {
        let root = scratch_dir("bare_button");
        write_view(
            &root,
            "app/views/work_packages/index.html.erb",
            "<%= button_to \"Watch\", watch_work_package_path(@wp) %>\n",
        );
        let edges = extract_action_edges(&root);
        assert_eq!(edges.len(), 1, "{edges:?}");
        assert_eq!(edges[0].verb, ActionVerb::Post);
        assert_eq!(edges[0].target, "watch_work_package");
        let _ = fs::remove_dir_all(&root);
    }

    /// A `link_to` with `method: :delete` is an action (Rails UJS turns it
    /// into a DELETE) — the destructive-link idiom.
    #[test]
    fn link_to_with_delete_is_an_action() {
        let root = scratch_dir("link_delete");
        write_view(
            &root,
            "app/views/work_packages/show.html.erb",
            "<%= link_to \"Delete\", work_package_path(@wp), method: :delete, data: { confirm: \"Sure?\" } %>\n",
        );
        let edges = extract_action_edges(&root);
        assert_eq!(edges.len(), 1, "{edges:?}");
        assert_eq!(edges[0].verb, ActionVerb::Delete);
        assert_eq!(edges[0].target, "work_package");
        assert_eq!(edges[0].label, "Delete");
        let _ = fs::remove_dir_all(&root);
    }

    /// A plain `link_to` (no method, or `method: :get`) is NAVIGATION, not an
    /// action — it belongs to `navigation.rs`, and must NOT be harvested here
    /// or counted in the denominator.
    #[test]
    fn plain_and_get_link_to_are_not_actions() {
        let root = scratch_dir("plain_link");
        write_view(
            &root,
            "app/views/work_packages/show.html.erb",
            "<%= link_to \"Edit\", edit_work_package_path(@wp) %>\n\
             <%= link_to \"Project\", project_path(@wp.project), method: :get %>\n",
        );
        let (edges, report) = extract_action_edges_with_report(&root);
        assert!(
            edges.is_empty(),
            "plain/get link_to is navigation: {edges:?}"
        );
        assert_eq!(report.raw_action_refs, 0);
        let _ = fs::remove_dir_all(&root);
    }

    /// The edge lifts into the shared `invokes_action` SPO triple: object is
    /// the ACTION (namespaced target), Inferred tier — the verb stays on the
    /// edge, off the object.
    #[test]
    fn edge_lifts_to_invokes_action_triple() {
        let edge = RubyActionEdge {
            source: "work_packages".to_string(),
            target: "complete_work_package".to_string(),
            verb: ActionVerb::Patch,
            label: "Complete".to_string(),
            file: "app/views/work_packages/show.html.erb".to_string(),
        };
        let t = edge.to_triple("openproject");
        assert_eq!(t.s, "openproject:work_packages");
        assert_eq!(t.p, "invokes_action");
        assert_eq!(t.p, Predicate::InvokesAction.as_str());
        assert_eq!(t.o, "openproject:complete_work_package");
        let (f, c) = Provenance::Inferred.truth();
        assert_eq!((t.f, t.c), (f, c));
    }

    /// The honest denominator: a `button_to` with no route helper is still
    /// counted as an affordance (`raw_action_refs`) but yields no edge.
    #[test]
    fn affordance_without_target_is_counted_not_edged() {
        let root = scratch_dir("no_target");
        write_view(
            &root,
            "app/views/sessions/show.html.erb",
            "<%= button_to \"Sign out\", \"/logout\", method: :delete %>\n",
        );
        let (edges, report) = extract_action_edges_with_report(&root);
        assert!(edges.is_empty(), "no *_path helper → no edge: {edges:?}");
        assert_eq!(report.raw_action_refs, 1, "but the affordance IS counted");
        let _ = fs::remove_dir_all(&root);
    }

    /// Ledger + multi-affordance file: two actions in one view, both shapes.
    #[test]
    fn ledger_counts_files_and_affordances() {
        let root = scratch_dir("ledger");
        write_view(
            &root,
            "app/views/work_packages/show.html.erb",
            "<%= button_to \"Complete\", complete_work_package_path(@wp), method: :patch %>\n\
             <%= link_to \"Delete\", work_package_path(@wp), method: :delete %>\n\
             <%= link_to \"Back\", work_packages_path %>\n",
        );
        write_view(
            &root,
            "app/views/work_packages/_form.html.erb",
            "<p>no actions</p>\n",
        );
        let (edges, report) = extract_action_edges_with_report(&root);
        assert_eq!(
            edges.len(),
            2,
            "complete + delete, not the plain Back link: {edges:?}"
        );
        assert_eq!(report.erb_files, 2);
        assert_eq!(report.files_with_actions, 1);
        assert_eq!(report.raw_action_refs, 2);
        let _ = fs::remove_dir_all(&root);
    }
}
