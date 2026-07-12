//! Odoo **menu-QUAD** harvest — the `location` + `purpose` axes of the
//! Klickwege menu quad (see `ruff_spo_triplet::quad`'s module docs), the
//! Odoo sibling of `ruff_ruby_spo::menu_regions`'s Rails arm.
//!
//! # The model
//!
//! Odoo declares its menu tree data-side, in XML: a `<menuitem id="X"
//! parent="Y" action="Z"/>` element. `parent` IS the `part_of` rail directly
//! (Odoo declares it, unlike Rails' derived-from-DSL-order rail) — no
//! resolution algorithm is needed, only a bare-token read.
//!
//! `purpose` is classified from the menuitem's target action's `view_mode`
//! — the SAME `<record model="ir.actions.act_window">` / `<field
//! name="view_mode">` shape [`super::odoo_nav`]'s Shape B already parses,
//! joined the same cross-file way (real addons declare actions in
//! per-model view files and reference them from a central menu file). Only
//! the *first* `view_mode` token drives classification (`"list,form"` →
//! `"list"`) — the primary view is the one that best signals the surface's
//! usability role; the remaining modes are alternate renderings of the same
//! screen, not additional roles.
//!
//! Reuses the shared closed-vocab predicates the [`ruff_spo_triplet::quad`]
//! engine already mints from (`part_of` / `purpose`) — no new predicate, no
//! vocab bump. Reuses [`crate::odoo_views`]'s `collect_xml_files` /
//! `relative_path` / `attr_value` helpers; the menuitem/action scan itself
//! is a line scanner in the [`super::odoo_nav`] style (menuitems and
//! `view_mode` fields are always single-line in the real corpus — no
//! multi-line tag has been observed here, unlike the arch-body scan
//! [`super::odoo_regions`] needs a full-text tokenizer for).
//!
//! # Identity binding (`surfaces_concept`, SPEC v2)
//!
//! Odoo's identity binding is the EASY arm (unlike Rails' derived
//! `controller -> singularize -> model`): the target action's
//! `res_model` (`account.move`) IS the model token directly — a DECLARED
//! literal read verbatim from `<field name="res_model">…</field>`, no
//! inflection, no roster cross-check needed. `identity_concept` is bound
//! from the SAME `act_window` join [`collect_action_records`] already does
//! for `view_mode`, at [`Provenance::Authoritative`] (the declaration IS
//! the fact — same certainty tier as C#'s `roomAliases` config row, per
//! the shared [`ruff_spo_triplet::MenuQuad::identity_tier`] doc).
//!
//! # What is NOT captured (by design, not oversight)
//!
//! - **Menuitems with no resolvable action, an action with no `res_model`
//!   (a server action), or no `view_mode`** — classify to the fallback
//!   [`PurposeRole::Detail`], the same "no signal → plain navigation
//!   trigger" doctrine the Rails arm applies to a target-less push (there
//!   it falls to `Action`; here Odoo's own semantics make an
//!   actionless/modeless menuitem a bare navigation entry, so `Detail` is
//!   the honest default rather than borrowing Rails' `Action` label for a
//!   different frontend's vocabulary). The same actionless/modeless
//!   menuitem also leaves `identity_concept` at `None` — no model, no
//!   identity, honest refusal.

use std::fs;
use std::path::Path;

use ruff_spo_triplet::{MenuQuad, Provenance, PurposeRole, PurposeRule, classify_purpose};

use crate::odoo_views::{attr_value, collect_xml_files, relative_path};

/// The Odoo `purpose`-axis config (the operator's "config over reusable"):
/// the target action's first `view_mode` token classifies into the shared
/// [`PurposeRole`] vocab via the shared [`classify_purpose`] engine. Rules
/// are existential (`min_hits: 1`) — one `view_mode` token drives the
/// classification. Priority: a chart surface, then a list/tree/kanban
/// surface, then a form, then the remaining single-record view kinds; an
/// unresolvable/unknown mode falls to [`ODOO_PURPOSE_FALLBACK`].
const ODOO_PURPOSE: &[PurposeRule] = &[
    PurposeRule {
        needles: &["graph", "pivot"],
        role: PurposeRole::Chart,
        min_hits: 1,
    },
    PurposeRule {
        needles: &["tree", "list", "kanban"],
        role: PurposeRole::List,
        min_hits: 1,
    },
    PurposeRule {
        needles: &["form"],
        role: PurposeRole::Form,
        min_hits: 1,
    },
    PurposeRule {
        needles: &["calendar", "activity", "gantt"],
        role: PurposeRole::Detail,
        min_hits: 1,
    },
];

/// A menuitem with no resolvable action/`view_mode` signal is a plain
/// navigation trigger, honestly labeled [`PurposeRole::Detail`] (see the
/// module docs for why this differs from the Rails arm's `Action` default).
const ODOO_PURPOSE_FALLBACK: PurposeRole = PurposeRole::Detail;

/// One collected `<menuitem>` site, pre-purpose-classification.
struct MenuItemRaw {
    /// The `id="…"` attribute — the node's bare token.
    id: String,
    /// The `parent="…"` attribute, if present — the `part_of` object's bare
    /// token. `None` for a root menuitem.
    parent: Option<String>,
    /// The target action's first `view_mode` token, or `""` when the
    /// menuitem has no resolvable action / the action has no `view_mode`.
    view_mode_token: String,
    /// The target action's `res_model`, verbatim (`account.move`) — the
    /// `identity` axis's DECLARED-literal token. `None` when the menuitem
    /// has no resolvable action, or the action has no `res_model` (a
    /// server action).
    res_model: Option<String>,
}

/// Harvest every `<menuitem>` as a [`MenuQuad`] — the `location`/`purpose`
/// half of the Klickwege menu quad. Nodes use the bare `{namespace}:{id}`
/// grammar (menuitem ids may be dotted external ids — that's fine, the IRI
/// is an opaque bare string, never split on the dot).
#[must_use]
pub fn extract_menu_quads(root: &Path, namespace: &str) -> Vec<MenuQuad> {
    let mut files = Vec::new();
    collect_xml_files(root, &mut files);

    let mut xml_contents: Vec<(String, String)> = Vec::new();
    for path in &files {
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        xml_contents.push((relative_path(root, path), content));
    }

    // Pass 1: every act_window record's (xml_id -> (first `view_mode` token,
    // `res_model`)), across ALL files — addons split actions and menuitems
    // across files (mirrors odoo_nav's Shape B cross-file join).
    let mut actions: Vec<(String, Option<String>, Option<String>)> = Vec::new();
    for (_, content) in &xml_contents {
        collect_action_records(content, &mut actions);
    }

    // Pass 2: menuitems, joined against the addon-wide action map.
    let mut items: Vec<MenuItemRaw> = Vec::new();
    for (_, content) in &xml_contents {
        scan_menuitems(content, &actions, &mut items);
    }

    items
        .into_iter()
        .map(|item| {
            let purpose = classify_purpose(
                &[item.view_mode_token.as_str()],
                ODOO_PURPOSE,
                ODOO_PURPOSE_FALLBACK,
            );
            MenuQuad {
                node: format!("{namespace}:{}", item.id),
                parent: item.parent.map(|p| format!("{namespace}:{p}")),
                part_of_tier: Provenance::Authoritative,
                purpose,
                // A DECLARED literal (`res_model`), read verbatim — no
                // inflection, no roster cross-check. Authoritative: the
                // declaration IS the fact.
                identity_concept: item.res_model,
                identity_tier: Provenance::Authoritative,
            }
        })
        .collect()
}

/// Pass 1: append every `<record id="X" model="ir.actions.act_window">`
/// block's `(xml_id -> (first view_mode token, res_model))` from one file
/// into the ADDON-WIDE `actions` map (mirrors
/// `odoo_nav::collect_act_window_records`, reading BOTH `view_mode` — the
/// `purpose` axis's signal — and `res_model` verbatim — the `identity`
/// axis's DECLARED-literal token, unlike `odoo_nav`'s `normalize_model`
/// which dot-to-underscore normalizes it for screen-name matching; here the
/// raw dotted form IS the model token `PortSpec::class_id` resolves).
fn collect_action_records(
    content: &str,
    actions: &mut Vec<(String, Option<String>, Option<String>)>,
) {
    let mut in_record: Option<usize> = None; // index into `actions`
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("<record")
            && t.contains("model=\"ir.actions.act_window\"")
            && let Some(id) = attr_value(t, "id")
        {
            actions.push((id, None, None));
            in_record = Some(actions.len() - 1);
        } else if t.starts_with("<record") {
            // A different record kind closes any open act_window context.
            in_record = None;
        } else if t.starts_with("</record>") {
            in_record = None;
        } else if let Some(idx) = in_record
            && t.starts_with("<field")
            && attr_value(t, "name").as_deref() == Some("view_mode")
            && let Some(modes) = tag_text(t)
        {
            let first = modes.split(',').next().unwrap_or("").trim().to_string();
            actions[idx].1 = Some(first);
        } else if let Some(idx) = in_record
            && t.starts_with("<field")
            && attr_value(t, "name").as_deref() == Some("res_model")
            && let Some(model) = tag_text(t)
        {
            actions[idx].2 = Some(model);
        }
    }
}

/// Pass 2: `<menuitem id="X" parent="Y" action="Z"/>` lines in one file,
/// each recorded as a [`MenuItemRaw`] with its `view_mode_token`/`res_model`
/// resolved by joining `action` against the addon-wide action map
/// (module-qualified `action="mod.X"` joins on its local part, mirroring
/// `odoo_nav::scan_menuitems`). A menuitem with no `action`, or whose action
/// has no known `view_mode`/`res_model`, gets an empty token / `None` (the
/// former classifies to the fallback purpose; the latter leaves identity
/// dormant).
fn scan_menuitems(
    content: &str,
    actions: &[(String, Option<String>, Option<String>)],
    items: &mut Vec<MenuItemRaw>,
) {
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("<menuitem")
            && let Some(id) = attr_value(t, "id")
        {
            let parent = attr_value(t, "parent");
            let matched = attr_value(t, "action").and_then(|action_ref| {
                let local = action_ref
                    .rsplit('.')
                    .next()
                    .unwrap_or(&action_ref)
                    .to_string();
                actions
                    .iter()
                    .find(|(aid, _, _)| *aid == local || *aid == action_ref)
            });
            let view_mode_token = matched
                .and_then(|(_, vm, _)| vm.clone())
                .unwrap_or_default();
            let res_model = matched.and_then(|(_, _, rm)| rm.clone());
            items.push(MenuItemRaw {
                id,
                parent,
                view_mode_token,
                res_model,
            });
        }
    }
}

/// The text content of a one-line `<tag …>text</tag>` element, if present
/// (mirrors `odoo_nav::tag_text`).
fn tag_text(line: &str) -> Option<String> {
    let start = line.find('>')? + 1;
    let end = line[start..].find('<')? + start;
    let text = line[start..end].trim();
    (!text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn write(dir: &Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn tempdir(case: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ruff_python_spo_odoo_quad_{}_{case}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: PathBuf) {
        let _ = fs::remove_dir_all(dir);
    }

    const MENU_XML: &str = r#"<?xml version="1.0"?>
<odoo>
    <record id="act_list" model="ir.actions.act_window">
        <field name="res_model">account.move</field>
        <field name="view_mode">list,form</field>
    </record>
    <record id="act_form" model="ir.actions.act_window">
        <field name="res_model">account.move</field>
        <field name="view_mode">form</field>
    </record>
    <record id="act_chart" model="ir.actions.act_window">
        <field name="res_model">account.move</field>
        <field name="view_mode">graph,pivot</field>
    </record>
    <menuitem id="menu_root"/>
    <menuitem id="menu_child" parent="menu_root" action="act_list"/>
    <menuitem id="menu_form" parent="menu_root" action="act_form"/>
    <menuitem id="menu_chart" parent="menu_root" action="act_chart"/>
    <menuitem id="menu_no_action" parent="menu_root"/>
</odoo>
"#;

    fn quad<'a>(quads: &'a [MenuQuad], item: &str) -> &'a MenuQuad {
        quads
            .iter()
            .find(|q| q.node == format!("odoo:{item}"))
            .unwrap_or_else(|| panic!("no quad for {item}"))
    }

    /// Location: `parent` from the `<menuitem parent=>` attribute, bare-node
    /// `{ns}:{id}` grammar. A root menuitem (no `parent`) has `parent ==
    /// None`.
    #[test]
    fn location_resolves_from_declared_parent_attribute() {
        let dir = tempdir("location");
        write(&dir, "views/menu.xml", MENU_XML);

        let quads = extract_menu_quads(&dir, "odoo");
        assert_eq!(quad(&quads, "menu_root").parent, None);
        assert_eq!(
            quad(&quads, "menu_child").parent.as_deref(),
            Some("odoo:menu_root")
        );

        cleanup(dir);
    }

    /// Identity: the target action's `res_model`, read verbatim (no
    /// inflection) — a DECLARED literal, `Authoritative` tier. A menuitem
    /// with no `action` at all, or whose action has no `res_model`, leaves
    /// `identity_concept` at `None`.
    #[test]
    fn identity_binds_from_declared_res_model_verbatim() {
        let dir = tempdir("identity");
        write(&dir, "views/menu.xml", MENU_XML);

        let quads = extract_menu_quads(&dir, "odoo");
        assert_eq!(
            quad(&quads, "menu_child").identity_concept.as_deref(),
            Some("account.move")
        );
        assert_eq!(
            quad(&quads, "menu_child").identity_tier,
            Provenance::Authoritative
        );
        // No `action` at all -> no res_model -> dormant identity.
        assert_eq!(quad(&quads, "menu_root").identity_concept, None);

        let triples = quad(&quads, "menu_child").to_triples();
        let sc = triples
            .iter()
            .find(|t| t.p == "surfaces_concept")
            .unwrap_or_else(|| panic!("no surfaces_concept triple: {triples:?}"));
        assert_eq!(sc.o, "account.move");
        assert_eq!((sc.f, sc.c), Provenance::Authoritative.truth());
        // A dormant identity emits no surfaces_concept triple at all.
        assert!(
            !quad(&quads, "menu_root")
                .to_triples()
                .iter()
                .any(|t| t.p == "surfaces_concept")
        );

        cleanup(dir);
    }

    /// Purpose: the target action's first `view_mode` token classifies
    /// through the shared engine — `list,form` -> List (first token
    /// `"list"`), `form` -> Form, `graph,pivot` -> Chart, no `action` ->
    /// fallback Detail.
    #[test]
    fn purpose_classifies_from_first_view_mode_token() {
        let dir = tempdir("purpose");
        write(&dir, "views/menu.xml", MENU_XML);

        let quads = extract_menu_quads(&dir, "odoo");
        assert_eq!(quad(&quads, "menu_child").purpose, PurposeRole::List);
        assert_eq!(quad(&quads, "menu_form").purpose, PurposeRole::Form);
        assert_eq!(quad(&quads, "menu_chart").purpose, PurposeRole::Chart);
        assert_eq!(quad(&quads, "menu_no_action").purpose, PurposeRole::Detail);
        // A menuitem with no action at all also falls to the fallback.
        assert_eq!(quad(&quads, "menu_root").purpose, PurposeRole::Detail);

        cleanup(dir);
    }

    /// The emitted facts carry the bare-node subject grammar: `part_of` at
    /// Authoritative tier (Odoo DECLARES the parent), `purpose` at Inferred
    /// (a heuristic over the `view_mode` signal).
    #[test]
    fn to_triples_emits_bare_part_of_authoritative_and_purpose_inferred() {
        let dir = tempdir("triples");
        write(&dir, "views/menu.xml", MENU_XML);

        let quads = extract_menu_quads(&dir, "odoo");
        let triples = quad(&quads, "menu_child").to_triples();

        let part_of = triples.iter().find(|t| t.p == "part_of").unwrap();
        assert_eq!(part_of.s, "odoo:menu_child"); // bare, no dot
        assert_eq!(part_of.o, "odoo:menu_root");
        let (af, ac) = Provenance::Authoritative.truth();
        assert_eq!((part_of.f, part_of.c), (af, ac));

        let purpose = triples.iter().find(|t| t.p == "purpose").unwrap();
        assert_eq!(purpose.s, "odoo:menu_child");
        assert_eq!(purpose.o, "list");
        let (inf_f, inf_c) = Provenance::Inferred.truth();
        assert_eq!((purpose.f, purpose.c), (inf_f, inf_c));

        cleanup(dir);
    }

    /// A module-qualified menuitem action ref (`action="mod.X"`) joins on
    /// its local part, mirroring `odoo_nav`'s equivalent behaviour.
    #[test]
    fn module_qualified_action_ref_joins_on_local_part() {
        let dir = tempdir("qualified");
        write(
            &dir,
            "views/menu.xml",
            r#"<odoo>
    <record id="act_x" model="ir.actions.act_window">
        <field name="res_model">res.partner</field>
        <field name="view_mode">kanban,form</field>
    </record>
    <menuitem id="menu_x" action="account.act_x"/>
</odoo>
"#,
        );

        let quads = extract_menu_quads(&dir, "odoo");
        assert_eq!(quad(&quads, "menu_x").purpose, PurposeRole::List);

        cleanup(dir);
    }

    /// The menuitem->action join is CROSS-FILE, like `odoo_nav`'s Shape B:
    /// the action record and the menuitem may live in different files.
    #[test]
    fn menuitem_joins_action_declared_in_another_file() {
        let dir = tempdir("cross_file");
        write(
            &dir,
            "views/account_move_views.xml",
            r#"<odoo>
    <record id="act_form_only" model="ir.actions.act_window">
        <field name="res_model">account.move</field>
        <field name="view_mode">form</field>
    </record>
</odoo>
"#,
        );
        write(
            &dir,
            "views/account_menuitem.xml",
            r#"<odoo>
    <menuitem id="menu_action_move" action="act_form_only"/>
</odoo>
"#,
        );

        let quads = extract_menu_quads(&dir, "odoo");
        assert_eq!(quad(&quads, "menu_action_move").purpose, PurposeRole::Form);

        cleanup(dir);
    }
}
