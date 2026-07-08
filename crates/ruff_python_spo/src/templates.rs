//! Jinja/Django template field-set extractor — the presentation-tier harvest.
//!
//! # One brick, three skins
//!
//! ERB (Rails), askama (Rust), and Jinja/Django (Python) are three renderers
//! over the **identical** `WideFieldMask` projection: a consumer takes the
//! field SET this harvest produces and mints the mask via
//! `WideFieldMask::from_universe_present(basis, fields)`. This module is the
//! **Jinja source** of that mask — the direct sibling of `ruff_ruby_spo`'s ERB
//! `views.rs`, with the same `ViewTarget` / `ViewFieldSet` vocabulary so the
//! three skins stay aligned across frontends.
//!
//! # Doctrine (fuzzy-recipe-codebook.md §8c — "detected config becomes data")
//!
//! A Jinja/Django template is a **detected configuration artifact**: it names,
//! via `<receiver>.<field>` references, exactly which model fields a route
//! projects to the user. Per the config-as-data rule, that artifact becomes a
//! **data input to the codebook** — the referenced field SET — never code to
//! transcribe. We do NOT parse Jinja structure, walk template expressions, or
//! reproduce layout/markup. The only fact recorded is *presence*: does this
//! template, anywhere, reference `<model>.<field>`? Two templates projecting
//! the same ten fields in different table layouts are identical for this
//! purpose.
//!
//! # Why a line scanner in an AST crate
//!
//! The rest of this crate is AST-based (via `ruff_python_parser`) because its
//! inputs are Python source. Templates are **not** Python — a `.html`/`.jinja`
//! file has no Python AST to walk — so the correct tool here is the same
//! closed-vocabulary line scanner the ERB arm uses, not a parser. Scanning
//! lines for `<receiver>.<ident>` automatically catches references inside
//! `{% if obj.field %}`, `{{ obj.field|date }}`, and `{% for x in obj.items %}`
//! — presence is presence, matching the ERB arm's "helper-wrapped reference is
//! still captured" stance. A filter (`obj.field|filter`) comes AFTER the
//! identifier, so the ident charset scan already terminates before the `|` and
//! `field` is captured.
//!
//! # Tier: Inferred, by construction
//!
//! A reference is only recorded when BOTH the receiver identifier and the
//! field identifier match caller-supplied closed vocabularies
//! ([`ViewTarget::receivers`] / [`ViewTarget::fields`]) — this bounds false
//! positives at the cost of requiring the harvest stratum (schema +
//! declarations) to already know the field list. It is Inferred, not
//! Authoritative: `{{ project.name|upper }}` and a bare `{{ project.name }}`
//! are indistinguishable here (both project the field, which is all this
//! stratum claims).
//!
//! # What is NOT captured (by design, not oversight)
//!
//! - **Presentation** — HTML structure, CSS classes, i18n strings,
//!   conditionals, loops. Only the field-name SET, per the doctrine above.
//! - **Multi-hop chains** (`project.owner.name`) — only the first hop off a
//!   registered receiver is read; `owner.name` is a second, independent
//!   reference the caller registers under its own [`ViewTarget`] if it wants
//!   it captured.
//! - **Jinja expressions/macros** — `{% macro %}` bodies, `set` locals, and
//!   arbitrary expressions are not evaluated; only textual
//!   `<receiver>.<ident>` presence counts.
//! - **Template inheritance resolution** — `{% extends %}` / `{% include %}`
//!   are not followed; each file is scanned independently. A field projected
//!   by a parent layout is attributed to the parent's file, not the child's.
//!
//! # The honest coverage denominator (`ViewFieldSet::referenced`)
//!
//! A `coverage = |known| / |referenced|` metric needs the RAW distinct
//! `<receiver>.<ident>` references as its denominator, not just the subset
//! that happens to already be in the harvested field vocabulary — otherwise
//! coverage is trivially `1.0` (every hit counted is, by construction, a
//! known hit). [`ViewFieldSet::referenced`] is that raw set: every distinct
//! identifier seen immediately after a *registered* receiver + `.`,
//! regardless of vocabulary membership. [`ViewFieldSet::fields`] stays the
//! known subset — `fields ⊆ referenced` always holds (enforced by
//! construction: a candidate is recorded into `referenced` unconditionally,
//! then additionally into `fields` when it matches the vocabulary).
//!
//! Note: unlike the ERB arm, the identifier charset here is plain
//! `[A-Za-z0-9_]` — **no `@`**, which is a Ruby ivar concept with no
//! Python/Jinja counterpart. Django context variables and Jinja locals are
//! plain names (`project`, `work_package`) and look identical to the scanner.

use std::fs;
use std::path::{Path, PathBuf};

/// One target model whose field references a template scan should look for.
///
/// `receivers` is the closed vocabulary of context-variable names a template
/// might bind the resource to (e.g. `["project"]` — Django context vars and
/// Jinja locals are plain identifiers). `fields` is the closed vocabulary of
/// known field names for `model` (typically the harvested schema +
/// declarations stratum for that model). Same shape as `ruff_ruby_spo`'s
/// `ViewTarget` — the cross-frontend vocabulary is deliberately identical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewTarget {
    /// Model name as harvested (e.g. `"Project"`).
    pub model: String,
    /// Receiver identifiers a template might bind the resource to.
    pub receivers: Vec<String>,
    /// Known field names for `model` — the closed vocabulary a
    /// `<receiver>.<name>` reference must match to count.
    pub fields: Vec<String>,
}

/// One template's model-field projection: which harvested fields of
/// `resource` the Jinja/Django template references. Presence-only (§8c
/// doctrine): the SET, never the presentation. Inferred tier by nature
/// (closed-vocab field-reference scan, no template parse).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewFieldSet {
    /// Model name as harvested (e.g. `"Project"`).
    pub resource: String,
    /// Template file path relative to the templates root (e.g.
    /// `"projects/detail.html"`).
    pub view: String,
    /// Referenced field names — deduped, sorted. Closed-vocab: ONLY names in
    /// the harvested field list count (a filtered reference like
    /// `{{ project.name|upper }}` still matches `project.name`).
    pub fields: Vec<String>,
    /// Every distinct identifier referenced immediately after a *registered*
    /// receiver + `.` — deduped, sorted — REGARDLESS of whether the
    /// identifier is in the harvested field vocabulary. The honest
    /// denominator for a `coverage = |fields| / |referenced|` metric (see the
    /// module doc). `fields` is always a subset of this set.
    pub referenced: Vec<String>,
}

/// Conservation-ledger totals for a template scan (same discipline as the
/// ERB arm's `ViewScanReport` — nothing drops silently).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ViewScanReport {
    /// Every template file (`*.html` / `*.jinja` / `*.jinja2` / `*.j2`)
    /// found under the templates root.
    pub template_files: usize,
    /// Files that produced at least one non-empty [`ViewFieldSet`] — a known
    /// field hit OR a raw `referenced` ident off a registered receiver.
    pub views_with_hits: usize,
}

/// The file extensions treated as template files. `.html` covers the Django
/// convention (templates are plain `.html` under a templates dir); the
/// `.jinja` / `.jinja2` / `.j2` trio covers explicit Jinja naming.
const TEMPLATE_EXTENSIONS: &[&str] = &["html", "jinja", "jinja2", "j2"];

/// Scan `<templates_root>` for template files and extract, per template file
/// and per target model, the set of known model fields referenced. Thin
/// wrapper over [`extract_template_field_sets_with_report`] for callers that
/// don't need the scan ledger.
#[must_use]
pub fn extract_template_field_sets(
    templates_root: &Path,
    targets: &[ViewTarget],
) -> Vec<ViewFieldSet> {
    extract_template_field_sets_with_report(templates_root, targets).0
}

/// Like [`extract_template_field_sets`], but also returns a
/// [`ViewScanReport`] ledger of how many template files were seen and how
/// many produced a hit.
#[must_use]
pub fn extract_template_field_sets_with_report(
    templates_root: &Path,
    targets: &[ViewTarget],
) -> (Vec<ViewFieldSet>, ViewScanReport) {
    let mut report = ViewScanReport::default();
    let mut files = Vec::new();
    collect_template_files(templates_root, &mut files);
    report.template_files = files.len();

    let mut results = Vec::new();
    for path in &files {
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        let view = relative_view_path(templates_root, path);
        let mut file_had_hit = false;
        for target in targets {
            let (fields, referenced) = referenced_fields(&content, target);
            if fields.is_empty() && referenced.is_empty() {
                continue;
            }
            debug_assert!(
                fields.iter().all(|f| referenced.contains(f)),
                "fields must be a subset of referenced: fields={fields:?} referenced={referenced:?}"
            );
            file_had_hit = true;
            results.push(ViewFieldSet {
                resource: target.model.clone(),
                view: view.clone(),
                fields,
                referenced,
            });
        }
        if file_had_hit {
            report.views_with_hits += 1;
        }
    }

    results.sort_by(|a, b| {
        a.view
            .cmp(&b.view)
            .then_with(|| a.resource.cmp(&b.resource))
    });
    (results, report)
}

/// Walk `dir` recursively, appending every file whose extension is one of
/// [`TEMPLATE_EXTENSIONS`]. Entries are sorted before recursing so the result
/// is deterministic — the same discipline as the ERB arm's file walk and
/// [`crate::navigation`]'s `collect_py_files`.
fn collect_template_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect_template_files(&path, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| TEMPLATE_EXTENSIONS.contains(&e))
        {
            out.push(path);
        }
    }
}

/// `path` relative to `root`, rendered with `/` separators regardless of
/// platform (the view path is a stable identifier, not a filesystem path to
/// reopen).
fn relative_view_path(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// The field references in `content` for one [`ViewTarget`]: every
/// `<receiver>.<ident>` where `receiver` is one of `target.receivers`.
/// Returns `(fields, referenced)` — `fields` is the closed-vocab subset (only
/// `<ident>` values in `target.fields`), `referenced` is every distinct
/// `<ident>` seen regardless of vocabulary membership. Both deduped + sorted;
/// `fields` is always a subset of `referenced`.
fn referenced_fields(content: &str, target: &ViewTarget) -> (Vec<String>, Vec<String>) {
    let mut found = std::collections::BTreeSet::new();
    let mut referenced = std::collections::BTreeSet::new();
    for line in content.lines() {
        for receiver in &target.receivers {
            scan_line_for_receiver(line, receiver, &target.fields, &mut found, &mut referenced);
        }
    }
    (
        found.into_iter().collect(),
        referenced.into_iter().collect(),
    )
}

/// Scan one `line` for occurrences of `receiver` immediately followed by
/// `.<identifier>`. Every such `<identifier>` is recorded into `referenced`
/// unconditionally; it is ADDITIONALLY recorded into `found` when it matches
/// one of `fields` exactly (so `found ⊆ referenced` by construction).
/// `receiver` must sit on a word boundary (the preceding character, if any,
/// must not itself be an identifier character) — this rejects
/// `subproject.name` as a match for receiver `project`. A trailing Jinja
/// filter (`{{ obj.field|date }}`) needs no special handling: `|` is not an
/// identifier character, so the field scan terminates before it and `field`
/// is captured.
fn scan_line_for_receiver(
    line: &str,
    receiver: &str,
    fields: &[String],
    found: &mut std::collections::BTreeSet<String>,
    referenced: &mut std::collections::BTreeSet<String>,
) {
    if receiver.is_empty() {
        return;
    }
    let chars: Vec<char> = line.chars().collect();
    let recv: Vec<char> = receiver.chars().collect();
    if chars.len() < recv.len() {
        return;
    }
    for start in 0..=(chars.len() - recv.len()) {
        if chars[start..start + recv.len()] != recv[..] {
            continue;
        }
        if start > 0 && is_ident_char(chars[start - 1]) {
            continue;
        }
        let end = start + recv.len();
        if end >= chars.len() || chars[end] != '.' {
            continue;
        }
        let field_start = end + 1;
        let mut field_end = field_start;
        while field_end < chars.len() && is_ident_char(chars[field_end]) {
            field_end += 1;
        }
        if field_end == field_start {
            continue;
        }
        let candidate: String = chars[field_start..field_end].iter().collect();
        referenced.insert(candidate.clone());
        if fields.iter().any(|f| f == &candidate) {
            found.insert(candidate);
        }
    }
}

/// Identifier-forming characters for the word-boundary check: plain
/// `[A-Za-z0-9_]`. Unlike the ERB arm, `@` is NOT included — a Ruby ivar
/// sigil has no Python/Jinja counterpart; Django context variables and Jinja
/// locals are plain identifiers.
fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_template(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn project_target() -> ViewTarget {
        ViewTarget {
            model: "Project".to_string(),
            receivers: vec!["project".to_string()],
            fields: vec![
                "name".to_string(),
                "active".to_string(),
                "status".to_string(),
            ],
        }
    }

    fn work_package_target() -> ViewTarget {
        ViewTarget {
            model: "WorkPackage".to_string(),
            receivers: vec!["work_package".to_string()],
            fields: vec!["subject".to_string(), "due_date".to_string()],
        }
    }

    fn scratch_dir(case: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "ruff_python_spo_templates_{}_{case}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }

    /// (1) A plain `{{ receiver.field }}` reference in an `.html` template is
    /// captured in both `fields` and `referenced`.
    #[test]
    fn simple_field_reference_is_captured() {
        let root = scratch_dir("simple");
        write_template(&root, "projects/detail.html", "{{ project.name }}\n");

        let sets = extract_template_field_sets(&root, &[project_target()]);
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].resource, "Project");
        assert_eq!(sets[0].view, "projects/detail.html");
        assert_eq!(sets[0].fields, vec!["name".to_string()]);
        assert_eq!(sets[0].referenced, vec!["name".to_string()]);

        let _ = fs::remove_dir_all(&root);
    }

    /// (2) A filtered reference (`{{ work_package.subject|upper }}`) is still
    /// captured — the `|` is not an identifier character, so the ident scan
    /// terminates before the filter.
    #[test]
    fn filtered_reference_is_captured() {
        let root = scratch_dir("filter");
        write_template(
            &root,
            "work_packages/show.jinja",
            "{{ work_package.subject|upper }}\n",
        );

        let sets = extract_template_field_sets(&root, &[work_package_target()]);
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].fields, vec!["subject".to_string()]);
        assert_eq!(sets[0].referenced, vec!["subject".to_string()]);

        let _ = fs::remove_dir_all(&root);
    }

    /// (3) A reference inside a tag (`{% if project.active %}`) is captured —
    /// the line scanner does not care about Jinja delimiters, only presence.
    #[test]
    fn tag_wrapped_reference_is_captured() {
        let root = scratch_dir("tag");
        write_template(
            &root,
            "projects/detail.html",
            "{% if project.active %}live{% endif %}\n",
        );

        let sets = extract_template_field_sets(&root, &[project_target()]);
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].fields, vec!["active".to_string()]);
        assert_eq!(sets[0].referenced, vec!["active".to_string()]);

        let _ = fs::remove_dir_all(&root);
    }

    /// (4) A field NOT in the closed vocabulary must not land in `fields` —
    /// but it IS captured in `referenced`, the raw honest-denominator set
    /// (§ module doc), and the set is still reported.
    #[test]
    fn unknown_field_is_not_captured() {
        let root = scratch_dir("unknown_field");
        write_template(&root, "projects/detail.html", "{{ project.frobnicate }}\n");

        let sets = extract_template_field_sets(&root, &[project_target()]);
        assert_eq!(sets.len(), 1, "referenced-only hit must still be reported");
        assert!(
            sets[0].fields.is_empty(),
            "unknown field must not be captured as a field: {:?}",
            sets[0].fields
        );
        assert_eq!(sets[0].referenced, vec!["frobnicate".to_string()]);

        let _ = fs::remove_dir_all(&root);
    }

    /// (5) A receiver NOT registered on the target must not be captured, even
    /// though its field name is in the closed vocabulary.
    #[test]
    fn unknown_receiver_is_not_captured() {
        let root = scratch_dir("unknown_receiver");
        write_template(&root, "projects/detail.html", "{{ other.name }}\n");

        let sets = extract_template_field_sets(&root, &[project_target()]);
        assert!(
            sets.is_empty(),
            "unregistered receiver must not be captured: {sets:?}"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// (6) Word-boundary: `subproject` must not match receiver `project`.
    #[test]
    fn word_boundary_rejects_receiver_substring() {
        let root = scratch_dir("word_boundary");
        write_template(&root, "projects/detail.html", "{{ subproject.name }}\n");

        let sets = extract_template_field_sets(&root, &[project_target()]);
        assert!(
            sets.is_empty(),
            "`subproject` must not match receiver `project`: {sets:?}"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// (7) The scan ledger counts every template file across the extension
    /// set (`.html` / `.jinja` / `.j2`), excludes non-template files
    /// (`.txt`), and reports how many produced at least one hit.
    #[test]
    fn report_counts_template_files_and_views_with_hits() {
        let root = scratch_dir("report");
        write_template(&root, "projects/detail.html", "{{ project.name }}\n");
        write_template(&root, "projects/list.jinja", "<p>no fields here</p>\n");
        write_template(&root, "layouts/base.j2", "<title>static</title>\n");
        write_template(
            &root,
            "notes/readme.txt",
            "{{ project.name }} not a template\n",
        );

        let (sets, report) = extract_template_field_sets_with_report(&root, &[project_target()]);
        assert_eq!(sets.len(), 1);
        assert_eq!(
            report.template_files, 3,
            "html + jinja + j2 count; txt excluded"
        );
        assert_eq!(report.views_with_hits, 1);

        let _ = fs::remove_dir_all(&root);
    }

    /// (8) Multiple targets against one template: each non-empty projection
    /// yields its own `ViewFieldSet`, sorted by view then resource.
    #[test]
    fn multiple_targets_each_yield_a_view_field_set() {
        let root = scratch_dir("multi_target");
        write_template(
            &root,
            "projects/detail.html",
            "{{ project.name }} — {{ work_package.subject }}\n",
        );

        let sets = extract_template_field_sets(&root, &[project_target(), work_package_target()]);
        assert_eq!(sets.len(), 2);
        assert_eq!(sets[0].resource, "Project");
        assert_eq!(sets[0].fields, vec!["name".to_string()]);
        assert_eq!(sets[1].resource, "WorkPackage");
        assert_eq!(sets[1].fields, vec!["subject".to_string()]);

        let _ = fs::remove_dir_all(&root);
    }

    /// (9) `fields ⊆ referenced` holds across a mixed fixture of known and
    /// unknown idents, in expressions and tags.
    #[test]
    fn fields_is_always_a_subset_of_referenced() {
        let root = scratch_dir("subset_invariant");
        write_template(
            &root,
            "projects/detail.html",
            "{% if project.active %}{{ project.name }}{% endif %}\n\
             {{ project.made_up_helper }} {{ project.status|title }}\n",
        );

        let sets = extract_template_field_sets(&root, &[project_target()]);
        assert_eq!(sets.len(), 1);
        assert!(
            sets[0].fields.len() < sets[0].referenced.len(),
            "fixture must contain at least one unknown ident: fields={:?} referenced={:?}",
            sets[0].fields,
            sets[0].referenced
        );
        for field in &sets[0].fields {
            assert!(
                sets[0].referenced.contains(field),
                "fields must be a subset of referenced: field {field:?} missing from {:?}",
                sets[0].referenced
            );
        }

        let _ = fs::remove_dir_all(&root);
    }
}
