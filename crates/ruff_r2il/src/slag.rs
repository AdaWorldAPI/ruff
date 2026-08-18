//! Stage 3b — the ADDRESSED residual ledger ("slag").
//!
//! [`crate::furnace`] melts the ore (`crate::ore::OreFact`, itself read off
//! [`crate::behavior::FunctionBehavior`]'s object graph) into flat,
//! facet-addressed rows under the CURRENT [`crate::convention::R2ilConvention`].
//! Not every fact melts. This module is the ledger of what didn't, and WHY —
//! addressed at the [`crate::facet::VarnodeFacet`] coordinate where the melt
//! failed, so a proposer can attach a new [`crate::convention::ConventionRow`]
//! at that exact address and re-run the drill.
//!
//! # The slag doctrine
//!
//! See `ruff_spo_triplet::concept_split`'s "The slag doctrine" — the same
//! discipline applies here verbatim, one layer down the stack: **the
//! residual is not waste, it is the empirical boundary of the current
//! convention.** A recurring [`ResidualReason`] across many addresses names
//! the next convention row to add, not a defect to hide.
//!
//! **`residual` is NOT to be driven to 0 by widening a match arm.** It falls
//! only when the *convention* (`R2ilConvention`) gains a row — never by
//! [`ResidualReason`] growing a catch-all that reclassifies a shape as
//! "handled" without a corresponding convention change. Every such widening
//! of the convention lands with its measured before/after residual counts in
//! the harvest ledger (see `crate::furnace::HarvestReport`), never as a
//! silent code edit here.
//!
//! # HARD RULE — no catch-all, ever
//!
//! [`ResidualReason`] has **no** `Other`, `Opaque`, `Unknown`, or `_ =>` arm
//! that manufactures a reason for a shape that fits no named variant. A
//! shape that doesn't fit an existing variant means a variant is ADDED (with
//! its before/after counts recorded), never that an existing variant is
//! widened to swallow it. [`ResidualReason::ALL`] and the
//! `there_is_no_catch_all_reason` test below exist specifically to catch a
//! future violation of this rule.
//!
//! # Addressing, not naming
//!
//! [`ResidualFact::shape_id`] is computed from the reason's discriminant and
//! its own typed payload ONLY — never from [`ResidualFact::provenance`] or
//! [`ResidualFact::at`]. Two residuals with the identical reason shape at two
//! different addresses therefore share a `shape_id` and group together
//! ([`ResidualLedger::grouped`]); the address is carried alongside so the
//! proposer still knows *where* to attach the fix ([`ResidualLedger::by_address`]).

use std::collections::BTreeMap;

use crate::facet::{FacetPrefix, VarnodeFacet};
use crate::ore::{FactProvenance, OpTag};

// FNV-1a 64, implemented inline — no hashing dependency. The exact constants
// from the FNV specification.
const FNV_OFFSET_BASIS_64: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME_64: u64 = 0x0100_0000_01b3;

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS_64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME_64);
    }
    hash
}

/// FNV-1a 64 over a residual's SHAPE — [`ResidualReason`]'s discriminant and
/// its own typed payload only. **Never** the [`FactProvenance`] and **never**
/// the [`VarnodeFacet`] address. Identical shapes at different sites
/// therefore group under the same id, exactly as MedCare-rs's `fnv1a:` class
/// fingerprints group identical DAL-method shapes across files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ShapeId(pub u64);

/// Why the CURRENT convention could not melt a fact.
///
/// See the module docs' § "The slag doctrine" and § "HARD RULE". There is
/// **no** catch-all variant. A shape that fits no variant here means a
/// variant is added — with its measured before/after residual counts — never
/// that an existing variant absorbs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidualReason {
    /// The op's [`OpTag`] is not in [`crate::convention::R2ilConvention`]'s
    /// `classified_opcodes` set.
    OpcodeNotInConvention { opcode: OpTag },
    /// `R2ilConvention::resolve` returned `None` at every prefix depth for
    /// this facet.
    NoConventionRowAtAddress,
    /// The op references a `CallOther` userop index the convention's userop
    /// table doesn't name.
    UserOpNotInConvention { userop: u32 },
    /// The varnode's `SpaceId::Custom(raw)` is not in the convention's
    /// [`crate::facet::CustomSpaceTable`].
    CustomSpaceNotInConvention { raw: u32 },
    /// The `Custom(raw)` space exists in the arch data but exceeds the
    /// interned-ordinal budget at CONFIG-KEY construction time (§4's
    /// promoted falsifier) — a typed overflow, never a silent truncation.
    FacetOverflowAtKey { raw: u32 },
    /// An op's input arity exceeds what the current convention/furnace pass
    /// is prepared to emit as fixed rows.
    VariadicArity { arity: usize },
    /// A `Phi`'s input count exceeds its block's predecessor count — the
    /// fan-in truncation `SSAFunction` construction performs by zipping
    /// sources with `cfg.predecessors(addr)`.
    PhiFanInExceedsPredecessors { inputs: usize, predecessors: usize },
    /// The memory fact's `ObjectKind` is `EscapedUnknown` — not a
    /// classifiable object for this pass.
    MemoryObjectEscaped,
    /// A branch/call target has no typed `Const`/`Ram` value — the target is
    /// indirect and this pass does not resolve indirect control flow.
    IndirectTarget,
    /// No source `Varnode` exists for this fact (a phi input or a
    /// `CallDefine` insertion) — the **only** `ResidualReason` whose
    /// [`ResidualFact::at`] is legitimately `None`.
    NoFacetCoordinate,
    /// The verified op-site join (§2's ⚠ note) found the R2IL op's tag did
    /// not match the SSA op's tag at the same `(block_addr, op_idx)` site —
    /// e.g. the `Multiequal` index shift or a `CallDefine` insertion.
    OpSiteJoinMismatch { expected: OpTag, found: OpTag },
}

impl ResidualReason {
    /// Every variant's stable `as_str()` name, for the no-catch-all test.
    /// Adding a variant to [`ResidualReason`] without adding its name here
    /// is exactly what `there_is_no_catch_all_reason` exists to catch.
    pub const ALL: &'static [&'static str] = &[
        "opcode_not_in_convention",
        "no_convention_row_at_address",
        "userop_not_in_convention",
        "custom_space_not_in_convention",
        "facet_overflow_at_key",
        "variadic_arity",
        "phi_fan_in_exceeds_predecessors",
        "memory_object_escaped",
        "indirect_target",
        "no_facet_coordinate",
        "op_site_join_mismatch",
    ];

    /// Stable `snake_case` name of the variant. Independent of payload.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            ResidualReason::OpcodeNotInConvention { .. } => "opcode_not_in_convention",
            ResidualReason::NoConventionRowAtAddress => "no_convention_row_at_address",
            ResidualReason::UserOpNotInConvention { .. } => "userop_not_in_convention",
            ResidualReason::CustomSpaceNotInConvention { .. } => "custom_space_not_in_convention",
            ResidualReason::FacetOverflowAtKey { .. } => "facet_overflow_at_key",
            ResidualReason::VariadicArity { .. } => "variadic_arity",
            ResidualReason::PhiFanInExceedsPredecessors { .. } => "phi_fan_in_exceeds_predecessors",
            ResidualReason::MemoryObjectEscaped => "memory_object_escaped",
            ResidualReason::IndirectTarget => "indirect_target",
            ResidualReason::NoFacetCoordinate => "no_facet_coordinate",
            ResidualReason::OpSiteJoinMismatch { .. } => "op_site_join_mismatch",
        }
    }

    /// FNV-1a 64 over the variant's discriminant name plus its own typed
    /// payload — **never** provenance, **never** address. See the module
    /// docs' § "Addressing, not naming".
    #[must_use]
    pub fn shape_id(&self) -> ShapeId {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(self.as_str().as_bytes());
        bytes.push(0xFF); // separator: variant name vs. payload
        match self {
            ResidualReason::OpcodeNotInConvention { opcode } => {
                bytes.extend_from_slice(opcode.as_str().as_bytes());
            }
            ResidualReason::NoConventionRowAtAddress => {}
            ResidualReason::UserOpNotInConvention { userop } => {
                bytes.extend_from_slice(&userop.to_le_bytes());
            }
            ResidualReason::CustomSpaceNotInConvention { raw } => {
                bytes.extend_from_slice(&raw.to_le_bytes());
            }
            ResidualReason::FacetOverflowAtKey { raw } => {
                bytes.extend_from_slice(&raw.to_le_bytes());
            }
            ResidualReason::VariadicArity { arity } => {
                bytes.extend_from_slice(&(*arity as u64).to_le_bytes());
            }
            ResidualReason::PhiFanInExceedsPredecessors {
                inputs,
                predecessors,
            } => {
                bytes.extend_from_slice(&(*inputs as u64).to_le_bytes());
                bytes.push(0xFE); // separator between the two payload fields
                bytes.extend_from_slice(&(*predecessors as u64).to_le_bytes());
            }
            ResidualReason::MemoryObjectEscaped => {}
            ResidualReason::IndirectTarget => {}
            ResidualReason::NoFacetCoordinate => {}
            ResidualReason::OpSiteJoinMismatch { expected, found } => {
                bytes.extend_from_slice(expected.as_str().as_bytes());
                bytes.push(0xFE);
                bytes.extend_from_slice(found.as_str().as_bytes());
            }
        }
        ShapeId(fnv1a64(&bytes))
    }
}

/// One ADDRESSED residual row: it records WHERE in varnode identity space
/// the melt failed, so a proposer can emit a proposed
/// [`crate::convention::ConventionRow`] at that address and re-run the drill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidualFact {
    /// Precomputed `reason.shape_id()` — carried on the row so
    /// [`ResidualLedger::grouped`]/[`ResidualLedger::by_address`] never
    /// recompute a hash per query.
    pub shape_id: ShapeId,
    pub reason: ResidualReason,
    /// The facet coordinate where the melt failed. `None` only for
    /// [`ResidualReason::NoFacetCoordinate`] — every other reason carries an
    /// address (`residuals_carry_the_address_they_failed_at` pins this).
    pub at: Option<VarnodeFacet>,
    /// The longest [`crate::convention::R2ilConvention`] prefix that DID
    /// resolve for this facet — where a proposer attaches the next, finer
    /// row. `None` means nothing resolved at all, so a proposal attaches at
    /// the coarsest `Space` prefix.
    pub at_prefix: Option<FacetPrefix>,
    pub provenance: FactProvenance,
}

/// The addressed residual ledger for one melt pass.
#[derive(Debug, Clone, Default)]
pub struct ResidualLedger {
    rows: Vec<ResidualFact>,
}

impl ResidualLedger {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, fact: ResidualFact) {
        self.rows.push(fact);
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    #[must_use]
    pub fn rows(&self) -> &[ResidualFact] {
        &self.rows
    }

    /// Grouped and counted by [`ShapeId`], sorted by count DESC then
    /// `ShapeId` ASC — deterministic artifact order across runs. Each group
    /// reports one example address so a proposal has a coordinate to attach
    /// at.
    #[must_use]
    pub fn grouped(&self) -> Vec<(ShapeId, &'static str, usize, Option<VarnodeFacet>)> {
        let mut groups: BTreeMap<ShapeId, (&'static str, usize, Option<VarnodeFacet>)> =
            BTreeMap::new();
        for row in &self.rows {
            let entry = groups
                .entry(row.shape_id)
                .or_insert_with(|| (row.reason.as_str(), 0, row.at));
            entry.1 += 1;
        }
        let mut out: Vec<(ShapeId, &'static str, usize, Option<VarnodeFacet>)> = groups
            .into_iter()
            .map(|(id, (name, count, at))| (id, name, count, at))
            .collect();
        out.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        out
    }

    /// Grouped by `(resolved prefix, shape)` — the proposer's actual work
    /// queue: for each address a fix would attach at, which shapes recur
    /// there and how often.
    #[must_use]
    pub fn by_address(&self) -> Vec<(Option<FacetPrefix>, ShapeId, usize)> {
        let mut groups: BTreeMap<(Option<FacetPrefix>, ShapeId), usize> = BTreeMap::new();
        for row in &self.rows {
            *groups.entry((row.at_prefix, row.shape_id)).or_insert(0) += 1;
        }
        groups
            .into_iter()
            .map(|((prefix, id), count)| (prefix, id, count))
            .collect()
    }

    /// The largest [`ShapeId`] group's share of the whole ledger, in
    /// `[0.0, 1.0]`. `0.0` for an empty ledger. A ledger where one shape
    /// absorbs (nearly) everything is a catch-all wearing a reason's
    /// clothes, and this is the number the pre-registered bar tests.
    #[must_use]
    pub fn dominant_share(&self) -> f64 {
        let total = self.rows.len();
        if total == 0 {
            return 0.0;
        }
        let dominant = self
            .grouped()
            .into_iter()
            .map(|(_, _, count, _)| count)
            .max()
            .unwrap_or(0);
        dominant as f64 / total as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prov_at_site(offset: u64, op_idx: usize) -> FactProvenance {
        FactProvenance {
            inst: None,
            block: None,
            op_site: Some((offset, op_idx)),
            value: None,
        }
    }

    fn prov_none() -> FactProvenance {
        FactProvenance {
            inst: None,
            block: None,
            op_site: None,
            value: None,
        }
    }

    #[test]
    fn shape_id_groups_identical_shapes_across_addresses() {
        let reason_a = ResidualReason::OpcodeNotInConvention {
            opcode: OpTag::AtomicCAS,
        };
        let reason_a_again = ResidualReason::OpcodeNotInConvention {
            opcode: OpTag::AtomicCAS,
        };
        let reason_b = ResidualReason::OpcodeNotInConvention {
            opcode: OpTag::CallOther,
        };

        // Same reason payload, different address/provenance -> same shape.
        let fact1 = ResidualFact {
            shape_id: reason_a.shape_id(),
            reason: reason_a,
            at: Some(VarnodeFacet([1; 16])),
            at_prefix: Some(FacetPrefix::Space { discriminant: 1 }),
            provenance: prov_at_site(0x1000, 0),
        };
        let fact2 = ResidualFact {
            shape_id: reason_a_again.shape_id(),
            reason: reason_a_again,
            at: Some(VarnodeFacet([2; 16])),
            at_prefix: Some(FacetPrefix::Space { discriminant: 9 }),
            provenance: prov_at_site(0x2000, 3),
        };
        assert_eq!(fact1.shape_id, fact2.shape_id);

        // A different reason payload (different opcode) must differ.
        assert_ne!(fact1.shape_id, reason_b.shape_id());
    }

    #[test]
    fn residuals_carry_the_address_they_failed_at() {
        let mut ledger = ResidualLedger::new();

        let reason = ResidualReason::NoConventionRowAtAddress;
        let prefix_a = FacetPrefix::Space { discriminant: 1 };
        let prefix_b = FacetPrefix::Space { discriminant: 2 };

        let addressed = |facet_byte: u8, prefix: FacetPrefix| ResidualFact {
            shape_id: reason.shape_id(),
            reason,
            at: Some(VarnodeFacet([facet_byte; 16])),
            at_prefix: Some(prefix),
            provenance: prov_none(),
        };

        // Two residuals of the same shape at the SAME prefix -> must merge
        // to one `by_address` entry with count 2.
        ledger.push(addressed(1, prefix_a));
        ledger.push(addressed(2, prefix_a));
        // One residual of the same shape at a DIFFERENT prefix -> its own
        // `by_address` entry.
        ledger.push(addressed(3, prefix_b));

        // The sanctioned exception: NoFacetCoordinate carries no address.
        let no_coord_reason = ResidualReason::NoFacetCoordinate;
        ledger.push(ResidualFact {
            shape_id: no_coord_reason.shape_id(),
            reason: no_coord_reason,
            at: None,
            at_prefix: None,
            provenance: prov_none(),
        });

        // Every row except NoFacetCoordinate carries an address.
        for row in ledger.rows() {
            if row.reason == ResidualReason::NoFacetCoordinate {
                assert!(row.at.is_none());
            } else {
                assert!(row.at.is_some());
            }
        }

        let by_addr = ledger.by_address();
        assert!(by_addr.contains(&(Some(prefix_a), reason.shape_id(), 2)));
        assert!(by_addr.contains(&(Some(prefix_b), reason.shape_id(), 1)));
        assert!(by_addr.contains(&(None, no_coord_reason.shape_id(), 1)));
        assert_eq!(by_addr.len(), 3);
    }

    #[test]
    fn grouping_is_exact_and_deterministically_ordered() {
        let mut ledger = ResidualLedger::new();

        let reason_three = ResidualReason::MemoryObjectEscaped;
        let reason_one = ResidualReason::IndirectTarget;
        let reason_two = ResidualReason::NoConventionRowAtAddress;

        let push_n = |ledger: &mut ResidualLedger, reason: ResidualReason, n: u8| {
            for i in 0..n {
                ledger.push(ResidualFact {
                    shape_id: reason.shape_id(),
                    reason,
                    at: Some(VarnodeFacet([i; 16])),
                    at_prefix: None,
                    provenance: prov_none(),
                });
            }
        };

        push_n(&mut ledger, reason_three, 3);
        push_n(&mut ledger, reason_one, 1);
        push_n(&mut ledger, reason_two, 2);

        let counts: Vec<usize> = ledger
            .grouped()
            .into_iter()
            .map(|(_, _, count, _)| count)
            .collect();
        assert_eq!(counts, vec![3, 2, 1]);

        // Deterministic: running the grouping again gives the identical order.
        let counts_again: Vec<usize> = ledger
            .grouped()
            .into_iter()
            .map(|(_, _, count, _)| count)
            .collect();
        assert_eq!(counts_again, vec![3, 2, 1]);

        let total: usize = counts.iter().sum();
        assert_eq!(total, ledger.len());
    }

    #[test]
    fn there_is_no_catch_all_reason() {
        // Exhaustive match with NO `_ =>` arm: if a variant is ever added to
        // `ResidualReason`, this fails to COMPILE until this test (and
        // `ResidualReason::ALL`) are updated to match. That non-compiling
        // failure mode is the falsifier for "no catch-all reason".
        let sample: [ResidualReason; 11] = [
            ResidualReason::OpcodeNotInConvention {
                opcode: OpTag::Copy,
            },
            ResidualReason::NoConventionRowAtAddress,
            ResidualReason::UserOpNotInConvention { userop: 0 },
            ResidualReason::CustomSpaceNotInConvention { raw: 0 },
            ResidualReason::FacetOverflowAtKey { raw: 0 },
            ResidualReason::VariadicArity { arity: 0 },
            ResidualReason::PhiFanInExceedsPredecessors {
                inputs: 0,
                predecessors: 0,
            },
            ResidualReason::MemoryObjectEscaped,
            ResidualReason::IndirectTarget,
            ResidualReason::NoFacetCoordinate,
            ResidualReason::OpSiteJoinMismatch {
                expected: OpTag::Copy,
                found: OpTag::Copy,
            },
        ];

        let mut variant_count = 0;
        for reason in sample {
            variant_count += match reason {
                ResidualReason::OpcodeNotInConvention { .. } => 1,
                ResidualReason::NoConventionRowAtAddress => 1,
                ResidualReason::UserOpNotInConvention { .. } => 1,
                ResidualReason::CustomSpaceNotInConvention { .. } => 1,
                ResidualReason::FacetOverflowAtKey { .. } => 1,
                ResidualReason::VariadicArity { .. } => 1,
                ResidualReason::PhiFanInExceedsPredecessors { .. } => 1,
                ResidualReason::MemoryObjectEscaped => 1,
                ResidualReason::IndirectTarget => 1,
                ResidualReason::NoFacetCoordinate => 1,
                ResidualReason::OpSiteJoinMismatch { .. } => 1,
                // Deliberately NO `_ =>` arm.
            };
        }
        assert_eq!(variant_count, 11);
        assert_eq!(ResidualReason::ALL.len(), variant_count);

        let mut sorted = ResidualReason::ALL.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            ResidualReason::ALL.len(),
            "ResidualReason::ALL has duplicate names"
        );

        for name in ResidualReason::ALL {
            assert!(
                !matches!(*name, "other" | "opaque" | "unknown" | "misc"),
                "catch-all-shaped reason name found: {name}"
            );
        }
    }
}
