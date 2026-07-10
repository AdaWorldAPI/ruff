//! Schema-surface classification — doctrine Phase 3 (config-as-schema).
//!
//! Phases 1-2 lift a method body to a [`crate::recipe::RecipeCentroid`] (a
//! *behavior* recipe) or leave it a hand-port. Phase 3 catches the methods
//! that never carried behavior to begin with: getters over a lookup /
//! option / template table — `get_combo_<concept>_<facet>`,
//! `get_option_<concept>_template`, `add_option_<concept>_template`. Each
//! one *names an enum/template space* (`enum_space CipherCombo`, `source
//! table combo`, `concept cipher`, `facet typ`) rather than doing anything.
//! That is config wearing a method's clothes, and it generalizes past this
//! one codebase: Rails `enum`, Django `choices=`, any lookup table read
//! through a getter method is the same shape.
//!
//! Classification here is deliberately upstream of action-lifting
//! ([`crate::recipe::classify`]): a schema surface must be pulled OUT of
//! the DO-arm candidate pool *before* recipe classification runs, or the
//! enum/template plumbing pollutes the capability surface with fake
//! "actions" that are really config reads. This ties directly into
//! param-enum fidelity — the walk_enums harvest (`ruff_cpp_spo`,
//! "Future-synergy-2") is what makes the *typed* enum/template space this
//! module only classifies the *surface* of; concept and facet residue
//! resolution (`["cipher", "typ"]` → the concrete `Cipher::typ` enum arm)
//! is [`crate::concept_split`]'s job, not this module's.
//!
//! # The shape
//!
//! A schema-surface method name is `<verb>_..._<surface>_...`: the first
//! token is a verb (`get`, `add`, ...), and some later token names the
//! surface convention (`combo`, `option`, `template`, ...). Everything
//! else in the name is concept+facet residue, left for
//! [`crate::concept_split::tokenize_method_name`]'s caller to resolve
//! against the alias table. Names that don't fit this shape (no verb
//! first, or no surface token anywhere) are ordinary domain actions or
//! slag — not schema surfaces — and classification returns `None`.

use crate::concept_split::tokenize_method_name;

/// Caller-supplied surface convention rows (data-as-config): surface token
/// -> surface kind. e.g. `("combo", SurfaceKind::EnumSource)`,
/// `("option", SurfaceKind::EnumSource)`,
/// `("template", SurfaceKind::TemplateSource)`.
#[derive(Debug, Clone, Default)]
pub struct SurfaceConvention {
    pub surfaces: Vec<(String, SurfaceKind)>,
}

/// What kind of schema surface a matched surface token names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceKind {
    /// A lookup/option table read (`get_combo_*`, `get_option_*`) — names
    /// an enum space.
    EnumSource,
    /// A template table read/write (`get_option_*_template`,
    /// `add_option_*_template`) — names a template space.
    TemplateSource,
    /// A sub-tab surface (UI-shaped config, not data-shaped). RESERVED:
    /// declared for the operator's surface-convention table (`patfile_sub`
    /// -> subtab) but not yet exercised by a shipped convention row.
    SubtabSurface,
    /// A grid/table surface (UI-shaped config, not data-shaped). The
    /// `dgv`-style grid-plumbing rows on the slag ledger are this
    /// variant's consumers (autosize / regenerate-line / select-line
    /// plumbing repeated per form class).
    GridSurface,
    /// A UI-localization pass (config-shaped, not data-shaped): the
    /// apply-language-strings-to-controls method every form class
    /// repeats (WinForms-style `Update<Lang>ToForm` families). The
    /// language table is SCHEMA (a localization space); the per-form
    /// method is plumbing over it — never a domain action.
    LocalizationSurface,
}

impl SurfaceKind {
    /// Parse a convention-config token (`surface=<token>:<kind>` rows in
    /// data-as-config files) into a kind. Tokens are the stable config
    /// vocabulary, deliberately decoupled from variant names.
    #[must_use]
    pub fn from_config_token(token: &str) -> Option<Self> {
        match token {
            "enum_source" => Some(Self::EnumSource),
            "template_source" => Some(Self::TemplateSource),
            "subtab" => Some(Self::SubtabSurface),
            "grid" => Some(Self::GridSurface),
            "localization" => Some(Self::LocalizationSurface),
            _ => None,
        }
    }
}

/// One classified schema surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaSurface {
    /// The surface kind (from the convention row that matched).
    pub kind: SurfaceKind,
    /// The surface token that matched (e.g. "combo").
    pub surface: String,
    /// Remaining tokens BEFORE alias resolution, minus verb + surface
    /// tokens: the concept+facet residue, lowercased, in name order
    /// (e.g. `["cipher", "typ"]`). Concept resolution is the
    /// `concept_split` pass's job — this module only classifies the
    /// surface.
    pub residue: Vec<String>,
}

/// Classify one method name against the surface convention.
///
/// The FIRST token must be a verb-looking token (member of `verbs`, the
/// same verb tokens the caller feeds `concept_split`), and SOME later
/// token must match a surface row; otherwise `None` (not a schema
/// surface — an ordinary domain action or slag).
///
/// When more than one surface token is present in the name (e.g. both
/// `option` and `template` in `add_option_widget_template`), the LAST
/// one in name order wins: the trailing table-kind token names the
/// actual storage (`… option … template` reads "a TEMPLATE of options"),
/// so `get_option_*_template` / `add_option_*_template` classify as
/// [`SurfaceKind::TemplateSource`], never as an enum source. Every
/// matched surface token is excluded from the residue — the residue is
/// the concept+facet material only.
#[must_use]
pub fn classify_surface(
    name: &str,
    verbs: &[String],
    conv: &SurfaceConvention,
) -> Option<SchemaSurface> {
    let tokens = tokenize_method_name(name);
    let first = tokens.first()?;

    let is_verb = verbs.iter().any(|v| v.eq_ignore_ascii_case(first));
    if !is_verb {
        return None;
    }

    let mut surface_indices: Vec<usize> = Vec::new();
    let mut matched: Option<(SurfaceKind, String)> = None;
    for (idx, tok) in tokens.iter().enumerate().skip(1) {
        if let Some((_, kind)) = conv
            .surfaces
            .iter()
            .find(|(surface_tok, _)| surface_tok.eq_ignore_ascii_case(tok))
        {
            surface_indices.push(idx);
            matched = Some((*kind, tok.clone()));
        }
    }

    let (kind, surface) = matched?;

    let residue = tokens
        .into_iter()
        .enumerate()
        .filter(|(idx, _)| *idx != 0 && !surface_indices.contains(idx))
        .map(|(_, tok)| tok)
        .collect();

    Some(SchemaSurface {
        kind,
        surface,
        residue,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verbs() -> Vec<String> {
        vec!["get".to_string(), "add".to_string()]
    }

    fn convention() -> SurfaceConvention {
        SurfaceConvention {
            surfaces: vec![
                ("combo".to_string(), SurfaceKind::EnumSource),
                ("option".to_string(), SurfaceKind::EnumSource),
                ("template".to_string(), SurfaceKind::TemplateSource),
            ],
        }
    }

    /// `get_combo_cipher_typ` — verb `get`, surface `combo` (EnumSource),
    /// residue `["cipher", "typ"]`.
    #[test]
    fn get_combo_names_enum_source_with_concept_facet_residue() {
        let result = classify_surface("get_combo_cipher_typ", &verbs(), &convention()).unwrap();
        assert_eq!(result.kind, SurfaceKind::EnumSource);
        assert_eq!(result.surface, "combo");
        assert_eq!(result.residue, vec!["cipher", "typ"]);
    }

    /// `add_option_widget_template` — BOTH `option` and `template` are
    /// present as surface tokens. The LAST match in name order wins: the
    /// trailing table-kind token names the actual storage ("a TEMPLATE
    /// of options"), so `add_option_*_template` is a TemplateSource —
    /// never an enum source (codex P2 on PR #72). Every matched surface
    /// token leaves the residue: only the concept material remains.
    #[test]
    fn last_matching_surface_token_in_name_order_wins() {
        let result =
            classify_surface("add_option_widget_template", &verbs(), &convention()).unwrap();
        assert_eq!(result.kind, SurfaceKind::TemplateSource);
        assert_eq!(result.surface, "template");
        assert_eq!(result.residue, vec!["widget"]);
    }

    /// `get_invoice` — verb present, but no surface token anywhere in the
    /// name: an ordinary domain action, not a schema surface.
    #[test]
    fn no_surface_token_is_not_a_schema_surface() {
        assert_eq!(
            classify_surface("get_invoice", &verbs(), &convention()),
            None
        );
    }

    /// `combo_cipher` — a surface token (`combo`) is present, but the
    /// first token isn't a verb: not a schema surface (and not any kind
    /// of recognizable action shape either).
    #[test]
    fn non_verb_first_token_is_not_a_schema_surface() {
        assert_eq!(
            classify_surface("combo_cipher", &verbs(), &convention()),
            None
        );
    }

    /// Case-insensitivity flows from the tokenizer (which lowercases) and
    /// from `eq_ignore_ascii_case` on both the verb and surface matches.
    #[test]
    fn classification_is_case_insensitive() {
        let result = classify_surface("Get_Combo_Cipher_Typ", &verbs(), &convention()).unwrap();
        assert_eq!(result.kind, SurfaceKind::EnumSource);
        assert_eq!(result.surface, "combo");
        assert_eq!(result.residue, vec!["cipher", "typ"]);
    }

    /// Grid plumbing (`autosize_grid_invoice_list`) classifies as a
    /// `GridSurface` once the convention row lands — the per-form
    /// autosize/select/regen-line families are UI config, not actions.
    #[test]
    fn grid_convention_row_classifies_grid_plumbing() {
        let verbs = vec!["autosize".to_string()];
        let conv = SurfaceConvention {
            surfaces: vec![("grid".to_string(), SurfaceKind::GridSurface)],
        };
        let result = classify_surface("autosize_grid_invoice_list", &verbs, &conv).unwrap();
        assert_eq!(result.kind, SurfaceKind::GridSurface);
        assert_eq!(result.surface, "grid");
        assert_eq!(result.residue, vec!["invoice", "list"]);
    }

    /// The per-form localization pass (`Update_Lang_To_Form` shape)
    /// classifies as a `LocalizationSurface`: the language table is
    /// schema, the repeated per-form applier is plumbing over it.
    #[test]
    fn localization_convention_row_classifies_language_pass() {
        let verbs = vec!["update".to_string()];
        let conv = SurfaceConvention {
            surfaces: vec![("lang".to_string(), SurfaceKind::LocalizationSurface)],
        };
        let result = classify_surface("Update_Lang_To_Form", &verbs, &conv).unwrap();
        assert_eq!(result.kind, SurfaceKind::LocalizationSurface);
        assert_eq!(result.surface, "lang");
        assert_eq!(result.residue, vec!["to", "form"]);
    }

    /// The config vocabulary round-trips every kind and rejects unknowns —
    /// `surface=<token>:<kind>` rows in data-as-config files depend on it.
    #[test]
    fn config_tokens_parse_to_kinds() {
        assert_eq!(
            SurfaceKind::from_config_token("enum_source"),
            Some(SurfaceKind::EnumSource)
        );
        assert_eq!(
            SurfaceKind::from_config_token("template_source"),
            Some(SurfaceKind::TemplateSource)
        );
        assert_eq!(
            SurfaceKind::from_config_token("subtab"),
            Some(SurfaceKind::SubtabSurface)
        );
        assert_eq!(
            SurfaceKind::from_config_token("grid"),
            Some(SurfaceKind::GridSurface)
        );
        assert_eq!(
            SurfaceKind::from_config_token("localization"),
            Some(SurfaceKind::LocalizationSurface)
        );
        assert_eq!(SurfaceKind::from_config_token("bogus"), None);
    }
}
