//! Stage 1 — the **ore carrier**.
//!
//! [`FunctionBehavior`] is a truthful, lossless, zero-copy assembly of upstream values. It
//! **never flattens at intake** (that is [`crate::furnace`]'s job), it invents no vocabulary, and
//! it decides nothing. It is NOT a "behavioral contract" — nothing here is refined, classified,
//! or proposed. It holds r2sleigh's own `SSAFunction` / `SsaGraph` / `PreparedFunctionFacts` (as
//! one [`SsaArtifact`]) and *names* that decomposition through borrowed accessors. Refined
//! concern contracts are a later, measured furnace output; this type must never grow into one.

use std::collections::BTreeMap;

use r2il::{ArchSpec, R2ILBlock};
use r2ssa::{
    CallSiteFacts, FunctionSemanticSummary, InstId, InterprocFunctionId, InterprocFunctionInput,
    InterprocSolveConfig, MemorySSAFacts, ObjectModel, PredicateFacts, PreparedFunctionFacts,
    SSAFunction, SSAVar, SsaArtifact, SsaGraph, UseSite, ValueId, solve_interproc_summary_set,
};

/// Identity metadata for one function, named apart from the ore itself.
///
/// `entry` mirrors [`SSAFunction::entry`]; `name` mirrors [`SSAFunction::name`]; `arch` is
/// **advisory provenance only** — the `ArchSpec::name` supplied at ingest, when one was — never
/// an address, never branched on. When `arch` is `None`, register operands fall back to
/// r2sleigh's own `"reg:<hex>"` display convention rather than a resolved name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionIdentity {
    pub entry: u64,
    pub name: Option<String>,
    pub arch: Option<String>,
}

/// The ORE CARRIER for one function.
///
/// NOT a "behavioral contract" — nothing here is refined, classified or proposed. It holds
/// r2sleigh's own `SSAFunction` / `SsaGraph` / `PreparedFunctionFacts` (as one [`SsaArtifact`])
/// and NAMES that decomposition through borrowed accessors. Refined concern contracts are a
/// later, measured furnace output; this type must never grow into one.
///
/// `JUDGMENT` — one [`SsaArtifact`] instead of three sibling fields. The plan sketches
/// `{identity, ssa, graph, facts, summary}`. `SsaArtifact` *is* that triple upstream, its
/// constructor `new` is **private**, and [`InterprocFunctionInput::prepared`] requires
/// `&SsaArtifact` — a hand-assembled triple could never produce the `summary` the same struct
/// declares. Holding the artifact keeps all five concerns verbatim as accessors. Nothing is
/// copied.
#[derive(Debug, Clone)]
pub struct FunctionBehavior {
    identity: FunctionIdentity,
    artifact: SsaArtifact,
    summary: Option<FunctionSemanticSummary>,
}

impl FunctionBehavior {
    /// RAW ingest: `SsaArtifact::raw(blocks, arch)` → `SSAFunction::from_blocks_raw` →
    /// `SsaGraph::from_function` → `PreparedFunctionFacts::collect`. `None` exactly when
    /// upstream is: empty `blocks`, or `CFG::from_blocks` fails.
    ///
    /// This is the losslessness-claiming ingest path — no optimization pass runs.
    pub fn from_blocks_raw(blocks: &[R2ILBlock], arch: Option<&ArchSpec>) -> Option<Self> {
        let artifact = SsaArtifact::raw(blocks, arch)?;
        let function = artifact.function();
        let identity = FunctionIdentity {
            entry: function.entry,
            name: function.name.clone(),
            arch: arch.map(|spec| spec.name.clone()),
        };
        Some(Self {
            identity,
            artifact,
            summary: None,
        })
    }

    /// GENERIC ingest (`SsaArtifact::from_blocks`) — applies constructor-time SCCP and can
    /// REWRITE ops. Never use it for anything claiming losslessness.
    pub fn from_blocks(blocks: &[R2ILBlock], arch: Option<&ArchSpec>) -> Option<Self> {
        let artifact = SsaArtifact::from_blocks(blocks, arch)?;
        let function = artifact.function();
        let identity = FunctionIdentity {
            entry: function.entry,
            name: function.name.clone(),
            arch: arch.map(|spec| spec.name.clone()),
        };
        Some(Self {
            identity,
            artifact,
            summary: None,
        })
    }

    /// Attach a name, keeping the carrier's own [`FunctionIdentity`] and the wrapped
    /// [`SsaArtifact`] (and thereby its [`SSAFunction::name`]) in sync.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        self.artifact = self.artifact.with_name(name.clone());
        self.identity.name = Some(name);
        self
    }

    /// Attach a precomputed [`FunctionSemanticSummary`] (e.g. seeded, or solved elsewhere).
    pub fn with_summary(mut self, summary: FunctionSemanticSummary) -> Self {
        self.summary = Some(summary);
        self
    }

    /// Solve one [`FunctionSemanticSummary`] for this function in isolation: one
    /// `InterprocFunctionInput { id, name, prepared: self.artifact() }` →
    /// `solve_interproc_summary_set(&[input], arch, Some(id), &BTreeMap::new(),
    /// InterprocSolveConfig::default())` → `set.summaries.remove(&id)`.
    ///
    /// Single-function scope: `has_unknown_calls` true whenever the function calls anything is
    /// CORRECT under this scope, not a defect — a wider call graph is out of scope for the ore
    /// carrier and belongs to a caller that assembles multiple `FunctionBehavior`s itself.
    pub fn solve_summary(&mut self, id: InterprocFunctionId, arch: Option<&ArchSpec>) {
        let input = InterprocFunctionInput {
            id,
            name: self.identity.name.clone(),
            prepared: &self.artifact,
        };
        let mut set = solve_interproc_summary_set(
            &[input],
            arch,
            Some(id),
            &BTreeMap::new(),
            InterprocSolveConfig::default(),
        );
        self.summary = set.summaries.remove(&id);
    }

    /// Identity metadata (entry / name / advisory arch provenance).
    pub fn identity(&self) -> &FunctionIdentity {
        &self.identity
    }

    /// CONTROL — CFG, `BlockTerminator`, `CFGEdge`, block order.
    pub fn control(&self) -> &SSAFunction {
        self.artifact.function()
    }

    /// Alias of [`Self::control`] — kept because the plan names the field `ssa`.
    pub fn ssa(&self) -> &SSAFunction {
        self.control()
    }

    /// VALUES + DEF/USE + PROVENANCE — insts, values, and the `(block_addr, op_idx)` join maps.
    pub fn values(&self) -> &SsaGraph {
        self.artifact.graph()
    }

    /// Alias of [`Self::values`].
    pub fn graph(&self) -> &SsaGraph {
        self.artifact.graph()
    }

    /// The full prepared-facts bundle (objects, memory, predicates, call sites).
    pub fn facts(&self) -> &PreparedFunctionFacts {
        self.artifact.facts()
    }

    pub fn objects(&self) -> &ObjectModel {
        self.artifact.objects()
    }

    pub fn memory(&self) -> &MemorySSAFacts {
        self.artifact.memory()
    }

    pub fn predicates(&self) -> &PredicateFacts {
        self.artifact.predicates()
    }

    pub fn calls(&self) -> &CallSiteFacts {
        self.artifact.call_sites()
    }

    /// The wrapped [`SsaArtifact`] itself — borrowed, never cloned out from under the carrier.
    pub fn artifact(&self) -> &SsaArtifact {
        &self.artifact
    }

    /// The solved interprocedural summary, if [`Self::solve_summary`] or [`Self::with_summary`]
    /// has been called.
    pub fn summary(&self) -> Option<&FunctionSemanticSummary> {
        self.summary.as_ref()
    }

    // ---- provenance helpers -------------------------------------------------------------

    /// `InstId` → `(block_addr, op_idx)`, via `SsaGraph::op_site_for_inst`.
    pub fn op_site(&self, inst: InstId) -> Option<(u64, usize)> {
        self.artifact.graph().op_site_for_inst(inst)
    }

    /// `(block_addr, op_idx)` → `InstId`, via `SsaGraph::inst_id_for_op_site`. The inverse of
    /// [`Self::op_site`] — see §8 test 6's round trip.
    pub fn inst_at(&self, block_addr: u64, op_idx: usize) -> Option<InstId> {
        self.artifact.graph().inst_id_for_op_site(block_addr, op_idx)
    }

    /// `ValueId` → the `SSAVar` it interns, via `SsaGraph::value`.
    pub fn value_var(&self, value: ValueId) -> Option<&SSAVar> {
        self.artifact.graph().value(value).map(|graph_value| &graph_value.var)
    }

    /// `ValueId` → the `InstId` that defines it, via `SsaGraph::def_inst`.
    pub fn def_inst(&self, value: ValueId) -> Option<InstId> {
        self.artifact.graph().def_inst(value)
    }

    /// `ValueId` → every site that uses it, via `SsaGraph::use_sites`.
    pub fn use_sites(&self, value: ValueId) -> &[UseSite] {
        self.artifact.graph().use_sites(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use r2ssa::InstPayload;

    fn reg(offset: u64, size: u32) -> r2il::Varnode {
        r2il::Varnode::register(offset, size)
    }

    fn con(value: u64, size: u32) -> r2il::Varnode {
        r2il::Varnode::constant(value, size)
    }

    /// A minimal 2-block, no-merge, no-phi fixture: `0x1000` computes and unconditionally
    /// branches to `0x1004`, which returns. Every op in the source survives 1:1 into SSA (no
    /// `Multiequal` here, so the total inst count is exactly the source op count).
    fn two_block_fixture() -> Vec<R2ILBlock> {
        let mut b0 = R2ILBlock::new(0x1000, 4);
        b0.push(r2il::R2ILOp::IntAdd {
            dst: reg(0x00, 8),
            a: reg(0x00, 8),
            b: con(1, 8),
        });
        b0.push(r2il::R2ILOp::Branch {
            target: con(0x1004, 8),
        });

        let mut b1 = R2ILBlock::new(0x1004, 4);
        b1.push(r2il::R2ILOp::Return {
            target: reg(0x00, 8),
        });

        vec![b0, b1]
    }

    #[test]
    fn from_blocks_raw_names_the_upstream_decomposition() {
        let blocks = two_block_fixture();
        let expected_ops: usize = blocks.iter().map(|block| block.ops.len()).sum();

        let behavior =
            FunctionBehavior::from_blocks_raw(&blocks, None).expect("two linear blocks ingest");

        assert_eq!(behavior.identity().entry, 0x1000);
        assert_eq!(behavior.control().num_blocks(), 2);
        assert_eq!(behavior.values().insts.len(), expected_ops);
        assert!(!behavior.values().values.is_empty());
    }

    #[test]
    fn empty_block_list_is_none_not_a_panic() {
        assert!(FunctionBehavior::from_blocks_raw(&[], None).is_none());

        let mut single = R2ILBlock::new(0x2000, 2);
        single.push(r2il::R2ILOp::Return {
            target: con(0, 8),
        });
        assert!(FunctionBehavior::from_blocks_raw(&[single], None).is_some());
    }

    #[test]
    fn op_site_round_trips_through_the_graph_provenance_map() {
        let blocks = two_block_fixture();
        let behavior =
            FunctionBehavior::from_blocks_raw(&blocks, None).expect("two linear blocks ingest");

        let mut checked = 0usize;
        for inst in &behavior.values().insts {
            if !matches!(inst.payload, InstPayload::Op(_)) {
                continue;
            }
            let site = behavior
                .op_site(inst.id)
                .expect("every Op-payload inst must carry a (block_addr, op_idx) site");
            assert_eq!(behavior.inst_at(site.0, site.1), Some(inst.id));
            checked += 1;
        }
        assert!(checked >= 3, "expected at least 3 op sites, got {checked}");
    }
}
