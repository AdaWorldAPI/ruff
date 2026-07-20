//! The ERB **form-widget** harvest — the "how a field renders" shape.
//!
//! # What this is (the missing half of the field harvest)
//!
//! [`crate::views`] harvests `ViewFieldSet` — *which* model fields a view
//! projects. It says nothing about *how* each field renders: is `status_id` a
//! text box, a dropdown, a checkbox? That is what a consumer's codegen needs
//! to emit an `<input>` vs a `<select>` vs a `<checkbox>` — and without it a
//! generated form is hand-built per field type. This arm harvests
//! [`Predicate::RendersAs`] `(screen.field, renders_as, widget:<kind>)` from
//! the Rails form-builder helper:
//!
//! | Helper (`f.<helper>`) | Widget |
//! |---|---|
//! | `text_field` / `string` | text |
//! | `select` / `collection_select` / `grouped_collection_select` / `time_zone_select` | select (dropdown) |
//! | `check_box` / `collection_check_boxes` | checkbox |
//! | `text_area` | textarea |
//! | `date_field` / `date_select` / `datetime_field` / `datetime_select` / `datetime_local_field` | date |
//! | `number_field` | number |
//! | `radio_button` / `collection_radio_buttons` | radio |
//! | `hidden_field` | hidden · `email_field` | email · `password_field` | password · `file_field` | file |
//!
//! The widget kind is a **reusable shape type** — an `OGAR` `ClassView` the field
//! `is_a` (dropdown `is_a` input); the object names it, the field is the
//! subject. Behaviour (render skin, validation) inherits with the widget
//! class, so codegen emits the right element from the harvested pairing
//! instead of a hand-rolled form.
//!
//! # Tier + doctrine (Inferred, closed-vocab line scanner)
//!
//! Closed-vocab ERB line scanner (no Ruby parser), like [`crate::views`] /
//! [`crate::navigation`]. The closed vocab is the form-builder helper set; the
//! field is the first symbol argument. Subject screen = the view's
//! controller-dir segment (the field's *model* is the enclosing form's model,
//! a cross-line join left to the consumer's `ClassView` — see non-captures).
//! Honest denominator: every form-builder helper call is counted in
//! [`WidgetScanReport::raw_widget_refs`], whether or not a field parsed.
//!
//! # What is NOT captured (by design)
//!
//! - **The field's model** — `form_with model: @wp` is a different line from
//!   `f.select :status_id`; joining them is cross-line (the #66 lesson). The
//!   subject is `screen.field`; the consumer binds `screen`→model via its
//!   `ClassView`.
//! - **Bare `*_tag` helpers** (`select_tag`, `text_field_tag`) — not
//!   form-builder-scoped, so the field they target is a free string, not a
//!   model attribute; out of scope for the model-field widget shape.

use std::fs;
use std::path::{Path, PathBuf};

use ruff_spo_triplet::{Predicate, Provenance, Triple};

/// The widget type a form field renders as — a closed, reusable shape-type
/// vocabulary (each is an `OGAR` `ClassView` the field `is_a`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WidgetKind {
    Text,
    Select,
    Checkbox,
    TextArea,
    Date,
    Number,
    Radio,
    Hidden,
    Email,
    Password,
    File,
}

impl WidgetKind {
    /// The lowercase kind name (the SPO object is `widget:<kind>`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Select => "select",
            Self::Checkbox => "checkbox",
            Self::TextArea => "textarea",
            Self::Date => "date",
            Self::Number => "number",
            Self::Radio => "radio",
            Self::Hidden => "hidden",
            Self::Email => "email",
            Self::Password => "password",
            Self::File => "file",
        }
    }
}

/// The Rails form-builder helper → widget-kind map (the closed vocab). Order
/// is longest-first-agnostic (each is matched as a whole `.helper` token).
const WIDGET_HELPERS: &[(&str, WidgetKind)] = &[
    ("text_field", WidgetKind::Text),
    ("string", WidgetKind::Text),
    ("select", WidgetKind::Select),
    ("collection_select", WidgetKind::Select),
    ("grouped_collection_select", WidgetKind::Select),
    ("time_zone_select", WidgetKind::Select),
    ("check_box", WidgetKind::Checkbox),
    ("collection_check_boxes", WidgetKind::Checkbox),
    ("text_area", WidgetKind::TextArea),
    ("date_field", WidgetKind::Date),
    ("date_select", WidgetKind::Date),
    ("datetime_field", WidgetKind::Date),
    ("datetime_select", WidgetKind::Date),
    ("datetime_local_field", WidgetKind::Date),
    ("number_field", WidgetKind::Number),
    ("radio_button", WidgetKind::Radio),
    ("collection_radio_buttons", WidgetKind::Radio),
    ("hidden_field", WidgetKind::Hidden),
    ("email_field", WidgetKind::Email),
    ("password_field", WidgetKind::Password),
    ("file_field", WidgetKind::File),
];

/// One harvested widget: field `field` on `screen` renders as `widget`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RubyWidgetEdge {
    /// The screen the form is on (the view's controller-dir segment).
    pub source: String,
    /// The model field the widget renders (the form-builder symbol arg).
    pub field: String,
    /// The widget type.
    pub widget: WidgetKind,
    /// Source file path relative to the views root (`/`-joined).
    pub file: String,
}

impl RubyWidgetEdge {
    /// Lift into the shared `renders_as` SPO triple: subject
    /// `<ns>:<screen>.<field>`, object `widget:<kind>`. Inferred tier.
    #[must_use]
    pub fn to_triple(&self, namespace: &str) -> Triple {
        Triple::new(
            format!("{namespace}:{}.{}", self.source, self.field),
            Predicate::RendersAs,
            format!("widget:{}", self.widget.as_str()),
            Provenance::Inferred,
        )
    }
}

/// Conservation-ledger totals for a widget scan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WidgetScanReport {
    /// Every `*.erb` file found.
    pub erb_files: usize,
    /// Files that produced at least one [`RubyWidgetEdge`].
    pub files_with_widgets: usize,
    /// Every form-builder helper call seen — the honest denominator,
    /// whether or not a field symbol parsed.
    pub raw_widget_refs: usize,
}

/// Scan `<views_root>` for `*.erb` form-widget helpers. Thin wrapper.
#[must_use]
pub fn extract_widget_edges(views_root: &Path) -> Vec<RubyWidgetEdge> {
    extract_widget_edges_with_report(views_root).0
}

/// Like [`extract_widget_edges`] but also returns the [`WidgetScanReport`].
/// Deduped by `(source, field, widget)`, sorted for determinism.
#[must_use]
pub fn extract_widget_edges_with_report(
    views_root: &Path,
) -> (Vec<RubyWidgetEdge>, WidgetScanReport) {
    let mut report = WidgetScanReport::default();
    let mut files = Vec::new();
    collect_erb_files(views_root, &mut files);
    report.erb_files = files.len();

    let mut edges: Vec<RubyWidgetEdge> = Vec::new();
    for path in &files {
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        let rel = relative_path(views_root, path);
        let source = erb_source_screen(&rel);
        let before = edges.len();
        for line in content.lines() {
            if let Some((widget, field)) = widget_call(line) {
                report.raw_widget_refs += 1;
                if let Some(field) = field {
                    edges.push(RubyWidgetEdge {
                        source: source.clone(),
                        field,
                        widget,
                        file: rel.clone(),
                    });
                }
            }
        }
        if edges.len() > before {
            report.files_with_widgets += 1;
        }
    }

    edges.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then_with(|| a.field.cmp(&b.field))
            .then_with(|| a.widget.cmp(&b.widget))
            .then_with(|| a.file.cmp(&b.file))
    });
    edges.dedup_by(|a, b| a.source == b.source && a.field == b.field && a.widget == b.widget);
    (edges, report)
}

/// The earliest form-builder widget call on `line` — its widget kind plus the
/// field symbol argument (if one parsed). `None` if the line has no
/// form-builder helper.
fn widget_call(line: &str) -> Option<(WidgetKind, Option<String>)> {
    let mut best: Option<(usize, WidgetKind)> = None;
    for (helper, kind) in WIDGET_HELPERS {
        if let Some(pos) = find_method_call(line, helper) {
            if best.is_none_or(|(bp, _)| pos < bp) {
                best = Some((pos, *kind));
            }
        }
    }
    let (pos, kind) = best?;
    // The field is the first `:symbol` after the helper call.
    let field = first_symbol(&line[pos..]);
    Some((kind, field))
}

/// The byte index of a `.<helper>` method call on `line` (helper on a whole
/// word boundary: preceded by `.`, followed by a non-identifier char). `None`
/// if not present.
fn find_method_call(line: &str, helper: &str) -> Option<usize> {
    let chars: Vec<char> = line.chars().collect();
    let h: Vec<char> = helper.chars().collect();
    if h.is_empty() || chars.len() <= h.len() {
        return None;
    }
    for start in 1..=(chars.len() - h.len()) {
        if chars[start - 1] != '.' {
            continue;
        }
        if chars[start..start + h.len()] != h[..] {
            continue;
        }
        let end = start + h.len();
        if end < chars.len() && is_ident_char(chars[end]) {
            continue; // `select` must not match inside `selectable`
        }
        // Byte index of the `.` (so the field search starts at/after the call).
        return Some(line.char_indices().nth(start - 1).map_or(0, |(b, _)| b));
    }
    None
}

/// The first `:symbol` (Ruby symbol literal) in `s` — a `:` followed by an
/// identifier run, not part of a `::` namespace. Returns the identifier.
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

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
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
            "ruff_ruby_spo_widgets_{}_{case}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }

    /// The three core widget helpers map to their kinds, screen + field
    /// parsed from the ERB.
    #[test]
    fn text_select_checkbox_map_to_kinds() {
        let root = scratch_dir("core");
        write_view(
            &root,
            "app/views/work_packages/_form.html.erb",
            "<%= f.text_field :subject %>\n\
             <%= f.select :status_id, options_for_status %>\n\
             <%= f.check_box :active %>\n",
        );
        let edges = extract_widget_edges(&root);
        let got: Vec<(&str, &str, WidgetKind)> = edges
            .iter()
            .map(|e| (e.source.as_str(), e.field.as_str(), e.widget))
            .collect();
        assert!(
            got.contains(&("work_packages", "subject", WidgetKind::Text)),
            "{edges:?}"
        );
        assert!(
            got.contains(&("work_packages", "status_id", WidgetKind::Select)),
            "{edges:?}"
        );
        assert!(
            got.contains(&("work_packages", "active", WidgetKind::Checkbox)),
            "{edges:?}"
        );
    }

    /// `date_field(:start_date)` — paren-call form, and a Select variant.
    #[test]
    fn paren_call_and_select_variants() {
        let root = scratch_dir("variants");
        write_view(
            &root,
            "app/views/work_packages/_form.html.erb",
            "<%= f.date_field(:start_date) %>\n\
             <%= f.collection_select :type_id, @types, :id, :name %>\n",
        );
        let edges = extract_widget_edges(&root);
        assert!(
            edges
                .iter()
                .any(|e| e.field == "start_date" && e.widget == WidgetKind::Date),
            "{edges:?}"
        );
        assert!(
            edges
                .iter()
                .any(|e| e.field == "type_id" && e.widget == WidgetKind::Select),
            "{edges:?}"
        );
    }

    /// Word boundary: a helper name embedded in a longer identifier
    /// (`f.selectable_thing`) must NOT match `select`.
    #[test]
    fn word_boundary_rejects_helper_substring() {
        let root = scratch_dir("boundary");
        write_view(
            &root,
            "app/views/work_packages/_form.html.erb",
            "<%= f.selectable_options :x %>\n",
        );
        let (edges, report) = extract_widget_edges_with_report(&root);
        assert!(edges.is_empty(), "selectable != select: {edges:?}");
        assert_eq!(report.raw_widget_refs, 0);
    }

    /// The edge lifts into `renders_as`: subject `ns:screen.field`, object
    /// `widget:<kind>`, Inferred tier.
    #[test]
    fn edge_lifts_to_renders_as_triple() {
        let edge = RubyWidgetEdge {
            source: "work_packages".to_string(),
            field: "status_id".to_string(),
            widget: WidgetKind::Select,
            file: "app/views/work_packages/_form.html.erb".to_string(),
        };
        let t = edge.to_triple("openproject");
        assert_eq!(t.s, "openproject:work_packages.status_id");
        assert_eq!(t.p, "renders_as");
        assert_eq!(t.p, Predicate::RendersAs.as_str());
        assert_eq!(t.o, "widget:select");
        let (f, c) = Provenance::Inferred.truth();
        assert_eq!((t.f, t.c), (f, c));
    }

    /// Ledger: helpers across a form; a non-form line contributes nothing.
    #[test]
    fn ledger_counts_files_and_helpers() {
        let root = scratch_dir("ledger");
        write_view(
            &root,
            "app/views/work_packages/_form.html.erb",
            "<%= f.text_field :subject %>\n\
             <%= f.text_area :description %>\n\
             <p>not a form field</p>\n",
        );
        write_view(
            &root,
            "app/views/work_packages/show.html.erb",
            "<h1><%= @wp.subject %></h1>\n",
        );
        let (edges, report) = extract_widget_edges_with_report(&root);
        assert_eq!(edges.len(), 2, "text + textarea: {edges:?}");
        assert_eq!(report.erb_files, 2);
        assert_eq!(report.files_with_widgets, 1);
        assert_eq!(report.raw_widget_refs, 2);
    }
}
