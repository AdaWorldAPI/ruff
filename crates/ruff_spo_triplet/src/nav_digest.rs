//! The Klickwege structure-parity oracle (transcode doctrine: "`MySQL` = value
//! parity, Klickwege = structure parity").
//!
//! [`build_nav_digest`] folds the UI-navigation plane of a harvest — the
//! screen graph (`navigates_to`), tab/view selection (`selects_view`),
//! concept bindings (`surfaces_concept`, resolved against an
//! [`crate::exam_config::ExamConfig`] codebook + alias convention), and the
//! per-screen control/handler surface (`contains_control` /
//! `handles_event`) — into one deterministic text digest, meant to be
//! diffed as a golden parity artifact: every section sorted lexicographically
//! and deduplicated, so the same harvest (in any triple order) always
//! produces byte-identical output. This is the structure-parity half of the
//! transcode oracle pair; the value-parity half is the MySQL/lance-datafusion
//! reconciler in the consumer repos.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use crate::exam_config::ExamConfig;
use crate::region::RegionSubject;
use crate::triple::Triple;

/// `(screen, region) -> [(dock/popup token, dock order)]` groupings used
/// while assembling the `[regions]` digest section.
type RegionEntries = BTreeMap<(String, String), Vec<(String, Option<u32>)>>;

/// Strip a triple's namespace prefix (`"ns:"`), returning the local part.
/// An IRI with no `:` passes through unchanged.
fn strip_ns(iri: &str) -> &str {
    iri.split_once(':').map_or(iri, |(_, tail)| tail)
}

/// The screen segment of a namespace-qualified, possibly control-qualified
/// local IRI: the namespace-stripped segment up to the **last** `.`, e.g.
/// `"csharp:uc_cipher_main.btn_save"` -> `"uc_cipher_main"`. Decoded through
/// the shared [`crate::RegionSubject::from_iri`] codec (last-dot rule), so a
/// dotted screen (Odoo's `widget_views.xml#view.field`) recovers the screen
/// correctly instead of splitting at the `.xml` dot.
fn screen_of(iri: &str) -> &str {
    let local = strip_ns(iri);
    RegionSubject::from_iri(local).map_or(local, |(head, _)| head)
}

/// The control segment of a namespace-qualified, screen-qualified local
/// IRI: the tail after the **last** `.`, e.g.
/// `"csharp:uc_cipher_main.btn_save"` -> `"btn_save"`. Sibling of
/// [`screen_of`] (the other half of the same [`crate::RegionSubject::from_iri`]
/// split); when there is no `.` the whole namespace-stripped string is
/// returned (degenerate case, not expected for `docked_at` / `tab_order` /
/// `opens_popup` subjects).
fn control_of(iri: &str) -> &str {
    let local = strip_ns(iri);
    RegionSubject::from_iri(local).map_or(local, |(_, tail)| tail)
}

/// Lower a menu node's LOCATION into the existing classid ontology: walk the
/// `part_of` rail from `node` up to its root and render the ancestor chain
/// root-first as a radix-trie path. Each element is the ancestor's classid
/// (`0x<ID>`) when it resolves, else its screen name (fallback). There is no
/// stored ordinal — the address IS the walked rail (V3 LE-contract §3: the
/// existing concept ontology is the radix trie; menu location is a path
/// through it). Cycle-guarded and depth-bounded so a misdeclared `part_of`
/// cycle terminates instead of looping.
fn menu_address(
    node: &str,
    parent_of: &BTreeMap<String, String>,
    classid_of: &BTreeMap<String, u16>,
) -> String {
    let mut chain: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut cur = node.to_string();
    loop {
        if !seen.insert(cur.clone()) || chain.len() >= 32 {
            break;
        }
        chain.push(
            classid_of
                .get(&cur)
                .map_or_else(|| cur.clone(), |id| format!("0x{id:04X}")),
        );
        match parent_of.get(&cur) {
            Some(parent) => cur = parent.clone(),
            None => break,
        }
    }
    chain.reverse();
    chain.join("/")
}

/// Resolve a `surfaces_concept` object token to a codebook concept id.
///
/// Tries, in order: (1) an exact codebook concept-name match; (2) a
/// [`crate::concept_split::ConceptConvention::concept_aliases`] key match
/// (case-insensitive, mirroring [`crate::concept_split`]'s own alias
/// resolution) whose target concept is itself bound in the codebook.
/// `None` when neither resolves — the caller renders `UNRESOLVED`.
fn resolve_token(token: &str, config: &ExamConfig) -> Option<u16> {
    if let Some((_, id)) = config.codebook.iter().find(|(name, _)| name == token) {
        return Some(*id);
    }
    let (_, target) = config
        .convention
        .concept_aliases
        .iter()
        .find(|(from, _)| from.eq_ignore_ascii_case(token))?;
    config
        .codebook
        .iter()
        .find(|(name, _)| name == target)
        .map(|(_, id)| *id)
}

/// Build the deterministic nav digest for a harvest.
///
/// Format (every section sorted lexicographically and deduplicated):
///
/// ```text
/// === nav digest v2 ===
/// screens: <N>
/// klickwege: <M>
/// views: <V>
/// concept bindings: <K> resolved, <U> unresolved
/// regions: <R>
/// [klickwege]
/// <from> -> <to>
/// [views]
/// <screen> => <view>
/// [concepts]
/// <screen> ~ <token> -> 0x<ID>
/// <screen> ~ <token> -> UNRESOLVED
/// [screen surface]
/// <screen> controls=<c> handlers=<h>
/// [regions]
/// <screen> / <region>: <control>(<order>), <control>(<order>) ...
/// [menu-tree]
/// <root-screen>
///   = <view>
///   -> <target-screen>
/// [menu-quad]
/// <node>  loc=<radix-address>  purpose=<role>  id=0x<ID>  action=<navigate|root|leaf>
/// ```
///
/// - `screens` is the distinct endpoint set of every `navigates_to` pair
///   (both ends) union every `selects_view` subject.
/// - `[klickwege]` / `[views]` strip the namespace prefix from both sides.
/// - `[concepts]` resolves each `surfaces_concept` object token via
///   [`resolve_token`]; the screen is the namespace-stripped subject.
/// - `[screen surface]` only lists screens with `controls + handlers > 0`;
///   the screen is the namespace-stripped subject segment up to the first
///   `.` (see [`screen_of`]).
/// - `regions` / `[regions]` fold `docked_at` facts into the region frame:
///   the dock token is resolved through
///   [`crate::exam_config::ExamConfig::regions`] to a region name, falling
///   back to `unmapped:<token>` when config carries no row for that token
///   (nothing drops silently). Within a region, controls are ordered by
///   `tab_order` ascending (missing `tab_order` sorts last, ties broken
///   lexicographically by control name) and rendered as
///   `<control>(<order>)` (`-` when no `tab_order` fact exists). A control
///   that is ALSO the subject of an `opens_popup` fact gets a `→popup`
///   suffix; the `opens_popup` OBJECT (the menu control it opens) is
///   listed as its own entry under region `popup` on its own screen.
///   `regions` counts the distinct screens carrying any region data
///   (docked controls or popup targets).
/// - `[menu-tree]` prints one block per screen with `selects_view` and/or
///   `navigates_to` OUT-edges (screens sorted): the screen name, then its
///   `selects_view` targets each as `  = <view>` (sorted), then its
///   `navigates_to` targets each as `  -> <target-screen>` (sorted).
///   Indentation is exactly two spaces.
/// - `[menu-quad]` lowers the `(location, purpose, identity, action)` quad per
///   menu node into the existing classid ontology. `location` is the `part_of`
///   rail walked as a radix-trie address ([`menu_address`]): the ancestor
///   classid path, root-first, `0x<ID>` per resolved ancestor and the bare
///   screen name as fallback — **no stored ordinal**, the address IS the walk
///   (V3 LE-contract §3). `identity` is the node's own classid (`-` when
///   unresolved); `purpose` the `purpose` role (`-` when none); `action` is
///   `navigate` (a `part_of` child), `root` (a parentless menu source), or
///   `leaf`. Nodes sorted lexicographically.
#[must_use]
pub fn build_nav_digest(triples: &[Triple], config: &ExamConfig) -> String {
    let mut klickwege: BTreeSet<(String, String)> = BTreeSet::new();
    let mut views: BTreeSet<(String, String)> = BTreeSet::new();
    let mut concepts: BTreeSet<(String, String)> = BTreeSet::new();
    let mut screens: BTreeSet<String> = BTreeSet::new();
    let mut surface: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut docked_raw: BTreeSet<(String, String, String)> = BTreeSet::new();
    let mut tab_order_raw: BTreeSet<(String, String, String)> = BTreeSet::new();
    let mut popup_raw: BTreeSet<(String, String, String)> = BTreeSet::new();
    // Klickwege-rail: the menu quad's location + purpose axes. Collected as
    // sorted raw sets first (like docked_at / tab_order below), so when a child
    // carries more than one `part_of` fact — or a screen more than one
    // `purpose` — the chosen value is deterministic (smallest wins via
    // `or_insert` over the sorted set), never dependent on input triple order.
    let mut part_of_raw: BTreeSet<(String, String)> = BTreeSet::new();
    let mut purpose_raw: BTreeSet<(String, String)> = BTreeSet::new();

    for t in triples {
        match t.p.as_str() {
            "navigates_to" => {
                let from = strip_ns(&t.s).to_string();
                let to = strip_ns(&t.o).to_string();
                screens.insert(from.clone());
                screens.insert(to.clone());
                klickwege.insert((from, to));
            }
            "selects_view" => {
                let screen = strip_ns(&t.s).to_string();
                let view = strip_ns(&t.o).to_string();
                screens.insert(screen.clone());
                views.insert((screen, view));
            }
            "surfaces_concept" => {
                concepts.insert((strip_ns(&t.s).to_string(), t.o.clone()));
            }
            "contains_control" => {
                surface
                    .entry(screen_of(&t.s).to_string())
                    .or_insert((0, 0))
                    .0 += 1;
            }
            "handles_event" => {
                surface
                    .entry(screen_of(&t.s).to_string())
                    .or_insert((0, 0))
                    .1 += 1;
            }
            "docked_at" => {
                docked_raw.insert((
                    screen_of(&t.s).to_string(),
                    control_of(&t.s).to_string(),
                    t.o.clone(),
                ));
            }
            "tab_order" => {
                tab_order_raw.insert((
                    screen_of(&t.s).to_string(),
                    control_of(&t.s).to_string(),
                    t.o.clone(),
                ));
            }
            "opens_popup" => {
                popup_raw.insert((
                    screen_of(&t.s).to_string(),
                    control_of(&t.s).to_string(),
                    t.o.clone(),
                ));
            }
            "part_of" => {
                let child = strip_ns(&t.s).to_string();
                let parent = strip_ns(&t.o).to_string();
                screens.insert(child.clone());
                screens.insert(parent.clone());
                part_of_raw.insert((child, parent));
            }
            "purpose" => {
                let screen = strip_ns(&t.s).to_string();
                screens.insert(screen.clone());
                purpose_raw.insert((screen, t.o.clone()));
            }
            _ => {}
        }
    }

    // Resolve raw (screen, control, value) sets into per-control lookups.
    // Building the lookup by iterating the already-sorted BTreeSets (rather
    // than the raw input order) keeps the choice among conflicting facts
    // for the same control deterministic regardless of harvest order.
    let mut dock_of: BTreeMap<(String, String), String> = BTreeMap::new();
    for (screen, control, token) in &docked_raw {
        dock_of
            .entry((screen.clone(), control.clone()))
            .or_insert_with(|| token.clone());
    }
    let mut order_of: BTreeMap<(String, String), String> = BTreeMap::new();
    for (screen, control, order) in &tab_order_raw {
        order_of
            .entry((screen.clone(), control.clone()))
            .or_insert_with(|| order.clone());
    }
    let mut popup_subjects: BTreeSet<(String, String)> = BTreeSet::new();
    let mut popup_targets: BTreeSet<(String, String)> = BTreeSet::new();
    for (screen, control, target) in &popup_raw {
        popup_subjects.insert((screen.clone(), control.clone()));
        popup_targets.insert((
            screen_of(target).to_string(),
            control_of(target).to_string(),
        ));
    }
    // Deterministic child->parent / screen->role lookups: iterate the sorted
    // raw sets so the smallest parent/role wins when a node carries several.
    let mut parent_of: BTreeMap<String, String> = BTreeMap::new();
    for (child, parent) in &part_of_raw {
        parent_of
            .entry(child.clone())
            .or_insert_with(|| parent.clone());
    }
    let mut purpose_of: BTreeMap<String, String> = BTreeMap::new();
    for (screen, role) in &purpose_raw {
        purpose_of
            .entry(screen.clone())
            .or_insert_with(|| role.clone());
    }

    // Group docked controls (+ popup targets) by resolved region.
    let mut region_entries: RegionEntries = BTreeMap::new();
    for ((screen, control), token) in &dock_of {
        let region = config
            .regions
            .iter()
            .find(|(tok, _)| tok == token)
            .map_or_else(|| format!("unmapped:{token}"), |(_, name)| name.clone());
        let order = order_of
            .get(&(screen.clone(), control.clone()))
            .and_then(|s| s.parse::<u32>().ok());
        region_entries
            .entry((screen.clone(), region))
            .or_default()
            .push((control.clone(), order));
    }
    for (screen, control) in &popup_targets {
        let order = order_of
            .get(&(screen.clone(), control.clone()))
            .and_then(|s| s.parse::<u32>().ok());
        region_entries
            .entry((screen.clone(), "popup".to_string()))
            .or_default()
            .push((control.clone(), order));
    }
    for entries in region_entries.values_mut() {
        entries.sort_by(|a, b| {
            let key_a = (a.1.unwrap_or(u32::MAX), a.0.clone());
            let key_b = (b.1.unwrap_or(u32::MAX), b.0.clone());
            key_a.cmp(&key_b)
        });
    }
    let regions_with_data: BTreeSet<&String> = region_entries.keys().map(|(s, _)| s).collect();

    // Menu-tree roots: screens with a `selects_view` and/or `navigates_to`
    // OUT-edge (reuses the already-collected, already-sorted sets).
    let mut menu_roots: BTreeSet<String> = BTreeSet::new();
    for (screen, _) in &views {
        menu_roots.insert(screen.clone());
    }
    for (from, _) in &klickwege {
        menu_roots.insert(from.clone());
    }

    let mut resolved = 0usize;
    let mut unresolved = 0usize;
    let concept_rows: Vec<(String, String, Option<u16>)> = concepts
        .into_iter()
        .map(|(screen, token)| {
            let id = resolve_token(&token, config);
            if id.is_some() {
                resolved += 1;
            } else {
                unresolved += 1;
            }
            (screen, token, id)
        })
        .collect();

    let mut out = String::new();
    let _ = writeln!(out, "=== nav digest v2 ===");
    let _ = writeln!(out, "screens: {}", screens.len());
    let _ = writeln!(out, "klickwege: {}", klickwege.len());
    let _ = writeln!(out, "views: {}", views.len());
    let _ = writeln!(
        out,
        "concept bindings: {resolved} resolved, {unresolved} unresolved"
    );
    let _ = writeln!(out, "regions: {}", regions_with_data.len());

    let _ = writeln!(out, "[klickwege]");
    for (from, to) in &klickwege {
        let _ = writeln!(out, "{from} -> {to}");
    }

    let _ = writeln!(out, "[views]");
    for (screen, view) in &views {
        let _ = writeln!(out, "{screen} => {view}");
    }

    let _ = writeln!(out, "[concepts]");
    for (screen, token, id) in &concept_rows {
        match id {
            Some(id) => {
                let _ = writeln!(out, "{screen} ~ {token} -> 0x{id:04X}");
            }
            None => {
                let _ = writeln!(out, "{screen} ~ {token} -> UNRESOLVED");
            }
        }
    }

    let _ = writeln!(out, "[screen surface]");
    for (screen, (controls, handlers)) in &surface {
        if controls + handlers > 0 {
            let _ = writeln!(out, "{screen} controls={controls} handlers={handlers}");
        }
    }

    let _ = writeln!(out, "[regions]");
    for ((screen, region), entries) in &region_entries {
        let rendered: Vec<String> = entries
            .iter()
            .map(|(control, order)| {
                let name = if popup_subjects.contains(&(screen.clone(), control.clone())) {
                    format!("{control}→popup")
                } else {
                    control.clone()
                };
                let order_display = order.map_or_else(|| "-".to_string(), |o| o.to_string());
                format!("{name}({order_display})")
            })
            .collect();
        let _ = writeln!(out, "{screen} / {region}: {}", rendered.join(", "));
    }

    let _ = writeln!(out, "[menu-tree]");
    for root in &menu_roots {
        let _ = writeln!(out, "{root}");
        for (screen, view) in &views {
            if screen == root {
                let _ = writeln!(out, "  = {view}");
            }
        }
        for (from, to) in &klickwege {
            if from == root {
                let _ = writeln!(out, "  -> {to}");
            }
        }
    }

    // [menu-quad] — the (location, purpose, identity, action) quad per menu
    // node, LOWERED into the existing classid ontology. `location` is the
    // `part_of` rail walked as a radix-trie address (ancestor classid path,
    // root-first) — no stored ordinal (V3 §3). `identity` is the node's own
    // classid; `purpose` the usability role; `action` its nav position
    // (navigate = a part_of child, root = a parentless menu source, leaf =
    // neither). This is the lowering the raw facts exist to feed.
    let mut classid_of: BTreeMap<String, u16> = BTreeMap::new();
    for (screen, _token, id) in &concept_rows {
        if let Some(id) = id {
            classid_of.entry(screen.clone()).or_insert(*id);
        }
    }
    let mut nodes: BTreeSet<String> = screens.iter().cloned().collect();
    nodes.extend(purpose_of.keys().cloned());
    nodes.extend(parent_of.keys().cloned());
    nodes.extend(parent_of.values().cloned());
    nodes.extend(classid_of.keys().cloned());
    let _ = writeln!(out, "[menu-quad]");
    for node in &nodes {
        let loc = menu_address(node, &parent_of, &classid_of);
        let purpose = purpose_of.get(node).map_or("-", String::as_str);
        let id = classid_of
            .get(node)
            .map_or_else(|| "-".to_string(), |id| format!("0x{id:04X}"));
        let action = if parent_of.contains_key(node) {
            "navigate"
        } else if menu_roots.contains(node) {
            "root"
        } else {
            "leaf"
        };
        let _ = writeln!(
            out,
            "{node}  loc={loc}  purpose={purpose}  id={id}  action={action}"
        );
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::concept_split::ConceptConvention;
    use crate::triple::{Predicate, Provenance};

    fn config() -> ExamConfig {
        ExamConfig {
            convention: ConceptConvention {
                concept_aliases: vec![("cipher".to_string(), "cipher_key".to_string())],
                ..ConceptConvention::default()
            },
            codebook: vec![("cipher_key".to_string(), 0x0C01)],
            regions: vec![
                ("top".to_string(), "top_bar".to_string()),
                ("fill".to_string(), "center".to_string()),
                ("left".to_string(), "left_nav".to_string()),
            ],
            ..ExamConfig::default()
        }
    }

    /// Screens `Invoice` / `CipherPanel`: one `navigates_to` edge, one
    /// `selects_view` tab selection, one `surfaces_concept` token that
    /// resolves through the alias -> codebook chain, one `contains_control`
    /// and one `handles_event` fact on `CipherPanel`, plus a three-control
    /// dock/tab-order/popup layout on `CipherPanel`: `grid1` docked `fill`
    /// (-> region `center`) with `tab_order` 1, `nav_tree` docked `left`
    /// (-> region `left_nav`) with `tab_order` 2, and `btn_save` docked
    /// `bottom` (a token [`config`] does NOT map, exercising the
    /// `unmapped:<token>` arm inline) with no `tab_order` and an
    /// `opens_popup` edge to `ctx_menu`.
    fn sample_triples() -> Vec<Triple> {
        vec![
            Triple::new(
                "app:Invoice",
                Predicate::NavigatesTo,
                "app:CipherPanel",
                Provenance::Inferred,
            ),
            Triple::new(
                "app:Invoice",
                Predicate::SelectsView,
                "app:tab_summary",
                Provenance::Inferred,
            ),
            Triple::new(
                "app:CipherPanel",
                Predicate::SurfacesConcept,
                "cipher",
                Provenance::Authoritative,
            ),
            Triple::new(
                "app:CipherPanel.panel1",
                Predicate::ContainsControl,
                "app:CipherPanel.grid1",
                Provenance::Authoritative,
            ),
            Triple::new(
                "app:CipherPanel.btn_save",
                Predicate::HandlesEvent,
                "Click:app:CipherPanel.btn_save_click",
                Provenance::Authoritative,
            ),
            Triple::new(
                "app:CipherPanel.grid1",
                Predicate::DockedAt,
                "fill",
                Provenance::Authoritative,
            ),
            Triple::new(
                "app:CipherPanel.grid1",
                Predicate::TabOrder,
                "1",
                Provenance::Authoritative,
            ),
            Triple::new(
                "app:CipherPanel.nav_tree",
                Predicate::DockedAt,
                "left",
                Provenance::Authoritative,
            ),
            Triple::new(
                "app:CipherPanel.nav_tree",
                Predicate::TabOrder,
                "2",
                Provenance::Authoritative,
            ),
            Triple::new(
                "app:CipherPanel.btn_save",
                Predicate::DockedAt,
                "bottom",
                Provenance::Authoritative,
            ),
            Triple::new(
                "app:CipherPanel.btn_save",
                Predicate::OpensPopup,
                "app:CipherPanel.ctx_menu",
                Provenance::Authoritative,
            ),
            // Klickwege-rail: CipherPanel's canonical menu parent is Invoice
            // (the part_of rail), so its lowered location walks to
            // `Invoice/0x0C01`; both carry a purpose role.
            Triple::new(
                "app:CipherPanel",
                Predicate::PartOf,
                "app:Invoice",
                Provenance::Inferred,
            ),
            Triple::new(
                "app:CipherPanel",
                Predicate::Purpose,
                "form",
                Provenance::Inferred,
            ),
            Triple::new(
                "app:Invoice",
                Predicate::Purpose,
                "list",
                Provenance::Inferred,
            ),
        ]
    }

    const EXPECTED: &str = "\
=== nav digest v2 ===
screens: 2
klickwege: 1
views: 1
concept bindings: 1 resolved, 0 unresolved
regions: 1
[klickwege]
Invoice -> CipherPanel
[views]
Invoice => tab_summary
[concepts]
CipherPanel ~ cipher -> 0x0C01
[screen surface]
CipherPanel controls=1 handlers=1
[regions]
CipherPanel / center: grid1(1)
CipherPanel / left_nav: nav_tree(2)
CipherPanel / popup: ctx_menu(-)
CipherPanel / unmapped:bottom: btn_save→popup(-)
[menu-tree]
Invoice
  = tab_summary
  -> CipherPanel
[menu-quad]
CipherPanel  loc=Invoice/0x0C01  purpose=form  id=0x0C01  action=navigate
Invoice  loc=Invoice  purpose=list  id=-  action=root
";

    #[test]
    fn digest_matches_the_exact_expected_string() {
        assert_eq!(build_nav_digest(&sample_triples(), &config()), EXPECTED);
    }

    /// Convergence proof (region-collapse spec §6): a Rails-shaped fact and an
    /// Odoo-shaped fact, lifted through the SHARED `region_triples`, both land
    /// in the `[regions]` section of the SAME digest — proving all frontends
    /// round-trip through one consumer. The Odoo case is the one that matters:
    /// its screen carries a `.xml` dot, so it exercises the shared codec's
    /// last-dot rule (`rsplit_once`). Before the collapse Odoo emitted a
    /// `{screen}::{control}` subject the dot-parsing digest could not read.
    #[test]
    fn rails_and_odoo_facts_both_round_trip_through_one_digest() {
        let facts = [
            crate::RegionFact {
                screen: "openproject:top_menu".to_string(),
                control: "projects".to_string(),
                dock_token: "top".to_string(),
                tab_order: Some(0),
                opens_popup: None,
                parent: None,
            },
            crate::RegionFact {
                // Dotted `.xml` screen — the codec must split on the LAST dot.
                screen: "widget_views.xml#view_form".to_string(),
                control: "partner_id".to_string(),
                dock_token: "fill".to_string(),
                tab_order: Some(0),
                opens_popup: None,
                parent: None,
            },
        ];
        let digest = build_nav_digest(&crate::region_triples(&facts), &config());
        // Rails: `top` -> top_bar, ns-stripped screen `top_menu`.
        assert!(
            digest.contains("top_menu / top_bar: projects(0)"),
            "Rails fact missing from [regions]:\n{digest}"
        );
        // Odoo: `fill` -> center, dotted screen recovered intact (the `.xml`
        // dot is NOT mistaken for the control boundary).
        assert!(
            digest.contains("widget_views.xml#view_form / center: partner_id(0)"),
            "Odoo dotted-screen fact missing or wrongly split in [regions]:\n{digest}"
        );
    }

    /// Same triples, different input order: the digest must be byte-identical
    /// (every grouping structure is a `BTree*`, order-independent by
    /// construction).
    #[test]
    fn digest_is_deterministic_under_input_shuffle() {
        let mut shuffled = sample_triples();
        shuffled.reverse();
        shuffled.swap(0, 2);
        assert_eq!(build_nav_digest(&shuffled, &config()), EXPECTED);
    }

    /// A `surfaces_concept` token with no codebook match and no alias match
    /// renders `UNRESOLVED` rather than being dropped.
    #[test]
    fn unresolved_token_renders_unresolved() {
        let triples = vec![Triple::new(
            "app:Invoice",
            Predicate::SurfacesConcept,
            "mystery",
            Provenance::Authoritative,
        )];
        let digest = build_nav_digest(&triples, &config());
        assert!(digest.contains("Invoice ~ mystery -> UNRESOLVED"));
        assert!(digest.contains("concept bindings: 0 resolved, 1 unresolved"));
    }

    /// The lowering claim: a multi-level `part_of` chain whose every ancestor
    /// resolves to a classid lowers to a PURE radix-trie path (root-first
    /// `0x<ID>/0x<ID>/0x<ID>`), with no stored ordinal — the address is the
    /// walked rail. A leaf's `purpose`/`action` ride alongside.
    #[test]
    fn menu_quad_lowers_location_as_a_classid_radix_path() {
        let config = ExamConfig {
            codebook: vec![
                ("root_c".to_string(), 0x0900),
                ("mid_c".to_string(), 0x0910),
                ("leaf_c".to_string(), 0x0911),
            ],
            ..ExamConfig::default()
        };
        let triples = vec![
            Triple::new(
                "app:Root",
                Predicate::SurfacesConcept,
                "root_c",
                Provenance::Authoritative,
            ),
            Triple::new(
                "app:Mid",
                Predicate::SurfacesConcept,
                "mid_c",
                Provenance::Authoritative,
            ),
            Triple::new(
                "app:Leaf",
                Predicate::SurfacesConcept,
                "leaf_c",
                Provenance::Authoritative,
            ),
            Triple::new(
                "app:Mid",
                Predicate::PartOf,
                "app:Root",
                Provenance::Inferred,
            ),
            Triple::new(
                "app:Leaf",
                Predicate::PartOf,
                "app:Mid",
                Provenance::Inferred,
            ),
            Triple::new("app:Leaf", Predicate::Purpose, "form", Provenance::Inferred),
        ];
        let digest = build_nav_digest(&triples, &config);
        // Leaf's location is the full ancestor classid path, root-first.
        assert!(
            digest.contains(
                "Leaf  loc=0x0900/0x0910/0x0911  purpose=form  id=0x0911  action=navigate"
            ),
            "leaf must lower to the pure radix path 0x0900/0x0910/0x0911:\n{digest}"
        );
        // Root: parentless, has no out-edge here, so it is a leaf node with its
        // own single-element address.
        assert!(digest.contains("Root  loc=0x0900  purpose=-  id=0x0900  action=leaf"));
    }

    /// Codex P2: when a child carries MORE THAN ONE `part_of` fact, the chosen
    /// parent must be deterministic (smallest via `or_insert` over the sorted
    /// raw set), so reversing the input yields a byte-identical digest — the
    /// digest's shuffle-invariance must hold for the rail arm too.
    #[test]
    fn menu_quad_conflicting_part_of_is_order_independent() {
        let mut triples = vec![
            Triple::new(
                "app:Leaf",
                Predicate::PartOf,
                "app:Zparent",
                Provenance::Inferred,
            ),
            Triple::new(
                "app:Leaf",
                Predicate::PartOf,
                "app:Apparent",
                Provenance::Inferred,
            ),
        ];
        let forward = build_nav_digest(&triples, &config());
        triples.reverse();
        let reversed = build_nav_digest(&triples, &config());
        assert_eq!(
            forward, reversed,
            "conflicting part_of must be order-independent"
        );
        // Apparent (lexicographically smallest) is the canonical parent.
        assert!(
            forward.contains("Leaf  loc=Apparent/Leaf"),
            "smallest parent must win deterministically:\n{forward}"
        );
    }

    /// A misdeclared `part_of` cycle must terminate (cycle-guarded walk), not
    /// loop forever — the address is the bounded chain up to the first repeat.
    #[test]
    fn menu_quad_part_of_cycle_terminates() {
        let triples = vec![
            Triple::new("app:A", Predicate::PartOf, "app:B", Provenance::Inferred),
            Triple::new("app:B", Predicate::PartOf, "app:A", Provenance::Inferred),
        ];
        // Must return (not hang) and emit both nodes under [menu-quad].
        let digest = build_nav_digest(&triples, &config());
        assert!(digest.contains("[menu-quad]"));
        assert!(digest.contains("A  loc="));
        assert!(digest.contains("B  loc="));
    }

    /// A `docked_at` token with no matching `config.regions` row lands in
    /// `unmapped:<token>` rather than being dropped — isolated from the
    /// main fixture (which also exercises this arm inline via `btn_save`).
    #[test]
    fn unmapped_dock_token_lands_in_unmapped_region() {
        let triples = vec![Triple::new(
            "app:Invoice.mystery_ctrl",
            Predicate::DockedAt,
            "right",
            Provenance::Authoritative,
        )];
        let digest = build_nav_digest(&triples, &config());
        assert!(digest.contains("Invoice / unmapped:right: mystery_ctrl(-)"));
        assert!(digest.contains("regions: 1"));
    }
}
