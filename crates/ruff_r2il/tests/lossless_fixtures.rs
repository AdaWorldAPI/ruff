//! Losslessness fixtures for the R2IL behavioral IR, PR 1.
//!
//! Every test here ingests a hand-built `R2ILBlock` list through
//! `FunctionBehavior::from_blocks_raw` (never the generic/SCCP-applying path — a
//! losslessness claim must not go through an optimizer) and checks that the typed
//! upstream decomposition (`r2ssa::SSAFunction` / `SsaGraph` / `PreparedFunctionFacts`)
//! survives ingest unchanged. Tests 10-12 additionally run the fixture through the
//! furnace (`furnace::smelt`) to prove the ore -> furnace -> slag loop: stressor ops
//! land in a *named, addressed* residual ledger under a deliberately narrow pass-1
//! convention, and widening the convention (in data, never in a match arm) moves
//! them out again.

use std::collections::BTreeSet;

use r2il::{
    ArchSpec, AtomicKind, MemoryOrdering, OpMetadata, PointerHint, R2ILBlock, R2ILOp, RegisterDef,
    ScalarKind, SpaceId, Varnode, VarnodeMetadata,
};
use r2ssa::{CompareKind, InstPayload, SSAOp};

use ruff_r2il::behavior::FunctionBehavior;
use ruff_r2il::convention::R2ilConvention;
use ruff_r2il::facet;
use ruff_r2il::furnace;
use ruff_r2il::ore::OpTag;
use ruff_r2il::slag::ResidualReason;

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

fn reg(off: u64, size: u32) -> Varnode {
    Varnode::register(off, size)
}

fn con(v: u64, size: u32) -> Varnode {
    Varnode::constant(v, size)
}

/// Unwraps a `Result` without requiring `E: Debug`. `FacetOverflow`'s derive set
/// isn't pinned by the spec text, so the fixtures that construct a
/// `R2ilConvention`/`VarnodeFacet` via a fallible path stay independent of it.
fn must_ok<T, E>(result: Result<T, E>, msg: &str) -> T {
    match result {
        Ok(value) => value,
        Err(_) => panic!("{msg}"),
    }
}

/// A stable, human-readable key for a `MemoryOrdering` value. `MemoryOrdering`
/// derives `Copy`/`Eq` but not `Ord`/`Hash`, so set membership is compared through
/// this discriminant string instead.
fn ordering_key(ordering: MemoryOrdering) -> &'static str {
    match ordering {
        MemoryOrdering::Relaxed => "relaxed",
        MemoryOrdering::Acquire => "acquire",
        MemoryOrdering::Release => "release",
        MemoryOrdering::AcqRel => "acq_rel",
        MemoryOrdering::SeqCst => "seq_cst",
        MemoryOrdering::Unknown => "unknown",
    }
}

/// A stable, human-readable key for a `CompareKind` value, for the same reason.
fn compare_key(kind: CompareKind) -> &'static str {
    match kind {
        CompareKind::Equal => "equal",
        CompareKind::NotEqual => "not_equal",
        CompareKind::Less => "less",
        CompareKind::SignedLess => "signed_less",
        CompareKind::LessEqual => "less_equal",
        CompareKind::SignedLessEqual => "signed_less_equal",
    }
}

/// 4 blocks, 3-way merge, deliberately loaded with every mandated op shape plus a
/// stressor block (B2) that pass-1 conventions cannot classify. See
/// `.claude/plans/r2il-behavioral-ir-v1-impl-spec.md` §10 for the exact op list this
/// transcribes verbatim.
fn fixture_function() -> Vec<R2ILBlock> {
    // B0 @ 0x1000 size 8 -> ConditionalBranch{true: 0x1018, false: 0x1008}
    let mut b0 = R2ILBlock::new(0x1000, 8);
    b0.push(R2ILOp::IntAdd {
        dst: reg(0x00, 8),
        a: reg(0x00, 8),
        b: con(0x10, 8),
    });
    b0.push(R2ILOp::IntSub {
        dst: reg(0x58, 8),
        a: reg(0x00, 8),
        b: con(4, 8),
    });
    b0.push(R2ILOp::IntLess {
        dst: reg(0x40, 1),
        a: reg(0x00, 8),
        b: con(0x100, 8),
    });
    b0.push(R2ILOp::CBranch {
        target: con(0x1018, 8),
        cond: reg(0x40, 1),
    });

    // B1 @ 0x1008 size 8 -> ConditionalBranch{true: 0x1018, false: 0x1010}
    let mut b1 = R2ILBlock::new(0x1008, 8);
    b1.push(R2ILOp::Load {
        dst: reg(0x08, 8),
        space: SpaceId::Ram,
        addr: reg(0x00, 8),
    });
    b1.push(R2ILOp::Store {
        space: SpaceId::Ram,
        addr: reg(0x00, 8),
        val: reg(0x08, 8),
    });
    b1.push(R2ILOp::Copy {
        dst: reg(0x00, 8),
        src: reg(0x08, 8),
    });
    b1.push(R2ILOp::IntSLess {
        dst: reg(0x44, 1),
        a: reg(0x08, 8),
        b: con(0, 8),
    });
    b1.push(R2ILOp::CBranch {
        target: con(0x1018, 8),
        cond: reg(0x44, 1),
    });

    // B2 @ 0x1010 size 8 -> Fallthrough{next: 0x1018} -- the stressor block.
    let mut b2 = R2ILBlock::new(0x1010, 8);
    b2.push(R2ILOp::AtomicCAS {
        dst: reg(0x00, 8),
        space: SpaceId::Ram,
        addr: reg(0x20, 8),
        expected: con(0, 8),
        replacement: con(1, 8),
        ordering: MemoryOrdering::SeqCst,
    });
    b2.push(R2ILOp::StoreConditional {
        result: Some(reg(0x28, 1)),
        space: SpaceId::Ram,
        addr: reg(0x20, 8),
        val: reg(0x00, 8),
        ordering: MemoryOrdering::Release,
    });
    b2.push(R2ILOp::StoreConditional {
        result: None,
        space: SpaceId::Ram,
        addr: reg(0x20, 8),
        val: reg(0x00, 8),
        ordering: MemoryOrdering::Relaxed,
    });
    b2.push(R2ILOp::LoadGuarded {
        dst: reg(0x30, 8),
        space: SpaceId::Custom(7),
        // The ADDRESS varnode genuinely lives in Custom(7), not merely the op's `space:` field.
        // MEASURED reason: an op's own `space:` is not a varnode and never surfaces through
        // `R2ILOp::inputs()/output()`, so a Custom space that exists ONLY there is invisible to
        // operand enumeration and could never reach `facet::project` -- the config-key falsifier
        // would silently test nothing. A varnode in the space exercises the real path.
        // (That op-level-space harvest gap is real and is recorded as a named plan item; it is a
        // missing ore fact kind, not a conservation violation.)
        addr: Varnode::new(SpaceId::Custom(7), 0x20, 8),
        guard: reg(0x28, 1),
        ordering: MemoryOrdering::Acquire,
    });
    b2.push(R2ILOp::StoreGuarded {
        space: SpaceId::Custom(7),
        addr: reg(0x20, 8),
        val: reg(0x30, 8),
        guard: reg(0x28, 1),
        ordering: MemoryOrdering::AcqRel,
    });
    b2.push(R2ILOp::CallOther {
        output: Some(reg(0x38, 8)),
        userop: 42,
        inputs: vec![reg(0x30, 8), reg(0x28, 1), con(1, 4), con(2, 4)],
    });
    b2.push(R2ILOp::CallOther {
        output: None,
        userop: 42,
        inputs: vec![reg(0x30, 8)],
    });
    b2.push(R2ILOp::Insert {
        dst: reg(0x48, 8),
        src: reg(0x30, 8),
        value: reg(0x38, 8),
        position: con(3, 4),
    });
    b2.push(R2ILOp::Load {
        dst: reg(0x50, 8),
        space: SpaceId::Ram,
        addr: Varnode::ram(0x1234_5678_9ABC_DEF0, 8),
    });
    b2.push(R2ILOp::Fence {
        ordering: MemoryOrdering::Unknown,
    });
    b2.push(R2ILOp::Copy {
        dst: reg(0x00, 8),
        src: reg(0x48, 8),
    });

    // Op metadata at index 0 (the AtomicCAS).
    b2.set_op_metadata(
        0,
        OpMetadata {
            memory_ordering: Some(MemoryOrdering::SeqCst),
            atomic_kind: Some(AtomicKind::CompareExchange),
            ..Default::default()
        },
    );

    // Varnode metadata on op 3's dst (the LoadGuarded).
    if let R2ILOp::LoadGuarded { dst, .. } = &mut b2.ops[3] {
        dst.set_meta(VarnodeMetadata {
            scalar_kind: Some(ScalarKind::UnsignedInt),
            pointer_hint: Some(PointerHint::PointerLike),
            ..Default::default()
        });
    } else {
        unreachable!("op index 3 must be the LoadGuarded pushed above");
    }

    // B3 @ 0x1018 size 8 -- merge, 3 predecessors, terminator Return (the reverse
    // scan hits Return before Call; correct and intended).
    let mut b3 = R2ILBlock::new(0x1018, 8);
    b3.push(R2ILOp::Call {
        target: con(0x2000, 8),
    });
    b3.push(R2ILOp::Return {
        target: reg(0x00, 8),
    });

    vec![b0, b1, b2, b3]
}

/// Own 2-block fixture: B0 branches unconditionally into B1, but B1's `Multiequal`
/// declares 3 inputs even though the CFG gives it only 1 real predecessor. This is
/// the phi-zip stressor test 8 probes.
fn multiequal_fixture() -> Vec<R2ILBlock> {
    let mut b0 = R2ILBlock::new(0x2000, 4);
    b0.push(R2ILOp::Branch {
        target: con(0x2004, 8),
    });

    let mut b1 = R2ILBlock::new(0x2004, 4);
    b1.push(R2ILOp::Multiequal {
        dst: reg(0x00, 8),
        inputs: vec![reg(0x08, 8), reg(0x10, 8), reg(0x18, 8)],
    });
    b1.push(R2ILOp::Return {
        target: reg(0x00, 8),
    });

    vec![b0, b1]
}

// ---------------------------------------------------------------------------
// 1. every_mandated_op_shape_survives_ingest_as_a_typed_ssa_op
// ---------------------------------------------------------------------------

#[test]
fn every_mandated_op_shape_survives_ingest_as_a_typed_ssa_op() {
    let blocks = fixture_function();
    let behavior =
        FunctionBehavior::from_blocks_raw(&blocks, None).expect("fixture_function must ingest");

    let mut observed: BTreeSet<&'static str> = BTreeSet::new();
    for inst in &behavior.values().insts {
        let tag = match &inst.payload {
            InstPayload::Phi { .. } => OpTag::Phi,
            InstPayload::Op(op) => OpTag::from_op(op),
        };
        observed.insert(tag.as_str());
    }

    let expected: BTreeSet<&'static str> = [
        OpTag::Phi,
        OpTag::Copy,
        OpTag::Load,
        OpTag::Store,
        OpTag::Fence,
        OpTag::StoreConditional,
        OpTag::AtomicCAS,
        OpTag::LoadGuarded,
        OpTag::StoreGuarded,
        OpTag::IntAdd,
        OpTag::IntSub,
        OpTag::IntLess,
        OpTag::IntSLess,
        OpTag::CBranch,
        OpTag::Call,
        OpTag::Return,
        OpTag::CallOther,
        OpTag::Insert,
    ]
    .into_iter()
    .map(OpTag::as_str)
    .collect();

    // Set equality: a missing op fails, and an unexpected extra fails too.
    assert_eq!(observed, expected);
}

// ---------------------------------------------------------------------------
// 2. op_order_within_a_block_is_preserved_in_ordinal_order
// ---------------------------------------------------------------------------

#[test]
fn op_order_within_a_block_is_preserved_in_ordinal_order() {
    let blocks = fixture_function();
    let behavior = FunctionBehavior::from_blocks_raw(&blocks, None).unwrap();
    let graph = behavior.values();

    // B2 carries no Multiequal, so the 1:1 rename rule holds and its op order must
    // survive untouched.
    let block = graph
        .blocks
        .iter()
        .find(|b| b.addr == 0x1010)
        .expect("block 0x1010 must exist");

    assert_eq!(block.insts.len(), 11, "B2 has no phis, 11 ops 1:1");

    let mut last_ordinal: Option<usize> = None;
    let mut observed_tags: Vec<&'static str> = Vec::new();
    for &inst_id in &block.insts {
        let inst = graph.inst(inst_id).expect("inst must resolve");
        if let Some(last) = last_ordinal {
            assert!(inst.ordinal > last, "ordinal must strictly increase");
        }
        last_ordinal = Some(inst.ordinal);

        let InstPayload::Op(op) = &inst.payload else {
            panic!("B2 has no merge point, so no inst there may be phi-shaped");
        };
        observed_tags.push(OpTag::from_op(op).as_str());
    }

    let expected_tags: Vec<&'static str> = [
        OpTag::AtomicCAS,
        OpTag::StoreConditional,
        OpTag::StoreConditional,
        OpTag::LoadGuarded,
        OpTag::StoreGuarded,
        OpTag::CallOther,
        OpTag::CallOther,
        OpTag::Insert,
        OpTag::Load,
        OpTag::Fence,
        OpTag::Copy,
    ]
    .into_iter()
    .map(OpTag::as_str)
    .collect();

    assert_eq!(observed_tags.len(), 11);
    assert_eq!(observed_tags, expected_tags);
}

// ---------------------------------------------------------------------------
// 3. facts_are_populated_and_counted_against_upstreams_own_classification
// ---------------------------------------------------------------------------

#[test]
fn facts_are_populated_and_counted_against_upstreams_own_classification() {
    let blocks = fixture_function();
    let behavior = FunctionBehavior::from_blocks_raw(&blocks, None).unwrap();

    // Calls: exactly the one Call in B3, resolving to a direct target.
    let calls = behavior.calls();
    assert_eq!(calls.by_id.len(), 1);
    let call = calls.by_id.values().next().expect("exactly one call site");
    assert_eq!(call.direct_target, Some(0x2000));

    // Predicates: exactly the two CBranch-terminated blocks (B0, B1), and their
    // comparison kinds are exactly the two compare ops that feed them.
    let predicates = behavior.predicates();
    assert_eq!(predicates.predicates.len(), 2);
    let kinds: BTreeSet<&'static str> = predicates
        .predicates
        .values()
        .filter_map(|p| p.comparison.as_ref())
        .map(|c| compare_key(c.kind))
        .collect();
    assert_eq!(
        kinds,
        BTreeSet::from(["less", "signed_less"]),
        "B0 compares via IntLess, B1 via IntSLess"
    );

    // Memory counts, computed from upstream's OWN classification rule (never
    // hardcoded): uses <- Load/LoadLinked/LoadGuarded/AtomicCAS/StoreConditional;
    // defs <- Store/StoreGuarded/StoreConditional/AtomicCAS; both <- Call/CallInd.
    let graph = behavior.values();
    let mut expected_uses = 0usize;
    let mut expected_defs = 0usize;
    for inst in &graph.insts {
        let InstPayload::Op(op) = &inst.payload else {
            continue;
        };
        match op {
            SSAOp::Load { .. } | SSAOp::LoadLinked { .. } | SSAOp::LoadGuarded { .. } => {
                expected_uses += 1;
            }
            SSAOp::AtomicCAS { .. } | SSAOp::StoreConditional { .. } => {
                expected_uses += 1;
                expected_defs += 1;
            }
            SSAOp::Store { .. } | SSAOp::StoreGuarded { .. } => {
                expected_defs += 1;
            }
            SSAOp::Call { .. } | SSAOp::CallInd { .. } => {
                expected_uses += 1;
                expected_defs += 1;
            }
            _ => {}
        }
    }
    assert!(
        expected_uses >= 5,
        "anti-vacuity: the fixture must exercise enough memory uses"
    );

    let memory = behavior.memory();
    let observed_uses: usize = memory.uses_by_inst.values().map(Vec::len).sum();
    let observed_defs: usize = memory.defs_by_inst.values().map(Vec::len).sum();
    assert_eq!(observed_uses, expected_uses);
    assert_eq!(observed_defs, expected_defs);

    // Objects: the 64-bit RAM literal in B2 must resolve to a global object.
    //
    // NOTE (measurement, not an assertion): the resolved `GlobalObjectKey.space`
    // is upstream's own Debug-formatted `SpaceId::Ram` ("Ram"), produced by
    // `r2ssa::rename`'s function-level renaming. That formatting is upstream's,
    // not ours, so it is recorded here rather than asserted on.
    let objects = behavior.objects();
    assert!(
        objects
            .global_objects
            .keys()
            .any(|key| key.address == 0x1234_5678_9ABC_DEF0),
        "the exact 64-bit RAM literal must resolve to a global object"
    );
}

// ---------------------------------------------------------------------------
// 4. phi_fan_in_equals_the_predecessor_count_at_the_three_way_merge
// ---------------------------------------------------------------------------

#[test]
fn phi_fan_in_equals_the_predecessor_count_at_the_three_way_merge() {
    let blocks = fixture_function();
    let behavior = FunctionBehavior::from_blocks_raw(&blocks, None).unwrap();

    let preds = behavior.control().predecessors(0x1018);
    assert_eq!(preds.len(), 3, "B3 merges three blocks");

    let block = behavior
        .control()
        .get_block(0x1018)
        .expect("the merge block must exist");
    assert!(
        !block.phis.is_empty(),
        "reg:0 is live-into B3 with three distinct definitions, so at least one phi must exist"
    );
    for phi in &block.phis {
        assert_eq!(phi.sources.len(), 3);
        assert_eq!(phi.sources.len(), preds.len());
    }
}

// ---------------------------------------------------------------------------
// 5. custom_space_and_every_memory_ordering_survive_into_typed_ssa_ops
// ---------------------------------------------------------------------------

#[test]
fn custom_space_and_every_memory_ordering_survive_into_typed_ssa_ops() {
    let blocks = fixture_function();
    let behavior = FunctionBehavior::from_blocks_raw(&blocks, None).unwrap();

    let mut load_guarded_space: Option<String> = None;
    let mut store_guarded_space: Option<String> = None;
    let mut load_guarded_ordering: Option<MemoryOrdering> = None;
    let mut store_guarded_ordering: Option<MemoryOrdering> = None;
    let mut orderings: BTreeSet<&'static str> = BTreeSet::new();

    for inst in &behavior.values().insts {
        let InstPayload::Op(op) = &inst.payload else {
            continue;
        };
        match op {
            SSAOp::LoadGuarded {
                space, ordering, ..
            } => {
                load_guarded_space = Some(space.clone());
                load_guarded_ordering = Some(*ordering);
                orderings.insert(ordering_key(*ordering));
            }
            SSAOp::StoreGuarded {
                space, ordering, ..
            } => {
                store_guarded_space = Some(space.clone());
                store_guarded_ordering = Some(*ordering);
                orderings.insert(ordering_key(*ordering));
            }
            SSAOp::AtomicCAS { ordering, .. } => {
                orderings.insert(ordering_key(*ordering));
            }
            SSAOp::StoreConditional { ordering, .. } => {
                orderings.insert(ordering_key(*ordering));
            }
            SSAOp::Fence { ordering } => {
                orderings.insert(ordering_key(*ordering));
            }
            _ => {}
        }
    }

    let lg_space = load_guarded_space.expect("LoadGuarded must survive ingest");
    let sg_space = store_guarded_space.expect("StoreGuarded must survive ingest");
    assert!(!lg_space.is_empty(), "the custom space must carry a name");
    assert_eq!(
        lg_space, sg_space,
        "LoadGuarded and StoreGuarded share the same custom space"
    );

    assert_eq!(load_guarded_ordering, Some(MemoryOrdering::Acquire));
    assert_eq!(store_guarded_ordering, Some(MemoryOrdering::AcqRel));

    let expected_orderings: BTreeSet<&'static str> = [
        MemoryOrdering::Relaxed,
        MemoryOrdering::Acquire,
        MemoryOrdering::Release,
        MemoryOrdering::AcqRel,
        MemoryOrdering::SeqCst,
        MemoryOrdering::Unknown,
    ]
    .into_iter()
    .map(ordering_key)
    .collect();
    assert_eq!(orderings, expected_orderings, "exact set equality");
}

// ---------------------------------------------------------------------------
// 6. op_metadata_rejoins_by_op_site_even_though_ssa_does_not_carry_it
// ---------------------------------------------------------------------------

#[test]
fn op_metadata_rejoins_by_op_site_even_though_ssa_does_not_carry_it() {
    let blocks = fixture_function();
    let behavior = FunctionBehavior::from_blocks_raw(&blocks, None).unwrap();

    let inst_id = behavior
        .inst_at(0x1010, 0)
        .expect("op site (0x1010, 0) must resolve to an inst");

    let round_trip = behavior
        .op_site(inst_id)
        .expect("op_site must round-trip the inst id");
    assert_eq!(round_trip, (0x1010, 0));

    let inst = behavior.values().inst(inst_id).expect("inst must resolve");
    let InstPayload::Op(SSAOp::AtomicCAS { .. }) = &inst.payload else {
        panic!("op site (0x1010, 0) must be the AtomicCAS the fixture placed there");
    };

    // The source R2IL block still carries the metadata; SSA never does, so this
    // rejoin can only happen through the (block_addr, op_idx) key.
    let source_meta = blocks
        .iter()
        .find(|b| b.addr == 0x1010)
        .and_then(|b| b.op_metadata(0))
        .expect("source op metadata must be present at index 0");
    assert_eq!(source_meta.memory_ordering, Some(MemoryOrdering::SeqCst));
    assert_eq!(source_meta.atomic_kind, Some(AtomicKind::CompareExchange));
}

// ---------------------------------------------------------------------------
// 7. varnode_metadata_is_advisory_and_does_not_change_ingest
// ---------------------------------------------------------------------------

#[test]
fn varnode_metadata_is_advisory_and_does_not_change_ingest() {
    fn one_op_block(with_meta: bool) -> Vec<R2ILBlock> {
        let mut dst = reg(0x30, 8);
        if with_meta {
            dst.set_meta(VarnodeMetadata {
                scalar_kind: Some(ScalarKind::UnsignedInt),
                pointer_hint: Some(PointerHint::PointerLike),
                ..Default::default()
            });
        }
        let mut block = R2ILBlock::new(0x3000, 4);
        block.push(R2ILOp::IntAdd {
            dst,
            a: reg(0x00, 8),
            b: con(1, 8),
        });
        block.push(R2ILOp::Return {
            target: reg(0x30, 8),
        });
        vec![block]
    }

    let plain = FunctionBehavior::from_blocks_raw(&one_op_block(false), None)
        .expect("plain fixture must ingest");
    let annotated = FunctionBehavior::from_blocks_raw(&one_op_block(true), None)
        .expect("annotated fixture must ingest");

    let value_triples = |behavior: &FunctionBehavior| -> Vec<(String, u32, u32)> {
        behavior
            .values()
            .values
            .iter()
            .map(|v| (v.var.name.clone(), v.var.version, v.var.size))
            .collect()
    };

    assert_eq!(value_triples(&plain), value_triples(&annotated));
    assert_eq!(plain.values().insts.len(), annotated.values().insts.len());
}

// ---------------------------------------------------------------------------
// 8. multiequal_ingest_becomes_a_phi_zipped_to_the_predecessor_count
// ---------------------------------------------------------------------------

#[test]
fn multiequal_ingest_becomes_a_phi_zipped_to_the_predecessor_count() {
    let blocks = multiequal_fixture();
    let behavior = FunctionBehavior::from_blocks_raw(&blocks, None).unwrap();

    let preds = behavior.control().predecessors(0x2004);
    let block = behavior
        .control()
        .get_block(0x2004)
        .expect("the multiequal block must exist");

    assert_eq!(
        block.phis.len(),
        1,
        "exactly one phi at the multiequal site"
    );
    let phi = &block.phis[0];

    // State the rule, not the number: fan-in is truncated to zip against the
    // CFG's own predecessor count, even though the source Multiequal declared 3
    // inputs against a block with only 1 real predecessor.
    assert_eq!(phi.sources.len(), preds.len());

    for op in &block.ops {
        assert!(
            !matches!(op, SSAOp::Phi { .. }),
            "phi-shaped ops must never remain in block.ops"
        );
    }
}

// ---------------------------------------------------------------------------
// 9. sixty_four_bit_offsets_are_not_truncated_on_ingest
// ---------------------------------------------------------------------------

#[test]
fn sixty_four_bit_offsets_are_not_truncated_on_ingest() {
    let blocks = fixture_function();
    let behavior = FunctionBehavior::from_blocks_raw(&blocks, None).unwrap();

    let objects = behavior.objects();
    assert!(
        objects
            .global_objects
            .keys()
            .any(|key| key.address == 0x1234_5678_9ABC_DEF0),
        "the exact 64-bit address must survive"
    );

    // Two-sided anti-truncation: the low-32-bit-truncated form must never appear.
    let truncated_name = "ram:9abcdef0";
    assert!(
        behavior
            .values()
            .values
            .iter()
            .all(|v| v.var.name != truncated_name),
        "no SSAVar name may equal the 32-bit-truncated form"
    );
}

// ---------------------------------------------------------------------------
// 10. stressors_land_in_slag_under_pass_one_and_are_named_and_addressed
// ---------------------------------------------------------------------------

#[test]
fn stressors_land_in_slag_under_pass_one_and_are_named_and_addressed() {
    let blocks = fixture_function();
    let behavior = FunctionBehavior::from_blocks_raw(&blocks, None).unwrap();
    let conv = R2ilConvention::minimal_pass_one();

    let (_flat, ledger, report) = furnace::smelt(&behavior, &blocks, &conv);

    assert_eq!(report.dropped, 0);
    assert!(report.is_conserved());
    assert!(report.residual > 0, "pass one must leave residue");
    assert!(
        !ledger.rows().is_empty(),
        "the ledger itself must be non-empty"
    );

    let mut found_atomic_cas = false;
    let mut found_call_other = false;
    let mut found_store_guarded = false;
    let mut found_insert = false;
    let mut found_custom_space = false;
    let mut found_variadic = false;

    for row in ledger.rows() {
        match &row.reason {
            ResidualReason::OpcodeNotInConvention {
                opcode: OpTag::AtomicCAS,
            } => found_atomic_cas = true,
            ResidualReason::OpcodeNotInConvention {
                opcode: OpTag::CallOther,
            } => found_call_other = true,
            ResidualReason::OpcodeNotInConvention {
                opcode: OpTag::StoreGuarded,
            } => found_store_guarded = true,
            ResidualReason::OpcodeNotInConvention {
                opcode: OpTag::Insert,
            } => found_insert = true,
            ResidualReason::CustomSpaceNotInConvention { raw: 7 } => found_custom_space = true,
            ResidualReason::VariadicArity { arity: 4 } => found_variadic = true,
            _ => {}
        }

        // The addressed-slag rule: every residual except NoFacetCoordinate carries
        // its facet coordinate. (Not printing `row.reason` here: `ResidualReason`'s
        // derive set isn't pinned by the spec text, so this assertion does not lean
        // on it implementing `Debug`.)
        if !matches!(row.reason, ResidualReason::NoFacetCoordinate) {
            assert!(
                row.at.is_some(),
                "every addressed residual must carry a facet coordinate"
            );
        }
    }

    // Each asserted individually so no single absorbing group can satisfy the test.
    assert!(
        found_atomic_cas,
        "AtomicCAS must land in slag under pass one"
    );
    assert!(
        found_call_other,
        "CallOther must land in slag under pass one"
    );
    assert!(
        found_store_guarded,
        "StoreGuarded must land in slag under pass one"
    );
    assert!(found_insert, "Insert must land in slag under pass one");
    assert!(
        found_custom_space,
        "the Custom(7) space must land in slag under pass one"
    );
    assert!(
        found_variadic,
        "the 4-input CallOther must land in slag under pass one"
    );
}

// ---------------------------------------------------------------------------
// 11. widening_the_convention_moves_a_stressor_out_of_slag
// ---------------------------------------------------------------------------

#[test]
fn widening_the_convention_moves_a_stressor_out_of_slag() {
    let blocks = fixture_function();
    let behavior = FunctionBehavior::from_blocks_raw(&blocks, None).unwrap();

    // Both conventions are built from the SAME (empty) ArchSpec, so their row sets
    // are identical -- the ONLY difference between them is whether AtomicCAS is in
    // the classified-opcode set. That isolates the measured delta to exactly what
    // widening the ladder changes, with nothing else free to move.
    let empty_arch = ArchSpec::new("widen-test");

    let base_conv = must_ok(
        R2ilConvention::from_arch(
            &empty_arch,
            [
                OpTag::Copy,
                OpTag::IntAdd,
                OpTag::Load,
                OpTag::Store,
                OpTag::CBranch,
                OpTag::Call,
                OpTag::Return,
            ],
        ),
        "the base convention must build within budget",
    );

    let widened_conv = must_ok(
        R2ilConvention::from_arch(
            &empty_arch,
            [
                OpTag::Copy,
                OpTag::IntAdd,
                OpTag::Load,
                OpTag::Store,
                OpTag::CBranch,
                OpTag::Call,
                OpTag::Return,
                OpTag::AtomicCAS,
            ],
        ),
        "the widened convention must build within budget",
    );

    let (_flat_before, ledger_before, report_before) =
        furnace::smelt(&behavior, &blocks, &base_conv);
    let (_flat_after, ledger_after, report_after) =
        furnace::smelt(&behavior, &blocks, &widened_conv);

    let atomic_cas_op_residual_before = ledger_before
        .rows()
        .iter()
        .filter(|r| {
            matches!(
                r.reason,
                ResidualReason::OpcodeNotInConvention {
                    opcode: OpTag::AtomicCAS
                }
            )
        })
        .count();
    // MEASURED, not assumed: the fixture holds ONE AtomicCAS op, but an unclassified op blocks
    // every ore fact that depends on it — its own `Op` row plus its four operand rows and the
    // memory use/def rows it generates, all tagged with the PARENT's opcode. Conservation demands
    // exactly that (every ore fact is classified or residual, never dropped), so the count is the
    // op's whole dependent fan-out, not 1. An earlier `== 1` here encoded the wrong model and is
    // the reason this comment exists.
    assert!(
        atomic_cas_op_residual_before > 1,
        "AtomicCAS must block its dependent fan-out, not merely its own Op row"
    );

    let atomic_cas_op_residual_after = ledger_after
        .rows()
        .iter()
        .filter(|r| {
            matches!(
                r.reason,
                ResidualReason::OpcodeNotInConvention {
                    opcode: OpTag::AtomicCAS
                }
            )
        })
        .count();
    assert_eq!(
        atomic_cas_op_residual_after, 0,
        "AtomicCAS's residual row must disappear once it is classified"
    );

    // Its residual rows disappear, `classified` rises by exactly that count and
    // `residual` falls by the same -- proving the ledger tracks the convention,
    // not noise. Since the two conventions differ ONLY in whether AtomicCAS
    // classifies, this delta is attributable entirely to that one change.
    let delta_classified = report_after.classified - report_before.classified;
    let delta_residual = report_before.residual - report_after.residual;
    assert!(
        delta_classified > 0,
        "widening must move something out of slag"
    );
    // MEASURED: widening reclassifies only the subset that has nothing else blocking it — the op
    // row and its memory rows melt, while its OPERAND rows stay residual under a DIFFERENT named
    // reason (`NoConventionRowAtAddress`: `minimal_pass_one()` carries no address rows, so an
    // operand's facet resolves to nothing). So the blocked fan-out is an upper bound on the
    // delta, never an equality — asserting equality here was wrong and this bound is the true
    // claim.
    assert!(
        delta_classified <= atomic_cas_op_residual_before,
        "widening cannot reclassify more rows than the opcode was blocking"
    );
    // Nothing evaporates in the transition: the same ore is harvested both times, so every row
    // that stopped being an AtomicCAS residual is either classified now or carries another named
    // reason — it cannot have been dropped.
    assert_eq!(
        report_before.harvested, report_after.harvested,
        "the same ore is harvested regardless of convention; only its fate changes"
    );
    assert_eq!(
        delta_classified, delta_residual,
        "every newly classified fact must vacate residual in lockstep"
    );

    assert_eq!(report_before.dropped, 0);
    assert_eq!(report_after.dropped, 0);
    assert!(report_before.is_conserved());
    assert!(report_after.is_conserved());
}

// ---------------------------------------------------------------------------
// 12. a_bootstrapped_convention_resolves_register_operands_that_pass_one_cannot
// ---------------------------------------------------------------------------

#[test]
fn a_bootstrapped_convention_resolves_register_operands_that_pass_one_cannot() {
    let blocks = fixture_function();
    let behavior = FunctionBehavior::from_blocks_raw(&blocks, None).unwrap();

    // Pass one: no rows at all, so register operands resolve nowhere.
    let base_conv = R2ilConvention::minimal_pass_one();
    let (_flat_a, ledger_a, report_a) = furnace::smelt(&behavior, &blocks, &base_conv);

    let unresolved_before = ledger_a
        .rows()
        .iter()
        .filter(|r| matches!(r.reason, ResidualReason::NoConventionRowAtAddress))
        .count();
    assert!(
        unresolved_before > 0,
        "pass one must leave register operands unresolved"
    );

    // Bootstrap from an ArchSpec naming reg 0x00/8 and reg 0x08/8.
    let mut spec = ArchSpec::new("bootstrap-test");
    spec.registers.push(RegisterDef::new("named0", 0x00, 8));
    spec.registers.push(RegisterDef::new("named8", 0x08, 8));

    let bootstrapped = must_ok(
        R2ilConvention::from_arch(
            &spec,
            [
                OpTag::Copy,
                OpTag::IntAdd,
                OpTag::Load,
                OpTag::Store,
                OpTag::CBranch,
                OpTag::Call,
                OpTag::Return,
            ],
        ),
        "two named registers must fit within the interning budget",
    );

    let (_flat_b, ledger_b, report_b) = furnace::smelt(&behavior, &blocks, &bootstrapped);

    let unresolved_after = ledger_b
        .rows()
        .iter()
        .filter(|r| matches!(r.reason, ResidualReason::NoConventionRowAtAddress))
        .count();

    assert!(
        unresolved_after < unresolved_before,
        "named register operands must move from residual to classified"
    );
    assert!(
        report_b.classified > report_a.classified,
        "classified must rise once register operands resolve"
    );

    // An UNNAMED offset (0x58, only touched by the un-classified IntSub in B0)
    // still resolves through the coarse Space fallback row, at depth() == 1 --
    // ties §5's bootstrap rule directly to the melt.
    let unnamed_facet = must_ok(
        facet::project(&reg(0x58, 8), bootstrapped.spaces()),
        "the register space is always within the interning budget",
    );
    bootstrapped
        .resolve(&unnamed_facet)
        .expect("the coarse register-space row must catch an unnamed offset");
    let prefix = bootstrapped
        .resolved_prefix(&unnamed_facet)
        .expect("resolved_prefix must report the matched prefix");
    assert_eq!(
        prefix.depth(),
        1,
        "an unnamed register offset resolves at the coarse Space depth"
    );
}
