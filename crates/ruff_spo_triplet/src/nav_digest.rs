//! The Klickwege structure-parity oracle (transcode doctrine: "MySQL = value
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
use crate::triple::Triple;

/// Strip a triple's namespace prefix (`"ns:"`), returning the local part.
/// An IRI with no `:` passes through unchanged.
fn strip_ns(iri: &str) -> &str {
    iri.split_once(':').map_or(iri, |(_, tail)| tail)
}

/// The screen segment of a namespace-qualified, possibly control-qualified
/// local IRI: the namespace-stripped segment up to the first `.`, e.g.
/// `"csharp:uc_cipher_main.btn_save"` -> `"uc_cipher_main"`.
fn screen_of(iri: &str) -> &str {
    let local = strip_ns(iri);
    local.split_once('.').map_or(local, |(head, _)| head)
}

/// The control segment of a namespace-qualified, screen-qualified local
/// IRI: the tail after the first `.`, e.g.
/// `"csharp:uc_cipher_main.btn_save"` -> `"btn_save"`. Sibling of
/// [`screen_of`] (the head half of the same split); when there is no `.`
/// the whole namespace-stripped string is returned (degenerate case, not
/// expected for `docked_at` / `tab_order` / `opens_popup` subjects).
fn control_of(iri: &str) -> &str {
    let local = strip_ns(iri);
    local.split_once('.').map_or(local, |(_, tail)| tail)
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
/// === nav digest v1 ===
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

    // Group docked controls (+ popup targets) by resolved region.
    let mut region_entries: BTreeMap<(String, String), Vec<(String, Option<u32>)>> =
        BTreeMap::new();
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
    let _ = writeln!(out, "=== nav digest v1 ===");
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
        ]
    }

    const EXPECTED: &str = "\
=== nav digest v1 ===
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
";

    #[test]
    fn digest_matches_the_exact_expected_string() {
        assert_eq!(build_nav_digest(&sample_triples(), &config()), EXPECTED);
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
