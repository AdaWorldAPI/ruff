//! Exam-config parsing — the corpus-agnostic, data-as-config directive
//! language shared by the furnace exam (`rekey_exam` example) and the
//! Klickwege structure-parity oracle (`nav_digest` example).
//!
//! One directive per line, `key=value`; `#` starts a trailing comment;
//! blank lines and unrecognised keys are silently ignored. No corpus
//! tokens live in this module — the corpus, the naming convention, the
//! codebook rows, and the expected-concept list all arrive as runtime
//! config, read fresh per invocation. See [`parse`] for the full
//! directive vocabulary.

use crate::concept_split::ConceptConvention;
use crate::structured_names::NameGrammar;
use crate::surface_schema::{SurfaceConvention, SurfaceKind};

/// A parsed exam-config file: everything a corpus-agnostic oracle needs to
/// re-derive domain concepts from a harvest — the naming convention, the
/// schema-surface convention, the structured-name grammar, the codebook
/// oracle rows, and the list of concepts the exam expects to bind.
#[derive(Debug, Clone, Default)]
pub struct ExamConfig {
    /// The verb/scope/alias naming convention (see
    /// [`crate::concept_split::ConceptConvention`]).
    pub convention: ConceptConvention,
    /// The schema-surface convention (see
    /// [`crate::surface_schema::SurfaceConvention`]).
    pub surfaces: SurfaceConvention,
    /// The structured-name grammar (see
    /// [`crate::structured_names::NameGrammar`]).
    pub grammar: NameGrammar,
    /// `(concept name, codebook id)` oracle rows.
    pub codebook: Vec<(String, u16)>,
    /// Concepts that MUST bind for the exam to pass.
    pub expect: Vec<String>,
}

/// Parse an exam-config file.
///
/// Directive vocabulary (one per line; `#` starts a trailing comment):
///
/// ```text
/// verb=add:create        # method-name verb token -> canonical verb
/// scope=pf               # scope token stripped after the verb
/// alias=ciphers:cipher_key     # residue -> canonical concept
/// codebook=cipher_key:0x0B01   # concept -> classid (the oracle rows)
/// expect=cipher_key      # concept that MUST bind for the exam to pass
/// surface=grid:grid      # surface token -> kind (config-as-schema plane;
///                        # kinds: enum_source / template_source / subtab /
///                        # grid / localization). Methods matching a surface
///                        # row are classified OUT of the concept plane
///                        # before residue accounting (doctrine Phase 3).
/// grammar_strip=mod      # structured-name grammar (doctrine Phase 5):
/// grammar_marker=f       # leading NOISE tokens to strip / the numbered-
/// grammar_tier=form      # path marker / tier names outermost-first.
/// grammar_tier=section   # Residues that parse land on the PROTOCOL plane
///                        # (part_of tree nodes), not the unbound ledger.
///                        # The plane arms ONLY with a grammar_marker —
///                        # marker-less mode would silently eat un-aliased
///                        # `<concept>_<digit>` residues (see the armed
///                        # gate in `rekey_exam`'s `main`).
/// ```
///
/// A malformed `codebook=` row (non-hex id, or no `:` separator) is
/// silently dropped — same "unrecognised is ignored" discipline as an
/// unknown key. An `surface=` row whose kind token doesn't match
/// [`SurfaceKind::from_config_token`] is dropped the same way.
#[must_use]
pub fn parse(text: &str) -> ExamConfig {
    let mut convention = ConceptConvention::default();
    let mut surfaces = SurfaceConvention::default();
    let mut grammar = NameGrammar::default();
    let mut codebook = Vec::new();
    let mut expect = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "verb" => {
                if let Some((tok, canon)) = value.split_once(':') {
                    convention
                        .verbs
                        .push((tok.trim().to_string(), canon.trim().to_string()));
                }
            }
            "scope" => convention.scopes.push(value.trim().to_string()),
            "alias" => {
                if let Some((from, to)) = value.split_once(':') {
                    convention
                        .concept_aliases
                        .push((from.trim().to_string(), to.trim().to_string()));
                }
            }
            "codebook" => {
                if let Some((name, id)) = value.split_once(':') {
                    let id = id.trim().trim_start_matches("0x");
                    if let Ok(id) = u16::from_str_radix(id, 16) {
                        codebook.push((name.trim().to_string(), id));
                    }
                }
            }
            "expect" => expect.push(value.trim().to_string()),
            "surface" => {
                if let Some((tok, kind)) = value.split_once(':')
                    && let Some(kind) = SurfaceKind::from_config_token(kind.trim())
                {
                    surfaces.surfaces.push((tok.trim().to_string(), kind));
                }
            }
            "grammar_strip" => grammar.strip_prefixes.push(value.trim().to_string()),
            "grammar_marker" => grammar.marker = value.trim().to_string(),
            "grammar_tier" => grammar.tier_names.push(value.trim().to_string()),
            _ => {}
        }
    }
    ExamConfig {
        convention,
        surfaces,
        grammar,
        codebook,
        expect,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verb_directive_parses() {
        let cfg = parse("verb=add:create\n");
        assert_eq!(
            cfg.convention.verbs,
            vec![("add".to_string(), "create".to_string())]
        );
    }

    #[test]
    fn scope_directive_parses() {
        let cfg = parse("scope=combo\n");
        assert_eq!(cfg.convention.scopes, vec!["combo".to_string()]);
    }

    #[test]
    fn alias_directive_parses() {
        let cfg = parse("alias=ciphers:cipher_key\n");
        assert_eq!(
            cfg.convention.concept_aliases,
            vec![("ciphers".to_string(), "cipher_key".to_string())]
        );
    }

    #[test]
    fn codebook_directive_parses_hex_id() {
        let cfg = parse("codebook=cipher_key:0x0B01\n");
        assert_eq!(cfg.codebook, vec![("cipher_key".to_string(), 0x0B01)]);
    }

    #[test]
    fn expect_directive_parses() {
        let cfg = parse("expect=cipher_key\n");
        assert_eq!(cfg.expect, vec!["cipher_key".to_string()]);
    }

    #[test]
    fn surface_directive_parses_known_kind() {
        let cfg = parse("surface=grid:grid\n");
        assert_eq!(
            cfg.surfaces.surfaces,
            vec![("grid".to_string(), SurfaceKind::GridSurface)]
        );
    }

    #[test]
    fn surface_directive_with_unknown_kind_is_dropped() {
        let cfg = parse("surface=grid:bogus\n");
        assert!(cfg.surfaces.surfaces.is_empty());
    }

    #[test]
    fn grammar_directives_parse() {
        let cfg =
            parse("grammar_strip=mod\ngrammar_marker=f\ngrammar_tier=form\ngrammar_tier=section\n");
        assert_eq!(cfg.grammar.strip_prefixes, vec!["mod".to_string()]);
        assert_eq!(cfg.grammar.marker, "f");
        assert_eq!(
            cfg.grammar.tier_names,
            vec!["form".to_string(), "section".to_string()]
        );
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let cfg = parse("bogus=whatever\nverb=add:create\n");
        assert_eq!(cfg.convention.verbs.len(), 1);
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let cfg = parse("# a full-line comment\n\nverb=add:create   # trailing comment\n\n");
        assert_eq!(
            cfg.convention.verbs,
            vec![("add".to_string(), "create".to_string())]
        );
    }

    #[test]
    fn bad_hex_codebook_row_is_skipped() {
        let cfg = parse("codebook=cipher_key:not_hex\ncodebook=widget:0x10\n");
        assert_eq!(cfg.codebook, vec![("widget".to_string(), 0x10)]);
    }
}
