//! `soc` — the "256-cap-is-a-lint" separation-of-concerns check.
//!
//! Promoted from a one-off real-corpus falsifier (`fn main()`) into a
//! **reusable library function** so it can run in `ruff check` / CI instead
//! of by hand (the first step of TD-23 / the `OGAR-SOC` lint).
//!
//! The law: every class whose sibling set overflows the per-tier cascade rank
//! is a DESIGN smell, never a storage limit, and is one (or both) of:
//!
//! 1. **Duplication** — the data members collapse to a representable number of
//!    distinct `field_type`s; mask them by classid into a `ClassView`.
//! 2. **Conflation** — data (`has_field`) and behaviour (`has_function`) are
//!    mixed under one parent; split the concerns.
//!
//! [`law_holds`] is the falsifier: `false` iff some over-cap class is *neither*.

use ruff_spo_triplet::Triple;
use std::collections::{BTreeMap, BTreeSet};

/// Per-tier sibling budget. The cascade rank is a 1-based `u8` with `0` reserved
/// ("no tier here"), so ranks `1..=255` are representable — a level with more
/// than `u8::MAX` (255) siblings overflows (the 256th saturates to rank 255 and
/// collides with the 255th, matching `ruff_spo_address::ranks`). The lint
/// therefore fires when `members > MAX_SIBLINGS_PER_TIER`. (Colloquially the
/// "256-cap": 256 is the byte's cardinality; 255 is the representable count.)
pub const MAX_SIBLINGS_PER_TIER: usize = u8::MAX as usize;

/// A class's distinct-field set must collapse to a `ClassView`-maskable count.
/// The ceiling is the **byte cardinality** — the *same* bound as the per-tier
/// sibling rank ([`MAX_SIBLINGS_PER_TIER`]), **not a second, locked cap**.
///
/// A `ClassView` is **mapped from the class's inherited format** and selected by
/// `classid` (the filter); its cascade **shape is class-conditioned**, never
/// restated or locked here — Rails → `6×2`, other frameworks → `4×3`, the
/// canonical GUID → `3×4` (all `G·D = 12`, 8-bit tiers; the per-group depth
/// `D ∈ {2,3,4}` is a *per-class* constant, picked from the class condition).
/// This module only bounds the god-object cardinality: `< 256` is maskable
/// (clean), `≥ 256` is the SoC split signal. (operator 2026-06-29: the shape is
/// inherited — don't lock a `[u64; 4]` "quadruplet"; `D` is class-conditioned.)
pub const FIELD_MASK_CAP: usize = MAX_SIBLINGS_PER_TIER;

/// God-object threshold: a class with this many members (or more) overflows the
/// per-tier `u8` rank and **triggers SoC branching** (decompose, don't widen).
/// Equals `MAX_SIBLINGS_PER_TIER + 1` (255 representable → the 256th overflows).
pub const GOD_OBJECT_MEMBERS: usize = MAX_SIBLINGS_PER_TIER + 1; // 256

/// The verdict for a class whose sibling set exceeds [`MAX_SIBLINGS_PER_TIER`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SocVerdict {
    /// All data members are typed and collapse to `<= FIELD_MASK_CAP` distinct
    /// `field_type`s — maskable by one `ClassView` (whose cascade shape is
    /// class-conditioned: `6×2` / `4×3` / `3×4`, selected by `classid`).
    Duplication,
    /// `has_field` data + `has_function` behaviour conflated under one parent — split.
    Conflation,
    /// Both duplication and conflation are present.
    DuplicationAndConflation,
    /// Neither — the law's counterexample (an over-cap set that is provably
    /// neither type-collapsible nor data⊥behaviour-mixed).
    Counterexample,
}

/// One over-cap class and its classification.
#[derive(Debug, Clone)]
pub struct SocFinding {
    /// The class IRI (the `has_field` / `has_function` subject).
    pub class: String,
    /// Total members (`has_field` ∪ `has_function`).
    pub members: usize,
    /// `has_field` members (data).
    pub data: usize,
    /// `has_function` members (behaviour).
    pub funcs: usize,
    /// Distinct `field_type`s among the typed data members.
    pub distinct_field_types: usize,
    /// Typed data rows reclaimable by a masked `ClassView` (`typed_data - distinct`).
    pub duplicate_rows: usize,
    /// The law's classification of this overflow.
    pub verdict: SocVerdict,
}

/// Classify every class whose sibling set exceeds [`MAX_SIBLINGS_PER_TIER`].
///
/// Mirrors the original real-corpus falsifier's logic, with two corrections
/// over that one-off script: `funcs` is derived from the `has_function`
/// predicate (not the untyped-data complement, which would false-positive on
/// `has_field` members whose type lives only in the IR, e.g. `cpp_field`),
/// and the overflow threshold is `> u8::MAX` siblings (the representable
/// rank count).
#[must_use]
pub fn soc_findings(triples: &[Triple]) -> Vec<SocFinding> {
    let field_type: BTreeMap<&str, &str> = triples
        .iter()
        .filter(|t| t.p == "field_type")
        .map(|t| (t.s.as_str(), t.o.as_str()))
        .collect();

    // Bucket each member with its predicate (true == has_function).
    let mut members_by_class: BTreeMap<&str, Vec<(&str, bool)>> = BTreeMap::new();
    for t in triples {
        let is_fn = t.p == "has_function";
        if is_fn || t.p == "has_field" {
            members_by_class
                .entry(t.s.as_str())
                .or_default()
                .push((t.o.as_str(), is_fn));
        }
    }

    let mut out = Vec::new();
    for (class, members) in &members_by_class {
        if members.len() < GOD_OBJECT_MEMBERS {
            continue;
        }
        let funcs = members.iter().filter(|(_, is_fn)| *is_fn).count();
        let data_members: Vec<&str> = members
            .iter()
            .filter(|(_, is_fn)| !*is_fn)
            .map(|(m, _)| *m)
            .collect();
        let data = data_members.len();
        let distinct: BTreeSet<&str> = data_members
            .iter()
            .filter_map(|m| field_type.get(m).copied())
            .collect();
        // Typed data rows reclaimable by a masked ClassView = typed members minus
        // the distinct types they collapse to.
        let typed = data_members
            .iter()
            .filter_map(|m| field_type.get(m))
            .count();
        let duplicate_rows = typed.saturating_sub(distinct.len());
        // Duplication ⇒ the data collapses to a ClassView-maskable view: every
        // data member is typed (untyped siblings are not proven collapsible) AND
        // the distinct types fit within the byte cardinality (FIELD_MASK_CAP).
        // The class-conditioned cascade shape (6×2/4×3/3×4) is the ClassView's,
        // selected by classid — not restated here.
        let is_dup = data > 0 && typed == data && distinct.len() <= FIELD_MASK_CAP;
        let is_conflated = funcs > 0 && data > 0;
        let verdict = match (is_dup, is_conflated) {
            (true, true) => SocVerdict::DuplicationAndConflation,
            (true, false) => SocVerdict::Duplication,
            (false, true) => SocVerdict::Conflation,
            (false, false) => SocVerdict::Counterexample,
        };
        out.push(SocFinding {
            class: (*class).to_string(),
            members: members.len(),
            data,
            funcs,
            distinct_field_types: distinct.len(),
            duplicate_rows,
            verdict,
        });
    }
    out
}

/// Does the corpus uphold the law (no counterexample)?
#[must_use]
pub fn law_holds(triples: &[Triple]) -> bool {
    soc_findings(triples)
        .iter()
        .all(|f| f.verdict != SocVerdict::Counterexample)
}

/// One SoC-clean branch a god object decomposes into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocBranch {
    /// `has_field` data → one masked `ClassView` (the distinct types fit one `FieldMask`).
    DataView { fields: usize, distinct_types: usize },
    /// `has_field` data whose distinct types exceed `FIELD_MASK_CAP` → paginate
    /// into `views` `ClassView`s via the class hierarchy.
    PaginatedDataView { fields: usize, distinct_types: usize, views: usize },
    /// `has_function` behaviour → `ActionDef`s rooted in reusable OGAR adapters
    /// (the logic is extracted as ontology, not reimplemented per class).
    BehaviourActions { funcs: usize },
}

/// A god object (`>= GOD_OBJECT_MEMBERS`) and the branches it decomposes into.
#[derive(Debug, Clone)]
pub struct SocPlan {
    /// The over-cap class IRI.
    pub class: String,
    /// Total members.
    pub members: usize,
    /// The classification that motivated the branching.
    pub verdict: SocVerdict,
    /// The SoC-clean branches: data → `ClassView`(s), behaviour → `ActionDef`s.
    pub branches: Vec<SocBranch>,
}

/// **SoC branching.** For every god object (`>= GOD_OBJECT_MEMBERS` members),
/// emit the decomposition plan instead of merely classifying it:
///
/// - `has_field` data → a [`SocBranch::DataView`] (one masked `ClassView`) if its
///   distinct `field_type`s fit one `FieldMask`, else a
///   [`SocBranch::PaginatedDataView`] across `ceil(distinct / FIELD_MASK_CAP)`
///   views (paginate via class hierarchy);
/// - `has_function` behaviour → a [`SocBranch::BehaviourActions`] (ActionDefs
///   rooted in reusable OGAR adapters — logic extracted as ontology).
///
/// This is the data⊥behaviour split the `Conflation` verdict names, made
/// executable: branch, never widen.
#[must_use]
pub fn soc_branches(triples: &[Triple]) -> Vec<SocPlan> {
    soc_findings(triples)
        .into_iter()
        .map(|f| {
            let mut branches = Vec::new();
            if f.data > 0 {
                if f.distinct_field_types <= FIELD_MASK_CAP {
                    branches.push(SocBranch::DataView {
                        fields: f.data,
                        distinct_types: f.distinct_field_types,
                    });
                } else {
                    branches.push(SocBranch::PaginatedDataView {
                        fields: f.data,
                        distinct_types: f.distinct_field_types,
                        views: f.distinct_field_types.div_ceil(FIELD_MASK_CAP),
                    });
                }
            }
            if f.funcs > 0 {
                branches.push(SocBranch::BehaviourActions { funcs: f.funcs });
            }
            SocPlan {
                class: f.class,
                members: f.members,
                verdict: f.verdict,
                branches,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str, p: &str, o: &str) -> Triple {
        Triple {
            s: s.into(),
            p: p.into(),
            o: o.into(),
            f: 1.0,
            c: 1.0,
        }
    }

    #[test]
    fn over_cap_pure_data_is_duplication() {
        let mut tr = Vec::new();
        for i in 0..300 {
            let m = format!("C.f{i}");
            tr.push(t("C", "has_field", &m));
            tr.push(t(&m, "field_type", if i % 2 == 0 { "i32" } else { "str" }));
        }
        let f = soc_findings(&tr);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].members, 300);
        assert_eq!(f[0].funcs, 0);
        assert_eq!(f[0].distinct_field_types, 2);
        assert_eq!(f[0].duplicate_rows, 298);
        assert_eq!(f[0].verdict, SocVerdict::Duplication);
        assert!(law_holds(&tr));
    }

    #[test]
    fn untyped_fields_are_not_counted_as_functions() {
        // 300 has_field members, NONE with a field_type triple (type in the IR).
        let mut tr = Vec::new();
        for i in 0..300 {
            tr.push(t("U", "has_field", &format!("U.f{i}")));
        }
        let f = soc_findings(&tr);
        assert_eq!(
            f[0].funcs, 0,
            "has_field with no field_type must not be a function"
        );
        assert_eq!(f[0].data, 300);
        // No types resolved and no functions -> not provably dup/conflation.
        assert_eq!(f[0].verdict, SocVerdict::Counterexample);
    }

    #[test]
    fn data_plus_functions_is_duplication_and_conflation() {
        let mut tr = Vec::new();
        for i in 0..200 {
            let m = format!("D.f{i}");
            tr.push(t("D", "has_field", &m));
            tr.push(t(&m, "field_type", "str"));
        }
        for i in 0..100 {
            tr.push(t("D", "has_function", &format!("D.fn{i}")));
        }
        let f = soc_findings(&tr);
        assert_eq!(f[0].members, 300);
        assert_eq!(f[0].funcs, 100);
        assert_eq!(f[0].data, 200);
        assert_eq!(f[0].verdict, SocVerdict::DuplicationAndConflation);
    }

    #[test]
    fn boundary_255_ignored_256_caught() {
        let mk = |n: usize| {
            let mut tr = Vec::new();
            for i in 0..n {
                let m = format!("B.f{i}");
                tr.push(t("B", "has_field", &m));
                tr.push(t(&m, "field_type", "str"));
            }
            tr
        };
        assert!(
            soc_findings(&mk(255)).is_empty(),
            "255 siblings are representable"
        );
        assert_eq!(soc_findings(&mk(256)).len(), 1, "256 overflows the u8 rank");
    }

    #[test]
    fn wide_distinct_types_exceed_field_mask_is_counterexample() {
        // 300 typed fields, every one a distinct type → exceeds the byte
        // cardinality (300 > FIELD_MASK_CAP), so NOT maskable duplication by any
        // class-conditioned shape — a genuine god object.
        let mut tr = Vec::new();
        for i in 0..300 {
            let m = format!("W.f{i}");
            tr.push(t("W", "has_field", &m));
            tr.push(t(&m, "field_type", &format!("T{i}")));
        }
        let f = soc_findings(&tr);
        assert_eq!(f[0].distinct_field_types, 300);
        assert!(f[0].distinct_field_types > FIELD_MASK_CAP);
        assert_eq!(f[0].verdict, SocVerdict::Counterexample);
        assert!(!law_holds(&tr));
    }

    #[test]
    fn odoo_109_distinct_fields_fit_a_classview() {
        // An Odoo-shaped over-cap class: >255 members so the lint fires, with 109
        // DISTINCT field types — too wide for the old single-u64 cap (64) but clean
        // within the byte cardinality (109 <= FIELD_MASK_CAP). The epiphany, tested:
        // expanding past 64 turns this from a Counterexample into Duplication — "if
        // odoo has 109 in classview and it's clean we're fine". The ClassView's
        // cascade shape (6×2/4×3/3×4) is class-conditioned, selected by classid.
        let mut tr = Vec::new();
        for i in 0..300 {
            let m = format!("account_move.f{i}");
            tr.push(t("account_move", "has_field", &m));
            tr.push(t(&m, "field_type", &format!("T{}", i % 109))); // 109 distinct types
        }
        let f = soc_findings(&tr);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].distinct_field_types, 109);
        assert!(f[0].distinct_field_types <= FIELD_MASK_CAP);
        assert_eq!(
            f[0].verdict,
            SocVerdict::Duplication,
            "clean in one ClassView, not a god object"
        );
        assert!(law_holds(&tr));
    }

    #[test]
    fn cardinality_boundary_clean_then_god_object() {
        let mk = |distinct: usize| {
            let mut tr = Vec::new();
            for i in 0..400 {
                let m = format!("B.f{i}");
                tr.push(t("B", "has_field", &m));
                tr.push(t(&m, "field_type", &format!("T{}", i % distinct)));
            }
            tr
        };
        // distinct == FIELD_MASK_CAP → maskable → Duplication (clean).
        let clean = soc_findings(&mk(FIELD_MASK_CAP));
        assert_eq!(clean[0].distinct_field_types, FIELD_MASK_CAP);
        assert_eq!(clean[0].verdict, SocVerdict::Duplication);
        // distinct > FIELD_MASK_CAP → god object → Counterexample (the SoC split
        // signal: split into sub-ClassViews, don't widen/lock a mask).
        let god = soc_findings(&mk(FIELD_MASK_CAP + 1));
        assert_eq!(god[0].distinct_field_types, FIELD_MASK_CAP + 1);
        assert_eq!(god[0].verdict, SocVerdict::Counterexample);
    }

    #[test]
    fn conflation_god_object_branches_data_and_behaviour() {
        let mut tr = Vec::new();
        for i in 0..200 {
            let m = format!("F.f{i}");
            tr.push(t("F", "has_field", &m));
            tr.push(t(&m, "field_type", "str"));
        }
        for i in 0..100 {
            tr.push(t("F", "has_function", &format!("F.fn{i}")));
        }
        let plans = soc_branches(&tr);
        assert_eq!(plans.len(), 1);
        assert!(plans[0].branches.contains(&SocBranch::DataView { fields: 200, distinct_types: 1 }));
        assert!(plans[0].branches.contains(&SocBranch::BehaviourActions { funcs: 100 }));
    }

    #[test]
    fn wide_data_god_object_paginates_views() {
        let mut tr = Vec::new();
        for i in 0..300 {
            let m = format!("P.f{i}");
            tr.push(t("P", "has_field", &m));
            tr.push(t(&m, "field_type", &format!("T{i}")));
        }
        let plans = soc_branches(&tr);
        assert_eq!(
            plans[0].branches,
            vec![SocBranch::PaginatedDataView {
                fields: 300,
                distinct_types: 300,
                views: 300usize.div_ceil(FIELD_MASK_CAP),
            }]
        );
    }

    #[test]
    fn under_threshold_yields_no_plan() {
        let tr = vec![t("S", "has_field", "S.a"), t("S.a", "field_type", "i32")];
        assert!(soc_branches(&tr).is_empty());
    }

    #[test]
    fn untyped_data_blocks_duplication_verdict() {
        // 256 has_field: 1 typed + 255 untyped → cannot approve duplication on
        // the strength of a single resolved type.
        let mut tr = vec![
            t("M", "has_field", "M.typed"),
            t("M.typed", "field_type", "i32"),
        ];
        for i in 0..255 {
            tr.push(t("M", "has_field", &format!("M.u{i}")));
        }
        let f = soc_findings(&tr);
        assert_eq!(f[0].data, 256);
        assert_ne!(f[0].verdict, SocVerdict::Duplication);
        assert_eq!(f[0].verdict, SocVerdict::Counterexample);
    }

    #[test]
    fn under_cap_is_ignored() {
        let tr = vec![t("E", "has_field", "E.a"), t("E.a", "field_type", "i32")];
        assert!(soc_findings(&tr).is_empty());
        assert!(law_holds(&tr));
    }
}
