//! Gate-3 fixtures for the round-trip reconstruction oracle
//! (`.claude/plans/r2il-roundtrip-oracle-spec-v1.md` §5, ratified v3).
//!
//! Every guard here has a can-fire AND a can-stay-silent half, and the two negative tests
//! (mismatch, orphan) corrupt REAL smelted rows rather than hand-built ones, so they exercise
//! the same partition path the positive tests do.

use r2il::memory::MemoryOrdering;
use r2il::{R2ILBlock, R2ILOp, SpaceId, Varnode};
use ruff_r2il::behavior::FunctionBehavior;
use ruff_r2il::convention::R2ilConvention;
use ruff_r2il::furnace::{self, FactKind, FlatFact};
use ruff_r2il::oracle::{self, GapAttribute, OracleVerdict, Reconstruction};
use ruff_r2il::sink::{self, OfflineSink, RefinedTruthSink};
use ruff_r2il::slag::ResidualLedger;

fn reg(offset: u64, size: u32) -> Varnode {
    Varnode::register(offset, size)
}

fn con(value: u64, size: u32) -> Varnode {
    Varnode::constant(value, size)
}

/// A diamond CFG that forces one 2-input phi at the merge block: `0x1000` conditionally
/// branches to `0x1008`; `0x1004` and `0x1008` write DIFFERENT values into the same register
/// and both branch to `0x100c`, which returns that register. Six source ops total, four
/// opcodes (`CBranch`, `Copy`, `Branch`, `Return`), operands only in the register/const
/// spaces so every operand resolves under a rooted convention.
fn diamond_with_phi() -> Vec<R2ILBlock> {
    let mut b0 = R2ILBlock::new(0x1000, 4);
    b0.push(R2ILOp::CBranch {
        target: con(0x1008, 8),
        cond: reg(0x8, 1),
    });

    let mut b1 = R2ILBlock::new(0x1004, 4);
    b1.push(R2ILOp::Copy {
        dst: reg(0x0, 8),
        src: con(1, 8),
    });
    b1.push(R2ILOp::Branch {
        target: con(0x100c, 8),
    });

    let mut b2 = R2ILBlock::new(0x1008, 4);
    b2.push(R2ILOp::Copy {
        dst: reg(0x0, 8),
        src: con(2, 8),
    });
    b2.push(R2ILOp::Branch {
        target: con(0x100c, 8),
    });

    let mut b3 = R2ILBlock::new(0x100c, 4);
    b3.push(R2ILOp::Return {
        target: reg(0x0, 8),
    });

    vec![b0, b1, b2, b3]
}

/// smelt + reconstruct + judge under one convention — the whole pipeline the oracle measures.
fn run_oracle(
    blocks: &[R2ILBlock],
    conv: &R2ilConvention,
) -> (Vec<FlatFact>, ResidualLedger, OracleVerdict) {
    let behavior = FunctionBehavior::from_blocks_raw(blocks, None).expect("fixture ingests");
    let (rows, ledger, report) = furnace::smelt(&behavior, blocks, conv);
    assert!(report.is_conserved(), "furnace conservation must hold");
    let recon = oracle::reconstruct(&rows, conv.spaces());
    let verdict = oracle::judge(blocks, &recon, &ledger);
    (rows, ledger, verdict)
}

#[test]
fn full_melt_under_the_permissive_convention_matches_every_source_op_exactly() {
    let blocks = diamond_with_phi();
    let conv = oracle::permissive_convention(&blocks).expect("fixed spaces cannot overflow");
    let (_, _, verdict) = run_oracle(&blocks, &conv);

    assert!(verdict.holds(), "orphans/mismatches must be empty");
    assert_eq!(verdict.matched, 6, "every source op site skeleton-matches");
    assert_eq!(
        verdict.ledger_accounted, 0,
        "nothing needs accounting under full melt"
    );
    // Four SSA-level constructs carry no source op site (`prov.op_site: None`) and land
    // exclusively in `ssa_only_residuals`, never against a matched op: the one phi's Op fact
    // (`ore.rs`'s `base_prov.op_site: None` for `InstPayload::Phi`) plus its two `PhiInput`
    // facts, and the CBranch's `Predicate` fact — `OreFact::Predicate` always carries
    // `prov.inst: None` (`ore.rs:997`), so `furnace.rs`'s parent-melted lookup keyed by `inst`
    // always misses for it and it residualizes regardless of convention; a pre-existing
    // furnace behaviour this oracle measures rather than changes.
    assert_eq!(verdict.ssa_only_residuals, 4);
    // Silence half of the gap channel: none of CBranch/Copy/Branch/Return carries
    // non-varnode semantic state.
    assert!(verdict.attribute_gaps.is_empty());
}

#[test]
fn minimal_pass_one_accounts_everything_and_matches_nothing() {
    let blocks = diamond_with_phi();
    let minimal = R2ilConvention::minimal_pass_one();
    let (_, _, verdict) = run_oracle(&blocks, &minimal);

    assert!(verdict.holds(), "accounted, not orphaned");
    // Pinned EXACT per the ratified spec (council ledger row 15): all 7 classified opcodes
    // carry >= 1 operand and no operand melts under a zero-row convention, so no op can fully
    // reconstruct. A `matched >= 1` assertion here would be unsatisfiable.
    assert_eq!(verdict.matched, 0);
    assert_eq!(
        verdict.ledger_accounted, 6,
        "every source op site is ledger-accounted"
    );

    let conv = oracle::permissive_convention(&blocks).expect("fixed spaces cannot overflow");
    let (_, _, permissive) = run_oracle(&blocks, &conv);
    assert!(
        verdict.matched < permissive.matched,
        "the two conventions must measure different things: minimal {} vs permissive {}",
        verdict.matched,
        permissive.matched
    );
}

/// Locate the OperandIn row of `site` with input index `index` among smelted rows.
fn operand_in_row_index(rows: &[FlatFact], site: (u64, usize), index: u64) -> usize {
    rows.iter()
        .position(|row| {
            row.kind == FactKind::OperandIn && row.prov.op_site == Some(site) && row.a == index
        })
        .expect("fixture must contain the operand row")
}

#[test]
fn a_corrupted_operand_facet_fires_exactly_one_mismatch() {
    let blocks = diamond_with_phi();
    let conv = oracle::permissive_convention(&blocks).expect("fixed spaces cannot overflow");
    let behavior = FunctionBehavior::from_blocks_raw(&blocks, None).expect("fixture ingests");
    let (mut rows, ledger, _) = furnace::smelt(&behavior, &blocks, &conv);

    let target = operand_in_row_index(&rows, (0x1004, 0), 0);
    rows[target].at =
        ruff_r2il::facet::project(&con(99, 8), conv.spaces()).expect("const projects in any table");

    let recon = oracle::reconstruct(&rows, conv.spaces());
    let verdict = oracle::judge(&blocks, &recon, &ledger);

    assert!(!verdict.holds());
    assert_eq!(verdict.mismatches.len(), 1, "exactly the corrupted site");
    assert_eq!(verdict.mismatches[0].site, (0x1004, 0));
    assert_eq!(verdict.matched, 5, "the other five sites still match");
}

#[test]
fn swapped_operand_facets_across_two_ops_fire_both_mismatches() {
    // Cross-row corruption must be visible at BOTH sites, not cancel out (spec gate 3 / S5's
    // forward-comparison blind-spot finding). The two Copy ops' source constants (1 vs 2)
    // guarantee the swapped facets differ.
    let blocks = diamond_with_phi();
    let conv = oracle::permissive_convention(&blocks).expect("fixed spaces cannot overflow");
    let behavior = FunctionBehavior::from_blocks_raw(&blocks, None).expect("fixture ingests");
    let (mut rows, ledger, _) = furnace::smelt(&behavior, &blocks, &conv);

    let first = operand_in_row_index(&rows, (0x1004, 0), 0);
    let second = operand_in_row_index(&rows, (0x1008, 0), 0);
    assert_ne!(
        rows[first].at, rows[second].at,
        "anti-vacuity: the two source constants must project to different facets"
    );
    let tmp = rows[first].at;
    rows[first].at = rows[second].at;
    rows[second].at = tmp;

    let recon = oracle::reconstruct(&rows, conv.spaces());
    let verdict = oracle::judge(&blocks, &recon, &ledger);

    assert!(!verdict.holds());
    let mut sites: Vec<(u64, usize)> = verdict.mismatches.iter().map(|m| m.site).collect();
    sites.sort_unstable();
    assert_eq!(sites, vec![(0x1004, 0), (0x1008, 0)]);
}

#[test]
fn a_site_with_neither_rows_nor_residuals_is_an_orphan() {
    let blocks = diamond_with_phi();
    let conv = oracle::permissive_convention(&blocks).expect("fixed spaces cannot overflow");
    let behavior = FunctionBehavior::from_blocks_raw(&blocks, None).expect("fixture ingests");
    let (rows, ledger, _) = furnace::smelt(&behavior, &blocks, &conv);

    let victim = (0x100c_u64, 0_usize);
    let filtered: Vec<FlatFact> = rows
        .iter()
        .copied()
        .filter(|row| row.prov.op_site != Some(victim))
        .collect();
    assert!(
        filtered.len() < rows.len(),
        "anti-vacuity: the filter must actually remove the victim's rows"
    );

    let recon = oracle::reconstruct(&filtered, conv.spaces());
    let verdict = oracle::judge(&blocks, &recon, &ledger);

    assert!(!verdict.holds());
    assert_eq!(verdict.orphans, vec![victim]);
    assert_eq!(verdict.matched, 5);
}

#[test]
fn load_and_fence_fire_their_typed_attribute_gaps() {
    let mut b0 = R2ILBlock::new(0x2000, 4);
    b0.push(R2ILOp::Load {
        dst: reg(0x0, 8),
        space: SpaceId::Ram,
        addr: con(0x8000, 8),
    });
    b0.push(R2ILOp::Fence {
        ordering: MemoryOrdering::SeqCst,
    });
    b0.push(R2ILOp::Branch {
        target: con(0x2004, 8),
    });
    let mut b1 = R2ILBlock::new(0x2004, 4);
    b1.push(R2ILOp::Return {
        target: reg(0x0, 8),
    });
    let blocks = vec![b0, b1];

    let conv = oracle::permissive_convention(&blocks).expect("fixed spaces cannot overflow");
    let (_, _, verdict) = run_oracle(&blocks, &conv);

    assert!(verdict.holds());
    // Fence's skeleton is (Fence, None, []) — trivially equal — so its ONLY semantic content
    // travels through the gap channel; a silent pass here would be the exact defect the
    // channel exists to prevent.
    let mut gaps: Vec<(&str, (u64, usize))> = verdict
        .attribute_gaps
        .iter()
        .map(|gap| (gap.attribute.as_str(), gap.site))
        .collect();
    gaps.sort_unstable();
    assert_eq!(
        gaps,
        vec![
            ("memory_ordering", (0x2000, 1)),
            ("memory_space", (0x2000, 0)),
        ]
    );
}

#[test]
fn gaps_of_covers_exactly_the_twelve_table_variants() {
    use ruff_r2il::ore::OpTag;

    let twelve = [
        OpTag::Load,
        OpTag::Store,
        OpTag::Fence,
        OpTag::LoadLinked,
        OpTag::StoreConditional,
        OpTag::AtomicCAS,
        OpTag::LoadGuarded,
        OpTag::StoreGuarded,
        OpTag::CallOther,
        OpTag::Subpiece,
        OpTag::PtrAdd,
        OpTag::PtrSub,
    ];
    for tag in twelve {
        assert!(
            !oracle::gaps_of(tag).is_empty(),
            "{} must carry at least one gap attribute",
            tag.as_str()
        );
    }
    for tag in [
        OpTag::Copy,
        OpTag::IntAdd,
        OpTag::Branch,
        OpTag::Return,
        OpTag::BoolAnd,
        OpTag::Phi,
    ] {
        assert!(
            oracle::gaps_of(tag).is_empty(),
            "{} must carry no gap attribute",
            tag.as_str()
        );
    }
}

#[test]
fn gap_attribute_has_no_catch_all_and_stable_names() {
    // Mirrors slag.rs's `there_is_no_catch_all_reason`: every variant's name is in ALL, ALL
    // has no duplicates, and none of the names is a catch-all.
    let all = GapAttribute::ALL;
    let variants = [
        GapAttribute::MemorySpace,
        GapAttribute::MemoryOrdering,
        GapAttribute::UserOpIndex,
        GapAttribute::SubpieceOffset,
        GapAttribute::PtrElementSize,
    ];
    assert_eq!(all.len(), variants.len());
    for v in variants {
        assert!(all.contains(&v.as_str()));
        assert!(!matches!(v.as_str(), "other" | "unknown" | "misc"));
    }
    let mut sorted: Vec<&str> = all.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), all.len(), "no duplicate names");
}

// Unlike `src/`, ruff's directory-scoped `clippy.toml` disallowed-methods list does NOT reach
// this integration-test target, so `std::env::temp_dir`/`std::fs` need no suppression here (an
// `#[expect]` would itself fail as an unfulfilled expectation under `-D warnings`).
#[test]
fn the_artifact_mediated_verdict_equals_the_in_memory_verdict() {
    let blocks = diamond_with_phi();
    let conv = oracle::permissive_convention(&blocks).expect("fixed spaces cannot overflow");
    let behavior = FunctionBehavior::from_blocks_raw(&blocks, None).expect("fixture ingests");
    let (rows, ledger, report) = furnace::smelt(&behavior, &blocks, &conv);

    let in_memory = oracle::judge(&blocks, &oracle::reconstruct(&rows, conv.spaces()), &ledger);

    let dir = std::env::temp_dir().join(format!("ruff_r2il_oracle_arm_{}", std::process::id()));
    let mut sink_backend = OfflineSink::new(&dir);
    sink_backend
        .write_harvest("oracle_arm", &rows, &ledger, &report)
        .expect("offline write succeeds");

    let facts_path = dir.join("oracle_arm.facts.tsv");
    let residuals_path = dir.join("oracle_arm.residuals.tsv");
    let read_rows = sink::read_facts(&facts_path).expect("v2 facts read back");
    let read_residuals = sink::read_residuals(&residuals_path).expect("v2 residuals read back");
    std::fs::remove_dir_all(&dir).ok();

    let mut read_ledger = ResidualLedger::new();
    for row in read_residuals {
        read_ledger.push(row);
    }

    let recon: Reconstruction = oracle::reconstruct(&read_rows, conv.spaces());
    let from_artifacts = oracle::judge(&blocks, &recon, &read_ledger);

    // The load-bearing claim of the v2 schemas: everything the oracle needs survives the
    // write→read round trip, so the two verdicts are EQUAL — same counts, same gap census,
    // same (empty) orphan/mismatch lists.
    assert_eq!(from_artifacts, in_memory);
    assert!(from_artifacts.holds());
    assert_eq!(from_artifacts.matched, 6);
}
