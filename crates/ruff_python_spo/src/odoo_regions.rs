//! Odoo view-arch **region grammar** harvester — the render half of the
//! Klickweg structure oracle.
//!
//! Knowledge transfer of the `WinForms` region grammar (ruff PR #76: `DockStyle`
//! → `docked_at`, `TabIndex` → `tab_order`, `ContextMenuStrip` →
//! `opens_popup`) onto the Odoo XML side. The furnace playbook §8.1: *"Odoo
//! XML arch tags (`form`/`tree`/`kanban`) map to regions exactly like
//! `DockStyle`."* This arm makes that concrete — it reuses the SAME closed-vocab
//! predicates ([`Predicate::DockedAt`] / [`Predicate::TabOrder`] /
//! [`Predicate::OpensPopup`], [`Provenance::Authoritative`]), mints nothing.
//!
//! # The docking rule (arch element stack)
//!
//! Every leaf **control** (a `<field>` or a `<button>` in the view arch) docks
//! in its **innermost enclosing region container** — the nearest ancestor whose
//! tag is a region-bearing element ([`REGION_CONTAINERS`]): `<header>` /
//! `<sheet>` / `<searchpanel>` / `<footer>` / `<notebook>` / `<group>` /
//! `<form>` / `<list>` / `<kanban>` / `<search>` / the `oe_chatter` div. The
//! emitted dock TOKEN is that container's tag; a consumer `region=` config
//! (odoo-rs `data/nav/odoo_regions.conf`) maps the token → one of the six
//! canonical regions. Region NAMES never appear here — layout is not a domain
//! concept (two-axis mint refusal), so no token earns a codebook classid.
//!
//! The scan is **full-text, position-ordered** (like [`super::odoo_views`]) —
//! real Odoo XML wraps long tags across lines, so a line-based scan leaks. A
//! field nested inside another `<field>` names a **comodel** field (depth-0
//! rule, mirror of the field-set arm) and is NOT docked — counting it would
//! attribute a comodel control to this view's region frame.
//!
//! `opens_popup` is emitted for a `<button type="action">` (the action/dropdown
//! button — an Odoo popup interaction). Full wizard discrimination (an
//! `act_window target="new"` modal vs an in-place navigation) needs the
//! `act_window` join that [`super::odoo_nav`] already parses; that refinement
//! is the named follow-up, not fabricated here.
//!
//! A control with NO region-bearing ancestor docks at the honest fallback
//! token `"root"` — this is the shape of an **extension view** (`<xpath …
//! position="…">` patch) whose true region lives in the parent view resolved
//! via `inherit_id`. That cross-record join (the mirror of `odoo_nav`'s
//! cross-file Shape B) is the named follow-up; until then the consumer
//! `region=` config maps `root` to the main content region (`root:center`).

use std::fs;
use std::path::Path;

pub use ruff_spo_triplet::{RegionFact, region_triples};

use crate::odoo_views::{
    attr_value, collect_xml_files, relative_path, tag_name_boundary, tag_slice,
};

/// Region-bearing container tags. A control's dock token is the innermost of
/// these on the arch element stack. Every token here is a key in the consumer
/// `region=` convention config (odoo-rs `data/nav/odoo_regions.conf`).
pub const REGION_CONTAINERS: &[&str] = &[
    "header",
    "search",
    "searchpanel",
    "sheet",
    "form",
    "list",
    "tree",
    "kanban",
    "group",
    "notebook",
    "page",
    "footer",
    "chatter",
    // Content view roots (center by convention) — a real deployment renders
    // these as the main data view, same as `list`/`kanban`.
    "pivot",
    "graph",
    "calendar",
    "activity",
    "gantt",
];

// The region fact type and its lift are the shared, frontend-agnostic
// `ruff_spo_triplet::{RegionFact, region_triples}` (re-exported above). This
// arm builds `RegionFact`s in [`push_fact`] and the shared lift emits the
// canonical `{screen}.{control}` subject — so Odoo, Rails, and the `WinForms`
// arm all round-trip through the one `ruff_spo_triplet::build_nav_digest`
// consumer. (Before the collapse this arm emitted a `{screen}::{control}`
// subject that the dot-parsing digest could not read; the shared codec fixes
// that. Odoo screens carry a `.xml` dot, but the digest decodes on the LAST
// dot — the dotless control is always the final segment.)

/// Scan `<root>` for Odoo view XML and extract, per `ir.ui.view` record, the
/// region placement of every depth-0 control in its arch.
#[must_use]
pub fn extract_odoo_view_regions(root: &Path) -> Vec<RegionFact> {
    let mut files = Vec::new();
    collect_xml_files(root, &mut files);
    let mut facts = Vec::new();
    for path in &files {
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        let rel = relative_path(root, path);
        scan_file_regions(&content, &rel, &mut facts);
    }
    facts.sort_by(|a, b| {
        a.screen
            .cmp(&b.screen)
            .then_with(|| a.tab_order.cmp(&b.tab_order))
    });
    facts
}

/// Extract region facts straight from an XML string (fixture/testing entry;
/// `rel` is the synthetic screen-file label).
#[must_use]
pub fn extract_regions_from_source(content: &str, rel: &str) -> Vec<RegionFact> {
    let mut facts = Vec::new();
    scan_file_regions(content, rel, &mut facts);
    facts
}

/// A classified arch tag in document order.
enum ArchTag {
    /// `<record … model="ir.ui.view" id="X">` opens a view record.
    ViewRecordOpen { id: Option<String> },
    /// `</record>` closes any record.
    RecordClose,
    /// The arch wrapper `<field name="arch" …>` opens the arch body.
    ArchOpen { self_closing: bool },
    /// A `<field …>` control inside the arch. `name` is its field name.
    FieldOpen {
        name: Option<String>,
        self_closing: bool,
    },
    /// A `<button …>` control inside the arch.
    ButtonOpen {
        control: String,
        opens_popup: Option<String>,
        self_closing: bool,
    },
    /// A region-bearing container open (its normalized dock token).
    ContainerOpen { token: String, self_closing: bool },
    /// Any element close `</tag>` — pops one arch stack entry.
    ElementClose,
    /// Any other non-self-closing element open (kept on the stack so open/close
    /// balance holds; not a region container).
    OtherOpen { token: String },
}

/// Position-ordered region scan of one XML file. Maintains, per view record, a
/// full arch element stack; a control docks at the innermost container on it.
fn scan_file_regions(content: &str, rel: &str, out: &mut Vec<RegionFact>) {
    let mut screen: Option<String> = None;
    let mut in_arch = false;
    // The arch element stack (dock tokens for containers, `field`/other for the
    // rest) — used both for innermost-container lookup and the depth-0 rule.
    let mut stack: Vec<String> = Vec::new();
    let mut order = 0usize;

    for tag in arch_tags(content) {
        match tag {
            ArchTag::ViewRecordOpen { id } => {
                screen = id.map(|i| format!("{rel}#{i}"));
                in_arch = false;
                stack.clear();
                order = 0;
            }
            ArchTag::RecordClose => {
                screen = None;
                in_arch = false;
                stack.clear();
            }
            ArchTag::ArchOpen { self_closing } => {
                if screen.is_some() && !self_closing {
                    in_arch = true;
                    stack.clear();
                }
            }
            ArchTag::FieldOpen { name, self_closing } => {
                if !in_arch {
                    continue;
                }
                // Depth-0 rule: a field with a `field` ancestor is a comodel
                // field — do not dock it.
                let is_comodel = stack.iter().any(|t| t == "field");
                if !is_comodel && let Some(n) = &name {
                    push_fact(out, screen.as_deref(), n, &stack, order, None);
                    order += 1;
                }
                if !self_closing {
                    stack.push("field".to_string());
                }
            }
            ArchTag::ButtonOpen {
                control,
                opens_popup,
                self_closing,
            } => {
                if !in_arch {
                    continue;
                }
                let is_comodel = stack.iter().any(|t| t == "field");
                if !is_comodel {
                    push_fact(out, screen.as_deref(), &control, &stack, order, opens_popup);
                    order += 1;
                }
                if !self_closing {
                    stack.push("button".to_string());
                }
            }
            ArchTag::ContainerOpen {
                token,
                self_closing,
            } => {
                if in_arch && !self_closing {
                    stack.push(token);
                }
            }
            ArchTag::OtherOpen { token } => {
                if in_arch {
                    stack.push(token);
                }
            }
            ArchTag::ElementClose => {
                if !in_arch {
                    continue;
                }
                if stack.is_empty() {
                    // The arch wrapper's own `</field>` — leave the arch body.
                    in_arch = false;
                } else {
                    stack.pop();
                }
            }
        }
    }
}

/// Emit one [`RegionFact`], docking `control` at the innermost region container
/// on `stack` (or `"root"` if none — an arch with no recognized container).
fn push_fact(
    out: &mut Vec<RegionFact>,
    screen: Option<&str>,
    control: &str,
    stack: &[String],
    order: usize,
    opens_popup: Option<String>,
) {
    let Some(screen) = screen else {
        return;
    };
    let dock_token = stack
        .iter()
        .rev()
        .find(|t| REGION_CONTAINERS.contains(&t.as_str()))
        .cloned()
        .unwrap_or_else(|| "root".to_string());
    out.push(RegionFact {
        screen: screen.to_string(),
        control: control.to_string(),
        dock_token,
        // DOM document position is the faithful ordinal; the shared field is
        // `Option<u32>` (a harvester that cannot assign an ordinal leaves it
        // `None`). Odoo always has one — the non-panicking cast mirrors
        // `menu_regions`'s `try_from(...).unwrap_or(u32::MAX)`.
        tab_order: Some(u32::try_from(order).unwrap_or(u32::MAX)),
        // Odoo nesting is implicit in the element stack; a `contains_control`
        // edge is not emitted (yet). The parent tree is the named follow-up.
        opens_popup,
        parent: None,
    });
}

/// All arch-relevant tags in `content`, in byte-position order. Every `<…>` is
/// classified; unknown element opens become [`ArchTag::OtherOpen`] so the
/// stack stays balanced.
fn arch_tags(content: &str) -> Vec<ArchTag> {
    let mut toks: Vec<(usize, ArchTag)> = Vec::new();

    // Element closes `</tag>`.
    for (idx, _) in content.match_indices("</") {
        let after = &content[idx + 2..];
        let name: String = after
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        if name.is_empty() {
            continue;
        }
        if name == "record" {
            toks.push((idx, ArchTag::RecordClose));
        } else {
            toks.push((idx, ArchTag::ElementClose));
        }
    }

    // Element opens `<tag …>` (skip closes, comments, declarations).
    for (idx, _) in content.match_indices('<') {
        let rest = &content[idx + 1..];
        let first = rest.chars().next().unwrap_or(' ');
        if first == '/' || first == '!' || first == '?' {
            continue;
        }
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        if name.is_empty() || !tag_name_boundary(content, idx + 1 + name.len()) {
            continue;
        }
        let tag = tag_slice(content, idx);
        let self_closing = tag.trim_end().ends_with('/');

        match name.as_str() {
            "record" => {
                if attr_value(tag, "model").as_deref() == Some("ir.ui.view") {
                    toks.push((
                        idx,
                        ArchTag::ViewRecordOpen {
                            id: attr_value(tag, "id"),
                        },
                    ));
                } else {
                    toks.push((idx, ArchTag::OtherOpen { token: name }));
                }
            }
            "field" if attr_value(tag, "name").as_deref() == Some("arch") => {
                toks.push((idx, ArchTag::ArchOpen { self_closing }));
            }
            "field" => {
                toks.push((
                    idx,
                    ArchTag::FieldOpen {
                        name: attr_value(tag, "name"),
                        self_closing,
                    },
                ));
            }
            "button" => {
                let control = attr_value(tag, "name")
                    .or_else(|| attr_value(tag, "string"))
                    .unwrap_or_else(|| "button".to_string());
                let opens_popup = if attr_value(tag, "type").as_deref() == Some("action") {
                    Some(attr_value(tag, "name").unwrap_or_else(|| "action".to_string()))
                } else {
                    None
                };
                toks.push((
                    idx,
                    ArchTag::ButtonOpen {
                        control,
                        opens_popup,
                        self_closing,
                    },
                ));
            }
            "div" => {
                // Only the `oe_chatter` div is a region container ("chatter");
                // any other div is a plain layout element (kept balanced).
                let is_chatter = attr_value(tag, "class")
                    .map(|c| c.split_whitespace().any(|w| w == "oe_chatter"))
                    .unwrap_or(false);
                if is_chatter {
                    toks.push((
                        idx,
                        ArchTag::ContainerOpen {
                            token: "chatter".to_string(),
                            self_closing,
                        },
                    ));
                } else {
                    toks.push((idx, ArchTag::OtherOpen { token: name }));
                }
            }
            n if REGION_CONTAINERS.contains(&n) => {
                toks.push((
                    idx,
                    ArchTag::ContainerOpen {
                        token: name.clone(),
                        self_closing,
                    },
                ));
            }
            _ => {
                toks.push((idx, ArchTag::OtherOpen { token: name }));
            }
        }
    }

    toks.sort_by_key(|(idx, _)| *idx);
    toks.into_iter().map(|(_, t)| t).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruff_spo_triplet::{Predicate, Provenance};

    /// A neutral synthetic view exercising: a `<header>` button (`top_bar`), a
    /// `<searchpanel>` field (`left_nav`), a `<sheet>` field + a `<notebook>`
    /// field (center), a `oe_chatter` div field (`right_panel`), a comodel field
    /// nested inside a `line_ids` field (must NOT dock), and an
    /// `type="action"` button (`opens_popup`). No corpus tokens — all invented.
    const FIXTURE: &str = r#"
<odoo>
  <record id="view_widget_form" model="ir.ui.view">
    <field name="model">a.widget</field>
    <field name="arch" type="xml">
      <form>
        <header>
          <button name="action_confirm" type="object" string="Confirm"/>
          <button name="%(act_open_wizard)d" type="action" string="Wizard"/>
        </header>
        <searchpanel>
          <field name="category_id"/>
        </searchpanel>
        <sheet>
          <group>
            <field name="partner_id"/>
          </group>
          <notebook>
            <page string="Lines">
              <field name="line_ids">
                <list>
                  <field name="quantity"/>
                </list>
              </field>
            </page>
          </notebook>
          <div class="oe_chatter">
            <field name="message_ids" widget="mail_thread"/>
          </div>
        </sheet>
      </form>
    </field>
  </record>
</odoo>
"#;

    fn fact<'a>(facts: &'a [RegionFact], control: &str) -> &'a RegionFact {
        facts
            .iter()
            .find(|f| f.control == control)
            .unwrap_or_else(|| panic!("no region fact for control {control:?}"))
    }

    #[test]
    fn controls_dock_at_their_innermost_container() {
        let facts = extract_regions_from_source(FIXTURE, "widget_views.xml");
        assert_eq!(fact(&facts, "action_confirm").dock_token, "header");
        assert_eq!(fact(&facts, "category_id").dock_token, "searchpanel");
        assert_eq!(fact(&facts, "partner_id").dock_token, "group");
        assert_eq!(fact(&facts, "message_ids").dock_token, "chatter");
    }

    #[test]
    fn comodel_field_nested_in_a_field_is_not_docked() {
        let facts = extract_regions_from_source(FIXTURE, "widget_views.xml");
        // `line_ids` docks (depth-0, inside notebook/page); its nested
        // `quantity` is a comodel field and must NOT produce a fact.
        assert!(facts.iter().any(|f| f.control == "line_ids"));
        assert!(
            !facts.iter().any(|f| f.control == "quantity"),
            "comodel field `quantity` (nested inside `line_ids`) must not dock"
        );
    }

    #[test]
    fn action_button_opens_popup() {
        let facts = extract_regions_from_source(FIXTURE, "widget_views.xml");
        let wizard = fact(&facts, "%(act_open_wizard)d");
        assert_eq!(wizard.opens_popup.as_deref(), Some("%(act_open_wizard)d"));
        // A non-action button does not open a popup.
        assert_eq!(fact(&facts, "action_confirm").opens_popup, None);
    }

    #[test]
    fn tab_order_follows_document_order() {
        let facts = extract_regions_from_source(FIXTURE, "widget_views.xml");
        // The header confirm button is first in document order.
        assert_eq!(fact(&facts, "action_confirm").tab_order, Some(0));
        // category_id (searchpanel) comes after both header buttons.
        assert!(fact(&facts, "category_id").tab_order > fact(&facts, "action_confirm").tab_order);
    }

    #[test]
    fn facts_lift_to_authoritative_triples() {
        let facts = extract_regions_from_source(FIXTURE, "widget_views.xml");
        let triples = region_triples(&facts);
        let docked = triples
            .iter()
            .find(|t| t.p == Predicate::DockedAt.as_str() && t.o == "header")
            .expect("a docked_at:header triple");
        let (f, c) = Provenance::Authoritative.truth();
        assert_eq!((docked.f, docked.c), (f, c));
        // opens_popup triple is emitted for the action button.
        assert!(
            triples
                .iter()
                .any(|t| t.p == Predicate::OpensPopup.as_str())
        );
    }

    /// Fence for the known codec edge (collapse-spec §2): the shared subject
    /// codec ([`RegionSubject::from_iri`](ruff_spo_triplet::RegionSubject))
    /// decodes on the LAST `.`, so a control that itself contains a `.` (a rare
    /// Odoo related-field path like `currency_id.symbol`) would split wrongly. On
    /// this corpus controls are Python identifiers / `%(...)d` action refs —
    /// dotless. If a real dotted control ever appears this trips, and the
    /// canonical separator must escalate to `::` (with the C# harvester
    /// migration filed as the paired follow-up).
    #[test]
    fn harvested_controls_carry_no_dot() {
        let facts = extract_regions_from_source(FIXTURE, "widget_views.xml");
        for f in &facts {
            assert!(
                !f.control.contains('.'),
                "dotted control {:?} would rsplit wrongly through the shared subject codec",
                f.control
            );
        }
    }

    #[test]
    fn every_emitted_dock_token_is_a_known_container_or_root() {
        let facts = extract_regions_from_source(FIXTURE, "widget_views.xml");
        for f in &facts {
            assert!(
                REGION_CONTAINERS.contains(&f.dock_token.as_str()) || f.dock_token == "root",
                "control {:?} got an out-of-vocab dock token {:?}",
                f.control,
                f.dock_token
            );
        }
    }
}
