//! The menu-QUAD harvest engine — the reusable core every frontend configures.
//!
//! A menu node harvests as a `(identity, location, purpose, action)` quad (see
//! `.claude/knowledge/menu-quad-rail-port.md` in the consumer repos). Two of the
//! four axes differ only in *where the signal lives*, so they share one engine
//! parameterised by a per-frontend config:
//!
//! - **purpose** — the usability role, classified from a frontend's signal
//!   tokens (C# control-type names / Rails REST verb / Odoo view tags) via the
//!   SAME [`classify_purpose`] over a per-frontend [`PurposeRule`] table.
//! - **location** — the `part_of` menu-tree rail; declared (Rails `parent:`,
//!   Odoo `<menuitem parent=>`) or inferred (C# first-opener). The difference is
//!   only a [`Provenance`] tier, carried per-emission — not a new type.
//!
//! `identity` is [`Predicate::SurfacesConcept`] (→ classid) and `action` is
//! [`Predicate::NavigatesTo`] / [`Predicate::OpensPopup`]; those resolve via the
//! existing nav/routes arms and are referenced, not re-emitted, here.
//!
//! **Subject grammar:** a menu node's subject is the BARE `{ns}:{name}` IRI —
//! identical to the shipped `navigates_to` / `part_of` / `surfaces_concept`
//! grammar (C# `csharp:OrderScreen`, Rails `navigation.rs` `{ns}:{name}`). It is
//! NOT the region plane's `{screen}.{control}` [`crate::RegionSubject`] — that
//! codec is control-granular *within* a menu screen, a different plane. Walking
//! the `part_of` rail of bare node IRIs yields the radix-trie menu address
//! (OGAR-side projection); no position ordinal is stored.

use crate::triple::{Predicate, Provenance, Triple};

/// The usability role of a surface — the `purpose` axis' closed vocab, matching
/// [`Predicate::Purpose`]'s documented roles. Frontend-agnostic: every arm
/// classifies INTO this via [`classify_purpose`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PurposeRole {
    List,
    Detail,
    Form,
    Chart,
    Settings,
    Action,
    Dialog,
}

impl PurposeRole {
    /// The on-the-wire role string (the `purpose` triple's object).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Detail => "detail",
            Self::Form => "form",
            Self::Chart => "chart",
            Self::Settings => "settings",
            Self::Action => "action",
            Self::Dialog => "dialog",
        }
    }

    /// Every role, so vocab-validity is machine-derived (mirrors
    /// [`Predicate::ALL`]) rather than a hand-maintained list.
    pub const ALL: [PurposeRole; 7] = [
        Self::List,
        Self::Detail,
        Self::Form,
        Self::Chart,
        Self::Settings,
        Self::Action,
        Self::Dialog,
    ];

    /// Whether `s` is a valid role string — the conformance check any frontend
    /// (including the out-of-Rust C# arm) must satisfy.
    #[must_use]
    pub fn is_valid(s: &str) -> bool {
        Self::ALL.iter().any(|r| r.as_str() == s)
    }
}

/// One priority-ordered purpose-classification rule: a token *hits* if it
/// CONTAINS any needle; the rule fires when at least `min_hits` tokens hit.
///
/// `min_hits: 1` is existential (a single hit) — the case for Rails (one REST
/// verb) and Odoo (any view tag), and for C#'s chart/list/action rules.
/// `min_hits: 2` is a composition threshold — C#'s `form` rule fires only for
/// ≥2 input controls. The count model is a strict superset of substring-only:
/// with every rule at `min_hits: 1` it is byte-identical to "any needle hits".
#[derive(Debug, Clone, Copy)]
pub struct PurposeRule {
    pub needles: &'static [&'static str],
    pub role: PurposeRole,
    pub min_hits: usize,
}

/// Classify a surface's `purpose` from its signal `tokens` against a
/// per-frontend `rules` table (tried in order; first rule reaching its
/// `min_hits` wins), falling back to `fallback`. The SAME engine for every
/// frontend — only the table and fallback are config.
#[must_use]
pub fn classify_purpose(
    tokens: &[&str],
    rules: &[PurposeRule],
    fallback: PurposeRole,
) -> PurposeRole {
    for rule in rules {
        let hits = tokens
            .iter()
            .filter(|t| rule.needles.iter().any(|n| t.contains(n)))
            .count();
        if hits >= rule.min_hits {
            return rule.role;
        }
    }
    fallback
}

/// One resolved menu-node quad. Frontend-agnostic: each arm builds these from
/// its own signals and the shared [`Self::to_triples`] emits the facts with the
/// bare-node subject grammar. `action` edges (`navigates_to` / `opens_popup`)
/// stay with the nav/routes arms — the quad references them, does not re-emit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuQuad {
    /// The node's BARE IRI (`{ns}:{name}`) — the subject of every emitted fact.
    pub node: String,
    /// `part_of` object: the canonical menu-tree parent's bare IRI. `None` for a
    /// root node (`part_of` the menu itself, not emitted).
    pub parent: Option<String>,
    /// The `part_of` provenance tier: [`Provenance::Authoritative`] when the
    /// parent is DECLARED (Rails `parent:`, Odoo `<menuitem parent=>`),
    /// [`Provenance::Inferred`] when INFERRED (C# first-opener).
    pub part_of_tier: Provenance,
    /// The classified usability role.
    pub purpose: PurposeRole,
    /// `surfaces_concept` object (→ classid), when the identity is bound at
    /// harvest time. `None` leaves identity to the concept-binding arm.
    pub identity_concept: Option<String>,
}

impl MenuQuad {
    /// Emit the quad's facts with the bare-node subject: `part_of` (when a
    /// parent exists, at `part_of_tier`), `purpose` (Inferred — a heuristic over
    /// the signal), and `surfaces_concept` (when identity is bound).
    #[must_use]
    pub fn to_triples(&self) -> Vec<Triple> {
        let mut out = Vec::new();
        if let Some(parent) = &self.parent {
            out.push(Triple::new(
                self.node.clone(),
                Predicate::PartOf,
                parent.clone(),
                self.part_of_tier,
            ));
        }
        out.push(Triple::new(
            self.node.clone(),
            Predicate::Purpose,
            self.purpose.as_str(),
            Provenance::Inferred,
        ));
        if let Some(concept) = &self.identity_concept {
            out.push(Triple::new(
                self.node.clone(),
                Predicate::SurfacesConcept,
                concept.clone(),
                Provenance::Authoritative,
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `min_hits: 1` reduces the count engine to plain existential match — the
    /// Rails/Odoo case. First matching rule wins.
    #[test]
    fn existential_rules_first_match_wins() {
        let rules = &[
            PurposeRule {
                needles: &["index"],
                role: PurposeRole::List,
                min_hits: 1,
            },
            PurposeRule {
                needles: &["show"],
                role: PurposeRole::Detail,
                min_hits: 1,
            },
            PurposeRule {
                needles: &["new", "edit"],
                role: PurposeRole::Form,
                min_hits: 1,
            },
        ];
        assert_eq!(
            classify_purpose(&["index"], rules, PurposeRole::Action),
            PurposeRole::List
        );
        assert_eq!(
            classify_purpose(&["edit"], rules, PurposeRole::Action),
            PurposeRole::Form
        );
        // No rule hits -> fallback.
        assert_eq!(
            classify_purpose(&["board"], rules, PurposeRole::Action),
            PurposeRole::Action
        );
    }

    /// C# engine-MODEL parity: C#'s `ClassifyPurpose` (harvester/Program.cs)
    /// transcribes into a `PurposeRule[min_hits]` table, and the shared engine
    /// reproduces its outputs EXACTLY — including the load-bearing count rule
    /// (`form` fires at ≥2 inputs, NOT at 1). This proves the C# arm is genuinely
    /// "a config over the same engine", not merely emitting a compatible vocab.
    #[test]
    fn csharp_classifier_is_expressible_as_config_over_the_shared_engine() {
        // The C# table, in C# priority order (Program.cs:384-410).
        const CSHARP_PURPOSE: &[PurposeRule] = &[
            PurposeRule {
                needles: &["Chart"],
                role: PurposeRole::Chart,
                min_hits: 1,
            },
            PurposeRule {
                needles: &["DataGridView", "GridControl", "GridView", "ListView"],
                role: PurposeRole::List,
                min_hits: 1,
            },
            PurposeRule {
                needles: &[
                    "TextBox",
                    "ComboBox",
                    "DateTimePicker",
                    "CheckBox",
                    "NumericUpDown",
                    "RadioButton",
                    "MaskedTextBox",
                    "RichTextBox",
                ],
                role: PurposeRole::Form,
                min_hits: 2, // the count rule
            },
            PurposeRule {
                needles: &["Button", "LinkLabel"],
                role: PurposeRole::Action,
                min_hits: 1,
            },
        ];
        let cls = |t: &[&str]| classify_purpose(t, CSHARP_PURPOSE, PurposeRole::Detail);

        // chart > grid/list > multi-input form > action > detail (Program.cs).
        assert_eq!(
            cls(&["System.Windows.Forms.Chart", "TextBox"]),
            PurposeRole::Chart
        );
        assert_eq!(
            cls(&["System.Windows.Forms.DataGridView"]),
            PurposeRole::List
        );
        assert_eq!(cls(&["TextBox", "ComboBox"]), PurposeRole::Form); // 2 inputs
        assert_eq!(cls(&["Button"]), PurposeRole::Action);
        assert_eq!(cls(&["Label"]), PurposeRole::Detail); // fallback

        // THE count rule: a SINGLE input must NOT be `form` (existential would
        // regress here). C# with one TextBox and no button -> detail.
        assert_eq!(cls(&["TextBox"]), PurposeRole::Detail);
        // one input + a button -> action (button rule reached before fallback).
        assert_eq!(cls(&["TextBox", "Button"]), PurposeRole::Action);
    }

    #[test]
    fn purpose_role_vocab_is_machine_derived() {
        for r in PurposeRole::ALL {
            assert!(PurposeRole::is_valid(r.as_str()));
        }
        assert!(!PurposeRole::is_valid("not_a_role"));
        assert_eq!(PurposeRole::ALL.len(), 7);
    }

    /// A declared-parent quad emits `part_of` (Authoritative) + `purpose`
    /// (Inferred) with the bare-node subject — same grammar as `navigates_to`.
    #[test]
    fn declared_quad_emits_bare_part_of_and_purpose() {
        let quad = MenuQuad {
            node: "openproject:work_packages".to_string(),
            parent: Some("openproject:modules".to_string()),
            part_of_tier: Provenance::Authoritative,
            purpose: PurposeRole::List,
            identity_concept: None,
        };
        let triples = quad.to_triples();
        assert_eq!(triples.len(), 2, "{triples:?}");
        let part_of = triples.iter().find(|t| t.p == "part_of").unwrap();
        assert_eq!(part_of.s, "openproject:work_packages"); // bare, no dot
        assert_eq!(part_of.o, "openproject:modules");
        let (af, ac) = Provenance::Authoritative.truth();
        assert_eq!((part_of.f, part_of.c), (af, ac)); // declared -> Authoritative
        let purpose = triples.iter().find(|t| t.p == "purpose").unwrap();
        assert_eq!(purpose.o, "list");
    }

    /// A root node (no parent) emits `purpose` only — no `part_of`.
    #[test]
    fn root_quad_emits_purpose_only() {
        let quad = MenuQuad {
            node: "openproject:top_menu_root".to_string(),
            parent: None,
            part_of_tier: Provenance::Authoritative,
            purpose: PurposeRole::Settings,
            identity_concept: None,
        };
        let triples = quad.to_triples();
        assert_eq!(triples.len(), 1);
        assert_eq!(triples[0].p, "purpose");
    }
}
