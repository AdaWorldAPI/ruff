//! Structured-name hierarchy parsing (doctrine Phase 5).
//!
//! Some identifiers encode a protocol/form **tree** directly in their
//! spelling: `mod_f_2_3_wing_relevant`, `con_f_2_7_push`, `f_2_3`. The
//! numbered positional segments are hierarchy levels (form, section, …)
//! and everything after them is concept/qualifier residue. The same
//! shape shows up outside any one corpus — wizard step ids, section
//! numbering, migration versions — anywhere a name-embedded tree is used
//! as a cheap ordinal address.
//!
//! This module parses a delimited identifier into hierarchy tiers per a
//! caller-supplied [`NameGrammar`]. The grammar is deliberately
//! **data-as-config**: no corpus tokens live in this file, only the
//! tokenizer and the tier-walk. The parsed [`StructuredName`] feeds
//! [`part_of_edges`], which is the natural input to the `FacetCascade`
//! `part_of` forest — each tier becomes a parent node, and the subject
//! is `part_of` the innermost one.

/// A caller-supplied structured-name grammar (data-as-config).
#[derive(Debug, Clone, Default)]
pub struct NameGrammar {
    /// Leading tokens to strip before matching (e.g. "mod","con","uc").
    pub strip_prefixes: Vec<String>,
    /// The marker token that introduces the numbered path (e.g. "f").
    /// Empty string = no marker required, the path starts at the first
    /// numeric token.
    pub marker: String,
    /// Names for each numeric tier, outermost first (e.g. `["form","section"]`).
    /// Extra numeric tokens beyond this list get tier name "`level<N>`".
    pub tier_names: Vec<String>,
}

/// One parsed tier: `(tier_name, number)`.
pub type Tier = (String, u32);

/// A parsed structured name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuredName {
    /// The numbered path, outermost first: [("form",2),("section",3)].
    pub tiers: Vec<Tier>,
    /// The remaining non-numeric tokens after the path (lowercased) —
    /// concept/qualifier residue for the `concept_split` pass to resolve.
    pub residue: Vec<String>,
}

/// Split `name` into lowercase tokens on `_` and camel/Pascal boundaries —
/// the same discipline as [`crate::codebook::concept_key`], applied
/// segment-by-segment so purely-numeric segments (`"2"`) survive as their
/// own token.
fn tokenize(name: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for raw_seg in name.split('_') {
        if raw_seg.is_empty() {
            continue;
        }
        let mut cur = String::new();
        let mut prev_lower_or_digit = false;
        for ch in raw_seg.chars() {
            if ch.is_uppercase() {
                if prev_lower_or_digit && !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                }
                cur.extend(ch.to_lowercase());
                prev_lower_or_digit = false;
            } else {
                cur.push(ch);
                prev_lower_or_digit = ch.is_lowercase() || ch.is_ascii_digit();
            }
        }
        if !cur.is_empty() {
            tokens.push(cur);
        }
    }
    tokens
}

fn is_numeric_token(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|c| c.is_ascii_digit())
}

/// Parse a structured name against `grammar`.
///
/// `None` when: no numeric path found after the (optional) marker, or the
/// marker is configured and absent. Tokenization: split on `_` and
/// camel/Pascal boundaries, lowercase (same discipline as
/// [`crate::codebook::concept_key`]).
#[must_use]
pub fn parse_structured_name(name: &str, grammar: &NameGrammar) -> Option<StructuredName> {
    let tokens = tokenize(name);
    let mut idx = 0;

    // Strip any leading prefix tokens the grammar names.
    while idx < tokens.len()
        && grammar
            .strip_prefixes
            .iter()
            .any(|p| p.eq_ignore_ascii_case(&tokens[idx]))
    {
        idx += 1;
    }

    // Locate where the numbered path begins.
    if grammar.marker.is_empty() {
        // No marker required: scan forward for the first numeric token.
        while idx < tokens.len() && !is_numeric_token(&tokens[idx]) {
            idx += 1;
        }
        if idx >= tokens.len() {
            return None;
        }
    } else {
        // Marker required: it must be present, and the path starts
        // immediately after it.
        if idx >= tokens.len() || !tokens[idx].eq_ignore_ascii_case(&grammar.marker) {
            return None;
        }
        idx += 1;
    }

    // Collect the contiguous run of numeric tokens as tiers.
    let mut tiers = Vec::new();
    while idx < tokens.len() {
        let Ok(n) = tokens[idx].parse::<u32>() else {
            break;
        };
        let tier_index = tiers.len();
        let tier_name = grammar
            .tier_names
            .get(tier_index)
            .cloned()
            .unwrap_or_else(|| format!("level{}", tier_index + 1));
        tiers.push((tier_name, n));
        idx += 1;
    }

    if tiers.is_empty() {
        return None;
    }

    let residue = tokens[idx..].to_vec();
    Some(StructuredName { tiers, residue })
}

/// Emit `part_of` hierarchy edges for a parsed name, deepest-first: the
/// subject is parented to the innermost tier node, and each tier node is
/// in turn parented to the one above it, up to (but not including) a
/// synthetic root.
///
/// For tiers `[("form",2),("section",3)]`, subject `s`, and namespace
/// `ns`, returns `[(s, "ns:form_2/section_3"), ("ns:form_2/section_3",
/// "ns:form_2")]` — the deepest node is the subject itself, parented to
/// the innermost tier node, and each tier node parents to the one above.
/// Node IRI format: `{ns}:{tier_name}_{n}` joined by `/` for nesting
/// (e.g. `"study:form_2/section_3"`).
#[must_use]
pub fn part_of_edges(subject: &str, ns: &str, parsed: &StructuredName) -> Vec<(String, String)> {
    if parsed.tiers.is_empty() {
        return Vec::new();
    }

    let mut nodes = Vec::with_capacity(parsed.tiers.len());
    let mut path = String::new();
    for (tier_name, n) in &parsed.tiers {
        if !path.is_empty() {
            path.push('/');
        }
        {
            use std::fmt::Write as _;
            let _ = write!(path, "{tier_name}_{n}");
        }
        nodes.push(format!("{ns}:{path}"));
    }

    let mut edges = Vec::with_capacity(parsed.tiers.len());
    edges.push((subject.to_string(), nodes[nodes.len() - 1].clone()));
    for i in (1..nodes.len()).rev() {
        edges.push((nodes[i].clone(), nodes[i - 1].clone()));
    }
    edges
}

#[cfg(test)]
mod tests {
    use super::{NameGrammar, StructuredName, parse_structured_name, part_of_edges};

    fn form_section_grammar() -> NameGrammar {
        NameGrammar {
            strip_prefixes: vec!["mod".to_string(), "con".to_string()],
            marker: "f".to_string(),
            tier_names: vec!["form".to_string(), "section".to_string()],
        }
    }

    #[test]
    fn strips_prefix_and_marker_then_splits_path_and_residue() {
        let parsed = parse_structured_name("mod_f_2_3_wing_relevant", &form_section_grammar())
            .expect("parses");
        assert_eq!(
            parsed.tiers,
            vec![("form".to_string(), 2), ("section".to_string(), 3)]
        );
        assert_eq!(
            parsed.residue,
            vec!["wing".to_string(), "relevant".to_string()]
        );
    }

    #[test]
    fn different_prefix_same_grammar() {
        let parsed =
            parse_structured_name("con_f_2_7_push", &form_section_grammar()).expect("parses");
        assert_eq!(
            parsed.tiers,
            vec![("form".to_string(), 2), ("section".to_string(), 7)]
        );
        assert_eq!(parsed.residue, vec!["push".to_string()]);
    }

    #[test]
    fn no_prefix_no_residue() {
        let parsed = parse_structured_name("f_2_3", &form_section_grammar()).expect("parses");
        assert_eq!(
            parsed.tiers,
            vec![("form".to_string(), 2), ("section".to_string(), 3)]
        );
        assert!(parsed.residue.is_empty());
    }

    #[test]
    fn no_numeric_path_is_none() {
        let grammar = NameGrammar::default();
        assert_eq!(parse_structured_name("uc_widget", &grammar), None);
    }

    #[test]
    fn marker_absent_grammar_starts_path_at_first_numeric_token() {
        let grammar = NameGrammar {
            strip_prefixes: Vec::new(),
            marker: String::new(),
            tier_names: vec!["form".to_string(), "section".to_string()],
        };
        let parsed = parse_structured_name("2_3", &grammar).expect("parses");
        assert_eq!(
            parsed.tiers,
            vec![("form".to_string(), 2), ("section".to_string(), 3)]
        );
        assert!(parsed.residue.is_empty());
    }

    #[test]
    fn marker_required_but_missing_is_none() {
        let grammar = form_section_grammar();
        assert_eq!(parse_structured_name("2_3_push", &grammar), None);
    }

    /// Marker-less mode scans forward to the first numeric token and
    /// DISCARDS every leading token — `vital_2` parses to bare node
    /// `form_2` with `vital` recorded nowhere (not a tier, not residue).
    /// This pin documents the footgun: a consumer separating unresolved
    /// domain residues from protocol addresses (the furnace exam) must
    /// gate the protocol plane on marker presence, because with a marker
    /// the same name safely returns `None` instead.
    #[test]
    fn marker_less_mode_discards_leading_tokens_the_exam_must_gate_on_marker() {
        let marker_less = NameGrammar {
            strip_prefixes: Vec::new(),
            marker: String::new(),
            tier_names: vec!["form".to_string(), "section".to_string()],
        };
        let parsed = parse_structured_name("vital_2", &marker_less).expect("parses");
        assert_eq!(parsed.tiers, vec![("form".to_string(), 2)]);
        assert!(parsed.residue.is_empty(), "leading `vital` is DISCARDED");
        // Marker-present mode is the safe configuration: the non-marker
        // leading token makes the parse refuse.
        assert_eq!(
            parse_structured_name("vital_2", &form_section_grammar()),
            None
        );
    }

    #[test]
    fn extra_numeric_tokens_get_level_n_tier_names() {
        let parsed =
            parse_structured_name("f_2_3_4_push", &form_section_grammar()).expect("parses");
        assert_eq!(
            parsed.tiers,
            vec![
                ("form".to_string(), 2),
                ("section".to_string(), 3),
                ("level3".to_string(), 4),
            ]
        );
        assert_eq!(parsed.residue, vec!["push".to_string()]);
    }

    #[test]
    fn part_of_edges_chain_shape_and_parent_ordering() {
        let parsed = StructuredName {
            tiers: vec![("form".to_string(), 2), ("section".to_string(), 3)],
            residue: Vec::new(),
        };
        let edges = part_of_edges("s", "study", &parsed);
        assert_eq!(
            edges,
            vec![
                ("s".to_string(), "study:form_2/section_3".to_string()),
                (
                    "study:form_2/section_3".to_string(),
                    "study:form_2".to_string()
                ),
            ]
        );
    }

    #[test]
    fn part_of_edges_single_tier_has_no_parent_above_root() {
        let parsed = StructuredName {
            tiers: vec![("form".to_string(), 2)],
            residue: Vec::new(),
        };
        let edges = part_of_edges("s", "study", &parsed);
        assert_eq!(edges, vec![("s".to_string(), "study:form_2".to_string())]);
    }

    #[test]
    fn part_of_edges_empty_tiers_is_empty() {
        let parsed = StructuredName {
            tiers: Vec::new(),
            residue: Vec::new(),
        };
        assert!(part_of_edges("s", "study", &parsed).is_empty());
    }
}
