//! Region-grammar plane — the frontend-agnostic layer between any
//! `ruff_*_spo` layout harvest and the shared `docked_at` / `tab_order` /
//! `opens_popup` / `contains_control` predicates (#76).
//!
//! # Why this module exists
//!
//! Three frontends produce region facts — `ruff_ruby_spo::menu_regions`
//! (Rails menu DSL), `ruff_python_spo::odoo_regions` (Odoo view arch), and
//! `ruff_csharp_spo` (`WinForms` `Designer`, ndjson) — and one consumer,
//! [`crate::build_nav_digest`], groups them into the six-region frame. Before
//! this module each frontend hand-formatted its subject IRI and the digest
//! hand-parsed it, so the subject grammar drifted (Rails/C# emitted a `.`
//! separator, Odoo emitted `::`), silently breaking the Odoo digest path.
//!
//! The fix is a **subject codec** ([`RegionSubject`]) plus one **shared fact
//! type** ([`RegionFact`]) and **one lift** ([`RegionFact::to_triples`]) that
//! every frontend calls. The subject convention now lives in exactly one
//! place, and the same [`RegionSubject::from_iri`] that the digest uses to
//! decode is the inverse of the [`RegionSubject::to_iri`] the lift uses to
//! encode — they cannot drift again.
//!
//! # The subject convention
//!
//! Canonical form is `"{screen}.{control}"`, where `screen` already carries
//! whatever namespace prefix the frontend uses (Rails `"{ns}:{menu}"`, Odoo
//! `"{rel}#{view_id}"`, C# `"{ns}:{class}"`). Decoding is
//! [`str::rsplit_once`] on `'.'`: the control is the tail after the **last**
//! dot. This is sound because no control on any of the three frontends
//! contains a dot in the common case — Rails symbols and C# member names
//! never do, and Odoo field/button names are Python identifiers. Crucially,
//! the `.xml` dot inside an Odoo screen (`widget_views.xml#view`) is *not* the
//! last dot (the control follows it), so `rsplit` still recovers
//! `(screen, control)` correctly.

use crate::triple::{Predicate, Provenance, Triple};

/// The canonical region-subject IRI codec — one encode/decode pair shared by
/// the region lift ([`RegionFact::to_triples`]) and the digest
/// ([`crate::build_nav_digest`]), so the subject grammar can never drift
/// between producer and consumer.
///
/// The borrowed `screen`/`control` already carry any namespace prefix the
/// producing frontend uses; see the module docs for the convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionSubject<'a> {
    /// Fully-qualified screen id (namespace already applied).
    pub screen: &'a str,
    /// Control id within the screen.
    pub control: &'a str,
}

impl<'a> RegionSubject<'a> {
    /// Encode to the canonical `"{screen}.{control}"` IRI.
    #[must_use]
    pub fn to_iri(&self) -> String {
        format!("{}.{}", self.screen, self.control)
    }

    /// Decode a canonical region-subject IRI back into `(screen, control)` by
    /// splitting on the **last** `.`. Returns `None` for a dot-less IRI (a
    /// malformed / control-less subject).
    ///
    /// The last-dot rule is what makes a dotted screen (Odoo's `.xml`)
    /// unambiguous — the control, which carries no dot, is always the final
    /// segment.
    #[must_use]
    pub fn from_iri(iri: &'a str) -> Option<(&'a str, &'a str)> {
        iri.rsplit_once('.')
    }
}

/// A resolved region-grammar fact — the frontend-agnostic unit each layout
/// harvest produces. Every optional field gates a predicate that already
/// exists in the closed vocabulary, so a frontend that does not produce a
/// given fact passes `None` and emits nothing (behaviour-preserving).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionFact {
    /// Fully-qualified screen id (namespace already applied — Rails
    /// `"{ns}:{menu}"`, Odoo `"{rel}#{view_id}"`). The subject is
    /// `"{screen}.{control}"`.
    pub screen: String,
    /// The control's identity within the screen.
    pub control: String,
    /// The raw innermost container token (the `region=` config maps it to a
    /// canonical region downstream; never hardcode the region name here).
    pub dock_token: String,
    /// Resolved 0-based sibling ordinal. `Option` so a harvester that fails to
    /// assign one surfaces as `None` (and a report counter) rather than a
    /// silent `0`.
    pub tab_order: Option<u32>,
    /// `Some(action)` for a popup-opening control (Odoo `<button
    /// type="action">`); `None` when the frontend has no clean popup signal
    /// (Rails: Primer/Angular, deferred).
    pub opens_popup: Option<String>,
    /// `Some(parent_control)` for a nesting edge (Rails `parent:`); `None`
    /// when nesting is not emitted (Odoo today).
    pub parent: Option<String>,
}

impl RegionFact {
    /// The canonical subject IRI for this fact (`"{screen}.{control}"`).
    #[must_use]
    pub fn subject(&self) -> String {
        RegionSubject {
            screen: &self.screen,
            control: &self.control,
        }
        .to_iri()
    }

    /// Lift this fact into its SPO triples, in the deterministic order
    /// `docked_at`, `[tab_order]`, `[opens_popup]`, `[contains_control]`. All
    /// at [`Provenance::Authoritative`].
    #[must_use]
    pub fn to_triples(&self) -> Vec<Triple> {
        let subject = self.subject();
        let mut out = vec![Triple::new(
            subject.clone(),
            Predicate::DockedAt,
            self.dock_token.clone(),
            Provenance::Authoritative,
        )];
        if let Some(order) = self.tab_order {
            out.push(Triple::new(
                subject.clone(),
                Predicate::TabOrder,
                order.to_string(),
                Provenance::Authoritative,
            ));
        }
        if let Some(action) = &self.opens_popup {
            out.push(Triple::new(
                subject.clone(),
                Predicate::OpensPopup,
                action.clone(),
                Provenance::Authoritative,
            ));
        }
        if let Some(parent) = &self.parent {
            let parent_subject = RegionSubject {
                screen: &self.screen,
                control: parent,
            }
            .to_iri();
            out.push(Triple::new(
                parent_subject,
                Predicate::ContainsControl,
                subject,
                Provenance::Authoritative,
            ));
        }
        out
    }
}

/// Lift every [`RegionFact`] into its SPO triples, concatenated in fact order.
#[must_use]
pub fn region_triples(facts: &[RegionFact]) -> Vec<Triple> {
    facts.iter().flat_map(RegionFact::to_triples).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The codec round-trips every producer's subject shape, including Odoo's
    /// dotted `.xml` screen (the last-dot rule keeps it unambiguous).
    #[test]
    fn subject_codec_round_trips_all_three_producer_shapes() {
        // Rails: ns-prefixed menu screen, dot separator.
        let rails = RegionSubject {
            screen: "openproject:top_menu",
            control: "projects",
        };
        assert_eq!(rails.to_iri(), "openproject:top_menu.projects");
        assert_eq!(
            RegionSubject::from_iri("openproject:top_menu.projects"),
            Some(("openproject:top_menu", "projects"))
        );

        // C#: ns-prefixed class screen, dot separator (single dot).
        let csharp = RegionSubject {
            screen: "csharp:uc_cipher_main",
            control: "btn_save",
        };
        assert_eq!(csharp.to_iri(), "csharp:uc_cipher_main.btn_save");
        assert_eq!(
            RegionSubject::from_iri("csharp:uc_cipher_main.btn_save"),
            Some(("csharp:uc_cipher_main", "btn_save"))
        );

        // Odoo: DOTTED screen (`.xml`) — the last dot is the control boundary.
        let odoo = RegionSubject {
            screen: "widget_views.xml#view_form",
            control: "partner_id",
        };
        assert_eq!(odoo.to_iri(), "widget_views.xml#view_form.partner_id");
        assert_eq!(
            RegionSubject::from_iri("widget_views.xml#view_form.partner_id"),
            Some(("widget_views.xml#view_form", "partner_id"))
        );
    }

    #[test]
    fn from_iri_none_on_dotless_subject() {
        assert_eq!(RegionSubject::from_iri("no_separator_here"), None);
    }

    /// A minimal fact emits exactly `docked_at` (+ `tab_order` when present),
    /// no `opens_popup` / `contains_control`.
    #[test]
    fn minimal_fact_emits_docked_at_and_tab_order_only() {
        let fact = RegionFact {
            screen: "openproject:top_menu".to_string(),
            control: "projects".to_string(),
            dock_token: "top_menu".to_string(),
            tab_order: Some(3),
            opens_popup: None,
            parent: None,
        };
        let triples = fact.to_triples();
        assert_eq!(triples.len(), 2, "{triples:?}");
        assert!(triples.iter().any(|t| {
            t.s == "openproject:top_menu.projects" && t.p == "docked_at" && t.o == "top_menu"
        }));
        assert!(triples.iter().any(|t| {
            t.s == "openproject:top_menu.projects" && t.p == "tab_order" && t.o == "3"
        }));
    }

    /// A parent fact emits a `contains_control` edge whose subject is the
    /// parent's subject and object is the child's subject.
    #[test]
    fn parent_fact_emits_contains_control_edge() {
        let fact = RegionFact {
            screen: "openproject:project_menu".to_string(),
            control: "settings_general".to_string(),
            dock_token: "project_menu".to_string(),
            tab_order: Some(0),
            opens_popup: None,
            parent: Some("settings".to_string()),
        };
        let triples = fact.to_triples();
        assert!(triples.iter().any(|t| {
            t.s == "openproject:project_menu.settings"
                && t.p == "contains_control"
                && t.o == "openproject:project_menu.settings_general"
        }));
    }

    /// An Odoo-shaped fact with a popup emits `opens_popup`; the dotted screen
    /// still yields a correct subject.
    #[test]
    fn odoo_popup_fact_emits_opens_popup_with_dotted_screen() {
        let fact = RegionFact {
            screen: "widget_views.xml#view_form".to_string(),
            control: "%(act_open_wizard)d".to_string(),
            dock_token: "header".to_string(),
            tab_order: Some(0),
            opens_popup: Some("%(act_open_wizard)d".to_string()),
            parent: None,
        };
        let triples = fact.to_triples();
        let subject = "widget_views.xml#view_form.%(act_open_wizard)d";
        assert!(
            triples
                .iter()
                .any(|t| t.s == subject && t.p == "opens_popup")
        );
        // And its subject round-trips back through the digest codec.
        assert_eq!(
            RegionSubject::from_iri(subject),
            Some(("widget_views.xml#view_form", "%(act_open_wizard)d"))
        );
    }

    /// Every emitted triple carries the Authoritative truth calibration.
    #[test]
    fn all_region_triples_are_authoritative() {
        let facts = [RegionFact {
            screen: "s".to_string(),
            control: "c".to_string(),
            dock_token: "header".to_string(),
            tab_order: Some(1),
            opens_popup: Some("act".to_string()),
            parent: Some("p".to_string()),
        }];
        let (f, c) = Provenance::Authoritative.truth();
        for t in region_triples(&facts) {
            assert_eq!((t.f, t.c), (f, c), "{t:?}");
        }
    }
}
