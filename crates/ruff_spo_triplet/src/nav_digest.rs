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
/// [klickwege]
/// <from> -> <to>
/// [views]
/// <screen> => <view>
/// [concepts]
/// <screen> ~ <token> -> 0x<ID>
/// <screen> ~ <token> -> UNRESOLVED
/// [screen surface]
/// <screen> controls=<c> handlers=<h>
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
#[must_use]
pub fn build_nav_digest(triples: &[Triple], config: &ExamConfig) -> String {
    let mut klickwege: BTreeSet<(String, String)> = BTreeSet::new();
    let mut views: BTreeSet<(String, String)> = BTreeSet::new();
    let mut concepts: BTreeSet<(String, String)> = BTreeSet::new();
    let mut screens: BTreeSet<String> = BTreeSet::new();
    let mut surface: BTreeMap<String, (usize, usize)> = BTreeMap::new();

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
            _ => {}
        }
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
            ..ExamConfig::default()
        }
    }

    /// Screens `Invoice` / `CipherPanel`: one `navigates_to` edge, one
    /// `selects_view` tab selection, one `surfaces_concept` token that
    /// resolves through the alias -> codebook chain, one `contains_control`
    /// and one `handles_event` fact on `CipherPanel`.
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
        ]
    }

    const EXPECTED: &str = "\
=== nav digest v1 ===
screens: 2
klickwege: 1
views: 1
concept bindings: 1 resolved, 0 unresolved
[klickwege]
Invoice -> CipherPanel
[views]
Invoice => tab_summary
[concepts]
CipherPanel ~ cipher -> 0x0C01
[screen surface]
CipherPanel controls=1 handlers=1
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
}
