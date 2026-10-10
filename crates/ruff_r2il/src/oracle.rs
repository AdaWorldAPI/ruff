//! Stage 6 — the round-trip reconstruction ORACLE: `R2IL → routes → semantic-equivalent R2IL`.
//!
//! The PR-2 gate deliverable from `.claude/plans/r2il-behavioral-ir-v1.md` §14, executed per the
//! council-ratified spec `.claude/plans/r2il-roundtrip-oracle-spec-v1.md` (v3). Three committed
//! properties:
//!
//! - **SPO is NOT the oracle.** The comparison happens on typed r2il values — [`OpSkeleton`]
//!   equality over [`r2il::Varnode`]s — never on a triple projection and never on
//!   textual/binary artifact equality (plan C4).
//! - **Conservation is the verdict's shape** (plan C3): every SOURCE OP SITE is either
//!   reconstructed-equal, mismatched, ledger-accounted, or an ORPHAN — a true 4-way partition
//!   with defined precedence, and orphans/mismatches are failures, never footnotes.
//! - **What the skeleton cannot carry is MEASURED, never silently passed.** Twelve `R2ILOp`
//!   variants hold semantic state beyond their `inputs()`/`output()` varnode projection (the
//!   spec §2 table); each matched op of those variants emits a typed [`AttributeGap`]. The gap
//!   census is the probe-first input for a FUTURE additive schema widening (plan open item O6)
//!   — this module changes no schema and no furnace semantics.
//!
//! # What a passing verdict does and does not prove
//!
//! A verdict that [`OracleVerdict::holds`] under [`permissive_convention`] proves the
//! reconstruction MECHANISM — facet inversion ([`facet::unproject`]) + row grouping + skeleton
//! comparison — is faithful. It says NOTHING about the shipped
//! [`R2ilConvention::minimal_pass_one`] default's coverage: under that convention no operand
//! row melts at all (zero rows ⇒ `resolve` is `None` everywhere), so its verdict holds through
//! ledger ACCOUNTING, with `matched == 0`. The two conventions measure different things; report
//! both, never conflate them (spec §3.5, normative).
//!
//! Rendering rule (spec frozen 8): every printed or persisted representation of a verdict is
//! produced from typed fields and `as_str()` — `format!("{:?}")` is FORBIDDEN as a data path.
//! `Debug` derives below exist solely for test-assertion diagnostics.

use std::collections::{BTreeMap, BTreeSet};

use r2il::{AddressSpace, ArchSpec, R2ILBlock, R2ILOp, SpaceId, Varnode};

use crate::convention::R2ilConvention;
use crate::facet::{self, CustomSpaceTable, FacetOverflow};
use crate::furnace::FactKind;
use crate::furnace::FlatFact;
use crate::ore::OpTag;
use crate::slag::ResidualLedger;

// ================================================================================================
// OpSkeleton — THE equivalence target
// ================================================================================================

/// The typed projection an op's fact rows carry: opcode + output varnode + input varnodes in
/// position order. Exactly what [`R2ILOp::output`]/[`R2ILOp::inputs`] project — which is exactly
/// what `ore::enumerate` fed the furnace, so skeleton equality at a site is the honest
/// definition of "the routes carried this op" (spec §3.2).
///
/// `Varnode`'s own manual `PartialEq` (space/offset/size, `meta` excluded — `varnode.rs:149`)
/// is the comparison; [`facet::unproject`] never reconstructs `meta`, and `meta` never entered
/// the projection, so the exclusion is load-bearing, not incidental.
#[derive(Debug, Clone, PartialEq)]
pub struct OpSkeleton {
    pub opcode: OpTag,
    pub output: Option<Varnode>,
    pub inputs: Vec<Varnode>,
}

impl OpSkeleton {
    /// Project one source op. This is the SOURCE side of the comparison; the reconstructed side
    /// is assembled from fact rows by [`reconstruct`].
    #[must_use]
    pub fn of(op: &R2ILOp) -> Self {
        Self {
            opcode: OpTag::from_r2il(op),
            output: op.output().cloned(),
            inputs: op.inputs().into_iter().cloned().collect(),
        }
    }
}

// ================================================================================================
// GapAttribute — the measured, typed "what the skeleton cannot carry" channel
// ================================================================================================

/// One kind of non-varnode semantic state a matched op's variant carries that neither the
/// skeleton nor any current fact row represents. Deliberately NOT a
/// [`crate::slag::ResidualReason`]: the furnace did not fail — the schema deliberately projects
/// (spec §3.4). Mirrors `ResidualReason`'s no-catch-all discipline: [`Self::ALL`] +
/// [`Self::as_str`] + the totality of [`gaps_of`] are all pinned by tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GapAttribute {
    /// `Load`/`Store` (and the atomic/guarded family): the `space: SpaceId` field.
    MemorySpace,
    /// `Fence` and the atomic/guarded family: the `ordering: MemoryOrdering` field. Note
    /// `Fence` carries ZERO varnodes — its skeleton is `(Fence, None, [])`, trivially equal, so
    /// this gap carries ALL of its semantics.
    MemoryOrdering,
    /// `CallOther`: the `userop: u32` index.
    UserOpIndex,
    /// `Subpiece`: the `offset: u32` field (r2il models it as a field, not a const operand).
    SubpieceOffset,
    /// `PtrAdd`/`PtrSub`: the `element_size: u32` field.
    PtrElementSize,
}

impl GapAttribute {
    /// Every variant's stable name — the no-catch-all pin, exactly like
    /// [`crate::slag::ResidualReason::ALL`].
    pub const ALL: &'static [&'static str] = &[
        "memory_space",
        "memory_ordering",
        "userop_index",
        "subpiece_offset",
        "ptr_element_size",
    ];

    /// Stable `snake_case` name. Never derived from `Debug`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            GapAttribute::MemorySpace => "memory_space",
            GapAttribute::MemoryOrdering => "memory_ordering",
            GapAttribute::UserOpIndex => "userop_index",
            GapAttribute::SubpieceOffset => "subpiece_offset",
            GapAttribute::PtrElementSize => "ptr_element_size",
        }
    }
}

/// The verified spec-§2 table: which non-varnode attributes each opcode's variant carries.
/// TWELVE variants return non-empty; everything else returns `&[]`. The
/// `gaps_of_covers_exactly_the_twelve_table_variants` test pins both halves (can-fire AND
/// can-stay-silent), so coverage drift is caught the way `there_is_no_catch_all_reason` catches
/// a new `ResidualReason`.
#[must_use]
pub fn gaps_of(tag: OpTag) -> &'static [GapAttribute] {
    match tag {
        OpTag::Load | OpTag::Store => &[GapAttribute::MemorySpace],
        OpTag::Fence => &[GapAttribute::MemoryOrdering],
        OpTag::LoadLinked
        | OpTag::StoreConditional
        | OpTag::AtomicCAS
        | OpTag::LoadGuarded
        | OpTag::StoreGuarded => &[GapAttribute::MemorySpace, GapAttribute::MemoryOrdering],
        OpTag::CallOther => &[GapAttribute::UserOpIndex],
        OpTag::Subpiece => &[GapAttribute::SubpieceOffset],
        OpTag::PtrAdd | OpTag::PtrSub => &[GapAttribute::PtrElementSize],
        _ => &[],
    }
}

// ================================================================================================
// Reconstruction
// ================================================================================================

/// One op rebuilt from its fact rows: the [`FactKind::Op`] row's site and ordinal, plus the
/// skeleton assembled from its [`FactKind::OperandIn`]/[`FactKind::OperandOut`] rows.
#[derive(Debug, Clone, PartialEq)]
pub struct ReconstructedOp {
    pub site: (u64, usize),
    pub ordinal: u64,
    pub skeleton: OpSkeleton,
}

/// Why one op row could NOT be rebuilt. A missed site is "not reconstructed" for
/// [`judge`]'s partition — it falls to the ledger criterion or orphans (spec §3.4 rules 3-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconstructionMiss {
    /// The operand rows present do not satisfy the op row's OWN payload
    /// (`b` = `input_arity | has_output << 32`). `have`/`need` count inputs AND the output
    /// slot together: `have = ins.len() + out_present`, `need = arity + has_output`.
    MissingOperands {
        site: (u64, usize),
        have: usize,
        need: usize,
    },
    /// [`facet::unproject`] refused an operand row's facet. `input_index` is `Some(i)` for the
    /// `i`-th input, `None` for the output operand. Carries the inversion's own typed error —
    /// never a bespoke shape (spec §3.1, council ledger row 2).
    FacetInversion {
        site: (u64, usize),
        input_index: Option<usize>,
        overflow: FacetOverflow,
    },
}

impl ReconstructionMiss {
    #[must_use]
    pub fn site(&self) -> (u64, usize) {
        match self {
            ReconstructionMiss::MissingOperands { site, .. }
            | ReconstructionMiss::FacetInversion { site, .. } => *site,
        }
    }
}

/// [`reconstruct`]'s output: rebuilt ops plus every miss, reported — never skipped.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Reconstruction {
    pub ops: Vec<ReconstructedOp>,
    pub misses: Vec<ReconstructionMiss>,
}

/// Rebuild op skeletons from flat rows (spec §3.3).
///
/// Grouping keys are the rows' OWN provenance: [`FactKind::Op`] rows are sited by
/// `prov.op_site` (a melted Op row always has one — the furnace ladder derives its block
/// anchor from it); operand rows attach by shared `prov.inst` (`ore.rs` emits the Op row and
/// its operand rows with the identical `base_prov`). `OperandIn` rows order by their own `a`
/// payload (the input index); completeness is judged against the op row's own `b` payload,
/// never against the source.
#[must_use]
pub fn reconstruct(rows: &[FlatFact], spaces: &CustomSpaceTable) -> Reconstruction {
    // inst.0 -> (ordered input facets by index, output facet)
    struct Operands {
        ins: BTreeMap<u64, crate::facet::VarnodeFacet>,
        out: Option<crate::facet::VarnodeFacet>,
    }
    let mut operands: BTreeMap<u32, Operands> = BTreeMap::new();
    let mut op_rows: Vec<&FlatFact> = Vec::new();

    for row in rows {
        match row.kind {
            FactKind::Op => op_rows.push(row),
            FactKind::OperandIn => {
                if let Some(inst) = row.prov.inst {
                    operands
                        .entry(inst.0)
                        .or_insert_with(|| Operands {
                            ins: BTreeMap::new(),
                            out: None,
                        })
                        .ins
                        .insert(row.a, row.at);
                }
            }
            FactKind::OperandOut => {
                if let Some(inst) = row.prov.inst {
                    operands
                        .entry(inst.0)
                        .or_insert_with(|| Operands {
                            ins: BTreeMap::new(),
                            out: None,
                        })
                        .out = Some(row.at);
                }
            }
            // Edge / memory / predicate / call rows carry no operand varnodes of their own
            // (block-anchored facets) — nothing to rebuild from them here.
            _ => {}
        }
    }

    let mut out = Reconstruction::default();

    for op_row in op_rows {
        // A melted Op row always carries an op site (the ladder's block anchor is derived from
        // it); a row without one cannot be sited and cannot participate in the source-site
        // universe, so it is skipped rather than guessed at.
        let Some(site) = op_row.prov.op_site else {
            continue;
        };
        let arity = (op_row.b & 0xFFFF_FFFF) as usize;
        let has_output = (op_row.b >> 32) & 1 == 1;

        let empty = Operands {
            ins: BTreeMap::new(),
            out: None,
        };
        let ops = op_row
            .prov
            .inst
            .and_then(|inst| operands.get(&inst.0))
            .unwrap_or(&empty);

        let have = ops.ins.len() + usize::from(ops.out.is_some());
        let need = arity + usize::from(has_output);
        if ops.ins.len() != arity || ops.out.is_some() != has_output {
            out.misses
                .push(ReconstructionMiss::MissingOperands { site, have, need });
            continue;
        }

        let mut inputs = Vec::with_capacity(arity);
        let mut inversion_miss = None;
        for (index, (_, at)) in ops.ins.iter().enumerate() {
            match facet::unproject(at, spaces) {
                Ok(vn) => inputs.push(vn),
                Err(overflow) => {
                    inversion_miss = Some(ReconstructionMiss::FacetInversion {
                        site,
                        input_index: Some(index),
                        overflow,
                    });
                    break;
                }
            }
        }
        if let Some(miss) = inversion_miss {
            out.misses.push(miss);
            continue;
        }

        let output = match &ops.out {
            None => None,
            Some(at) => match facet::unproject(at, spaces) {
                Ok(vn) => Some(vn),
                Err(overflow) => {
                    out.misses.push(ReconstructionMiss::FacetInversion {
                        site,
                        input_index: None,
                        overflow,
                    });
                    continue;
                }
            },
        };

        out.ops.push(ReconstructedOp {
            site,
            ordinal: op_row.a,
            skeleton: OpSkeleton {
                opcode: op_row.opcode,
                output,
                inputs,
            },
        });
    }

    out
}

// ================================================================================================
// judge — the verdict
// ================================================================================================

/// One site where the reconstructed skeleton differs from the source's — both carried as typed
/// values so a report renders fields, never `Debug`.
#[derive(Debug, Clone, PartialEq)]
pub struct SkeletonMismatch {
    pub site: (u64, usize),
    pub source: OpSkeleton,
    pub reconstructed: OpSkeleton,
}

/// One matched op whose variant carries semantic state the skeleton cannot (see [`gaps_of`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttributeGap {
    pub site: (u64, usize),
    pub opcode: OpTag,
    pub attribute: GapAttribute,
}

/// The oracle's conservation reading over the source-op-site universe (spec §3.4).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OracleVerdict {
    /// Rule 1: reconstructed complete AND skeleton-equal.
    pub matched: usize,
    /// Rule 3: not reconstructed, but ≥1 ledger residual anchored at the site.
    pub ledger_accounted: usize,
    /// Ledger rows with `provenance.op_site == None` — SSA-level facts (phi inputs,
    /// `CallDefine`, the Edge no-facet case) OUTSIDE the source-site universe. Counted, never
    /// errors, never silently dropped.
    pub ssa_only_residuals: usize,
    /// Rule 4: neither reconstructed nor accounted — the failure signal.
    pub orphans: Vec<(u64, usize)>,
    /// Rule 2: reconstructed but unequal — checked BEFORE the ledger criterion; a mismatch is
    /// never excused by a coincident residual at the same site.
    pub mismatches: Vec<SkeletonMismatch>,
    /// The measured "what the skeleton cannot carry" census over MATCHED ops.
    pub attribute_gaps: Vec<AttributeGap>,
}

impl OracleVerdict {
    /// The oracle passes iff every source op site is matched or ledger-accounted.
    /// Attribute gaps and SSA-only residuals do not fail the verdict — they are measurements.
    #[must_use]
    pub fn holds(&self) -> bool {
        self.orphans.is_empty() && self.mismatches.is_empty()
    }
}

/// Partition every source op site per spec §3.4's 4-way precedence.
#[must_use]
pub fn judge(
    source: &[R2ILBlock],
    recon: &Reconstruction,
    ledger: &ResidualLedger,
) -> OracleVerdict {
    let by_site: BTreeMap<(u64, usize), &ReconstructedOp> =
        recon.ops.iter().map(|op| (op.site, op)).collect();

    let mut ledger_sites: BTreeSet<(u64, usize)> = BTreeSet::new();
    let mut ssa_only_residuals = 0usize;
    for row in ledger.rows() {
        match row.provenance.op_site {
            Some(site) => {
                ledger_sites.insert(site);
            }
            None => ssa_only_residuals += 1,
        }
    }

    let mut verdict = OracleVerdict {
        ssa_only_residuals,
        ..OracleVerdict::default()
    };

    for block in source {
        for (op_idx, op) in block.ops.iter().enumerate() {
            let site = (block.addr, op_idx);
            let source_skeleton = OpSkeleton::of(op);
            if let Some(rebuilt) = by_site.get(&site) {
                if rebuilt.skeleton == source_skeleton {
                    verdict.matched += 1;
                    for &attribute in gaps_of(source_skeleton.opcode) {
                        verdict.attribute_gaps.push(AttributeGap {
                            site,
                            opcode: source_skeleton.opcode,
                            attribute,
                        });
                    }
                } else {
                    verdict.mismatches.push(SkeletonMismatch {
                        site,
                        source: source_skeleton,
                        reconstructed: rebuilt.skeleton.clone(),
                    });
                }
            } else if ledger_sites.contains(&site) {
                verdict.ledger_accounted += 1;
            } else {
                verdict.orphans.push(site);
            }
        }
    }

    verdict
}

// ================================================================================================
// permissive_convention — the oracle's measurement config
// ================================================================================================

/// Build the oracle's MEASUREMENT convention for `blocks`: classify every opcode present, and
/// give every space that appears (the four fixed spaces via [`R2ilConvention::from_arch`]'s own
/// fall-through rows, plus a deliberate fall-through row per CUSTOM space) a resolvable root.
///
/// This is pure config — data through the existing constructors, never a new `smelt` arm
/// (spec frozen 6). It is NOT a shipped default and deliberately inverts `from_arch`'s
/// no-custom-fall-through doctrine: the oracle WANTS everything resolvable so that what fails
/// to round-trip is the machinery's fault, not the config's. Production conventions keep the
/// doctrine; this one is for measuring.
///
/// # Errors
/// [`FacetOverflow`] if the blocks' custom-space raw ids exceed the interned-ordinal budget —
/// a config-key must be lossless, same rule as everywhere else in `facet.rs`.
pub fn permissive_convention(blocks: &[R2ILBlock]) -> Result<R2ilConvention, FacetOverflow> {
    let mut tags: BTreeSet<OpTag> = BTreeSet::new();
    let mut custom_raws: BTreeSet<u32> = BTreeSet::new();

    for block in blocks {
        for op in &block.ops {
            tags.insert(OpTag::from_r2il(op));
            for vn in op.inputs().into_iter().chain(op.output()) {
                if let SpaceId::Custom(raw) = vn.space {
                    custom_raws.insert(raw);
                }
            }
        }
    }

    let mut arch = ArchSpec::new("oracle-permissive");
    for &raw in &custom_raws {
        arch.spaces.push(AddressSpace {
            id: SpaceId::Custom(raw),
            name: format!("custom{raw}"),
            addr_size: 8,
            word_size: 1,
            is_default: false,
            endianness: None,
            memory_class: None,
            permissions: None,
            valid_ranges: Vec::new(),
            bank_id: None,
            segment_id: None,
        });
    }

    let mut conv = R2ilConvention::from_arch(&arch, tags)?;

    for &raw in &custom_raws {
        // `from_arch` interned the raw id into the table above, so the ordinal exists; a raw
        // the table does not know is exactly the lossless-config-key refusal `from_arch`
        // already returned Err for.
        if let Some(discriminant) = conv.spaces().ordinal_of(raw) {
            let at = crate::facet::FacetPrefix::Space { discriminant };
            conv.insert(crate::convention::ConventionRow {
                at,
                name: Some(format!("custom{raw}")),
                note: Some("oracle permissive fall-through".to_string()),
                state: crate::convention::ValidationState::Unmeasured,
            });
        }
    }

    Ok(conv)
}
