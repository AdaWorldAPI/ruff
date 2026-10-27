//! Predicate roles — what kind of fact a predicate states.
//!
//! [`Provenance`](crate::Provenance) says how sure a triple is. This module
//! says what the triple is *about*: whether the predicate describes a node,
//! links it to another node, gives it a capability, makes it act, or guards
//! when it may act. The two axes are independent; a fact can be certain and
//! still say nothing about behaviour.
//!
//! A role is a bitmask, not a single value, because one predicate can state
//! several things at once. `writes_if_blank` is both an effect and a
//! precondition (the write happens only when the field is blank);
//! `has_function` both places a function under its model and gives the model
//! the capability that function provides.
//!
//! The role is decided once, at harvest time, by [`Predicate::role`]. That
//! function is an exhaustive `match` with no wildcard arm, so adding a
//! [`Predicate`] variant does not compile until somebody decides its role.
//! Consumers read the role from the predicate instead of comparing predicate
//! strings.

use std::collections::BTreeMap;

use crate::{Predicate, Triple};

/// A set of [`PredicateRole`] bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct PredicateRole(u16);

impl PredicateRole {
    /// What the node is: its type, a scalar attribute, a declared kind.
    pub const DESCRIBES: Self = Self(1 << 0);
    /// Where the node sits: containment (`part_of`) and composition or
    /// inheritance (`is_a`, mixins, includes).
    pub const STRUCTURE: Self = Self(1 << 1);
    /// A typed link to another node that is neither containment nor
    /// inheritance (an association, a relational field's comodel).
    pub const RELATES: Self = Self(1 << 2);
    /// The body reads this value or walks this relation.
    pub const READS: Self = Self(1 << 3);
    /// The value is computed from other values (`depends_on`, normalisers,
    /// counter caches).
    pub const DERIVES: Self = Self(1 << 4);
    /// Acting changes state or leaves the function abnormally: writes,
    /// raises, unresolved calls, persistence side effects.
    pub const EFFECTS: Self = Self(1 << 5);
    /// An invariant that holds for every valid record, independent of any
    /// one action (validations, not-null, static asserts).
    pub const CONSTRAINS: Self = Self(1 << 6);
    /// A condition that must hold before something may happen (a guarded
    /// write, a C++20 `requires` clause, a permission check).
    pub const REQUIRES: Self = Self(1 << 7);
    /// Who may see or do it (permissions, visibility, `friend`).
    pub const AUTHORIZES: Self = Self(1 << 8);
    /// The node can do this: it owns a method, operator, scope or route.
    pub const ENABLES: Self = Self(1 << 9);
    /// Something happening causes this to run (callbacks, event handlers, a
    /// screen invoking an action).
    pub const TRIGGERS: Self = Self(1 << 10);
    /// How the node is shown or navigated in a user interface.
    pub const PRESENTS: Self = Self(1 << 11);
    /// The harvester could not tell what the fact means (the catch-all
    /// `has_dsl_call`). Kept distinct from the empty set so that "unknown"
    /// is never mistaken for "no role".
    pub const OPAQUE: Self = Self(1 << 12);

    /// Description: what the node is and how it is connected or shown.
    pub const DESCRIPTION: Self =
        Self(Self::DESCRIBES.0 | Self::STRUCTURE.0 | Self::RELATES.0 | Self::PRESENTS.0);
    /// Capability: what the node can do.
    pub const CAPABILITY: Self = Self::ENABLES;
    /// The DO arm: facts about acting.
    pub const DO_ARM: Self = Self(Self::EFFECTS.0 | Self::TRIGGERS.0);
    /// Preconditions: what must hold before acting, and who may act.
    pub const PRECONDITION: Self = Self(Self::REQUIRES.0 | Self::AUTHORIZES.0);
    /// Every guard, including invariants that are not tied to one action.
    pub const GUARD: Self = Self(Self::CONSTRAINS.0 | Self::REQUIRES.0 | Self::AUTHORIZES.0);
    /// Dataflow: what is read and what is computed from it.
    pub const DATAFLOW: Self = Self(Self::READS.0 | Self::DERIVES.0);

    /// Every single-bit role with its name, in bit order.
    pub const NAMED: [(Self, &'static str); 13] = [
        (Self::DESCRIBES, "describes"),
        (Self::STRUCTURE, "structure"),
        (Self::RELATES, "relates"),
        (Self::READS, "reads"),
        (Self::DERIVES, "derives"),
        (Self::EFFECTS, "effects"),
        (Self::CONSTRAINS, "constrains"),
        (Self::REQUIRES, "requires"),
        (Self::AUTHORIZES, "authorizes"),
        (Self::ENABLES, "enables"),
        (Self::TRIGGERS, "triggers"),
        (Self::PRESENTS, "presents"),
        (Self::OPAQUE, "opaque"),
    ];

    /// The empty set.
    pub const NONE: Self = Self(0);

    /// The raw bits.
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// The union of two role sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// True if every bit of `other` is set in `self`.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// True if `self` and `other` share at least one bit.
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// True if no bit is set.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The names of the bits set in `self`, in bit order.
    pub fn names(self) -> impl Iterator<Item = &'static str> {
        Self::NAMED
            .into_iter()
            .filter(move |(bit, _)| self.contains(*bit))
            .map(|(_, name)| name)
    }
}

impl std::ops::BitOr for PredicateRole {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl Predicate {
    /// What kind of fact this predicate states.
    ///
    /// Exhaustive by design: a new variant fails to compile here until its
    /// role is decided. `calls` is classed as an effect because a dispatch
    /// whose target is not resolved may change state; the absence of a
    /// `calls` fact is not evidence that a body dispatches nothing, because
    /// not every frontend harvests calls.
    pub const fn role(self) -> PredicateRole {
        use PredicateRole as R;
        match self {
            Self::RdfType
            | Self::AliasesAttribute
            | Self::ColumnOverride
            | Self::FieldType
            | Self::AssociationKind
            | Self::RelationKind
            | Self::UsesMacroExpansion
            | Self::IsPureVirtual
            | Self::IsConstexpr
            | Self::IsNoexcept
            | Self::ReturnsType
            | Self::HasParamType
            | Self::IsConst
            | Self::IsStatic
            | Self::RouteScope => R::DESCRIBES,

            Self::IncludesModule
            | Self::ExtendsModule
            | Self::PrependsModule
            | Self::ConcernClassMethods
            | Self::ConcernIncludedBlock
            | Self::HasAttribute
            | Self::HasClosureTree
            | Self::UsesRefinement
            | Self::InheritsFrom
            | Self::HasField
            | Self::TemplateSpecialises
            | Self::TemplateInstantiates
            | Self::VirtuallyOverrides
            | Self::PartOf => R::STRUCTURE,

            Self::HasFunction | Self::ActsAs | Self::DefinesMethod => {
                R::STRUCTURE.union(R::ENABLES)
            }

            Self::DeclaresAssociation | Self::ClassName | Self::Target | Self::InverseName => {
                R::RELATES
            }

            Self::ReadsField | Self::TraversesRelation => R::READS,

            Self::DependsOn | Self::NormalizesAttribute | Self::AutoStrips => R::DERIVES,
            Self::CounterCultures => R::DERIVES.union(R::EFFECTS),

            Self::EmittedBy
            | Self::Raises
            | Self::MountsUploader
            | Self::HasPaperTrail
            | Self::WritesField
            | Self::Calls => R::EFFECTS,
            Self::WritesIfBlank => R::EFFECTS.union(R::REQUIRES),

            Self::ValidatesConstraint
            | Self::HasDefaultScope
            | Self::ValidationKind
            | Self::ValidationParam
            | Self::StaticAsserts
            | Self::ColumnNotNull => R::CONSTRAINS,

            Self::RequiresConcept => R::REQUIRES,
            Self::GuardedByPermission => R::AUTHORIZES.union(R::REQUIRES),
            Self::IsFriendOf | Self::HasVisibility => R::AUTHORIZES,

            Self::AliasesMethod | Self::HasScope | Self::DefinesOperator | Self::RoutesTo => {
                R::ENABLES
            }
            Self::DelegatesTo => R::ENABLES.union(R::RELATES),

            Self::HasCallback | Self::HandlesEvent => R::TRIGGERS,
            Self::InvokesAction => R::TRIGGERS.union(R::PRESENTS),

            Self::RegistersJournalFormatter
            | Self::RegistersJournalFormattedFields
            | Self::SelectsView
            | Self::RendersAs
            | Self::DockedAt
            | Self::TabOrder
            | Self::Purpose => R::PRESENTS,
            Self::NavigatesTo | Self::SurfacesConcept | Self::OpensPopup => {
                R::PRESENTS.union(R::RELATES)
            }
            Self::ContainsControl => R::PRESENTS.union(R::STRUCTURE),

            Self::HasDslCall => R::OPAQUE,
        }
    }
}

/// How often each role occurs in a set of triples.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoleCensus {
    /// Triples counted.
    pub triples: usize,
    /// Per bit of [`PredicateRole::NAMED`], in the same order: how many
    /// triples carry that bit. A triple with several bits counts once per
    /// bit, so these do not sum to `triples`.
    pub per_role: [usize; 13],
    /// Triples whose predicate is not in the closed vocabulary, by
    /// predicate string. These get no role; they are reported, not guessed.
    pub unknown: BTreeMap<String, usize>,
}

impl RoleCensus {
    /// The count for one named role bit, or `None` for a multi-bit set.
    pub fn count(&self, role: PredicateRole) -> Option<usize> {
        PredicateRole::NAMED
            .iter()
            .position(|(bit, _)| *bit == role)
            .map(|i| self.per_role[i])
    }

    /// The named role bits no triple carries.
    pub fn absent(&self) -> impl Iterator<Item = &'static str> + '_ {
        PredicateRole::NAMED
            .iter()
            .zip(self.per_role)
            .filter(|(_, n)| *n == 0)
            .map(|((_, name), _)| *name)
    }
}

/// Count the roles carried by `triples`.
pub fn role_census(triples: &[Triple]) -> RoleCensus {
    let mut census = RoleCensus {
        triples: triples.len(),
        ..RoleCensus::default()
    };
    for t in triples {
        let Some(predicate) = Predicate::from_str(&t.p) else {
            *census.unknown.entry(t.p.clone()).or_default() += 1;
            continue;
        };
        let role = predicate.role();
        for (i, (bit, _)) in PredicateRole::NAMED.iter().enumerate() {
            if role.contains(*bit) {
                census.per_role[i] += 1;
            }
        }
    }
    census
}

#[cfg(test)]
mod tests {
    use super::*;

    fn triple(p: &str) -> Triple {
        Triple {
            s: "odoo:m.a".to_string(),
            p: p.to_string(),
            o: "odoo:m.b".to_string(),
            f: 1.0,
            c: 1.0,
        }
    }

    #[test]
    fn every_predicate_has_a_role_and_no_stray_bits() {
        let known = PredicateRole::NAMED
            .iter()
            .fold(PredicateRole::NONE, |acc, (bit, _)| acc | *bit);
        for &p in Predicate::ALL {
            let role = p.role();
            assert!(!role.is_empty(), "{} has no role", p.as_str());
            assert!(known.contains(role), "{} has an unnamed bit", p.as_str());
        }
    }

    #[test]
    fn opaque_is_used_only_by_the_catch_all() {
        let opaque: Vec<&str> = Predicate::ALL
            .iter()
            .filter(|p| p.role().intersects(PredicateRole::OPAQUE))
            .map(|p| p.as_str())
            .collect();
        assert_eq!(opaque, ["has_dsl_call"]);
    }

    #[test]
    fn the_four_kinds_the_operator_named_are_each_reachable() {
        assert!(
            Predicate::HasFunction
                .role()
                .intersects(PredicateRole::CAPABILITY)
        );
        assert!(
            Predicate::EmittedBy
                .role()
                .intersects(PredicateRole::DO_ARM)
        );
        assert!(
            Predicate::GuardedByPermission
                .role()
                .intersects(PredicateRole::PRECONDITION)
        );
        assert!(
            Predicate::RdfType
                .role()
                .intersects(PredicateRole::DESCRIPTION)
        );
        assert_eq!(Predicate::DependsOn.role(), PredicateRole::DERIVES);
    }

    #[test]
    fn a_description_is_not_an_action_or_a_guard() {
        for p in [Predicate::RdfType, Predicate::FieldType, Predicate::Target] {
            assert!(
                !p.role().intersects(PredicateRole::DO_ARM),
                "{}",
                p.as_str()
            );
            assert!(!p.role().intersects(PredicateRole::GUARD), "{}", p.as_str());
        }
        assert!(
            !Predicate::ReadsField
                .role()
                .intersects(PredicateRole::EFFECTS)
        );
    }

    #[test]
    fn a_guarded_write_is_both_an_effect_and_a_precondition() {
        let role = Predicate::WritesIfBlank.role();
        assert!(role.contains(PredicateRole::EFFECTS));
        assert!(role.contains(PredicateRole::REQUIRES));
        assert!(
            !Predicate::WritesField
                .role()
                .contains(PredicateRole::REQUIRES)
        );
    }

    #[test]
    fn census_counts_each_bit_and_reports_unknown_predicates() {
        let triples = [
            triple("depends_on"),
            triple("depends_on"),
            triple("writes_if_blank"),
            triple("rdf:type"),
            triple("no_such_predicate"),
        ];
        let census = role_census(&triples);
        assert_eq!(census.triples, 5);
        assert_eq!(census.count(PredicateRole::DERIVES), Some(2));
        assert_eq!(census.count(PredicateRole::EFFECTS), Some(1));
        assert_eq!(census.count(PredicateRole::REQUIRES), Some(1));
        assert_eq!(census.count(PredicateRole::DESCRIBES), Some(1));
        assert_eq!(census.unknown.get("no_such_predicate"), Some(&1));
        assert_eq!(census.count(PredicateRole::DO_ARM), None);
        let absent: Vec<&str> = census.absent().collect();
        assert!(absent.contains(&"authorizes"));
        assert!(!absent.contains(&"derives"));
    }
}
