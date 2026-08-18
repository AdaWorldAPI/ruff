//! Stage 3 — the **furnace**: melts the stage-1/2 object graph into flat, facet-addressed,
//! concern-separated rows.
//!
//! [`FlatFact`] is `FactId + VarnodeFacet + Concern + fixed scalar payload` — plain flat
//! `Vec<FlatFact>`. **It must never grow back into a nested object graph.** No `Vec`, no `Box`,
//! no map, no `String` inside a `FlatFact`; cardinality is handled by EMITTING MORE ROWS (a
//! `CallOther` with four inputs is one [`FactKind::Op`] row plus four [`FactKind::OperandIn`]
//! rows), never by nesting a collection. A `FlatFact` refers to another only by [`FactId`] (via
//! its shared `prov.inst`/`prov.op_site` — `furnace.rs` never invents a graph edge between rows).
//!
//! # The whole pass-1 ladder — nothing else classifies
//!
//! - An [`crate::ore::OreFact::Op`] melts iff `conv.classifies(opcode)` **and** a block-anchor
//!   facet is derivable (`facet::project(&Varnode::ram(block_addr, 0), conv.spaces())`, from the
//!   op's own `prov.op_site`). When it doesn't classify, the residual reason is
//!   [`crate::slag::ResidualReason::VariadicArity`] when `input_arity` exceeds
//!   [`VARIADIC_ARITY_THRESHOLD`] (a "this needs Vec-routing regardless of classification" signal
//!   — CallOther's four-input stressor is the fixture that motivates the threshold value, see its
//!   doc comment), else [`crate::slag::ResidualReason::OpcodeNotInConvention`].
//! - An [`crate::ore::OreFact::Operand`] melts iff its OWN varnode facet-projects
//!   **and** its parent op (by `prov.inst`) melted **and** `conv.resolve(&facet)` is `Some`.
//!   **Facet-projection failure is checked FIRST, independent of the parent-melted gate** — an
//!   operand referencing a `SpaceId::Custom` raw id the convention's space table doesn't know
//!   fails with [`crate::slag::ResidualReason::CustomSpaceNotInConvention`] even when its parent
//!   op also fails to classify (both residuals are real and both are pushed, from two distinct
//!   ore facts: the `Op` row and the `Operand` row — never one row absorbing two reasons).
//! - [`crate::ore::OreFact::Edge`], [`crate::ore::OreFact::MemoryUse`],
//!   [`crate::ore::OreFact::MemoryDef`], [`crate::ore::OreFact::Predicate`],
//!   [`crate::ore::OreFact::CallSite`] are all BLOCK-ANCHORED (they carry no varnode of their
//!   own): `at = facet::project(&Varnode::ram(block_addr, 0), conv.spaces())`, using the
//!   enclosing op's `prov.op_site` (or, for `Edge`, the source block's own address — `Edge`
//!   carries no `prov` at all, see its own arm below).
//!   - `MemoryUse`/`MemoryDef` additionally need their `object` to resolve to a NON-`EscapedUnknown`
//!     [`r2ssa::ObjectKind`] (`ResidualReason::MemoryObjectEscaped` otherwise).
//!   - `CallSite` additionally needs `direct_target.is_some()` (`ResidualReason::IndirectTarget`
//!     otherwise).
//!   - `Predicate` and `Edge` have no additional per-row condition beyond their parent melting
//!     (`Edge` has no parent at all — see below).
//!   - All four (Edge excepted) gate on their parent op having melted, exactly like `Operand`.
//! - [`crate::ore::OreFact::PhiInput`] **never** becomes a [`FlatFact`] — `FlatFact.at` is a
//!   non-optional [`VarnodeFacet`], and a phi input has no source varnode at all (this crate's
//!   upstream-facts table: `SSAVar` carries no offset and no `SpaceId`, and a phi input is an
//!   abstract SSA join edge, not a single varnode occurrence). It is ALWAYS residual: the
//!   well-formed case (`index < predecessors_count` for the merge block) is
//!   [`crate::slag::ResidualReason::NoFacetCoordinate`] (`at: None`, matching the spec text
//!   naming phi inputs as the row kind whose facet is legitimately absent); the anomalous case
//!   (`index >= predecessors_count`, which should not occur given `SSAFunction`'s own fan-in
//!   zip — see this crate's upstream-facts table) is
//!   [`crate::slag::ResidualReason::PhiFanInExceedsPredecessors`], best-effort block-anchored via
//!   `prov.block` so it still carries an address.
//! - [`crate::ore::OreFact::JoinFailure`] never melts —
//!   [`crate::slag::ResidualReason::OpSiteJoinMismatch`], block-anchored via `prov.op_site`.
//!
//! There is no third outcome besides melt/slag: every [`crate::ore::OreFact`] produces EXACTLY
//! one [`FlatFact`] or EXACTLY one [`crate::slag::ResidualFact`], so `dropped == 0` holds by
//! construction of [`smelt`]'s control flow, not by a separate bookkeeping check.
//!
//! # `FlatFact` payload table (`a`, `b` — both `u64`)
//!
//! | `kind` | `a` | `b` |
//! |---|---|---|
//! | [`FactKind::Op`] | the source instruction's `ordinal` within its block | `input_arity` in bits `0..32`, `has_output` (`0`/`1`) in bit `32` |
//! | [`FactKind::OperandIn`] | the input index (`OperandPos::Input(i)`) | the operand's `ValueId.0 + 1`, or `0` when the operand carries no SSA value |
//! | [`FactKind::OperandOut`] | unused, always `0` (an output has no index) | the operand's `ValueId.0 + 1`, or `0` |
//! | [`FactKind::Edge`] | the destination `BlockId.0`, widened | the [`crate::ore::EdgeTag`] ordinal: `Normal=0, True=1, False=2, Back=3` |
//! | [`FactKind::MemUse`] | the `ObjectId.0`, widened | `version` in bits `0..32`, `size` in bits `32..64` |
//! | [`FactKind::MemDef`] | `previous` in bits `0..32`, `next` in bits `32..64` | the `ObjectId.0` in bits `0..32`, `size` in bits `32..64` |
//! | [`FactKind::Predicate`] | `true_target` (a real code address) | `false_target` (a real code address) |
//! | [`FactKind::CallSite`] | `direct_target` (only ever melted when `Some`) | the call target's `ValueId.0`, widened |
//!
//! `PhiInput` never appears as a `FlatFact.kind` — see the ladder above. `condition` and
//! `comparison` on a melted `Predicate` row are NOT captured in the fixed two-slot payload (a
//! documented, honest PR-1 loss, not a silent drop — the source `OreFact::Predicate` retains full
//! fidelity; a future pass may split `Predicate` into a targets row and a comparison row rather
//! than widen the payload past two `u64` slots). `id` on `Predicate`/`CallSite` is likewise not
//! duplicated into the payload — it is recoverable from `prov.inst`.

use std::collections::BTreeMap;

use r2il::{R2ILBlock, Varnode};
use r2ssa::{BlockId, ObjectKind};

use crate::behavior::FunctionBehavior;
use crate::convention::R2ilConvention;
use crate::facet::{self, FacetOverflow, VarnodeFacet};
use crate::ore::{self, EdgeTag, FactProvenance, OpTag, OperandPos, OreFact};
use crate::slag::{ResidualFact, ResidualLedger, ResidualReason};

/// An op's `input_arity` strictly above this threshold gets
/// [`crate::slag::ResidualReason::VariadicArity`] instead of
/// [`crate::slag::ResidualReason::OpcodeNotInConvention`] when it fails to classify.
///
/// `JUDGMENT` — the plan names the residual but not the exact cutoff. Pinned against the §8/§10
/// stressor block's own arities: `AtomicCAS`/`StoreGuarded`/`Insert` all have arity 3 and are
/// expected to surface as `OpcodeNotInConvention`; the 4-input `CallOther` is the ONLY op in that
/// fixture with arity 4 and is expected to surface as `VariadicArity{arity: 4}` — `3` is the
/// largest threshold consistent with both halves of that expectation.
const VARIADIC_ARITY_THRESHOLD: usize = 3;

// ================================================================================================
// FactId / Concern / FactKind
// ================================================================================================

/// The index of a [`FlatFact`] within one [`smelt`] call's output `Vec`. Assigned sequentially in
/// emission order; NOT a `prov.inst`/`prov.op_site` alias — those already carry the upstream
/// identity, this is purely the flat row's own position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactId(pub u32);

/// The route a [`FlatFact`] belongs to — r2sleigh's own decomposition, named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Concern {
    /// [`FactKind::Op`], [`FactKind::Edge`] — instruction identity and control-flow structure.
    Control,
    /// [`FactKind::OperandIn`], [`FactKind::OperandOut`] — SSA value flow.
    Values,
    /// Reserved for a future object-lifecycle `FactKind`; no pass-1 `FactKind` uses it.
    Objects,
    /// [`FactKind::MemUse`], [`FactKind::MemDef`] — memory SSA versioning.
    Memory,
    /// [`FactKind::Predicate`].
    Predicates,
    /// [`FactKind::CallSite`].
    Calls,
}

/// Which shape of ore fact a [`FlatFact`] flattens. See the module docs' payload table for the
/// `a`/`b` meaning per variant. `PhiInput` from [`crate::ore::OreFact`] has no corresponding
/// variant here — it never melts (module docs, "the whole pass-1 ladder").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactKind {
    Op,
    OperandIn,
    OperandOut,
    Edge,
    MemUse,
    MemDef,
    Predicate,
    CallSite,
}

impl FactKind {
    /// Stable `snake_case` name, for [`Census`]. Never derived from `Debug`.
    pub fn as_str(self) -> &'static str {
        match self {
            FactKind::Op => "op",
            FactKind::OperandIn => "operand_in",
            FactKind::OperandOut => "operand_out",
            FactKind::Edge => "edge",
            FactKind::MemUse => "mem_use",
            FactKind::MemDef => "mem_def",
            FactKind::Predicate => "predicate",
            FactKind::CallSite => "call_site",
        }
    }
}

// ================================================================================================
// FlatFact
// ================================================================================================

/// ONE flat row. No `Vec`, no `Box`, no map, no `String` — see the module docs' opening
/// paragraph. The compile-time guard below is the never-a-nested-graph regression test,
/// mechanised: a field addition that breaks `Copy` or grows the struct past the pinned budget
/// fails the BUILD, not merely a test run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlatFact {
    pub id: FactId,
    /// The drill key. Operand rows carry their OWN varnode's facet; rows with no varnode of
    /// their own are BLOCK-ANCHORED — `facet::project(&Varnode::ram(block_addr, 0), spaces)` —
    /// a documented convention (see the module docs' ladder), never an implicit default.
    pub at: VarnodeFacet,
    pub concern: Concern,
    pub kind: FactKind,
    pub opcode: OpTag,
    /// Two fixed typed payload slots. Meaning per `kind`: the module docs' payload table.
    pub a: u64,
    pub b: u64,
    pub prov: FactProvenance,
}

/// The never-a-nested-graph guard, mechanised: `FlatFact` must stay `Copy` and must not exceed a
/// 64-byte budget. `JUDGMENT` / flag for the orchestrator: `FactProvenance` (owned by `ore.rs`)
/// carries four `Option` fields, one of which (`op_site: Option<(u64, usize)>`) has no available
/// niche and is therefore ABI-sized around 24 bytes on a 64-bit target; combined with `at`'s 16
/// bytes, `a`/`b`'s 16 bytes, and `id`/`concern`/`kind`/`opcode`'s handful of bytes, `FlatFact` is
/// very plausibly OVER 64 bytes as spec'd with `prov: FactProvenance` inlined verbatim. This
/// assert is written exactly as specified (`assert!(size_of::<FlatFact>() <= 64)`) rather than
/// silently dropping or shrinking the `prov` field — a per-worker file may not improvise a
/// different `FlatFact` shape than the one the spec gives verbatim. If this fails to compile, the
/// reconciliation belongs to whoever owns the cross-file budget decision (shrink
/// `FactProvenance`, or widen the byte budget, or turn `prov` into a `FactId`-style index rather
/// than an inline copy) — not to a silent per-file deviation.
const _: () = assert!(core::mem::size_of::<FlatFact>() <= 64);

// ================================================================================================
// HarvestReport / Census
// ================================================================================================

/// The conservation ledger — the `harvested N / classified X / residual Y / dropped 0` line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HarvestReport {
    pub harvested: usize,
    pub classified: usize,
    pub residual: usize,
    pub dropped: usize,
}

impl HarvestReport {
    /// `harvested == classified + residual` AND `dropped == 0`.
    #[must_use]
    pub fn is_conserved(&self) -> bool {
        self.dropped == 0 && self.harvested == self.classified + self.residual
    }
}

/// Per-fact-kind and per-opcode counts over one [`smelt`]'s `Vec<FlatFact>` output —
/// the "N triples by predicate" analog. `BTreeMap`, so the census artifact is byte-stable across
/// runs (matching `FactKind`/`OpTag`'s own `as_str()` discipline: never a `Debug`-derived key).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Census {
    pub by_fact_kind: BTreeMap<&'static str, usize>,
    pub by_opcode: BTreeMap<&'static str, usize>,
}

#[must_use]
pub fn census(rows: &[FlatFact]) -> Census {
    let mut by_fact_kind: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut by_opcode: BTreeMap<&'static str, usize> = BTreeMap::new();
    for row in rows {
        *by_fact_kind.entry(row.kind.as_str()).or_insert(0) += 1;
        *by_opcode.entry(row.opcode.as_str()).or_insert(0) += 1;
    }
    Census { by_fact_kind, by_opcode }
}

// ================================================================================================
// smelt
// ================================================================================================

/// Melt one function's ore into flat, addressed rows under `conv`. `Ok` rows are flat and
/// addressed; everything else is addressed slag. See the module docs for the complete pass-1
/// ladder — this is the ONLY place that ladder is implemented; widening classification is a
/// `R2ilConvention` data change, never a new match arm here.
#[must_use]
pub fn smelt(
    behavior: &FunctionBehavior,
    blocks: &[R2ILBlock],
    conv: &R2ilConvention,
) -> (Vec<FlatFact>, ResidualLedger, HarvestReport) {
    let ore_facts = ore::enumerate(behavior, blocks);
    let harvested = ore_facts.len();

    let mut flat: Vec<FlatFact> = Vec::new();
    let mut ledger = ResidualLedger::new();
    // `InstId.0` -> (did this instruction's Op row actually melt?, its OpTag). Populated as we
    // go; every Op row for a given inst is guaranteed (by `ore::enumerate`'s documented
    // enumeration order — "per op the Op row, then Operand rows...") to be processed before any
    // dependent fact referencing the same `prov.inst`, so a single forward pass suffices.
    let mut op_status: BTreeMap<u32, (bool, OpTag)> = BTreeMap::new();

    for fact in ore_facts {
        match fact {
            OreFact::Op { prov, opcode, ordinal, input_arity, has_output } => {
                let at = block_anchor_facet(prov.op_site, conv);
                let classifies = conv.classifies(opcode);
                let melted = classifies && at.is_some();
                if let Some(inst) = prov.inst {
                    op_status.insert(inst.0, (melted, opcode));
                }
                if !classifies {
                    let reason = if input_arity > VARIADIC_ARITY_THRESHOLD {
                        ResidualReason::VariadicArity { arity: input_arity }
                    } else {
                        ResidualReason::OpcodeNotInConvention { opcode }
                    };
                    push_named_residual(&mut ledger, reason, at, conv, prov);
                } else if let Some(at) = at {
                    flat.push(FlatFact {
                        id: FactId(flat.len() as u32),
                        at,
                        concern: Concern::Control,
                        kind: FactKind::Op,
                        opcode,
                        a: ordinal as u64,
                        b: pack_op_metadata(input_arity, has_output),
                        prov,
                    });
                } else {
                    push_named_residual(&mut ledger, ResidualReason::NoFacetCoordinate, None, conv, prov);
                }
            }

            OreFact::Operand { prov, position, value, space, offset, size } => {
                let (parent_melted, parent_opcode) = prov
                    .inst
                    .and_then(|inst| op_status.get(&inst.0).copied())
                    // Structurally unreachable given `ore::enumerate`'s Op-before-Operand
                    // ordering guarantee (module docs) — `OpTag::Copy` is an arbitrary
                    // placeholder for the "not melted" fallback this can never actually take.
                    .unwrap_or((false, OpTag::Copy));
                let varnode = Varnode::new(space, offset, size);
                match facet::project(&varnode, conv.spaces()) {
                    Ok(own_facet) => {
                        if !parent_melted {
                            push_named_residual(
                                &mut ledger,
                                ResidualReason::OpcodeNotInConvention { opcode: parent_opcode },
                                Some(own_facet),
                                conv,
                                prov,
                            );
                        } else if conv.resolve(&own_facet).is_some() {
                            let kind = match position {
                                OperandPos::Input(_) => FactKind::OperandIn,
                                OperandPos::Output => FactKind::OperandOut,
                            };
                            let a = match position {
                                OperandPos::Input(idx) => idx as u64,
                                OperandPos::Output => 0,
                            };
                            let b = value.map(|v| v.0 as u64 + 1).unwrap_or(0);
                            flat.push(FlatFact {
                                id: FactId(flat.len() as u32),
                                at: own_facet,
                                concern: Concern::Values,
                                kind,
                                opcode: parent_opcode,
                                a,
                                b,
                                prov,
                            });
                        } else {
                            push_named_residual(
                                &mut ledger,
                                ResidualReason::NoConventionRowAtAddress,
                                Some(own_facet),
                                conv,
                                prov,
                            );
                        }
                    }
                    Err(FacetOverflow::UnknownCustomSpace { raw }) => {
                        // Own-facet projection failed before the parent-melted gate is even
                        // consulted — see the module docs' ladder note on priority. Falls back
                        // to the enclosing op's block-anchor so the residual still carries an
                        // address (every reason but `NoFacetCoordinate` must).
                        let at = block_anchor_facet(prov.op_site, conv);
                        push_named_residual(
                            &mut ledger,
                            ResidualReason::CustomSpaceNotInConvention { raw },
                            at,
                            conv,
                            prov,
                        );
                    }
                    Err(FacetOverflow::CustomOrdinalExhausted { count }) => {
                        // Structurally unreachable for a validly-constructed `CustomSpaceTable`
                        // (the budget is enforced at `from_ids`/`from_arch` time) — handled
                        // rather than unwrapped, per the crate's no-panic discipline.
                        let at = block_anchor_facet(prov.op_site, conv);
                        push_named_residual(
                            &mut ledger,
                            ResidualReason::FacetOverflowAtKey { raw: count as u32 },
                            at,
                            conv,
                            prov,
                        );
                    }
                }
            }

            OreFact::Edge { from, to, kind } => {
                // `Edge` carries no `prov` at all (`crate::ore::OreFact::Edge`'s own definition)
                // — there is no parent op to gate on, so a CFG edge melts unconditionally given a
                // derivable block-anchor facet. `JUDGMENT`: CFG topology is intrinsic model
                // structure (a block's successor list exists independent of whether its
                // terminating op happens to be classified), unlike every other fact kind, which
                // is gated on its owning instruction.
                let from_addr = block_addr_of(behavior, from);
                let at = from_addr
                    .and_then(|addr| facet::project(&Varnode::ram(addr, 0), conv.spaces()).ok());
                let opcode = from_addr.and_then(|addr| terminator_opcode(blocks, addr));
                let synth_prov = FactProvenance { inst: None, block: Some(from), op_site: None, value: None };
                match (at, opcode) {
                    (Some(at), Some(opcode)) => {
                        flat.push(FlatFact {
                            id: FactId(flat.len() as u32),
                            at,
                            concern: Concern::Control,
                            kind: FactKind::Edge,
                            opcode,
                            a: to.0 as u64,
                            b: edge_tag_ordinal(kind),
                            prov: synth_prov,
                        });
                    }
                    _ => {
                        push_named_residual(&mut ledger, ResidualReason::NoFacetCoordinate, None, conv, synth_prov);
                    }
                }
            }

            OreFact::PhiInput { prov, index, .. } => {
                // Never melts — see the module docs' ladder. Fan-in well-formedness (index vs.
                // the merge block's real predecessor count) selects WHICH residual reason
                // applies, not whether one applies at all.
                let predecessor_count = prov
                    .block
                    .and_then(|block| block_addr_of(behavior, block))
                    .map(|addr| behavior.control().predecessors(addr).len());
                match predecessor_count {
                    Some(count) if index >= count => {
                        let at = prov
                            .block
                            .and_then(|block| block_addr_of(behavior, block))
                            .and_then(|addr| facet::project(&Varnode::ram(addr, 0), conv.spaces()).ok());
                        push_named_residual(
                            &mut ledger,
                            ResidualReason::PhiFanInExceedsPredecessors { inputs: index + 1, predecessors: count },
                            at,
                            conv,
                            prov,
                        );
                    }
                    _ => {
                        push_named_residual(&mut ledger, ResidualReason::NoFacetCoordinate, None, conv, prov);
                    }
                }
            }

            OreFact::MemoryUse { prov, object, version, size } => {
                let (parent_melted, parent_opcode) = prov
                    .inst
                    .and_then(|inst| op_status.get(&inst.0).copied())
                    .unwrap_or((false, OpTag::Copy));
                let at = block_anchor_facet(prov.op_site, conv);
                let escaped = object_is_escaped(behavior, object);
                if !parent_melted {
                    push_named_residual(
                        &mut ledger,
                        ResidualReason::OpcodeNotInConvention { opcode: parent_opcode },
                        at,
                        conv,
                        prov,
                    );
                } else if escaped {
                    push_named_residual(&mut ledger, ResidualReason::MemoryObjectEscaped, at, conv, prov);
                } else if let Some(at) = at {
                    flat.push(FlatFact {
                        id: FactId(flat.len() as u32),
                        at,
                        concern: Concern::Memory,
                        kind: FactKind::MemUse,
                        opcode: parent_opcode,
                        a: object.0 as u64,
                        b: (version as u64) | ((size as u64) << 32),
                        prov,
                    });
                } else {
                    push_named_residual(&mut ledger, ResidualReason::NoFacetCoordinate, None, conv, prov);
                }
            }

            OreFact::MemoryDef { prov, object, previous, next, size } => {
                let (parent_melted, parent_opcode) = prov
                    .inst
                    .and_then(|inst| op_status.get(&inst.0).copied())
                    .unwrap_or((false, OpTag::Copy));
                let at = block_anchor_facet(prov.op_site, conv);
                let escaped = object_is_escaped(behavior, object);
                if !parent_melted {
                    push_named_residual(
                        &mut ledger,
                        ResidualReason::OpcodeNotInConvention { opcode: parent_opcode },
                        at,
                        conv,
                        prov,
                    );
                } else if escaped {
                    push_named_residual(&mut ledger, ResidualReason::MemoryObjectEscaped, at, conv, prov);
                } else if let Some(at) = at {
                    flat.push(FlatFact {
                        id: FactId(flat.len() as u32),
                        at,
                        concern: Concern::Memory,
                        kind: FactKind::MemDef,
                        opcode: parent_opcode,
                        a: (previous as u64) | ((next as u64) << 32),
                        b: (object.0 as u64) | ((size as u64) << 32),
                        prov,
                    });
                } else {
                    push_named_residual(&mut ledger, ResidualReason::NoFacetCoordinate, None, conv, prov);
                }
            }

            OreFact::Predicate { prov, true_target, false_target, .. } => {
                let (parent_melted, parent_opcode) = prov
                    .inst
                    .and_then(|inst| op_status.get(&inst.0).copied())
                    .unwrap_or((false, OpTag::Copy));
                let at = block_anchor_facet(prov.op_site, conv);
                if !parent_melted {
                    push_named_residual(
                        &mut ledger,
                        ResidualReason::OpcodeNotInConvention { opcode: parent_opcode },
                        at,
                        conv,
                        prov,
                    );
                } else if let Some(at) = at {
                    flat.push(FlatFact {
                        id: FactId(flat.len() as u32),
                        at,
                        concern: Concern::Predicates,
                        kind: FactKind::Predicate,
                        opcode: parent_opcode,
                        a: true_target,
                        b: false_target,
                        prov,
                    });
                } else {
                    push_named_residual(&mut ledger, ResidualReason::NoFacetCoordinate, None, conv, prov);
                }
            }

            OreFact::CallSite { prov, target, direct_target, .. } => {
                let (parent_melted, parent_opcode) = prov
                    .inst
                    .and_then(|inst| op_status.get(&inst.0).copied())
                    .unwrap_or((false, OpTag::Copy));
                let at = block_anchor_facet(prov.op_site, conv);
                if !parent_melted {
                    push_named_residual(
                        &mut ledger,
                        ResidualReason::OpcodeNotInConvention { opcode: parent_opcode },
                        at,
                        conv,
                        prov,
                    );
                } else if let Some(target_addr) = direct_target {
                    if let Some(at) = at {
                        flat.push(FlatFact {
                            id: FactId(flat.len() as u32),
                            at,
                            concern: Concern::Calls,
                            kind: FactKind::CallSite,
                            opcode: parent_opcode,
                            a: target_addr,
                            b: target.0 as u64,
                            prov,
                        });
                    } else {
                        push_named_residual(&mut ledger, ResidualReason::NoFacetCoordinate, None, conv, prov);
                    }
                } else {
                    push_named_residual(&mut ledger, ResidualReason::IndirectTarget, at, conv, prov);
                }
            }

            OreFact::JoinFailure { prov, expected, found } => {
                let at = block_anchor_facet(prov.op_site, conv);
                push_named_residual(
                    &mut ledger,
                    ResidualReason::OpSiteJoinMismatch { expected, found },
                    at,
                    conv,
                    prov,
                );
            }
        }
    }

    let report = HarvestReport {
        harvested,
        classified: flat.len(),
        residual: ledger.len(),
        dropped: 0,
    };
    (flat, ledger, report)
}

// ================================================================================================
// helpers
// ================================================================================================

/// The block-anchored facet convention: `facet::project(&Varnode::ram(block_addr, 0), spaces)`,
/// using the enclosing op's `(block_addr, op_idx)` site. `None` when `op_site` itself is `None`,
/// or (defensively; should not occur for a `Ram` varnode under any convention) when projection
/// fails.
fn block_anchor_facet(op_site: Option<(u64, usize)>, conv: &R2ilConvention) -> Option<VarnodeFacet> {
    let (block_addr, _) = op_site?;
    facet::project(&Varnode::ram(block_addr, 0), conv.spaces()).ok()
}

/// `BlockId` -> its real address, via [`crate::behavior::FunctionBehavior::values`]'s
/// `SsaGraph::block`.
fn block_addr_of(behavior: &FunctionBehavior, block: BlockId) -> Option<u64> {
    behavior.values().block(block).map(|graph_block| graph_block.addr)
}

/// The `OpTag` of a block's terminating (last) source op — used as `Edge`'s `opcode`, since an
/// `Edge` has no parent instruction of its own to take one from. `None` when the block can't be
/// found in `blocks` or has no ops at all (both defensive — every real block has a terminator).
fn terminator_opcode(blocks: &[R2ILBlock], addr: u64) -> Option<OpTag> {
    blocks
        .iter()
        .find(|block| block.addr == addr)
        .and_then(|block| block.ops.last())
        .map(OpTag::from_r2il)
}

/// Whether `object` resolves to `ObjectKind::EscapedUnknown` — or is simply unknown to the
/// object model, which is treated the same conservative way (not classifiable this pass).
fn object_is_escaped(behavior: &FunctionBehavior, object: r2ssa::ObjectId) -> bool {
    behavior
        .objects()
        .object(object)
        .map(|fact| matches!(fact.kind, ObjectKind::EscapedUnknown))
        .unwrap_or(true)
}

/// Pack an `Op` row's `input_arity`/`has_output` into one `u64`: arity in bits `0..32`,
/// `has_output` in bit `32`.
fn pack_op_metadata(input_arity: usize, has_output: bool) -> u64 {
    (input_arity as u64) | ((has_output as u64) << 32)
}

/// `EdgeTag` -> its `u64` payload ordinal (see the module docs' payload table).
fn edge_tag_ordinal(tag: EdgeTag) -> u64 {
    match tag {
        EdgeTag::Normal => 0,
        EdgeTag::True => 1,
        EdgeTag::False => 2,
        EdgeTag::Back => 3,
    }
}

/// Push one residual, computing `shape_id` from the reason and `at_prefix` (opportunistically,
/// from whatever facet we DO have — see [`crate::convention::R2ilConvention::resolved_prefix`])
/// in one place, so every one of the nine `OreFact` arms above shares the exact same bookkeeping.
fn push_named_residual(
    ledger: &mut ResidualLedger,
    reason: ResidualReason,
    at: Option<VarnodeFacet>,
    conv: &R2ilConvention,
    provenance: FactProvenance,
) {
    let at_prefix = at.as_ref().and_then(|facet| conv.resolved_prefix(facet));
    ledger.push(ResidualFact {
        shape_id: reason.shape_id(),
        reason,
        at,
        at_prefix,
        provenance,
    });
}

// ================================================================================================
// tests
// ================================================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use r2il::{ArchSpec, MemoryOrdering, RegisterDef, SpaceId};

    #[test]
    fn a_flat_fact_stays_flat() {
        // The compile-time guard above (`const _: () = assert!(...)`) is the real contract; this
        // test additionally pins the runtime observation and the `Copy` bound, matching the
        // spec's "assert_eq!(size_of::<FlatFact>(), <the pinned constant>) and assert!(FlatFact:
        // Copy)". The exact byte count is target/ABI-dependent (enum niche packing, alignment
        // padding), so this pins the BOUND the const assert already enforces rather than a
        // literal that could differ across hosts.
        assert!(core::mem::size_of::<FlatFact>() <= 64);
        fn assert_copy<T: Copy>() {}
        assert_copy::<FlatFact>();
    }

    #[test]
    fn cardinality_is_rows_not_nesting() {
        let arch = {
            let mut arch = ArchSpec::new("test-arch");
            arch.registers = vec![
                RegisterDef::new("r0", 0, 8),
                RegisterDef::new("r1", 8, 8),
                RegisterDef::new("r2", 16, 8),
                RegisterDef::new("r3", 24, 8),
                RegisterDef::new("r4", 40, 8),
            ];
            arch
        };
        let mut block = R2ILBlock::new(0x3000, 4);
        block.push(r2il::R2ILOp::CallOther {
            output: Some(Varnode::register(40, 8)),
            userop: 1,
            inputs: vec![
                Varnode::register(0, 8),
                Varnode::register(8, 8),
                Varnode::register(16, 8),
                Varnode::register(24, 8),
            ],
        });
        block.push(r2il::R2ILOp::Return { target: Varnode::register(0, 8) });
        let blocks = vec![block];
        let behavior = FunctionBehavior::from_blocks_raw(&blocks, Some(&arch))
            .expect("single-block CallOther+Return fixture must ingest");
        let conv = R2ilConvention::from_arch(&arch, [OpTag::CallOther, OpTag::Return])
            .expect("no custom spaces on this arch, must not overflow");

        let (flat, _ledger, report) = smelt(&behavior, &blocks, &conv);
        assert!(report.is_conserved());

        let op_rows: Vec<&FlatFact> = flat
            .iter()
            .filter(|row| row.kind == FactKind::Op && row.opcode == OpTag::CallOther)
            .collect();
        let operand_in_rows: Vec<&FlatFact> = flat
            .iter()
            .filter(|row| row.kind == FactKind::OperandIn && row.opcode == OpTag::CallOther)
            .collect();
        assert_eq!(op_rows.len(), 1, "expected exactly 1 Op row, got {op_rows:?}");
        assert_eq!(
            operand_in_rows.len(),
            4,
            "expected exactly 4 OperandIn rows (cardinality is rows, never nesting), got {operand_in_rows:?}"
        );

        let inst = op_rows[0].prov.inst;
        assert!(inst.is_some());
        assert!(
            operand_in_rows.iter().all(|row| row.prov.inst == inst),
            "every operand row must trace back to the SAME instruction as the Op row"
        );
    }

    #[test]
    fn melt_is_conserved_and_drops_nothing() {
        // A self-contained, linear (single-block, no branches) fixture reproducing the SHAPE of
        // the plan's §8 stressor block (many diverse ops) without depending on
        // `tests/lossless_fixtures.rs`, which this file does not own. 12 binary ops (2 inputs + 1
        // output each) plus Load/Store/Copy/Call/Return comfortably clear the >= 50 anti-vacuity
        // bound on Op+Operand facts ALONE, independent of however many memory/call-site facts
        // upstream additionally derives.
        let mut block = R2ILBlock::new(0x4000, 40);
        let reg_a = Varnode::register(8, 8);
        let reg_b = Varnode::register(16, 8);
        let binary_ops = [
            r2il::R2ILOp::IntAdd { dst: Varnode::register(0, 8), a: reg_a.clone(), b: reg_b.clone() },
            r2il::R2ILOp::IntSub { dst: Varnode::register(0, 8), a: reg_a.clone(), b: reg_b.clone() },
            r2il::R2ILOp::IntMult { dst: Varnode::register(0, 8), a: reg_a.clone(), b: reg_b.clone() },
            r2il::R2ILOp::IntAnd { dst: Varnode::register(0, 8), a: reg_a.clone(), b: reg_b.clone() },
            r2il::R2ILOp::IntOr { dst: Varnode::register(0, 8), a: reg_a.clone(), b: reg_b.clone() },
            r2il::R2ILOp::IntXor { dst: Varnode::register(0, 8), a: reg_a.clone(), b: reg_b.clone() },
            r2il::R2ILOp::IntLess { dst: Varnode::register(0, 1), a: reg_a.clone(), b: reg_b.clone() },
            r2il::R2ILOp::IntSLess { dst: Varnode::register(0, 1), a: reg_a.clone(), b: reg_b.clone() },
            r2il::R2ILOp::IntLeft { dst: Varnode::register(0, 8), a: reg_a.clone(), b: reg_b.clone() },
            r2il::R2ILOp::IntRight { dst: Varnode::register(0, 8), a: reg_a.clone(), b: reg_b.clone() },
            r2il::R2ILOp::IntEqual { dst: Varnode::register(0, 1), a: reg_a.clone(), b: reg_b.clone() },
            r2il::R2ILOp::IntNotEqual { dst: Varnode::register(0, 1), a: reg_a.clone(), b: reg_b.clone() },
        ];
        for op in binary_ops {
            block.push(op);
        }
        block.push(r2il::R2ILOp::Load {
            dst: Varnode::register(24, 8),
            space: SpaceId::Ram,
            addr: reg_a.clone(),
        });
        block.push(r2il::R2ILOp::Store {
            space: SpaceId::Ram,
            addr: reg_a.clone(),
            val: Varnode::register(24, 8),
        });
        block.push(r2il::R2ILOp::Copy { dst: Varnode::register(32, 8), src: Varnode::register(24, 8) });
        block.push(r2il::R2ILOp::Call { target: Varnode::constant(0x9000, 8) });
        block.push(r2il::R2ILOp::Return { target: Varnode::register(0, 8) });

        let blocks = vec![block];
        let behavior =
            FunctionBehavior::from_blocks_raw(&blocks, None).expect("linear fixture must ingest");
        let conv = R2ilConvention::minimal_pass_one();

        let (flat, ledger, report) = smelt(&behavior, &blocks, &conv);
        assert_eq!(report.harvested, flat.len() + ledger.len());
        assert_eq!(report.classified, flat.len());
        assert_eq!(report.residual, ledger.len());
        assert_eq!(report.dropped, 0);
        assert!(report.is_conserved());
        assert!(
            report.harvested >= 50,
            "expected >= 50 ore facts (anti-vacuity bound), got {}",
            report.harvested
        );
    }

    #[test]
    fn the_convention_is_the_knob_not_the_code() {
        // §5's `R2ilConvention` API has no incremental "add one opcode" mutator — widening the
        // convention is expressed as a second `from_arch` call with `AtomicCAS` added to the
        // classified set, the only constructor spec'd that can grow the set at all.
        let arch = {
            let mut arch = ArchSpec::new("test-arch");
            arch.registers = vec![
                RegisterDef::new("dst", 0, 8),
                RegisterDef::new("addr", 8, 8),
                RegisterDef::new("expected", 16, 8),
                RegisterDef::new("replacement", 24, 8),
            ];
            arch
        };
        let mut block = R2ILBlock::new(0x5000, 8);
        block.push(r2il::R2ILOp::AtomicCAS {
            dst: Varnode::register(0, 8),
            space: SpaceId::Ram,
            addr: Varnode::register(8, 8),
            expected: Varnode::register(16, 8),
            replacement: Varnode::register(24, 8),
            ordering: MemoryOrdering::SeqCst,
        });
        block.push(r2il::R2ILOp::Return { target: Varnode::register(0, 8) });
        let blocks = vec![block];
        let behavior = FunctionBehavior::from_blocks_raw(&blocks, Some(&arch))
            .expect("AtomicCAS+Return fixture must ingest");

        let narrow = R2ilConvention::from_arch(&arch, [OpTag::Return])
            .expect("no custom spaces on this arch, must not overflow");
        let (narrow_flat, _narrow_ledger, narrow_report) = smelt(&behavior, &blocks, &narrow);
        assert!(
            narrow_flat.iter().all(|row| row.opcode != OpTag::AtomicCAS),
            "AtomicCAS must not classify under the narrow convention"
        );

        let wide = R2ilConvention::from_arch(&arch, [OpTag::Return, OpTag::AtomicCAS])
            .expect("no custom spaces on this arch, must not overflow");
        let (wide_flat, _wide_ledger, wide_report) = smelt(&behavior, &blocks, &wide);

        // The AtomicCAS instruction's own facts: 1 Op row + 3 input operands (addr, expected,
        // replacement — all registers, matched by `arch.registers` above, so all resolve once
        // classified) + 1 output operand (dst).
        let atomic_cas_facts = 1 + 3 + 1;
        assert_eq!(
            wide_report.classified,
            narrow_report.classified + atomic_cas_facts,
            "classified must rise by exactly the AtomicCAS fact count"
        );
        assert_eq!(
            narrow_report.residual,
            wide_report.residual + atomic_cas_facts,
            "residual must fall by exactly the same count"
        );
        assert!(
            wide_flat
                .iter()
                .any(|row| row.opcode == OpTag::AtomicCAS && row.kind == FactKind::Op)
        );
    }

    #[test]
    fn block_anchored_rows_are_addressed_not_defaulted() {
        let mut block = R2ILBlock::new(0x6000, 8);
        block.push(r2il::R2ILOp::IntAdd {
            dst: Varnode::register(0, 8),
            a: Varnode::register(8, 8),
            b: Varnode::register(16, 8),
        });
        block.push(r2il::R2ILOp::Return { target: Varnode::register(0, 8) });
        let blocks = vec![block];
        let behavior =
            FunctionBehavior::from_blocks_raw(&blocks, None).expect("IntAdd+Return fixture must ingest");
        let conv = R2ilConvention::minimal_pass_one();
        let (flat, ledger, report) = smelt(&behavior, &blocks, &conv);
        assert!(report.is_conserved());

        // Every Op row (the only block-anchored FlatFact kind in this fixture) carries the REAL
        // block address, never the all-zero facet a lazy `VarnodeFacet::default()`-style
        // implementation would produce.
        let mut op_rows_checked = 0;
        for row in &flat {
            if row.kind == FactKind::Op {
                assert_eq!(row.at.offset(), 0x6000);
                assert_eq!(row.at.space_discriminant(), facet::SPACE_RAM);
                op_rows_checked += 1;
            }
        }
        assert_eq!(op_rows_checked, 2, "expected both IntAdd and Return to classify under pass 1");

        // And any residual that DID land a Ram-space address must likewise be the real block
        // address, not zero.
        for residual in ledger.rows() {
            if let Some(at) = residual.at
                && at.space_discriminant() == facet::SPACE_RAM
            {
                assert_eq!(at.offset(), 0x6000);
            }
        }
    }
}
