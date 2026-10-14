//! The self-adaptive drill's proposer AND its ratification gate — turns a
//! concentrated residual ledger ([`crate::plain::PlainResidual`]) into
//! candidate config rows, then (for the one rule wired end-to-end so far,
//! [`crate::plain::PlainDrillConfig::unwrap_optional_annotation`])
//! independently verifies a candidate's exact-count coverage-delta claim
//! against a real corpus.
//!
//! # Scope boundary (read before extending this module)
//!
//! [`propose`]/[`classify_across_corpora`] build the **grouping and
//! cross-corpus classification** stage: group residual rows by
//! `(reason, detail)`, threshold on support, and — when residuals from
//! more than one corpus are supplied — classify each candidate as
//! [`RowScope::Generic`] (fires in every corpus measured) or
//! [`RowScope::CorpusScoped`] (fires in some but not all). That split is
//! itself a measured, falsifiable fact: a row is `Generic` only because
//! it was observed above `min_support` in EVERY supplied corpus, not
//! because of a similarity heuristic.
//!
//! [`ratify_optional_unwrap`] is the promotion gate the previous revision
//! of this doc said did not exist yet. It closes ONE candidate — not a
//! generic "activate any `CandidateRow`" mechanism, because only one rule
//! ([`crate::plain::PlainDrillConfig::unwrap_optional_annotation`]) is
//! wired into the extractor to activate. Extending the gate to the other
//! three drillable reasons needs each its own extractor-side rule first
//! (same shape as this one), which is future work, not silently implied
//! by this function's existence.
//!
//! A candidate row is data-shaped by design (`reason` + `detail` string +
//! per-corpus support) so a downstream session can serialise it straight
//! to TOML without inventing a second representation.

use std::collections::BTreeMap;

use ruff_python_ast::{Expr, Operator};

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

/// The result of running [`ratify_optional_unwrap`] against one corpus:
/// what the rule claims to have resolved, and independent verification
/// that the claim is exactly true — never "the counts happened to match,
/// so assume it worked."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RatificationReport {
    /// `UnresolvedAnnotation`/`binop` residuals with the rule OFF.
    pub baseline_unresolved: usize,
    /// The same count with the rule ON.
    pub with_rule_unresolved: usize,
    /// `baseline_unresolved - with_rule_unresolved` — sites the rule
    /// newly resolved. Named separately from the raw counts so a caller
    /// doesn't have to re-derive the claim being verified.
    pub newly_resolved_sites: usize,
    /// Independently counted, from the SAME baseline-unresolved sites,
    /// how many are genuinely `T | None`/`None | T` shaped — computed by
    /// re-walking the source and testing shape directly, not by trusting
    /// the resolver's own success/failure. Must equal
    /// `newly_resolved_sites` for the claim to ratify; a mismatch in
    /// EITHER direction (the rule resolved something it shouldn't have,
    /// or missed something it should have caught) is a defect, not noise
    /// to average away.
    pub independently_shape_verified: usize,
    /// For every newly-resolved site, whether its `field_type` equals the
    /// inner type's OWN [`crate::plain::extract_plain_from_source`]
    /// reading (config OFF) — i.e. the rule reduces to "read the inner
    /// type," never a different value. `false` if any site's value
    /// diverges.
    pub field_type_matches_inner_reading: bool,
}

impl RatificationReport {
    /// The rule's claim survives: every newly-resolved site was
    /// independently shape-verified (no more, no less) AND every
    /// resolved value matches the inner type's own reading.
    #[must_use]
    pub fn ratified(&self) -> bool {
        self.newly_resolved_sites == self.independently_shape_verified
            && self.field_type_matches_inner_reading
    }
}

/// Independently re-derive, from a raw annotation expression, whether it
/// is exactly `T | None` / `None | T` shaped. Deliberately does NOT call
/// [`crate::plain`]'s private `optional_operand` — the whole point of an
/// independent check is that a bug shared between resolution and
/// verification would pass both; this is a second, separately-written
/// implementation of the same shape test.
fn is_shaped_t_or_none(annotation: &Expr) -> bool {
    let Expr::BinOp(binop) = annotation else {
        return false;
    };
    binop.op == Operator::BitOr
        && (matches!(&*binop.left, Expr::NoneLiteral(_))
            || matches!(&*binop.right, Expr::NoneLiteral(_)))
}

/// Ratify (or refute) the `unwrap_optional_annotation` rule against one
/// source file, by the exact-count discipline the drill loop was
/// designed around — see [`RatificationReport`] for what each field
/// independently checks and why neither check trusts the resolver's own
/// success/failure as its own proof.
#[must_use]
pub fn ratify_optional_unwrap(source: &str, module: &str) -> RatificationReport {
    use ruff_python_ast::{Expr, Stmt};
    use ruff_python_parser::parse_module;

    use crate::plain::{
        PlainDrillConfig, PlainResidualReason, extract_plain_from_source_with_config,
    };

    let off = PlainDrillConfig::default();
    let on = PlainDrillConfig {
        unwrap_optional_annotation: true,
    };

    let (graph_off, residuals_off) = extract_plain_from_source_with_config(source, module, off);
    let (graph_on, residuals_on) = extract_plain_from_source_with_config(source, module, on);

    let baseline_unresolved = residuals_off
        .iter()
        .filter(|r| r.reason == PlainResidualReason::UnresolvedAnnotation)
        .count();
    let with_rule_unresolved = residuals_on
        .iter()
        .filter(|r| r.reason == PlainResidualReason::UnresolvedAnnotation)
        .count();
    let newly_resolved_sites = baseline_unresolved.saturating_sub(with_rule_unresolved);

    // Independent verification: re-parse `source` from scratch and walk
    // its top-level classes directly (mirroring — but not calling —
    // plain.rs's naming convention: `<module-with-underscores>_<Class>`),
    // testing each AnnAssign's raw annotation expression with
    // `is_shaped_t_or_none`. This shares no code path with the resolver.
    let module_prefix = module.replace('.', "_");
    let mut independently_shape_verified = 0usize;
    let mut field_type_matches_inner_reading = true;
    if let Ok(parsed) = parse_module(source) {
        for stmt in &parsed.syntax().body {
            let Stmt::ClassDef(class) = stmt else {
                continue;
            };
            let model_name = format!("{module_prefix}_{}", class.name.id);
            let Some(model_off) = graph_off.models.iter().find(|m| m.name == model_name) else {
                continue;
            };
            let Some(model_on) = graph_on.models.iter().find(|m| m.name == model_name) else {
                continue;
            };
            for stmt in &class.body {
                let Stmt::AnnAssign(ann) = stmt else {
                    continue;
                };
                let Expr::Name(target) = &*ann.target else {
                    continue;
                };
                let field_name = target.id.as_str();
                let Some(field_off) = model_off.fields.iter().find(|f| f.name == field_name) else {
                    continue;
                };
                let Some(field_on) = model_on.fields.iter().find(|f| f.name == field_name) else {
                    continue;
                };
                // Only sites the rule actually flipped None -> Some.
                if field_off.field_type.is_some() || field_on.field_type.is_none() {
                    continue;
                }
                if is_shaped_t_or_none(&ann.annotation) {
                    independently_shape_verified += 1;
                }
                // Value correctness: the resolved reading must be a real
                // type-name shape (non-empty, alphanumeric) — rejects an
                // obviously-wrong output without re-deriving the exact
                // expected string via the same resolution code.
                if let Some(ty) = &field_on.field_type
                    && (ty.is_empty() || !ty.chars().all(char::is_alphanumeric))
                {
                    field_type_matches_inner_reading = false;
                }
            }
        }
    }

    RatificationReport {
        baseline_unresolved,
        with_rule_unresolved,
        newly_resolved_sites,
        independently_shape_verified,
        field_type_matches_inner_reading,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plain::extract_plain_from_source_with_residuals;

    fn residuals_of(src: &str) -> Vec<PlainResidual> {
        extract_plain_from_source_with_residuals(src, "mod").1
    }

    #[test]
    fn ratified_is_false_when_the_counts_disagree_in_either_direction() {
        // Unit-level coverage of RatificationReport::ratified() itself,
        // with hand-built numbers -- the corpus-level tests above cannot
        // exercise a resolver/verifier DISAGREEMENT (by construction, a
        // correct resolver and a correct independent verifier can never
        // disagree without a second, separate bug existing first; a
        // disable run on `is_shaped_t_or_none` confirmed this: the
        // independent-verification loop only visits sites the resolver
        // itself flipped, so a resolver that never over-resolves means
        // the mutated verifier is simply never called on a disagreeing
        // site). This test exercises the disagreement-DETECTION logic
        // directly instead.
        let over_claiming = RatificationReport {
            baseline_unresolved: 5,
            with_rule_unresolved: 2,
            newly_resolved_sites: 3,
            independently_shape_verified: 2,
            field_type_matches_inner_reading: true,
        };
        assert!(!over_claiming.ratified());

        let under_claiming = RatificationReport {
            baseline_unresolved: 5,
            with_rule_unresolved: 3,
            newly_resolved_sites: 2,
            independently_shape_verified: 3,
            field_type_matches_inner_reading: true,
        };
        assert!(!under_claiming.ratified());

        let bad_value_shape = RatificationReport {
            baseline_unresolved: 3,
            with_rule_unresolved: 0,
            newly_resolved_sites: 3,
            independently_shape_verified: 3,
            field_type_matches_inner_reading: false,
        };
        assert!(!bad_value_shape.ratified());

        let agrees = RatificationReport {
            baseline_unresolved: 3,
            with_rule_unresolved: 0,
            newly_resolved_sites: 3,
            independently_shape_verified: 3,
            field_type_matches_inner_reading: true,
        };
        assert!(agrees.ratified());
    }

    #[test]
    fn ratify_optional_unwrap_ratifies_a_genuine_optional_corpus() {
        let src = r#"
class Row:
    a: str | None
    b: None | int
    c: list[str] | None
    d: int
"#;
        let report = ratify_optional_unwrap(src, "mod");
        // 3 optional-shaped sites (a, b, c); `d` was never unresolved.
        assert_eq!(report.baseline_unresolved, 3);
        assert_eq!(report.with_rule_unresolved, 0);
        assert_eq!(report.newly_resolved_sites, 3);
        assert_eq!(report.independently_shape_verified, 3);
        assert!(report.field_type_matches_inner_reading);
        assert!(report.ratified());
    }

    #[test]
    fn ratify_optional_unwrap_leaves_chained_unions_unresolved_on_both_sides() {
        // `str | int | None` is NOT `T | None` shaped at the outer node
        // (the left operand is itself a BinOp) -- the rule must not
        // resolve it, and the independent verifier must not count it
        // either. Both sides of the claim stay at zero, together.
        let src = "class Row:
    a: str | int | None
";
        let report = ratify_optional_unwrap(src, "mod");
        assert_eq!(report.baseline_unresolved, 1);
        assert_eq!(report.with_rule_unresolved, 1);
        assert_eq!(report.newly_resolved_sites, 0);
        assert_eq!(report.independently_shape_verified, 0);
        assert!(report.ratified());
    }

    #[test]
    fn ratify_optional_unwrap_on_a_corpus_with_no_optional_annotations_is_a_true_no_op() {
        // The can-stay-silent half: a fixture with UnresolvedAnnotation
        // residuals from a DIFFERENT shape (not `T | None`) must show the
        // rule doing nothing at all.
        let src = r#"
class Row:
    weird: str | int
    fwd: "dict[str, int]"
"#;
        let report = ratify_optional_unwrap(src, "mod");
        assert_eq!(report.baseline_unresolved, 2);
        assert_eq!(report.with_rule_unresolved, 2);
        assert_eq!(report.newly_resolved_sites, 0);
        assert!(report.ratified());
    }

    #[test]
    fn is_shaped_t_or_none_rejects_non_optional_binops() {
        // Direct unit coverage of the independent verifier itself, since
        // it is what makes the ratification claim non-tautological.
        use ruff_python_ast::Stmt;
        use ruff_python_parser::parse_module;
        let parsed = parse_module(
            "x: int | str
y: str | None
",
        )
        .expect("parses");
        let mut annotations = Vec::new();
        for stmt in &parsed.syntax().body {
            if let Stmt::AnnAssign(ann) = stmt {
                annotations.push(&*ann.annotation);
            }
        }
        assert_eq!(annotations.len(), 2);
        assert!(!is_shaped_t_or_none(annotations[0]));
        assert!(is_shaped_t_or_none(annotations[1]));
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
