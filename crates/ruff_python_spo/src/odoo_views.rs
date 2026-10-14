//! Odoo `ir.ui.view` field-set extractor — the presentation-tier harvest,
//! **fourth skin**.
//!
//! # One brick, four skins
//!
//! ERB (Rails), askama (Rust), Jinja/Django (Python, [`crate::templates`]) and
//! **Odoo view XML** (this module) are renderers over the **identical**
//! `WideFieldMask` projection: a consumer takes the field SET this harvest
//! produces and mints the mask via
//! `WideFieldMask::from_universe_present(basis, fields)`. This is the Odoo
//! source of that mask — the upstream (frontend-crate) home of the harvest
//! odoo-rs's `view_mask.rs` prototyped consumer-side against the real
//! `account_move_form_view.xml`.
//!
//! # Why record-scoped, not receiver-scoped (the ONE structural divergence)
//!
//! A Jinja/ERB template references fields through a context variable, so the
//! sibling arms need a caller-supplied *receiver* vocabulary. An Odoo view
//! **binds its model in the artifact itself**: the `<record model="ir.ui.view">`
//! block declares `<field name="model">account.move</field>` and then its
//! `arch` names the projected fields as `<field name="partner_id"/>`. So this
//! arm matches on the record's own declared model and [`ViewTarget::receivers`]
//! is deliberately ignored (documented, not silently) — the artifact IS the
//! receiver. Everything else — the [`ViewTarget`] / [`ViewFieldSet`]
//! vocabulary, presence-only doctrine, closed-vocab `fields` + raw
//! `referenced` honest denominator — is byte-aligned with the sibling arms.
//!
//! # The meta-field / arch-field split (the scanner's ONE load-bearing rule)
//!
//! Inside a view record, `<field name="model">` / `<field name="arch">` /
//! `<field name="inherit_id">` etc. are META fields describing the view — NOT
//! model fields the view projects. The projected fields live INSIDE the
//! `arch`. The scanner therefore only starts collecting `<field name="X"`
//! occurrences AFTER the `<field name="arch"` line of the current view
//! record, and stops at `</record>`. An inherit-view's xpath-positioned
//! fields live inside its arch too, so extension views harvest the same way.
//!
//! # Doctrine (identical to the sibling arms)
//!
//! Presence-only (fuzzy-recipe-codebook §8c "detected config becomes data"):
//! the field SET, never layout/widgets/attrs. Inferred tier. A field is
//! recorded into [`ViewFieldSet::referenced`] unconditionally and ADDITIONALLY
//! into [`ViewFieldSet::fields`] when it matches the target's closed
//! vocabulary — `fields ⊆ referenced` by construction.
//!
//! # What is NOT captured (by design, not oversight)
//!
//! - **`QWeb` report templates** (`t-field="o.partner_id.name"` in
//!   `ir.actions.report` XML) — the PDF/report skin is a receiver-style
//!   reference surface (like Jinja) with its own idioms; adding it is a
//!   deliberate extension, not a default.
//! - **Widget/attrs semantics** — `widget="monetary"`, `invisible`,
//!   `readonly` modifiers: presentation, not projection membership.
//! - **Nested (relation-hop) sub-fields** — a field element nested inside
//!   another (`<field name="invoice_line_ids"><field name="quantity"/>
//!   </field>`) names a COMODEL field, not a this-model field; the depth-0
//!   rule excludes it from `referenced` (counting it would poison the mask
//!   whenever names collide across models — `date` exists on both
//!   `account.move` and `account.move.line`). The `(outer, inner)` hop PAIRS
//!   are a consumer refinement (odoo-rs `view_mask.rs::ViewFields::
//!   relation_hops`) this set-level arm does not carry.

use std::fs;
use std::path::{Path, PathBuf};

use crate::templates::{ViewFieldSet, ViewTarget};

/// Conservation-ledger totals for an Odoo view scan (nothing drops silently).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OdooViewScanReport {
    /// Every `*.xml` file found under the scanned root.
    pub xml_files: usize,
    /// Every `ir.ui.view` record seen — the honest denominator, regardless
    /// of whether its model matched a target.
    pub view_records: usize,
    /// View records that produced a non-empty [`ViewFieldSet`].
    pub views_with_hits: usize,
}

/// Scan `<root>` for Odoo view XML and extract, per `ir.ui.view` record and
/// per target model, the set of known model fields the view's arch projects.
/// Thin wrapper over [`extract_odoo_view_field_sets_with_report`].
#[must_use]
pub fn extract_odoo_view_field_sets(root: &Path, targets: &[ViewTarget]) -> Vec<ViewFieldSet> {
    extract_odoo_view_field_sets_with_report(root, targets).0
}

/// Like [`extract_odoo_view_field_sets`] but also returns the
/// [`OdooViewScanReport`] ledger.
///
/// The [`ViewFieldSet::view`] identifier is `<rel_path>#<view_xml_id>` — one
/// XML file carries MANY view records (a real `account` view file bundles
/// form + list + search views), so the record id disambiguates where the
/// sibling arms' one-file-one-view convention would conflate them.
#[must_use]
pub fn extract_odoo_view_field_sets_with_report(
    root: &Path,
    targets: &[ViewTarget],
) -> (Vec<ViewFieldSet>, OdooViewScanReport) {
    let mut report = OdooViewScanReport::default();
    let mut files = Vec::new();
    collect_xml_files(root, &mut files);
    report.xml_files = files.len();

    let mut results: Vec<ViewFieldSet> = Vec::new();
    for path in &files {
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        let rel = relative_path(root, path);
        for record in scan_view_records(&content) {
            report.view_records += 1;
            let Some(model) = &record.model else {
                continue;
            };
            let normalized = model.replace('.', "_");
            for target in targets {
                if target.model != normalized {
                    continue;
                }
                let mut referenced = record.arch_fields.clone();
                referenced.sort_unstable();
                referenced.dedup();
                let fields: Vec<String> = referenced
                    .iter()
                    .filter(|f| target.fields.iter().any(|k| &k == f))
                    .cloned()
                    .collect();
                if referenced.is_empty() {
                    continue;
                }
                report.views_with_hits += 1;
                results.push(ViewFieldSet {
                    resource: target.model.clone(),
                    view: format!("{rel}#{}", record.id.as_deref().unwrap_or("?")),
                    fields,
                    referenced,
                });
            }
        }
    }

    results.sort_by(|a, b| {
        a.view
            .cmp(&b.view)
            .then_with(|| a.resource.cmp(&b.resource))
    });
    (results, report)
}

/// One `ir.ui.view` record's harvest-relevant slice.
struct ViewRecord {
    /// The record's `id="…"` attribute (the view's XML id), if present.
    id: Option<String>,
    /// The `<field name="model">` value (dotted), if present. An inherit-only
    /// extension view without an explicit model yields `None` and is skipped
    /// (counted in the denominator) — resolving it needs the inherited view's
    /// record, a cross-record join deferred until measured to matter.
    model: Option<String>,
    /// Every `<field name="X"` element name inside the record's `arch`.
    arch_fields: Vec<String>,
}

/// Stateful line scan of one XML file for `ir.ui.view` records, applying the
/// meta/arch split AND the relation-hop split: inside the arch, only
/// **depth-0** field elements (direct children of the view's own markup)
/// count as this-model fields. A field nested INSIDE another field element
/// (`<field name="invoice_line_ids"><field name="quantity"/></field>`) names
/// a COMODEL field — counting it would poison the this-model projection
/// whenever names collide across models (`date` exists on both
/// `account.move` and `account.move.line`). This mirrors the hop-splitting
/// semantics odoo-rs's consumer-side `view_mask.rs` proved on the real
/// `account_move_form_view.xml`; the hop PAIRS themselves stay a consumer
/// refinement this set-level arm does not carry.
fn scan_view_records(content: &str) -> Vec<ViewRecord> {
    // FULL-TEXT, position-ordered token scan — deliberately NOT line-based.
    // Real Odoo XML wraps long tags across lines (a search-view field with a
    // filter_domain routinely closes its `/>` two lines later); a line-based
    // scan misreads such a tag as a non-self-closing open, the depth counter
    // leaks, and everything after is swallowed as "nested" (measured on
    // `view_account_invoice_filter`: 13 referenced fields collapsed to 1
    // before this fix).
    let mut records = Vec::new();
    let mut current: Option<ViewRecord> = None;
    let mut in_arch = false;
    let mut depth = 0usize;

    for tok in xml_tokens(content) {
        match tok {
            XmlToken::RecordOpen { id, is_view } => {
                // Any record open closes the previous context (records don't
                // nest).
                if let Some(rec) = current.take() {
                    records.push(rec);
                }
                in_arch = false;
                depth = 0;
                if is_view {
                    current = Some(ViewRecord {
                        id,
                        model: None,
                        arch_fields: Vec::new(),
                    });
                }
            }
            XmlToken::RecordClose => {
                if let Some(rec) = current.take() {
                    records.push(rec);
                }
                in_arch = false;
                depth = 0;
            }
            XmlToken::FieldOpen {
                name,
                self_closing,
                text,
            } => {
                let Some(rec) = current.as_mut() else {
                    continue;
                };
                if in_arch {
                    if depth == 0
                        && let Some(n) = &name
                    {
                        rec.arch_fields.push(n.clone());
                    }
                    if !self_closing {
                        depth += 1;
                    }
                } else if name.as_deref() == Some("arch") {
                    if !self_closing {
                        in_arch = true;
                        depth = 0;
                    }
                } else if name.as_deref() == Some("model") {
                    if let Some(value) = text {
                        rec.model = Some(value);
                    }
                }
            }
            XmlToken::FieldClose => {
                if current.is_some() && in_arch {
                    if depth > 0 {
                        depth -= 1;
                    } else {
                        // The close with depth 0 is the arch wrapper's own
                        // `</field>`.
                        in_arch = false;
                    }
                }
            }
        }
    }
    if let Some(rec) = current.take() {
        records.push(rec);
    }
    records
}

/// One record/field open-or-close token in document order.
enum XmlToken {
    /// A `<record …>` open, with its `id` and whether it declares
    /// `model="ir.ui.view"`.
    RecordOpen { id: Option<String>, is_view: bool },
    /// A `</record>` close.
    RecordClose,
    /// A `<field …>` / `<field …/>` open: its `name` attribute, whether the
    /// tag self-closes, and — for a plain open — the simple text right after
    /// the tag (the `<field name="model">a.b</field>` value shape).
    FieldOpen {
        name: Option<String>,
        self_closing: bool,
        text: Option<String>,
    },
    /// A `</field>` close.
    FieldClose,
}

/// All record/field tokens in `content`, ordered by byte position. Tag
/// slices run to the tag's own `>` wherever it is — across newlines — so a
/// multi-line tag classifies correctly.
fn xml_tokens(content: &str) -> Vec<XmlToken> {
    let mut toks: Vec<(usize, XmlToken)> = Vec::new();
    for (idx, _) in content.match_indices("<record") {
        if !tag_name_boundary(content, idx + "<record".len()) {
            continue;
        }
        let tag = tag_slice(content, idx);
        toks.push((
            idx,
            XmlToken::RecordOpen {
                id: attr_value(tag, "id"),
                is_view: attr_value(tag, "model").as_deref() == Some("ir.ui.view"),
            },
        ));
    }
    for (idx, _) in content.match_indices("</record") {
        toks.push((idx, XmlToken::RecordClose));
    }
    for (idx, _) in content.match_indices("<field") {
        if !tag_name_boundary(content, idx + "<field".len()) {
            continue;
        }
        let tag = tag_slice(content, idx);
        let self_closing = tag.trim_end().ends_with('/');
        let text = if self_closing {
            None
        } else {
            simple_text_after(content, idx + tag.len())
        };
        toks.push((
            idx,
            XmlToken::FieldOpen {
                name: attr_value(tag, "name"),
                self_closing,
                text,
            },
        ));
    }
    for (idx, _) in content.match_indices("</field") {
        toks.push((idx, XmlToken::FieldClose));
    }
    toks.sort_by_key(|(idx, _)| *idx);
    toks.into_iter().map(|(_, tok)| tok).collect()
}

/// The tag slice starting at `idx`, up to (excluding) its own `>` — wherever
/// that is, newlines included. An unterminated tag runs to end-of-content.
pub(crate) fn tag_slice(content: &str, idx: usize) -> &str {
    match content[idx..].find('>') {
        Some(rel) => &content[idx..idx + rel],
        None => &content[idx..],
    }
}

/// Whether the character at `pos` terminates a tag NAME (whitespace, `/`,
/// `>`, or end-of-content) — rejects `<fields…`/`<recording…` false matches.
pub(crate) fn tag_name_boundary(content: &str, pos: usize) -> bool {
    content
        .as_bytes()
        .get(pos)
        .is_none_or(|b| b.is_ascii_whitespace() || *b == b'/' || *b == b'>')
}

/// The simple text between a tag's `>` (at `tag_end`, i.e. the index OF the
/// `>`) and the next `<`, trimmed — the `<field name="model">a.b</field>`
/// value shape. `None` when the tag is unterminated or the text is empty.
fn simple_text_after(content: &str, tag_end: usize) -> Option<String> {
    if content.as_bytes().get(tag_end) != Some(&b'>') {
        return None;
    }
    let after = tag_end + 1;
    let rel = content[after..].find('<')?;
    let text = content[after..after + rel].trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// The value of `key="…"` inside a tag slice, if present.
pub(crate) fn attr_value(tag: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=\"");
    let start = tag.find(&pat)? + pat.len();
    let end = tag[start..].find('"')? + start;
    Some(tag[start..end].to_string())
}

/// Recursively collect every `*.xml` file under `dir` (sorted for
/// determinism — the sibling arms' file-walk discipline).
pub(crate) fn collect_xml_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect_xml_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "xml") {
            out.push(path);
        }
    }
}

/// `path` relative to `root`, `/`-joined (a stable id, not reopened).
pub(crate) fn relative_path(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn move_target() -> ViewTarget {
        ViewTarget {
            model: "account_move".to_string(),
            // Receivers are deliberately IGNORED by this arm (the artifact
            // binds the model itself) — empty here to document that.
            receivers: vec![],
            fields: vec![
                "partner_id".to_string(),
                "date".to_string(),
                "amount_total".to_string(),
            ],
        }
    }

    fn scratch(case: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ruff_python_spo_odoo_views_{}_{case}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    const FORM_VIEW: &str = r#"<odoo>
    <record id="view_move_form" model="ir.ui.view">
        <field name="name">account.move.form</field>
        <field name="model">account.move</field>
        <field name="arch" type="xml">
            <form>
                <field name="partner_id"/>
                <field name="date" optional="show"/>
                <field name="frobnicate_count" invisible="1"/>
            </form>
        </field>
    </record>
</odoo>
"#;

    /// (1) Arch fields land in `referenced`; the closed-vocab subset lands in
    /// `fields`; META fields (`name`/`model`/`arch`) never count.
    #[test]
    fn arch_fields_captured_meta_fields_excluded() {
        let root = scratch("arch");
        write(&root, "views/account_move_views.xml", FORM_VIEW);
        let (sets, report) = extract_odoo_view_field_sets_with_report(&root, &[move_target()]);
        assert_eq!(sets.len(), 1, "{sets:?}");
        assert_eq!(sets[0].resource, "account_move");
        assert_eq!(sets[0].view, "views/account_move_views.xml#view_move_form");
        assert_eq!(
            sets[0].fields,
            vec!["date".to_string(), "partner_id".to_string()]
        );
        assert_eq!(
            sets[0].referenced,
            vec![
                "date".to_string(),
                "frobnicate_count".to_string(),
                "partner_id".to_string()
            ],
            "referenced carries the unknown field too (honest denominator); \
             name/model/arch meta-fields excluded"
        );
        assert_eq!(report.view_records, 1);
        assert_eq!(report.views_with_hits, 1);
        let _ = fs::remove_dir_all(&root);
    }

    /// (2) A view record whose model matches NO target still counts in the
    /// denominator; a non-view record (`act_window`) never counts at all.
    #[test]
    fn unmatched_model_counts_in_denominator_only() {
        let root = scratch("unmatched");
        write(
            &root,
            "views/other.xml",
            r#"<odoo>
    <record id="view_partner_form" model="ir.ui.view">
        <field name="model">res.partner</field>
        <field name="arch" type="xml">
            <form><field name="name"/></form>
        </field>
    </record>
    <record id="action_x" model="ir.actions.act_window">
        <field name="res_model">res.partner</field>
    </record>
</odoo>
"#,
        );
        let (sets, report) = extract_odoo_view_field_sets_with_report(&root, &[move_target()]);
        assert!(sets.is_empty(), "{sets:?}");
        assert_eq!(
            report.view_records, 1,
            "the act_window record is not a view"
        );
        assert_eq!(report.views_with_hits, 0);
        let _ = fs::remove_dir_all(&root);
    }

    /// (3) One file, many view records: each matching record yields its own
    /// `ViewFieldSet`, disambiguated by `#<xml_id>` in the view identifier.
    #[test]
    fn multiple_view_records_per_file_each_yield_a_set() {
        let root = scratch("multi");
        write(
            &root,
            "views/account_move_views.xml",
            r#"<odoo>
    <record id="view_move_form" model="ir.ui.view">
        <field name="model">account.move</field>
        <field name="arch" type="xml">
            <form><field name="partner_id"/></form>
        </field>
    </record>
    <record id="view_move_list" model="ir.ui.view">
        <field name="model">account.move</field>
        <field name="arch" type="xml">
            <list><field name="date"/><field name="amount_total"/></list>
        </field>
    </record>
</odoo>
"#,
        );
        let sets = extract_odoo_view_field_sets(&root, &[move_target()]);
        assert_eq!(sets.len(), 2, "{sets:?}");
        assert_eq!(sets[0].view, "views/account_move_views.xml#view_move_form");
        assert_eq!(sets[0].fields, vec!["partner_id".to_string()]);
        assert_eq!(sets[1].view, "views/account_move_views.xml#view_move_list");
        assert_eq!(
            sets[1].fields,
            vec!["amount_total".to_string(), "date".to_string()]
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// (4) An inherit view with an explicit model harvests its xpath-added
    /// arch fields like any other view.
    #[test]
    fn inherit_view_with_model_harvests_arch_fields() {
        let root = scratch("inherit");
        write(
            &root,
            "views/ext.xml",
            r#"<odoo>
    <record id="view_move_form_ext" model="ir.ui.view">
        <field name="model">account.move</field>
        <field name="inherit_id" ref="account.view_move_form"/>
        <field name="arch" type="xml">
            <xpath expr="//field[@name='partner_id']" position="after">
                <field name="amount_total"/>
            </xpath>
        </field>
    </record>
</odoo>
"#,
        );
        let sets = extract_odoo_view_field_sets(&root, &[move_target()]);
        assert_eq!(sets.len(), 1, "{sets:?}");
        assert_eq!(sets[0].fields, vec!["amount_total".to_string()]);
        // The xpath's @name-in-expr is NOT a field element; only the added
        // <field name="amount_total"/> counts.
        assert_eq!(sets[0].referenced, vec!["amount_total".to_string()]);
        let _ = fs::remove_dir_all(&root);
    }

    /// (5) Relation-hop exclusion: a field element nested inside another
    /// (`invoice_line_ids` → `quantity`/`date`) names a COMODEL field and
    /// must NOT enter `referenced` — even when the inner name collides with
    /// a this-model field (`date` here is on BOTH models; only the top-level
    /// occurrence counts). Depth returns to 0 after the nest closes, so a
    /// later top-level field is still captured. Mirrors odoo-rs
    /// `view_mask.rs`'s proven hop-splitting semantics.
    #[test]
    fn nested_relation_fields_are_excluded_from_referenced() {
        let root = scratch("hops");
        write(
            &root,
            "views/v.xml",
            r#"<odoo>
    <record id="view_move_form" model="ir.ui.view">
        <field name="model">account.move</field>
        <field name="arch" type="xml">
            <form>
                <field name="date"/>
                <field name="invoice_line_ids">
                    <list>
                        <field name="quantity"/>
                        <field name="date"/>
                    </list>
                </field>
                <field name="partner_id"/>
            </form>
        </field>
    </record>
</odoo>
"#,
        );
        let mut target = move_target();
        target.fields.push("invoice_line_ids".to_string());
        let sets = extract_odoo_view_field_sets(&root, &[target]);
        assert_eq!(sets.len(), 1, "{sets:?}");
        assert_eq!(
            sets[0].referenced,
            vec![
                "date".to_string(),
                "invoice_line_ids".to_string(),
                "partner_id".to_string()
            ],
            "comodel fields (quantity, nested date) must not enter referenced; \
             the post-nest top-level partner_id must still be captured"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// (5c) A field tag WRAPPED ACROSS LINES (real search-view shape: a long
    /// `filter_domain` pushes the closing `/>` onto a later line) must still
    /// classify as self-closing — a line-based scan misread it as an open,
    /// leaked the depth counter, and swallowed every following field as
    /// "nested" (measured: `view_account_invoice_filter` collapsed 13→1
    /// referenced before the full-text tokenizer).
    #[test]
    fn multi_line_field_tag_does_not_leak_depth() {
        let root = scratch("multiline");
        write(
            &root,
            "views/v.xml",
            r#"<odoo>
    <record id="view_account_invoice_filter" model="ir.ui.view">
        <field name="model">account.move</field>
        <field name="arch" type="xml">
            <search>
                <field name="partner_id" string="Partner"
                       filter_domain="['|', ('partner_id', 'ilike', self), ('ref', 'ilike', self)]"
                       />
                <field name="date"/>
                <field name="amount_total"/>
            </search>
        </field>
    </record>
</odoo>
"#,
        );
        let sets = extract_odoo_view_field_sets(&root, &[move_target()]);
        assert_eq!(sets.len(), 1, "{sets:?}");
        assert_eq!(
            sets[0].referenced,
            vec![
                "amount_total".to_string(),
                "date".to_string(),
                "partner_id".to_string()
            ],
            "the wrapped tag is self-closing; the fields after it are top-level, not nested"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// (5b) Inline nesting on ONE line still splits hops correctly — the
    /// token walk is position-ordered, not line-scoped.
    #[test]
    fn inline_nested_relation_fields_are_excluded() {
        let root = scratch("hops_inline");
        write(
            &root,
            "views/v.xml",
            r#"<odoo>
    <record id="v" model="ir.ui.view">
        <field name="model">account.move</field>
        <field name="arch" type="xml">
            <form><field name="line_ids"><field name="debit"/></field><field name="date"/></form>
        </field>
    </record>
</odoo>
"#,
        );
        let mut target = move_target();
        target.fields.push("line_ids".to_string());
        let sets = extract_odoo_view_field_sets(&root, &[target]);
        assert_eq!(sets.len(), 1, "{sets:?}");
        assert_eq!(
            sets[0].referenced,
            vec!["date".to_string(), "line_ids".to_string()],
            "inline-nested debit is a comodel field; date after the nest is top-level"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// (5) `fields ⊆ referenced` holds by construction.
    #[test]
    fn fields_is_always_a_subset_of_referenced() {
        let root = scratch("subset");
        write(&root, "views/v.xml", FORM_VIEW);
        let sets = extract_odoo_view_field_sets(&root, &[move_target()]);
        assert_eq!(sets.len(), 1);
        for f in &sets[0].fields {
            assert!(sets[0].referenced.contains(f));
        }
        assert!(sets[0].fields.len() < sets[0].referenced.len());
        let _ = fs::remove_dir_all(&root);
    }
}
