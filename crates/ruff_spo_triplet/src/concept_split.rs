//! Concept re-keying — Phase 1 of the God-object transcode doctrine
//! ("fix CONCEPT = CLASS").
//!
//! A God-object DAL collapses every domain concept onto ONE class: a single
//! `DataAccessLayer`/`Repository`/`Manager` type carrying hundreds of
//! `verb_concept`-shaped methods (`Add_Invoice`, `add_iv_cipher`,
//! `get_combo_cipher_typ`, `del_AllCipherValues_StaticModule`, …). The
//! class boundary the source AST hands us is useless — it names "the DAL",
//! not "the domain". Before any of the OGAR lift machinery
//! ([`crate::classify`], [`crate::check_model_graph`]) can run per-concept,
//! the flat method list has to be **re-keyed**: each method inferred back to
//! the concept it actually belongs to, from its own name.
//!
//! This module is that re-keying step, and only that step — it does not
//! parse bodies, does not touch [`crate::ir::Function`]'s fact-sets, and
//! does not decide recipes. It runs BEFORE [`crate::classify`] in the
//! pipeline: re-key first, correlate second.
//!
//! # Data-as-config, not corpus tokens
//!
//! This module ships **zero domain vocabulary**. The verb set (`add` →
//! create, `get` → read, …), the scope-qualifier set (`pf`, `combo`, `all`,
//! …), and the concept-alias table are all supplied by the caller as a
//! [`ConceptConvention`] built from the specific corpus being transcoded.
//! The same ladder that splits one bespoke DAL's `Add_Invoice` splits an
//! invoice DAL's `Add_Invoice` — the convention is the only thing that
//! differs between corpora, never this module's code.
//!
//! # The slag doctrine
//!
//! Not every method splits. A name with no configured verb prefix
//! (`InitializeComponent`), or one that reduces to nothing after the verb
//! and its scope qualifiers are stripped (`Add_`), cannot be re-keyed under
//! the CURRENT convention. Those are captured as [`ResidualMethod`] rows —
//! the slag ledger — rather than silently dropped. **The residual is not
//! waste: it is the empirical boundary of the current convention.** A
//! recurring residual reason across many methods names the next convention
//! fact to add (a missing verb, a missing scope token, a missing alias),
//! the same way `.claude/knowledge/fuzzy-recipe-codebook.md` §5 treats an
//! unclassified fact-set as naming the next centroid rather than a failure.
//!
//! # Boundary-4 — concept output is `concept_key`-fixed-point
//!
//! [`SplitName::concept`] is always emitted lowercase, `_`-joined —
//! deliberately the same `snake_case` shape [`crate::codebook::concept_key`]
//! normalises class names into. Running `concept_key` on a [`SplitName`]
//! concept is therefore a no-op (see `concept_key`'s own "already `snake_case`
//! passes through unchanged" guarantee), so a re-keyed method's inferred
//! concept and a harvested class name resolve through
//! [`crate::check_model_graph`] identically, with no second normalisation
//! step at the seam.

use crate::ir::Model;

/// Lowercase, `_`/case-boundary tokens of a method name.
///
/// Splits on `_` AND on camel/Pascal-case boundaries (the same case-fold
/// discipline [`crate::codebook::concept_key`] uses on class names — a run
/// of uppercase letters starting a new word is folded to lowercase and, if
/// the previous character was lowercase or a digit, starts a new token).
/// Empty tokens (from leading/trailing/doubled `_`) are dropped.
///
/// ```text
/// "del_AllCipherValues_StaticModule" -> ["del","all","cipher","values","static","module"]
/// "Add_Invoice"                     -> ["add","invoice"]
/// ```
#[must_use]
pub fn tokenize_method_name(name: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut prev_lower_or_digit = false;
    for ch in name.chars() {
        if ch == '_' {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            prev_lower_or_digit = false;
            continue;
        }
        if ch.is_uppercase() {
            if prev_lower_or_digit && !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            current.extend(ch.to_lowercase());
            prev_lower_or_digit = false;
        } else {
            current.push(ch);
            prev_lower_or_digit = ch.is_lowercase() || ch.is_ascii_digit();
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// A caller-supplied naming convention — the only corpus-specific input
/// this module accepts. All matching is case-insensitive against the
/// lowercase tokens [`tokenize_method_name`] produces.
#[derive(Debug, Clone, Default)]
pub struct ConceptConvention {
    /// `(verb token, canonical verb)` pairs, e.g. `("add", "create")`,
    /// `("get", "read")`, `("del", "delete")`, `("update", "update")`,
    /// `("set", "update")`, `("show", "navigate")`, `("list", "list")`.
    /// The FIRST token of a method name must match one of these or the
    /// name does not split at all (a [`ResidualReason::NoVerbPrefix`]).
    pub verbs: Vec<(String, String)>,
    /// Scope-qualifier tokens stripped immediately after the verb — they
    /// qualify the concept (which record, which subset) but never NAME it,
    /// e.g. `"pf"`, `"pat"`, `"praxis"`, `"list"`, `"combo"`, `"option"`,
    /// `"all"`. Stripped greedily from the front of what remains after the
    /// verb, stopping at the first token that isn't a configured scope.
    pub scopes: Vec<String>,
    /// `(residue, canonical concept)` aliases. Checked LONGEST-residue-first:
    /// the remaining tokens concatenated with no separator are tried as one
    /// candidate key first (e.g. `("ciphervalues", "cipher_key")`), then
    /// each individual remaining token is tried on its own (e.g.
    /// `("ciphers", "cipher_key")`, `("iv", "cipher_key")`). First
    /// match wins; no match falls back to the raw, `_`-joined residue.
    pub concept_aliases: Vec<(String, String)>,
}

/// One split method name — the output of [`split_method_name`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitName {
    /// The canonical verb, from [`ConceptConvention::verbs`].
    pub verb: String,
    /// Scope tokens stripped after the verb, in order.
    pub scopes: Vec<String>,
    /// The inferred concept: alias-mapped if any [`ConceptConvention::concept_aliases`]
    /// entry matched, else the `_`-joined residue tokens (already lowercase
    /// `snake_case` — see the module docs' Boundary-4 note).
    pub concept: String,
    /// `true` when [`Self::concept`] came from an alias row, `false` when it
    /// is the raw, unmapped residue.
    pub aliased: bool,
}

/// Why a method name refused to split — one row of the slag ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResidualReason {
    /// The first token isn't a configured [`ConceptConvention::verbs`] key
    /// (or the name tokenized to nothing at all).
    NoVerbPrefix,
    /// A verb matched, but nothing remained once the verb and any leading
    /// [`ConceptConvention::scopes`] tokens were stripped.
    EmptyResidue,
}

/// One method the current convention could not re-key.
///
/// See the module docs' § "The slag doctrine" — this is not discarded
/// waste, it is the recorded, named boundary of [`ConceptConvention`] as it
/// stands today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidualMethod {
    /// The method name exactly as it appeared on the source [`Model`].
    pub function: String,
    /// Why it didn't split.
    pub reason: ResidualReason,
}

/// Split one method name against a [`ConceptConvention`].
///
/// `None` when the first token is not a configured verb, OR no residue
/// tokens remain after verb+scope stripping — both cases are slag; a
/// caller re-keying a whole [`Model`] should route them into
/// [`ResidualMethod`] rather than discard the method. See [`rekey_model`],
/// which does exactly that.
#[must_use]
pub fn split_method_name(name: &str, conv: &ConceptConvention) -> Option<SplitName> {
    try_split_method_name(name, conv).ok()
}

/// The re-keying outcome for one [`Model`]: every method that split, paired
/// with the concept it was keyed to, and every method that didn't — the
/// slag ledger.
#[derive(Debug, Clone, Default)]
pub struct RekeyOutcome {
    /// `(method name, split result)` for every function/helper that split.
    pub keyed: Vec<(String, SplitName)>,
    /// The slag ledger — every function/helper the convention could not
    /// re-key, with why.
    pub residuals: Vec<ResidualMethod>,
}

/// Re-key every method on a [`Model`] — both its routable `functions` and
/// its non-routable `helpers` (God-object DALs mix public CRUD entry points
/// and private plumbing under the one class; both carry a `verb_concept`
/// name and both are worth re-keying) — by inferred concept.
#[must_use]
pub fn rekey_model(model: &Model, conv: &ConceptConvention) -> RekeyOutcome {
    let mut outcome = RekeyOutcome::default();
    for function in model.functions.iter().chain(model.helpers.iter()) {
        match try_split_method_name(&function.name, conv) {
            Ok(split) => outcome.keyed.push((function.name.clone(), split)),
            Err(reason) => outcome.residuals.push(ResidualMethod {
                function: function.name.clone(),
                reason,
            }),
        }
    }
    outcome
}

/// The single source of truth behind both [`split_method_name`] (which
/// discards the failure reason) and [`rekey_model`] (which needs it).
fn try_split_method_name(
    name: &str,
    conv: &ConceptConvention,
) -> Result<SplitName, ResidualReason> {
    let tokens = tokenize_method_name(name);
    let mut iter = tokens.iter();
    let first = iter.next().ok_or(ResidualReason::NoVerbPrefix)?;
    let verb = conv
        .verbs
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(first))
        .map(|(_, canonical)| canonical.clone())
        .ok_or(ResidualReason::NoVerbPrefix)?;

    let rest: Vec<String> = iter.cloned().collect();
    let mut scopes = Vec::new();
    let mut idx = 0;
    while idx < rest.len()
        && conv
            .scopes
            .iter()
            .any(|scope| scope.eq_ignore_ascii_case(&rest[idx]))
    {
        scopes.push(rest[idx].clone());
        idx += 1;
    }
    let remaining = &rest[idx..];
    if remaining.is_empty() {
        return Err(ResidualReason::EmptyResidue);
    }

    let (concept, aliased) = resolve_concept(remaining, conv);
    Ok(SplitName {
        verb,
        scopes,
        concept,
        aliased,
    })
}

/// Resolve the residue tokens to a concept: longest (full-concatenation)
/// alias candidate first, then each individual token, then the raw
/// `_`-joined fallback. See [`ConceptConvention::concept_aliases`].
fn resolve_concept(remaining: &[String], conv: &ConceptConvention) -> (String, bool) {
    // The alias VALUE is folded through `concept_key` so the documented
    // fixed-point invariant holds on BOTH branches — a convention row
    // carrying a PascalCase value cannot smuggle a case-divergent concept
    // past the Boundary-4 seam (the raw-residue branch is fixed-point by
    // construction: the tokenizer lowercases).
    let full_key: String = remaining.concat();
    if let Some((_, concept)) = conv
        .concept_aliases
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(&full_key))
    {
        return (crate::codebook::concept_key(concept), true);
    }
    for token in remaining {
        if let Some((_, concept)) = conv
            .concept_aliases
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(token))
        {
            return (crate::codebook::concept_key(concept), true);
        }
    }
    (remaining.join("_"), false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Function;

    /// Boundary-4 on the ALIAS branch: a convention row whose VALUE is
    /// `PascalCase` cannot smuggle a case-divergent concept past the seam —
    /// `resolve_concept` folds the alias value through `concept_key`, so
    /// `SplitName::concept` is a fixed point on BOTH branches.
    #[test]
    fn alias_value_is_folded_to_concept_key_fixed_point() {
        let conv = ConceptConvention {
            verbs: vec![("add".into(), "create".into())],
            scopes: vec![],
            concept_aliases: vec![("cipher".into(), "CipherKey".into())],
        };
        let split = split_method_name("add_cipher", &conv).expect("splits");
        assert!(split.aliased);
        assert_eq!(split.concept, "cipher_key");
        assert_eq!(crate::codebook::concept_key(&split.concept), split.concept);
    }
    use crate::codebook::concept_key;

    // ── tokenizer ────────────────────────────────────────────────────────

    #[test]
    fn tokenizer_splits_underscore_and_case_boundaries() {
        assert_eq!(
            tokenize_method_name("del_AllCipherValues_StaticModule"),
            vec!["del", "all", "cipher", "values", "static", "module"]
        );
        assert_eq!(tokenize_method_name("Add_Invoice"), vec!["add", "invoice"]);
    }

    #[test]
    fn tokenizer_keeps_digits_attached_to_the_preceding_letters() {
        assert_eq!(
            tokenize_method_name("GetItem2Count"),
            vec!["get", "item2", "count"]
        );
    }

    #[test]
    fn tokenizer_folds_consecutive_uppercase_acronyms_into_one_token() {
        // "ADD" is a run of uppercase letters with no lowercase in between:
        // it folds to a single "add" token, not three.
        assert_eq!(tokenize_method_name("ADD_Widget"), vec!["add", "widget"]);
    }

    #[test]
    fn tokenizer_drops_empty_tokens_from_trailing_underscore() {
        assert_eq!(tokenize_method_name("add_"), vec!["add"]);
    }

    #[test]
    fn tokenizer_on_empty_input_is_empty() {
        assert!(tokenize_method_name("").is_empty());
    }

    // ── convention plumbing ─────────────────────────────────────────────

    fn convention() -> ConceptConvention {
        ConceptConvention {
            verbs: vec![
                ("add".to_string(), "create".to_string()),
                ("get".to_string(), "read".to_string()),
                ("del".to_string(), "delete".to_string()),
            ],
            scopes: vec![
                "iv".to_string(),
                "combo".to_string(),
                "all".to_string(),
                "static".to_string(),
            ],
            concept_aliases: vec![
                // Full 2-token-concat alias — deliberately distinct from the
                // single-token "cipher" alias below, so a residue of
                // ["cipher", "typ"] can prove longest-first precedence.
                ("ciphertyp".to_string(), "cipher_kind".to_string()),
                ("cipher".to_string(), "cipher_alone".to_string()),
                ("widget".to_string(), "generic_widget".to_string()),
            ],
        }
    }

    #[test]
    fn verb_match_is_case_insensitive() {
        let mut conv = convention();
        conv.verbs = vec![("ADD".to_string(), "create".to_string())];
        let split =
            split_method_name("add_Widget", &conv).expect("verb matches case-insensitively");
        assert_eq!(split.verb, "create");
    }

    #[test]
    fn scope_tokens_are_stripped_after_the_verb() {
        // remaining = ["cipher"]: single-token residue, matches the
        // single-token "cipher" alias.
        let split = split_method_name("add_iv_cipher", &convention()).expect("splits");
        assert_eq!(split.verb, "create");
        assert_eq!(split.scopes, vec!["iv".to_string()]);
        assert_eq!(split.concept, "cipher_alone");
        assert!(split.aliased);
    }

    #[test]
    fn scope_stripping_stops_at_the_first_non_scope_token() {
        // "combo" is a configured scope, "cipher" and "typ" are not — only
        // the leading "combo" strips.
        let split = split_method_name("get_combo_cipher_typ", &convention()).expect("splits");
        assert_eq!(split.scopes, vec!["combo".to_string()]);
    }

    #[test]
    fn alias_resolution_prefers_the_longest_full_residue_match() {
        // remaining = ["cipher", "typ"]; the full-concatenation candidate
        // "ciphertyp" has its own alias distinct from the single-token
        // "cipher" alias — longest-first must pick "ciphertyp"'s mapping
        // ("cipher_kind"), NOT the single-token "cipher" mapping
        // ("cipher_alone") that would win if tokens were checked first.
        let split = split_method_name("get_combo_cipher_typ", &convention()).expect("splits");
        assert_eq!(split.concept, "cipher_kind");
        assert!(split.aliased);
    }

    #[test]
    fn alias_resolution_falls_back_to_an_individual_token_match() {
        // remaining = ["widget", "extra"]: the full-concat candidate
        // "widgetextra" has no alias, but the individual "widget" token
        // does — the fallback tier must catch it.
        let split = split_method_name("get_all_widget_extra", &convention()).expect("splits");
        assert_eq!(split.concept, "generic_widget");
        assert!(split.aliased);
    }

    #[test]
    fn unaliased_residue_falls_back_to_raw_lowercase_snake_case() {
        let split = split_method_name("del_static_orderline", &convention()).expect("splits");
        assert_eq!(split.concept, "orderline");
        assert!(!split.aliased);

        // Boundary-4: the raw fallback concept is already concept_key's
        // fixed point — re-normalising it is a no-op.
        assert_eq!(concept_key(&split.concept), split.concept);
    }

    #[test]
    fn multi_token_unaliased_residue_is_underscore_joined() {
        // Only the LEADING "all" strips (contiguous-from-verb rule); the
        // later "static" token is mid-residue, not a leading scope, so it
        // survives into the joined concept.
        let split = split_method_name("del_AllOrderLineTotals_StaticModule", &convention())
            .expect("splits");
        assert_eq!(split.concept, "order_line_totals_static_module");
        assert!(!split.aliased);
    }

    #[test]
    fn aliased_flag_is_true_only_when_an_alias_matched() {
        assert!(
            split_method_name("add_widget", &convention())
                .expect("splits")
                .aliased
        );
        assert!(
            !split_method_name("del_static_orderline", &convention())
                .expect("splits")
                .aliased
        );
    }

    // ── residual reasons ────────────────────────────────────────────────

    #[test]
    fn no_configured_verb_prefix_is_a_residual() {
        assert!(split_method_name("InitializeComponent", &convention()).is_none());
    }

    #[test]
    fn empty_residue_after_verb_is_a_residual() {
        // "add_" tokenizes to just ["add"] — verb matches, nothing is left.
        assert!(split_method_name("add_", &convention()).is_none());
    }

    // ── rekey_model ─────────────────────────────────────────────────────

    fn function(name: &str) -> Function {
        Function {
            name: name.to_string(),
            ..Function::default()
        }
    }

    #[test]
    fn rekey_model_splits_functions_and_helpers_and_collects_residuals() {
        let model = Model {
            name: "LegacyDal".to_string(),
            functions: vec![
                function("Add_Invoice"),
                function("del_AllCipherValues_StaticModule"),
                function("InitializeComponent"),
            ],
            helpers: vec![function("get_combo_cipher_typ"), function("add_")],
            ..Model::default()
        };

        let outcome = rekey_model(&model, &convention());

        assert_eq!(outcome.keyed.len(), 3);
        assert_eq!(outcome.residuals.len(), 2);

        let keyed: std::collections::HashMap<_, _> = outcome.keyed.into_iter().collect();
        assert_eq!(keyed["Add_Invoice"].concept, "invoice");
        // remaining = ["cipher", "values", "static", "module"]: the full
        // concatenation has no alias, so it falls back to the individual
        // "cipher" token alias.
        assert_eq!(
            keyed["del_AllCipherValues_StaticModule"].concept,
            "cipher_alone"
        );
        assert_eq!(keyed["get_combo_cipher_typ"].concept, "cipher_kind");

        assert_eq!(
            outcome.residuals,
            vec![
                ResidualMethod {
                    function: "InitializeComponent".to_string(),
                    reason: ResidualReason::NoVerbPrefix,
                },
                ResidualMethod {
                    function: "add_".to_string(),
                    reason: ResidualReason::EmptyResidue,
                },
            ]
        );
    }

    #[test]
    fn empty_convention_sends_every_method_to_residuals() {
        let model = Model {
            name: "LegacyDal".to_string(),
            functions: vec![function("Add_Invoice"), function("get_widget")],
            ..Model::default()
        };

        let outcome = rekey_model(&model, &ConceptConvention::default());

        assert!(outcome.keyed.is_empty());
        assert_eq!(outcome.residuals.len(), 2);
        assert!(
            outcome
                .residuals
                .iter()
                .all(|r| r.reason == ResidualReason::NoVerbPrefix)
        );
    }
}
