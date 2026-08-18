//! The self-adaptive drill's proposer — turns a concentrated residual
//! ledger ([`crate::plain::PlainResidual`]) into candidate config rows.
//!
//! # Scope boundary (read before extending this module)
//!
//! This module builds the **grouping and cross-corpus classification**
//! stage only. It does NOT build a promotion gate that re-runs extraction
//! with a candidate row "active" and checks the coverage delta is exactly
//! the row's support — that check needs a config-consuming extractor
//! (a trie the plain arm reads before deciding a site's classification),
//! which does not exist yet. Building that engine is the next increment;
//! claiming this module ratifies rows would overstate what it measures.
//!
//! What this module DOES do, honestly: group residual rows by
//! `(reason, detail)`, threshold on support, and — when residuals from
//! more than one corpus are supplied — classify each candidate as
//! [`RowScope::Generic`] (fires in every corpus measured) or
//! [`RowScope::CorpusScoped`] (fires in some but not all). That
//! generic/scoped split is itself a measured, falsifiable fact: a row is
//! `Generic` only because it was observed above `min_support` in EVERY
//! supplied corpus, not because of a similarity heuristic.
//!
//! A candidate row is data-shaped by design (`reason` + `detail` string +
//! per-corpus support) so a downstream session can serialise it straight
//! to TOML without inventing a second representation.

use std::collections::BTreeMap;

use crate::plain::{PlainResidual, PlainResidualReason};

/// Reasons whose `detail` field is dense enough to drill on. The other
/// four reasons ([`PlainResidualReason::UnparsableSource`],
/// [`PlainResidualReason::ModuleConstant`] without a CURIE shape,
/// [`PlainResidualReason::NestedClass`],
/// [`PlainResidualReason::NonNameAssignTarget`]) never carry a `detail`
/// today, so grouping by `(reason, detail)` would only ever produce a
/// `None` bucket for them — filtered out rather than proposed as an
/// empty-detail row.
const DRILLABLE_REASONS: &[PlainResidualReason] = &[
    PlainResidualReason::CurieConstant,
    PlainResidualReason::NonLiteralAssign,
    PlainResidualReason::UnresolvedAnnotation,
    PlainResidualReason::UnresolvedBase,
];

/// One proposed config row: a `(reason, detail)` pair that recurred at
/// least `min_support` times in a residual ledger. Never auto-applied —
/// see the module doc's scope boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateRow {
    pub reason: PlainResidualReason,
    pub detail: String,
    pub support: usize,
}

/// Group `residuals` by `(reason, detail)` for the `DRILLABLE_REASONS`,
/// keeping only groups with `support >= min_support`. Sorted by support
/// descending, then `(reason, detail)` for determinism.
#[must_use]
pub fn propose(residuals: &[PlainResidual], min_support: usize) -> Vec<CandidateRow> {
    let mut counts: BTreeMap<(PlainResidualReason, &str), usize> = BTreeMap::new();
    for r in residuals {
        if !DRILLABLE_REASONS.contains(&r.reason) {
            continue;
        }
        let Some(detail) = r.detail.as_deref() else {
            continue;
        };
        *counts.entry((r.reason, detail)).or_insert(0) += 1;
    }
    let mut rows: Vec<CandidateRow> = counts
        .into_iter()
        .filter(|&(_, support)| support >= min_support)
        .map(|((reason, detail), support)| CandidateRow {
            reason,
            detail: detail.to_string(),
            support,
        })
        .collect();
    rows.sort_by(|a, b| {
        b.support
            .cmp(&a.support)
            .then_with(|| a.reason.as_str().cmp(b.reason.as_str()))
            .then_with(|| a.detail.cmp(&b.detail))
    });
    rows
}

/// Whether a cross-corpus candidate fired everywhere it was measured, or
/// only in a subset — see the module doc for exactly what this claims
/// and what it deliberately does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowScope {
    /// Cleared `min_support` in every supplied corpus.
    Generic,
    /// Cleared `min_support` in at least one, but not all, supplied
    /// corpora.
    CorpusScoped,
}

/// A [`CandidateRow`] classified across the corpora it was measured
/// against, with the per-corpus support that produced the classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossCorpusRow {
    pub reason: PlainResidualReason,
    pub detail: String,
    pub scope: RowScope,
    /// `(corpus label, support)`, one entry per corpus the row cleared
    /// `min_support` in — sorted by label for determinism. A corpus the
    /// row did NOT clear the bar in is simply absent, not zero-padded:
    /// absence here means "not measured as support", the same posture
    /// the residual ledger itself takes (a reason that never fired gets
    /// no row, not a row with `support: 0`).
    pub per_corpus: Vec<(String, usize)>,
}

/// Propose candidates independently over each `(corpus_label, residuals)`
/// pair, then classify each `(reason, detail)` that appears in at least
/// one corpus's proposal by how many of the SUPPLIED corpora it cleared
/// `min_support` in. Requires 2+ corpora to be meaningful — with one
/// corpus every row is trivially cross-corpus-generic, which the calling
/// convention (never call this with fewer than two) exists to avoid.
#[must_use]
pub fn classify_across_corpora(
    labeled: &[(String, Vec<PlainResidual>)],
    min_support: usize,
) -> Vec<CrossCorpusRow> {
    let total_corpora = labeled.len();
    let per_corpus_candidates: Vec<(&str, Vec<CandidateRow>)> = labeled
        .iter()
        .map(|(label, residuals)| (label.as_str(), propose(residuals, min_support)))
        .collect();

    let mut merged: BTreeMap<(PlainResidualReason, String), Vec<(String, usize)>> = BTreeMap::new();
    for (label, rows) in &per_corpus_candidates {
        for row in rows {
            merged
                .entry((row.reason, row.detail.clone()))
                .or_default()
                .push(((*label).to_string(), row.support));
        }
    }

    merged
        .into_iter()
        .map(|((reason, detail), mut per_corpus)| {
            per_corpus.sort_by(|a, b| a.0.cmp(&b.0));
            let scope = if per_corpus.len() == total_corpora {
                RowScope::Generic
            } else {
                RowScope::CorpusScoped
            };
            CrossCorpusRow {
                reason,
                detail,
                scope,
                per_corpus,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plain::extract_plain_from_source_with_residuals;

    fn residuals_of(src: &str) -> Vec<PlainResidual> {
        extract_plain_from_source_with_residuals(src, "mod").1
    }

    #[test]
    fn propose_groups_by_reason_and_detail_and_thresholds_on_support() {
        let src = r#"
class A:
    x = field()
    y = field()
    z = field()
    w = other()
"#;
        let residuals = residuals_of(src);
        let rows = propose(&residuals, 2);
        // "call:field" support=3 clears; "call:other" support=1 doesn't.
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].reason, PlainResidualReason::NonLiteralAssign);
        assert_eq!(rows[0].detail, "call:field");
        assert_eq!(rows[0].support, 3);
    }

    #[test]
    fn propose_never_emits_a_row_for_a_detail_free_reason() {
        // ModuleConstant (non-CURIE) and NestedClass never carry `detail`
        // — propose() must not synthesise an empty-detail row for them.
        let src = r#"
NAME = "x"
COUNT = 3
class Outer:
    class Inner:
        pass
    a, b = 1, 2
"#;
        let residuals = residuals_of(src);
        assert!(!residuals.is_empty(), "fixture must produce residue");
        let rows = propose(&residuals, 1);
        assert!(rows.is_empty());
    }

    #[test]
    fn classify_across_corpora_splits_generic_from_corpus_scoped() {
        // "call:field" fires in BOTH corpora -> Generic.
        // "call:only_a" fires ONLY in corpus a -> CorpusScoped.
        let corpus_a = r#"
class A:
    x = field()
    y = field()
    z = only_a()
    w = only_a()
"#;
        let corpus_b = r#"
class B:
    x = field()
    y = field()
"#;
        let labeled = vec![
            ("a".to_string(), residuals_of(corpus_a)),
            ("b".to_string(), residuals_of(corpus_b)),
        ];
        let rows = classify_across_corpora(&labeled, 2);
        assert_eq!(rows.len(), 2);

        let field_row = rows
            .iter()
            .find(|r| r.detail == "call:field")
            .expect("call:field row");
        assert_eq!(field_row.scope, RowScope::Generic);
        assert_eq!(field_row.per_corpus.len(), 2);
        assert_eq!(
            field_row.per_corpus,
            vec![("a".to_string(), 2), ("b".to_string(), 2)]
        );

        let only_a_row = rows
            .iter()
            .find(|r| r.detail == "call:only_a")
            .expect("call:only_a row");
        assert_eq!(only_a_row.scope, RowScope::CorpusScoped);
        assert_eq!(only_a_row.per_corpus, vec![("a".to_string(), 2)]);
    }

    #[test]
    fn classify_across_corpora_with_curie_prefixes_matches_the_measured_shape() {
        // Mirrors the real measurement: a CURIE prefix that only appears
        // in an ontology-bearing corpus must classify CorpusScoped, never
        // Generic, when a sibling corpus has zero CURIE constants.
        let ontology_corpus = r#"
A = "dismech:a"
B = "dismech:b"
"#;
        let plain_corpus = r#"
NAME = "x"
"#;
        let labeled = vec![
            ("ontology".to_string(), residuals_of(ontology_corpus)),
            ("plain".to_string(), residuals_of(plain_corpus)),
        ];
        let rows = classify_across_corpora(&labeled, 2);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].reason, PlainResidualReason::CurieConstant);
        assert_eq!(rows[0].detail, "dismech");
        assert_eq!(rows[0].scope, RowScope::CorpusScoped);
        assert_eq!(rows[0].per_corpus, vec![("ontology".to_string(), 2)]);
    }
}
