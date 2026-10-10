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
//! # Extension views: the `inherit_id` join
//!
//! An **extension view** (`<field name="inherit_id" ref="…"/>`) does not carry
//! a full arch. It patches its parent with **locators**: an `<xpath
//! expr="…" position="…">` element, or the shorthand form, any element that
//! carries a `position` attribute (`<field name="amount" position="after">`).
//! A locator is never a control itself. The controls it brings dock in the
//! parent's region at the locator's TARGET:
//!
//! - `after` / `before` / `replace` → the region the target sits in;
//! - `inside` a region container → that container's token;
//! - `inside` a `<field>` → the field's comodel subview, so not docked (the
//!   depth-0 rule);
//! - `attributes` → no controls.
//!
//! A container opened INSIDE the locator block still wins for the controls it
//! wraps (a new `<group>` docks its fields at `group`). Targets resolve against
//! the parent's anchors, then its own parent's, up the `inherit_id` chain, so
//! an extension of an extension resolves through what the first one added.
//! References join on the LOCAL part of an xml id (`analytic.view_x` →
//! `view_x`), the convention [`super::odoo_nav`] uses for cross-file refs.
//!
//! Only `name`-addressed or bare-tag targets resolve (`//field[@name='x']`,
//! `//sheet`); an xpath whose last step filters on anything else
//! (`[hasclass(…)]`, `[@string=…]`, positional) is left unresolved rather than
//! matched to the first element of that tag. An unresolved locator (unknown
//! target, or a parent outside the scanned root) docks its controls at the
//! honest fallback token `"root"`, which the consumer `region=` config maps to
//! the main content region (`root:center`).
//!
//! A control with NO region-bearing ancestor in a base view docks at `"root"`
//! for the same reason.

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
/// region placement of every depth-0 control in its arch. Extension views are
/// resolved against their `inherit_id` parent across every scanned file.
#[must_use]
pub fn extract_odoo_view_regions(root: &Path) -> Vec<RegionFact> {
    let mut files = Vec::new();
    collect_xml_files(root, &mut files);
    let mut views = Vec::new();
    for path in &files {
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        let rel = relative_path(root, path);
        collect_views(&content, &rel, &mut views);
    }
    let mut facts = regions_of_views(&views);
    facts.sort_by(|a, b| {
        a.screen
            .cmp(&b.screen)
            .then_with(|| a.tab_order.cmp(&b.tab_order))
    });
    facts
}

/// Extract region facts straight from an XML string (fixture/testing entry;
/// `rel` is the synthetic screen-file label). Extension views resolve against
/// parents in the same string only.
#[must_use]
pub fn extract_regions_from_source(content: &str, rel: &str) -> Vec<RegionFact> {
    let mut views = Vec::new();
    collect_views(content, rel, &mut views);
    regions_of_views(&views)
}

/// A classified tag in document order.
enum ArchTag {
    /// `<record … model="ir.ui.view" id="X">` opens a view record.
    ViewRecordOpen { id: Option<String> },
    /// `</record>` closes any record.
    RecordClose,
    /// `<field name="inherit_id" ref="X"/>` — the view's parent.
    InheritRef(String),
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
    /// A region-bearing container open: its raw tag, its `name` attribute and
    /// its normalized dock token.
    ContainerOpen {
        tag: String,
        name: Option<String>,
        token: String,
        self_closing: bool,
    },
    /// An extension-view locator: an `<xpath>` or any element carrying a
    /// `position` attribute. `target` is `(tag, name)` when it is resolvable.
    LocatorOpen {
        target: Option<(String, Option<String>)>,
        position: String,
        self_closing: bool,
    },
    /// Any element close `</tag>`.
    ElementClose,
    /// Any other element open (kept so open/close balance holds; not a region
    /// container, but addressable as a locator target by tag and name).
    OtherOpen {
        tag: String,
        name: Option<String>,
        self_closing: bool,
    },
}

impl ArchTag {
    /// Whether this open pushes a frame that a later `</tag>` pops.
    fn opens_frame(&self) -> bool {
        match self {
            Self::FieldOpen { self_closing, .. }
            | Self::ButtonOpen { self_closing, .. }
            | Self::ContainerOpen { self_closing, .. }
            | Self::LocatorOpen { self_closing, .. }
            | Self::OtherOpen { self_closing, .. } => !self_closing,
            _ => false,
        }
    }
}

/// One `ir.ui.view` record: its screen label, the local part of its xml id,
/// its parent's local id (extension views), and its arch-body tags.
struct ViewRec {
    screen: String,
    local_id: String,
    inherit: Option<String>,
    arch: Vec<ArchTag>,
}

/// The local part of an xml id (`module.name` → `name`).
fn local_id(id: &str) -> String {
    id.rsplit_once('.').map_or(id, |(_, l)| l).to_string()
}

/// Split one file into view records, each with its arch-body tags.
fn collect_views(content: &str, rel: &str, out: &mut Vec<ViewRec>) {
    let mut cur: Option<ViewRec> = None;
    let mut in_arch = false;
    let mut depth = 0usize;
    for tag in arch_tags(content) {
        match tag {
            ArchTag::ViewRecordOpen { id } => {
                out.extend(cur.take());
                cur = id.map(|i| ViewRec {
                    screen: format!("{rel}#{i}"),
                    local_id: local_id(&i),
                    inherit: None,
                    arch: Vec::new(),
                });
                in_arch = false;
            }
            ArchTag::RecordClose => {
                out.extend(cur.take());
                in_arch = false;
            }
            ArchTag::InheritRef(r) => {
                if !in_arch && let Some(v) = cur.as_mut() {
                    v.inherit = Some(local_id(&r));
                }
            }
            ArchTag::ArchOpen { self_closing } => {
                if cur.is_some() && !self_closing {
                    in_arch = true;
                    depth = 0;
                }
            }
            ArchTag::ElementClose => {
                if !in_arch {
                    continue;
                }
                if depth == 0 {
                    // The arch wrapper's own `</field>` — leave the arch body.
                    in_arch = false;
                } else {
                    depth -= 1;
                    if let Some(v) = cur.as_mut() {
                        v.arch.push(ArchTag::ElementClose);
                    }
                }
            }
            other => {
                if !in_arch {
                    continue;
                }
                if other.opens_frame() {
                    depth += 1;
                }
                if let Some(v) = cur.as_mut() {
                    v.arch.push(other);
                }
            }
        }
    }
    out.extend(cur);
}

/// An addressable element of a resolved view: what an extension's locator can
/// target. `self_token` is set for region containers; `enclosing` is the region
/// the element itself sits in.
#[derive(Clone)]
struct Anchor {
    tag: String,
    name: Option<String>,
    self_token: Option<String>,
    enclosing: String,
}

/// One frame of the arch element stack.
enum Frame {
    Container(String),
    Field,
    Other,
    /// A locator block: where its controls dock (`None` = unresolved → root),
    /// and whether it sits inside a field target (comodel subview).
    Locator {
        dock: Option<String>,
        comodel: bool,
    },
}

/// The region a control at this stack position docks in.
fn dock_of(stack: &[Frame]) -> String {
    for f in stack.iter().rev() {
        match f {
            Frame::Container(t) => return t.clone(),
            Frame::Locator { dock, .. } => {
                return dock.clone().unwrap_or_else(|| "root".to_string());
            }
            Frame::Field | Frame::Other => {}
        }
    }
    "root".to_string()
}

/// Whether a control at this stack position is a comodel subview field: it has
/// a `<field>` ancestor before the nearest locator, or the locator itself
/// targets the inside of a field.
fn is_comodel(stack: &[Frame]) -> bool {
    for f in stack.iter().rev() {
        match f {
            Frame::Field => return true,
            Frame::Locator { comodel, .. } => return *comodel,
            Frame::Container(_) | Frame::Other => {}
        }
    }
    false
}

/// Resolve a locator's target and position against the parent's anchors.
fn resolve_locator(
    target: Option<&(String, Option<String>)>,
    position: &str,
    parent: &[Anchor],
) -> Frame {
    let unresolved = Frame::Locator {
        dock: None,
        comodel: false,
    };
    let Some((tag, name)) = target else {
        return unresolved;
    };
    let Some(anchor) = parent
        .iter()
        .find(|a| &a.tag == tag && (name.is_none() || &a.name == name))
    else {
        return unresolved;
    };
    match position {
        "attributes" => Frame::Locator {
            dock: None,
            comodel: true,
        },
        "inside" => match &anchor.self_token {
            Some(t) => Frame::Locator {
                dock: Some(t.clone()),
                comodel: false,
            },
            None if anchor.tag == "field" => Frame::Locator {
                dock: None,
                comodel: true,
            },
            None => Frame::Locator {
                dock: Some(anchor.enclosing.clone()),
                comodel: false,
            },
        },
        // after / before / replace (and any future sibling position): the
        // region the target itself sits in.
        _ => Frame::Locator {
            dock: Some(anchor.enclosing.clone()),
            comodel: false,
        },
    }
}

/// Walk one view's arch: its region facts, and the anchors it contributes for
/// views that extend it. `parent` are the resolved anchors of its parent chain.
fn walk(view: &ViewRec, parent: &[Anchor]) -> (Vec<RegionFact>, Vec<Anchor>) {
    let mut facts = Vec::new();
    let mut anchors = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut order = 0usize;
    for tag in &view.arch {
        match tag {
            ArchTag::FieldOpen { name, self_closing } => {
                if !is_comodel(&stack)
                    && let Some(n) = name
                {
                    let dock = dock_of(&stack);
                    push_fact(&mut facts, &view.screen, n, &dock, order, None);
                    order += 1;
                    anchors.push(Anchor {
                        tag: "field".to_string(),
                        name: Some(n.clone()),
                        self_token: None,
                        enclosing: dock,
                    });
                }
                if !self_closing {
                    stack.push(Frame::Field);
                }
            }
            ArchTag::ButtonOpen {
                control,
                opens_popup,
                self_closing,
            } => {
                if !is_comodel(&stack) {
                    let dock = dock_of(&stack);
                    push_fact(
                        &mut facts,
                        &view.screen,
                        control,
                        &dock,
                        order,
                        opens_popup.clone(),
                    );
                    order += 1;
                    anchors.push(Anchor {
                        tag: "button".to_string(),
                        name: Some(control.clone()),
                        self_token: None,
                        enclosing: dock,
                    });
                }
                if !self_closing {
                    stack.push(Frame::Other);
                }
            }
            ArchTag::ContainerOpen {
                tag,
                name,
                token,
                self_closing,
            } => {
                if !is_comodel(&stack) {
                    anchors.push(Anchor {
                        tag: tag.clone(),
                        name: name.clone(),
                        self_token: Some(token.clone()),
                        enclosing: dock_of(&stack),
                    });
                }
                if !self_closing {
                    stack.push(Frame::Container(token.clone()));
                }
            }
            ArchTag::OtherOpen {
                tag,
                name,
                self_closing,
            } => {
                if !is_comodel(&stack) {
                    anchors.push(Anchor {
                        tag: tag.clone(),
                        name: name.clone(),
                        self_token: None,
                        enclosing: dock_of(&stack),
                    });
                }
                if !self_closing {
                    stack.push(Frame::Other);
                }
            }
            ArchTag::LocatorOpen {
                target,
                position,
                self_closing,
            } => {
                if !self_closing {
                    stack.push(resolve_locator(target.as_ref(), position, parent));
                }
            }
            ArchTag::ElementClose => {
                stack.pop();
            }
            ArchTag::ViewRecordOpen { .. }
            | ArchTag::RecordClose
            | ArchTag::InheritRef(_)
            | ArchTag::ArchOpen { .. } => {}
        }
    }
    (facts, anchors)
}

/// Resolve every view (parents first, through `inherit_id`) and return the
/// region facts in view order.
fn regions_of_views(views: &[ViewRec]) -> Vec<RegionFact> {
    let mut index: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (i, v) in views.iter().enumerate() {
        index.entry(v.local_id.as_str()).or_insert(i);
    }
    let mut resolved: Vec<Option<(Vec<RegionFact>, Vec<Anchor>)>> = vec![None; views.len()];
    let mut visiting = vec![false; views.len()];
    for i in 0..views.len() {
        resolve_view(i, views, &index, &mut resolved, &mut visiting);
    }
    resolved
        .into_iter()
        .flat_map(|r| r.map(|(facts, _)| facts).unwrap_or_default())
        .collect()
}

/// Resolve view `i` after its parent; its anchors are its own followed by the
/// parent chain's. A cycle in `inherit_id` resolves the repeated view with no
/// parent anchors instead of looping.
fn resolve_view(
    i: usize,
    views: &[ViewRec],
    index: &std::collections::HashMap<&str, usize>,
    resolved: &mut Vec<Option<(Vec<RegionFact>, Vec<Anchor>)>>,
    visiting: &mut Vec<bool>,
) {
    if resolved[i].is_some() || visiting[i] {
        return;
    }
    visiting[i] = true;
    let parent = views[i]
        .inherit
        .as_deref()
        .and_then(|p| index.get(p).copied())
        .filter(|&p| p != i);
    let parent_anchors = match parent {
        Some(p) => {
            resolve_view(p, views, index, resolved, visiting);
            resolved[p]
                .as_ref()
                .map(|(_, a)| a.clone())
                .unwrap_or_default()
        }
        None => Vec::new(),
    };
    let (facts, mut anchors) = walk(&views[i], &parent_anchors);
    anchors.extend(parent_anchors);
    resolved[i] = Some((facts, anchors));
    visiting[i] = false;
}

/// Emit one [`RegionFact`] docked at `dock_token`.
fn push_fact(
    out: &mut Vec<RegionFact>,
    screen: &str,
    control: &str,
    dock_token: &str,
    order: usize,
    opens_popup: Option<String>,
) {
    out.push(RegionFact {
        screen: screen.to_string(),
        control: control.to_string(),
        dock_token: dock_token.to_string(),
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

/// The `(tag, name)` an xpath's LAST step addresses, if it is resolvable: a
/// bare tag, or a tag filtered by `@name` equality. Anything else
/// (`[hasclass(…)]`, `[@string=…]`, positional, `*`, functions) is `None`
/// rather than a guess.
fn xpath_target(expr: &str) -> Option<(String, Option<String>)> {
    let first = expr.split('|').next()?.trim();
    let mut depth = 0i32;
    let mut cut = 0usize;
    for (i, c) in first.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            '/' if depth == 0 => cut = i + 1,
            _ => {}
        }
    }
    let step = &first[cut..];
    let (tag, pred) = match step.find('[') {
        Some(b) => (&step[..b], Some(&step[b..])),
        None => (step, None),
    };
    let tag = tag.trim();
    if tag.is_empty()
        || !tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    let name = match pred {
        None => None,
        Some(p) => Some(name_in_predicate(p)?),
    };
    Some((tag.to_string(), name))
}

/// The value of an `@name='…'` / `@name="…"` equality inside an xpath
/// predicate.
fn name_in_predicate(pred: &str) -> Option<String> {
    let rest = &pred[pred.find("@name")? + "@name".len()..];
    let rest = rest.trim_start().strip_prefix('=')?.trim_start();
    let quote = rest.chars().next().filter(|q| *q == '\'' || *q == '"')?;
    let body = &rest[1..];
    Some(body[..body.find(quote)?].to_string())
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

        if name != "record"
            && let Some(position) = attr_value(tag, "position")
        {
            let target = if name == "xpath" {
                attr_value(tag, "expr").and_then(|e| xpath_target(&e))
            } else {
                Some((name.clone(), attr_value(tag, "name")))
            };
            toks.push((
                idx,
                ArchTag::LocatorOpen {
                    target,
                    position,
                    self_closing,
                },
            ));
            continue;
        }

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
                    toks.push((
                        idx,
                        ArchTag::OtherOpen {
                            tag: name,
                            name: None,
                            self_closing,
                        },
                    ));
                }
            }
            "field" if attr_value(tag, "name").as_deref() == Some("arch") => {
                toks.push((idx, ArchTag::ArchOpen { self_closing }));
            }
            "field" if attr_value(tag, "name").as_deref() == Some("inherit_id") => {
                if let Some(r) = attr_value(tag, "ref") {
                    toks.push((idx, ArchTag::InheritRef(r)));
                }
                if !self_closing {
                    toks.push((
                        idx,
                        ArchTag::OtherOpen {
                            tag: name,
                            name: None,
                            self_closing,
                        },
                    ));
                }
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
                let el_name = attr_value(tag, "name");
                if is_chatter {
                    toks.push((
                        idx,
                        ArchTag::ContainerOpen {
                            tag: name,
                            name: el_name,
                            token: "chatter".to_string(),
                            self_closing,
                        },
                    ));
                } else {
                    toks.push((
                        idx,
                        ArchTag::OtherOpen {
                            tag: name,
                            name: el_name,
                            self_closing,
                        },
                    ));
                }
            }
            n if REGION_CONTAINERS.contains(&n) => {
                toks.push((
                    idx,
                    ArchTag::ContainerOpen {
                        tag: name.clone(),
                        name: attr_value(tag, "name"),
                        token: name.clone(),
                        self_closing,
                    },
                ));
            }
            _ => {
                let el_name = attr_value(tag, "name");
                toks.push((
                    idx,
                    ArchTag::OtherOpen {
                        tag: name,
                        name: el_name,
                        self_closing,
                    },
                ));
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

    /// A neutral base view and extensions of it (all names invented). The base
    /// docks `amount` in `group`, has a named `notebook` page, a named `group`,
    /// a `line_ids` one2many, and a self-closing `<separator/>` before the end of
    /// its arch (the stack-balance case).
    const EXT_FIXTURE: &str = r#"
<odoo>
  <record id="view_gadget_form" model="ir.ui.view">
    <field name="model">a.gadget</field>
    <field name="arch" type="xml">
      <form>
        <header>
          <button name="action_ship" type="object"/>
        </header>
        <sheet>
          <group name="money">
            <field name="amount"/>
          </group>
          <separator/>
          <notebook>
            <page name="extra" string="Extra">
              <field name="note"/>
            </page>
          </notebook>
          <field name="line_ids">
            <list>
              <field name="qty"/>
            </list>
          </field>
        </sheet>
      </form>
    </field>
    <field name="priority">20</field>
  </record>
  <record id="view_gadget_form_ext" model="ir.ui.view">
    <field name="model">a.gadget</field>
    <field name="inherit_id" ref="other_module.view_gadget_form"/>
    <field name="arch" type="xml">
      <xpath expr="//field[@name='amount']" position="after">
        <field name="discount"/>
      </xpath>
      <field name="amount" position="before">
        <field name="currency_code"/>
      </field>
      <xpath expr="//page[@name='extra']" position="inside">
        <field name="remark"/>
      </xpath>
      <xpath expr="//group[@name='money']" position="after">
        <group name="tax">
          <field name="tax_code"/>
        </group>
      </xpath>
      <xpath expr="//header" position="inside">
        <button name="action_audit" type="object"/>
      </xpath>
      <field name="line_ids" position="inside">
        <field name="sub_qty"/>
      </field>
      <xpath expr="//div[hasclass('title')]" position="after">
        <field name="unresolved_target"/>
      </xpath>
    </field>
  </record>
  <record id="view_gadget_form_ext2" model="ir.ui.view">
    <field name="model">a.gadget</field>
    <field name="inherit_id" ref="view_gadget_form_ext"/>
    <field name="arch" type="xml">
      <field name="tax_code" position="after">
        <field name="tax_note"/>
      </field>
    </field>
  </record>
  <record id="view_orphan_ext" model="ir.ui.view">
    <field name="model">a.gadget</field>
    <field name="inherit_id" ref="missing_module.view_not_scanned"/>
    <field name="arch" type="xml">
      <field name="whatever" position="after">
        <field name="orphan_field"/>
      </field>
    </field>
  </record>
</odoo>
"#;

    fn fact_in<'a>(facts: &'a [RegionFact], screen: &str, control: &str) -> Option<&'a RegionFact> {
        facts
            .iter()
            .find(|f| f.screen.ends_with(&format!("#{screen}")) && f.control == control)
    }

    #[test]
    fn extension_controls_dock_at_their_target_region() {
        let facts = extract_regions_from_source(EXT_FIXTURE, "gadget_views.xml");
        let dock = |c: &str| {
            fact_in(&facts, "view_gadget_form_ext", c)
                .unwrap_or_else(|| panic!("no fact for {c}"))
                .dock_token
                .as_str()
        };
        // after/before a field: the region the field sits in.
        assert_eq!(dock("discount"), "group", "xpath after //field[@name]");
        assert_eq!(dock("currency_code"), "group", "shorthand before <field>");
        // inside a container: that container.
        assert_eq!(dock("remark"), "page", "inside //page[@name]");
        assert_eq!(dock("action_audit"), "header", "inside bare //header");
        // a container opened inside the locator block wins for its contents.
        assert_eq!(dock("tax_code"), "group");
    }

    #[test]
    fn a_locator_is_never_a_control() {
        let facts = extract_regions_from_source(EXT_FIXTURE, "gadget_views.xml");
        // `amount` and `line_ids` appear in the extension only as shorthand
        // locators; they must not be emitted as the extension's controls.
        assert!(fact_in(&facts, "view_gadget_form_ext", "amount").is_none());
        assert!(fact_in(&facts, "view_gadget_form_ext", "line_ids").is_none());
        // The base view still docks its own `amount`.
        assert_eq!(
            fact_in(&facts, "view_gadget_form", "amount")
                .unwrap()
                .dock_token,
            "group"
        );
    }

    #[test]
    fn inside_a_field_target_is_a_comodel_subview() {
        let facts = extract_regions_from_source(EXT_FIXTURE, "gadget_views.xml");
        assert!(
            fact_in(&facts, "view_gadget_form_ext", "sub_qty").is_none(),
            "a field added inside `line_ids` belongs to its comodel subview"
        );
    }

    #[test]
    fn unresolvable_locators_fall_back_to_root() {
        let facts = extract_regions_from_source(EXT_FIXTURE, "gadget_views.xml");
        // An xpath filtered by something other than @name is not guessed.
        assert_eq!(
            fact_in(&facts, "view_gadget_form_ext", "unresolved_target")
                .unwrap()
                .dock_token,
            "root"
        );
        // A parent outside the scanned source cannot be resolved.
        assert_eq!(
            fact_in(&facts, "view_orphan_ext", "orphan_field")
                .unwrap()
                .dock_token,
            "root"
        );
        assert!(fact_in(&facts, "view_orphan_ext", "whatever").is_none());
    }

    #[test]
    fn an_extension_of_an_extension_resolves_through_what_it_added() {
        let facts = extract_regions_from_source(EXT_FIXTURE, "gadget_views.xml");
        // `tax_code` exists only in the first extension (inside its new group).
        assert_eq!(
            fact_in(&facts, "view_gadget_form_ext2", "tax_note")
                .unwrap()
                .dock_token,
            "group"
        );
    }

    #[test]
    fn a_self_closing_element_keeps_the_stack_balanced() {
        let facts = extract_regions_from_source(EXT_FIXTURE, "gadget_views.xml");
        // `<separator/>` must not leave a frame behind: the arch ends at its own
        // `</field>`, so the record's trailing `priority` field is not a control.
        assert!(fact_in(&facts, "view_gadget_form", "priority").is_none());
        assert_eq!(
            fact_in(&facts, "view_gadget_form", "note")
                .unwrap()
                .dock_token,
            "page"
        );
    }

    #[test]
    fn xpath_targets_resolve_only_name_or_bare_steps() {
        assert_eq!(
            xpath_target("//field[@name='amount']"),
            Some(("field".into(), Some("amount".into())))
        );
        assert_eq!(
            xpath_target(r#"/form/sheet//group[@name="money"]"#),
            Some(("group".into(), Some("money".into())))
        );
        assert_eq!(xpath_target("//header"), Some(("header".into(), None)));
        assert_eq!(xpath_target("//div[hasclass('x')]"), None);
        assert_eq!(xpath_target("//field[@string='Amount']"), None);
        assert_eq!(xpath_target("//*[@name='x']"), None);
        assert_eq!(xpath_target("//field[2]"), None);
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
