//! Stage 2 — the **ore**: deterministic typed fact enumeration over the object graph.
//!
//! > **"Varnode in the first stage is pointer chasing stacked god objects — hence the ore
//! > furnace slag."** (operator, 2026-08-18)
//!
//! The upstream object graph [`crate::behavior::FunctionBehavior`] carries — a private
//! `HashMap<u64, SSABlock>`, a petgraph CFG, `BTreeMap<SSAVar, ValueId>` keyed by
//! String-carrying vars, facts as nested `BTreeMap`s of structs — is GOOD ORE and structurally
//! still stage-1 pointer chasing. **Typed ≠ refined.** Mistaking the cleanliness of r2il's Rust
//! types for refinement *is* the privileged-direct-path the pivot forbids. This module *reads*
//! that graph and emits typed [`OreFact`] rows; it does **not** flatten (that is
//! [`crate::furnace`]) and it does **not** classify (that is [`crate::convention`], applied by
//! [`crate::furnace`]).
//!
//! # The load-bearing consequence: the facet coordinate is NOT recoverable from SSA
//!
//! `SSAVar` carries `name` / `version` / `size` and **no offset and no `SpaceId`**. The only
//! SSA-side trace of a varnode's offset is inside its *display name* (`"reg:10"`,
//! `"space7:1000"`) — and parsing display strings is forbidden as a data path
//! (`format!("{:?}")` is FORBIDDEN as a data path throughout this crate). Therefore
//! [`enumerate`] takes BOTH the [`crate::behavior::FunctionBehavior`] AND the source
//! `&[R2ILBlock]`: operand coordinates come from the **typed** [`Varnode`]s in the source ops,
//! joined to SSA instructions by the `(block_addr, op_idx)` key
//! `SsaGraph::op_inst_by_site` / `op_site_by_inst` provide.
//!
//! The join is **verified, not assumed**: at each `InstPayload::Op` site we compare
//! `OpTag::from_op(ssa_op)` (what SSA expects at this site) against `OpTag::from_r2il` of the
//! r2il op actually found at the joined `(block_addr, op_idx)` (what the source actually has). A
//! mismatch — the `Multiequal` index shift (a phi extracted from `.ops` into `.phis` leaves a
//! later r2il op's `.ops` index pointing at what SSA calls a different instruction), or a
//! `CallDefine` insertion (a synthetic op with no r2il counterpart at all, shifting every later
//! index) — emits [`OreFact::JoinFailure`], **never** a silently mis-attributed coordinate.
//!
//! Rows with no source varnode at all — phi inputs (routed through the dedicated
//! [`OreFact::PhiInput`], which carries no facet-coordinate fields to mis-attribute) and
//! `SSAOp::CallDefine` (a fresh unknown-register value with no source op whatsoever, so no join
//! is even attempted for it) — are the furnace's `NoFacetCoordinate` residual once melted; this
//! module simply never manufactures a coordinate for them. Nothing is dropped: every
//! `InstPayload` produces an [`OreFact::Op`] row regardless of whether its operand coordinates
//! could be recovered.
//!
//! # `FactProvenance.value` convention
//!
//! [`FactProvenance::value`] is "the single [`ValueId`] this particular row is most directly
//! about, if any": an [`OreFact::Op`] row carries the instruction's own output (the value it
//! defines); an [`OreFact::Operand`] row carries that operand's own value (duplicating the
//! variant's own `value` field so a generic consumer that only inspects `prov` still sees it);
//! an [`OreFact::PhiInput`] row carries that input's value; an [`OreFact::Predicate`] row
//! carries the branch condition; an [`OreFact::CallSite`] row carries the call target;
//! [`OreFact::MemoryUse`] / [`OreFact::MemoryDef`] leave it `None` (a memory fact's identity is
//! an `ObjectId`, a distinct fact-space from a single SSA value).
//!
//! # Enumeration order — fixed and documented (the determinism guarantee)
//!
//! 1. **Blocks** in [`SSAFunction::block_addrs`] order (reverse postorder, a `Vec` — *never* the
//!    private `HashMap`).
//! 2. **Within a block**, `GraphBlock::insts` order (phis then ops, each in `GraphInst::ordinal`
//!    order — already the construction order upstream; never re-sorted here):
//!    - a `Phi` payload emits one [`OreFact::Op`] (`opcode = OpTag::Phi`) and nothing else at
//!      this step (no join target exists for a phi);
//!    - an `Op` payload emits one [`OreFact::Op`], then — unless the op is `SSAOp::CallDefine`
//!      (no source op exists at all) — attempts the op-site join: on a tag match, one
//!      [`OreFact::Operand`] per source-op input (in `R2ILOp::inputs()` order) then one for the
//!      output if present; on a tag mismatch, one [`OreFact::JoinFailure`]; if no r2il op exists
//!      at the joined site at all (an index-shift landed past the end of the source block),
//!      neither is emitted — the [`OreFact::Op`] row already stands for the instruction.
//!    - then, once every inst in the block has been visited: one [`OreFact::Edge`] per
//!      `GraphBlock::successors` entry, in that `Vec`'s order;
//!    - then one [`OreFact::PhiInput`] per phi source, for every phi in the block, in phi-then-
//!      source order.
//! 3. **Then, function-wide** (every container touched is a `BTreeMap` — deterministic key
//!    order, never a `HashMap`):
//!    - [`OreFact::MemoryUse`] then [`OreFact::MemoryDef`], both by ascending [`InstId`]
//!      (`MemorySSAFacts::uses_by_inst` / `defs_by_inst` are already `BTreeMap<InstId, _>`);
//!    - [`OreFact::Predicate`], by ascending [`PredicateId`] (`PredicateFacts::predicates`);
//!    - [`OreFact::CallSite`], by ascending [`CallSiteId`] (`CallSiteFacts::by_id`).

use r2il::{R2ILBlock, R2ILOp, SpaceId};
use r2ssa::{
    BlockId, BlockTerminator, CallSiteId, CompareKind, InstId, InstPayload, ObjectId, PredicateId,
    SSAOp, ValueId,
};

use crate::behavior::FunctionBehavior;

// ============================================================================================
// OpTag
// ============================================================================================

/// One variant per `SSAOp` variant. Closed, exhaustive, stable `as_str()` — the discipline
/// `ruff_spo_triplet::Predicate` enforces ("frontends MUST NOT emit raw predicate strings").
/// `from_op` and `from_r2il` are TOTAL matches; `format!("{:?}")` is FORBIDDEN as a data path.
///
/// `Phi` and `CallDefine` exist only on the SSA side (`r2il::R2ILOp` has neither: a phi arrives
/// as `Multiequal`, and `CallDefine` is synthesized during renaming with no r2il counterpart at
/// all). `r2il::R2ILOp::Multiequal` and `::Indirect` exist only on the r2il side and map onto
/// `OpTag::Phi` / `OpTag::Copy` respectively, mirroring `rename_op`'s documented mapping
/// (`Multiequal → SSAOp::Phi`, `Indirect → SSAOp::Copy`, `r2ssa/src/rename.rs:1087,1101`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OpTag {
    Phi,
    Copy,
    Load,
    Store,
    Fence,
    LoadLinked,
    StoreConditional,
    AtomicCAS,
    LoadGuarded,
    StoreGuarded,
    IntAdd,
    IntSub,
    IntMult,
    IntDiv,
    IntSDiv,
    IntRem,
    IntSRem,
    IntNegate,
    IntCarry,
    IntSCarry,
    IntSBorrow,
    IntAnd,
    IntOr,
    IntXor,
    IntNot,
    IntLeft,
    IntRight,
    IntSRight,
    IntEqual,
    IntNotEqual,
    IntLess,
    IntSLess,
    IntLessEqual,
    IntSLessEqual,
    IntZExt,
    IntSExt,
    BoolNot,
    BoolAnd,
    BoolOr,
    BoolXor,
    Piece,
    Subpiece,
    PopCount,
    Lzcount,
    Branch,
    CBranch,
    BranchInd,
    Call,
    CallInd,
    CallDefine,
    Return,
    FloatAdd,
    FloatSub,
    FloatMult,
    FloatDiv,
    FloatNeg,
    FloatAbs,
    FloatSqrt,
    FloatCeil,
    FloatFloor,
    FloatRound,
    FloatNaN,
    FloatEqual,
    FloatNotEqual,
    FloatLess,
    FloatLessEqual,
    Int2Float,
    Float2Int,
    FloatFloat,
    Trunc,
    CallOther,
    Nop,
    Unimplemented,
    CpuId,
    Breakpoint,
    PtrAdd,
    PtrSub,
    SegmentOp,
    New,
    Cast,
    Extract,
    Insert,
}

impl OpTag {
    /// One arm per `SSAOp` variant (`r2ssa/src/op.rs`), total match — a variant added upstream
    /// without a corresponding arm here fails to compile, which is the point.
    pub fn from_op(op: &SSAOp) -> Self {
        match op {
            SSAOp::Phi { .. } => OpTag::Phi,
            SSAOp::Copy { .. } => OpTag::Copy,
            SSAOp::Load { .. } => OpTag::Load,
            SSAOp::Store { .. } => OpTag::Store,
            SSAOp::Fence { .. } => OpTag::Fence,
            SSAOp::LoadLinked { .. } => OpTag::LoadLinked,
            SSAOp::StoreConditional { .. } => OpTag::StoreConditional,
            SSAOp::AtomicCAS { .. } => OpTag::AtomicCAS,
            SSAOp::LoadGuarded { .. } => OpTag::LoadGuarded,
            SSAOp::StoreGuarded { .. } => OpTag::StoreGuarded,
            SSAOp::IntAdd { .. } => OpTag::IntAdd,
            SSAOp::IntSub { .. } => OpTag::IntSub,
            SSAOp::IntMult { .. } => OpTag::IntMult,
            SSAOp::IntDiv { .. } => OpTag::IntDiv,
            SSAOp::IntSDiv { .. } => OpTag::IntSDiv,
            SSAOp::IntRem { .. } => OpTag::IntRem,
            SSAOp::IntSRem { .. } => OpTag::IntSRem,
            SSAOp::IntNegate { .. } => OpTag::IntNegate,
            SSAOp::IntCarry { .. } => OpTag::IntCarry,
            SSAOp::IntSCarry { .. } => OpTag::IntSCarry,
            SSAOp::IntSBorrow { .. } => OpTag::IntSBorrow,
            SSAOp::IntAnd { .. } => OpTag::IntAnd,
            SSAOp::IntOr { .. } => OpTag::IntOr,
            SSAOp::IntXor { .. } => OpTag::IntXor,
            SSAOp::IntNot { .. } => OpTag::IntNot,
            SSAOp::IntLeft { .. } => OpTag::IntLeft,
            SSAOp::IntRight { .. } => OpTag::IntRight,
            SSAOp::IntSRight { .. } => OpTag::IntSRight,
            SSAOp::IntEqual { .. } => OpTag::IntEqual,
            SSAOp::IntNotEqual { .. } => OpTag::IntNotEqual,
            SSAOp::IntLess { .. } => OpTag::IntLess,
            SSAOp::IntSLess { .. } => OpTag::IntSLess,
            SSAOp::IntLessEqual { .. } => OpTag::IntLessEqual,
            SSAOp::IntSLessEqual { .. } => OpTag::IntSLessEqual,
            SSAOp::IntZExt { .. } => OpTag::IntZExt,
            SSAOp::IntSExt { .. } => OpTag::IntSExt,
            SSAOp::BoolNot { .. } => OpTag::BoolNot,
            SSAOp::BoolAnd { .. } => OpTag::BoolAnd,
            SSAOp::BoolOr { .. } => OpTag::BoolOr,
            SSAOp::BoolXor { .. } => OpTag::BoolXor,
            SSAOp::Piece { .. } => OpTag::Piece,
            SSAOp::Subpiece { .. } => OpTag::Subpiece,
            SSAOp::PopCount { .. } => OpTag::PopCount,
            SSAOp::Lzcount { .. } => OpTag::Lzcount,
            SSAOp::Branch { .. } => OpTag::Branch,
            SSAOp::CBranch { .. } => OpTag::CBranch,
            SSAOp::BranchInd { .. } => OpTag::BranchInd,
            SSAOp::Call { .. } => OpTag::Call,
            SSAOp::CallInd { .. } => OpTag::CallInd,
            SSAOp::CallDefine { .. } => OpTag::CallDefine,
            SSAOp::Return { .. } => OpTag::Return,
            SSAOp::FloatAdd { .. } => OpTag::FloatAdd,
            SSAOp::FloatSub { .. } => OpTag::FloatSub,
            SSAOp::FloatMult { .. } => OpTag::FloatMult,
            SSAOp::FloatDiv { .. } => OpTag::FloatDiv,
            SSAOp::FloatNeg { .. } => OpTag::FloatNeg,
            SSAOp::FloatAbs { .. } => OpTag::FloatAbs,
            SSAOp::FloatSqrt { .. } => OpTag::FloatSqrt,
            SSAOp::FloatCeil { .. } => OpTag::FloatCeil,
            SSAOp::FloatFloor { .. } => OpTag::FloatFloor,
            SSAOp::FloatRound { .. } => OpTag::FloatRound,
            SSAOp::FloatNaN { .. } => OpTag::FloatNaN,
            SSAOp::FloatEqual { .. } => OpTag::FloatEqual,
            SSAOp::FloatNotEqual { .. } => OpTag::FloatNotEqual,
            SSAOp::FloatLess { .. } => OpTag::FloatLess,
            SSAOp::FloatLessEqual { .. } => OpTag::FloatLessEqual,
            SSAOp::Int2Float { .. } => OpTag::Int2Float,
            SSAOp::Float2Int { .. } => OpTag::Float2Int,
            SSAOp::FloatFloat { .. } => OpTag::FloatFloat,
            SSAOp::Trunc { .. } => OpTag::Trunc,
            SSAOp::CallOther { .. } => OpTag::CallOther,
            SSAOp::Nop => OpTag::Nop,
            SSAOp::Unimplemented => OpTag::Unimplemented,
            SSAOp::CpuId { .. } => OpTag::CpuId,
            SSAOp::Breakpoint => OpTag::Breakpoint,
            SSAOp::PtrAdd { .. } => OpTag::PtrAdd,
            SSAOp::PtrSub { .. } => OpTag::PtrSub,
            SSAOp::SegmentOp { .. } => OpTag::SegmentOp,
            SSAOp::New { .. } => OpTag::New,
            SSAOp::Cast { .. } => OpTag::Cast,
            SSAOp::Extract { .. } => OpTag::Extract,
            SSAOp::Insert { .. } => OpTag::Insert,
        }
    }

    /// One arm per `R2ILOp` variant (`r2il/src/opcode.rs`), total match. `Multiequal` and
    /// `Indirect` are the two r2il-only variants and route to `Phi` / `Copy` respectively,
    /// matching `rename_op`; every other variant maps to the identically-named `OpTag`.
    pub fn from_r2il(op: &R2ILOp) -> Self {
        match op {
            R2ILOp::Copy { .. } => OpTag::Copy,
            R2ILOp::Load { .. } => OpTag::Load,
            R2ILOp::Store { .. } => OpTag::Store,
            R2ILOp::Fence { .. } => OpTag::Fence,
            R2ILOp::LoadLinked { .. } => OpTag::LoadLinked,
            R2ILOp::StoreConditional { .. } => OpTag::StoreConditional,
            R2ILOp::AtomicCAS { .. } => OpTag::AtomicCAS,
            R2ILOp::LoadGuarded { .. } => OpTag::LoadGuarded,
            R2ILOp::StoreGuarded { .. } => OpTag::StoreGuarded,
            R2ILOp::IntAdd { .. } => OpTag::IntAdd,
            R2ILOp::IntSub { .. } => OpTag::IntSub,
            R2ILOp::IntMult { .. } => OpTag::IntMult,
            R2ILOp::IntDiv { .. } => OpTag::IntDiv,
            R2ILOp::IntSDiv { .. } => OpTag::IntSDiv,
            R2ILOp::IntRem { .. } => OpTag::IntRem,
            R2ILOp::IntSRem { .. } => OpTag::IntSRem,
            R2ILOp::IntNegate { .. } => OpTag::IntNegate,
            R2ILOp::IntCarry { .. } => OpTag::IntCarry,
            R2ILOp::IntSCarry { .. } => OpTag::IntSCarry,
            R2ILOp::IntSBorrow { .. } => OpTag::IntSBorrow,
            R2ILOp::IntAnd { .. } => OpTag::IntAnd,
            R2ILOp::IntOr { .. } => OpTag::IntOr,
            R2ILOp::IntXor { .. } => OpTag::IntXor,
            R2ILOp::IntNot { .. } => OpTag::IntNot,
            R2ILOp::IntLeft { .. } => OpTag::IntLeft,
            R2ILOp::IntRight { .. } => OpTag::IntRight,
            R2ILOp::IntSRight { .. } => OpTag::IntSRight,
            R2ILOp::IntEqual { .. } => OpTag::IntEqual,
            R2ILOp::IntNotEqual { .. } => OpTag::IntNotEqual,
            R2ILOp::IntLess { .. } => OpTag::IntLess,
            R2ILOp::IntSLess { .. } => OpTag::IntSLess,
            R2ILOp::IntLessEqual { .. } => OpTag::IntLessEqual,
            R2ILOp::IntSLessEqual { .. } => OpTag::IntSLessEqual,
            R2ILOp::IntZExt { .. } => OpTag::IntZExt,
            R2ILOp::IntSExt { .. } => OpTag::IntSExt,
            R2ILOp::BoolNot { .. } => OpTag::BoolNot,
            R2ILOp::BoolAnd { .. } => OpTag::BoolAnd,
            R2ILOp::BoolOr { .. } => OpTag::BoolOr,
            R2ILOp::BoolXor { .. } => OpTag::BoolXor,
            R2ILOp::Piece { .. } => OpTag::Piece,
            R2ILOp::Subpiece { .. } => OpTag::Subpiece,
            R2ILOp::PopCount { .. } => OpTag::PopCount,
            R2ILOp::Lzcount { .. } => OpTag::Lzcount,
            R2ILOp::Branch { .. } => OpTag::Branch,
            R2ILOp::CBranch { .. } => OpTag::CBranch,
            R2ILOp::BranchInd { .. } => OpTag::BranchInd,
            R2ILOp::Call { .. } => OpTag::Call,
            R2ILOp::CallInd { .. } => OpTag::CallInd,
            R2ILOp::Return { .. } => OpTag::Return,
            R2ILOp::FloatAdd { .. } => OpTag::FloatAdd,
            R2ILOp::FloatSub { .. } => OpTag::FloatSub,
            R2ILOp::FloatMult { .. } => OpTag::FloatMult,
            R2ILOp::FloatDiv { .. } => OpTag::FloatDiv,
            R2ILOp::FloatNeg { .. } => OpTag::FloatNeg,
            R2ILOp::FloatAbs { .. } => OpTag::FloatAbs,
            R2ILOp::FloatSqrt { .. } => OpTag::FloatSqrt,
            R2ILOp::FloatCeil { .. } => OpTag::FloatCeil,
            R2ILOp::FloatFloor { .. } => OpTag::FloatFloor,
            R2ILOp::FloatRound { .. } => OpTag::FloatRound,
            R2ILOp::FloatNaN { .. } => OpTag::FloatNaN,
            R2ILOp::FloatEqual { .. } => OpTag::FloatEqual,
            R2ILOp::FloatNotEqual { .. } => OpTag::FloatNotEqual,
            R2ILOp::FloatLess { .. } => OpTag::FloatLess,
            R2ILOp::FloatLessEqual { .. } => OpTag::FloatLessEqual,
            R2ILOp::Int2Float { .. } => OpTag::Int2Float,
            R2ILOp::Float2Int { .. } => OpTag::Float2Int,
            R2ILOp::FloatFloat { .. } => OpTag::FloatFloat,
            R2ILOp::Trunc { .. } => OpTag::Trunc,
            R2ILOp::CallOther { .. } => OpTag::CallOther,
            R2ILOp::Nop => OpTag::Nop,
            R2ILOp::Unimplemented => OpTag::Unimplemented,
            R2ILOp::CpuId { .. } => OpTag::CpuId,
            R2ILOp::Breakpoint => OpTag::Breakpoint,
            // The two r2il-only variants — see `rename_op` (`r2ssa/src/rename.rs:1087,1101`).
            R2ILOp::Multiequal { .. } => OpTag::Phi,
            R2ILOp::Indirect { .. } => OpTag::Copy,
            R2ILOp::PtrAdd { .. } => OpTag::PtrAdd,
            R2ILOp::PtrSub { .. } => OpTag::PtrSub,
            R2ILOp::SegmentOp { .. } => OpTag::SegmentOp,
            R2ILOp::New { .. } => OpTag::New,
            R2ILOp::Cast { .. } => OpTag::Cast,
            R2ILOp::Extract { .. } => OpTag::Extract,
            R2ILOp::Insert { .. } => OpTag::Insert,
        }
    }

    /// The on-the-wire opcode string (stable snake_case; never reformat).
    pub fn as_str(self) -> &'static str {
        match self {
            OpTag::Phi => "phi",
            OpTag::Copy => "copy",
            OpTag::Load => "load",
            OpTag::Store => "store",
            OpTag::Fence => "fence",
            OpTag::LoadLinked => "load_linked",
            OpTag::StoreConditional => "store_conditional",
            OpTag::AtomicCAS => "atomic_cas",
            OpTag::LoadGuarded => "load_guarded",
            OpTag::StoreGuarded => "store_guarded",
            OpTag::IntAdd => "int_add",
            OpTag::IntSub => "int_sub",
            OpTag::IntMult => "int_mult",
            OpTag::IntDiv => "int_div",
            OpTag::IntSDiv => "int_sdiv",
            OpTag::IntRem => "int_rem",
            OpTag::IntSRem => "int_srem",
            OpTag::IntNegate => "int_negate",
            OpTag::IntCarry => "int_carry",
            OpTag::IntSCarry => "int_scarry",
            OpTag::IntSBorrow => "int_sborrow",
            OpTag::IntAnd => "int_and",
            OpTag::IntOr => "int_or",
            OpTag::IntXor => "int_xor",
            OpTag::IntNot => "int_not",
            OpTag::IntLeft => "int_left",
            OpTag::IntRight => "int_right",
            OpTag::IntSRight => "int_sright",
            OpTag::IntEqual => "int_equal",
            OpTag::IntNotEqual => "int_not_equal",
            OpTag::IntLess => "int_less",
            OpTag::IntSLess => "int_sless",
            OpTag::IntLessEqual => "int_less_equal",
            OpTag::IntSLessEqual => "int_sless_equal",
            OpTag::IntZExt => "int_zext",
            OpTag::IntSExt => "int_sext",
            OpTag::BoolNot => "bool_not",
            OpTag::BoolAnd => "bool_and",
            OpTag::BoolOr => "bool_or",
            OpTag::BoolXor => "bool_xor",
            OpTag::Piece => "piece",
            OpTag::Subpiece => "subpiece",
            OpTag::PopCount => "pop_count",
            OpTag::Lzcount => "lzcount",
            OpTag::Branch => "branch",
            OpTag::CBranch => "cbranch",
            OpTag::BranchInd => "branch_ind",
            OpTag::Call => "call",
            OpTag::CallInd => "call_ind",
            OpTag::CallDefine => "call_define",
            OpTag::Return => "return",
            OpTag::FloatAdd => "float_add",
            OpTag::FloatSub => "float_sub",
            OpTag::FloatMult => "float_mult",
            OpTag::FloatDiv => "float_div",
            OpTag::FloatNeg => "float_neg",
            OpTag::FloatAbs => "float_abs",
            OpTag::FloatSqrt => "float_sqrt",
            OpTag::FloatCeil => "float_ceil",
            OpTag::FloatFloor => "float_floor",
            OpTag::FloatRound => "float_round",
            OpTag::FloatNaN => "float_nan",
            OpTag::FloatEqual => "float_equal",
            OpTag::FloatNotEqual => "float_not_equal",
            OpTag::FloatLess => "float_less",
            OpTag::FloatLessEqual => "float_less_equal",
            OpTag::Int2Float => "int2float",
            OpTag::Float2Int => "float2int",
            OpTag::FloatFloat => "float_float",
            OpTag::Trunc => "trunc",
            OpTag::CallOther => "call_other",
            OpTag::Nop => "nop",
            OpTag::Unimplemented => "unimplemented",
            OpTag::CpuId => "cpu_id",
            OpTag::Breakpoint => "breakpoint",
            OpTag::PtrAdd => "ptr_add",
            OpTag::PtrSub => "ptr_sub",
            OpTag::SegmentOp => "segment_op",
            OpTag::New => "new",
            OpTag::Cast => "cast",
            OpTag::Extract => "extract",
            OpTag::Insert => "insert",
        }
    }
}

// ============================================================================================
// EdgeTag / CompareTag / OperandPos
// ============================================================================================

/// How a CFG edge relates to its source block's terminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EdgeTag {
    /// Neither branch of a conditional, nor closing a cycle (fallthrough, unconditional
    /// branch, call continuation, switch case, …).
    Normal,
    /// The true target of a `BlockTerminator::ConditionalBranch`.
    True,
    /// The false target of a `BlockTerminator::ConditionalBranch`.
    False,
    /// Closes a cycle: the successor's `BlockId` does not come strictly after the source's in
    /// `block_addrs()` (reverse-postorder) order. `JUDGMENT`: a conditional edge that also
    /// closes a cycle (a `while`-loop's back branch) is classified `True`/`False` — the more
    /// specific, locally available fact — never `Back`; this classification applies only to
    /// non-conditional successors.
    Back,
}

/// One `SSAOp::Predicate`/`R2ILOp`-family comparison kind — mirrors `r2ssa::CompareKind`
/// one-for-one so `ore.rs` never has to `format!("{:?}")` an upstream enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CompareTag {
    Equal,
    NotEqual,
    Less,
    SignedLess,
    LessEqual,
    SignedLessEqual,
}

fn compare_tag(kind: CompareKind) -> CompareTag {
    match kind {
        CompareKind::Equal => CompareTag::Equal,
        CompareKind::NotEqual => CompareTag::NotEqual,
        CompareKind::Less => CompareTag::Less,
        CompareKind::SignedLess => CompareTag::SignedLess,
        CompareKind::LessEqual => CompareTag::LessEqual,
        CompareKind::SignedLessEqual => CompareTag::SignedLessEqual,
    }
}

/// Which slot of an op an [`OreFact::Operand`] row describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OperandPos {
    /// The `n`th entry of `R2ILOp::inputs()`, in that `Vec`'s order.
    Input(usize),
    /// `R2ILOp::output()`.
    Output,
}

// ============================================================================================
// FactProvenance
// ============================================================================================

/// Where a fact came from — see the module docs' "`FactProvenance.value` convention" section
/// for the per-`OreFact`-variant population rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FactProvenance {
    pub inst: Option<InstId>,
    pub block: Option<BlockId>,
    pub op_site: Option<(u64, usize)>,
    pub value: Option<ValueId>,
}

// ============================================================================================
// OreFact
// ============================================================================================

/// One typed ore row. Deterministic, total, lossless w.r.t. the SSA surface — see the module
/// docs for the fixed enumeration order [`enumerate`] produces these in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OreFact {
    Op {
        prov: FactProvenance,
        opcode: OpTag,
        ordinal: usize,
        input_arity: usize,
        has_output: bool,
    },
    /// The TYPED coordinate components, taken from the source `Varnode` via the verified
    /// op-site join — never from an `SSAVar` name.
    Operand {
        prov: FactProvenance,
        position: OperandPos,
        value: Option<ValueId>,
        space: SpaceId,
        offset: u64,
        size: u32,
    },
    Edge {
        from: BlockId,
        to: BlockId,
        kind: EdgeTag,
    },
    PhiInput {
        prov: FactProvenance,
        index: usize,
        pred: BlockId,
        value: ValueId,
    },
    MemoryUse {
        prov: FactProvenance,
        object: ObjectId,
        version: u32,
        size: u32,
    },
    MemoryDef {
        prov: FactProvenance,
        object: ObjectId,
        previous: u32,
        next: u32,
        size: u32,
    },
    Predicate {
        prov: FactProvenance,
        id: PredicateId,
        condition: ValueId,
        comparison: Option<CompareTag>,
        true_target: u64,
        false_target: u64,
    },
    CallSite {
        prov: FactProvenance,
        id: CallSiteId,
        target: ValueId,
        direct_target: Option<u64>,
    },
    /// The join failed at this site — emitted so the failure is enumerated, not skipped.
    JoinFailure {
        prov: FactProvenance,
        expected: OpTag,
        found: OpTag,
    },
}

// ============================================================================================
// enumerate
// ============================================================================================

/// Enumerate the ore. Deterministic, total, lossless w.r.t. the SSA surface.
///
/// `blocks` is the SOURCE r2il, required for the typed operand coordinates (see the module
/// docs' "load-bearing consequence" section) — it MUST be the same block list `behavior` was
/// ingested from, or the op-site join has nothing correct to compare against.
pub fn enumerate(behavior: &FunctionBehavior, blocks: &[R2ILBlock]) -> Vec<OreFact> {
    let mut facts = Vec::new();
    let graph = behavior.values();

    for &block_addr in behavior.control().block_addrs() {
        let Some(block_id) = graph.block_id_for_addr(block_addr) else {
            continue;
        };
        let Some(graph_block) = graph.block(block_id) else {
            continue;
        };

        // Phis then ops, by GraphInst::ordinal — already the construction order upstream.
        for &inst_id in &graph_block.insts {
            let Some(inst) = graph.inst(inst_id) else {
                continue;
            };

            match &inst.payload {
                InstPayload::Phi { .. } => {
                    let prov = FactProvenance {
                        inst: Some(inst_id),
                        block: Some(block_id),
                        op_site: None,
                        value: inst.output,
                    };
                    facts.push(OreFact::Op {
                        prov,
                        opcode: OpTag::Phi,
                        ordinal: inst.ordinal,
                        input_arity: inst.inputs.len(),
                        has_output: inst.output.is_some(),
                    });
                }
                InstPayload::Op(ssa_op) => {
                    let opcode = OpTag::from_op(ssa_op);
                    let op_site = behavior.op_site(inst_id);
                    let base_prov = FactProvenance {
                        inst: Some(inst_id),
                        block: Some(block_id),
                        op_site,
                        value: inst.output,
                    };
                    facts.push(OreFact::Op {
                        prov: base_prov,
                        opcode,
                        ordinal: inst.ordinal,
                        input_arity: inst.inputs.len(),
                        has_output: inst.output.is_some(),
                    });

                    // `CallDefine` is synthetic — no source varnode exists at all, so no join
                    // is even attempted (it becomes the furnace's `NoFacetCoordinate` residual).
                    if matches!(ssa_op, SSAOp::CallDefine { .. }) {
                        continue;
                    }

                    let Some((site_addr, site_idx)) = op_site else {
                        continue;
                    };
                    let Some(source_op) = find_source_op(blocks, site_addr, site_idx) else {
                        // No r2il op survives at this site (an index shift landed past the end
                        // of the source block). No comparison is possible, so — unlike a real
                        // tag mismatch — neither an Operand row nor a JoinFailure is fabricated;
                        // the Op row already emitted stands for this instruction.
                        continue;
                    };

                    let found = OpTag::from_r2il(source_op);
                    if found != opcode {
                        facts.push(OreFact::JoinFailure {
                            prov: base_prov,
                            expected: opcode,
                            found,
                        });
                        continue;
                    }

                    for (index, varnode) in source_op.inputs().into_iter().enumerate() {
                        let value = inst.inputs.get(index).copied();
                        facts.push(OreFact::Operand {
                            prov: FactProvenance { value, ..base_prov },
                            position: OperandPos::Input(index),
                            value,
                            space: varnode.space,
                            offset: varnode.offset,
                            size: varnode.size,
                        });
                    }
                    if let Some(varnode) = source_op.output() {
                        let value = inst.output;
                        facts.push(OreFact::Operand {
                            prov: FactProvenance { value, ..base_prov },
                            position: OperandPos::Output,
                            value,
                            space: varnode.space,
                            offset: varnode.offset,
                            size: varnode.size,
                        });
                    }
                }
            }
        }

        // Edge rows, in `GraphBlock::successors` order.
        let terminator = behavior
            .control()
            .cfg()
            .get_block(block_addr)
            .map(|basic_block| &basic_block.terminator);
        for &succ_id in &graph_block.successors {
            let Some(succ_block) = graph.block(succ_id) else {
                continue;
            };
            let kind = classify_edge(terminator, succ_block.addr, block_id, succ_id);
            facts.push(OreFact::Edge {
                from: block_id,
                to: succ_id,
                kind,
            });
        }

        // PhiInput rows: every phi in this block, in phi-then-source order.
        for &inst_id in &graph_block.insts {
            let Some(inst) = graph.inst(inst_id) else {
                continue;
            };
            if let InstPayload::Phi { predecessors } = &inst.payload {
                let base_prov = FactProvenance {
                    inst: Some(inst_id),
                    block: Some(block_id),
                    op_site: None,
                    value: inst.output,
                };
                for (index, (&pred, &value)) in
                    predecessors.iter().zip(inst.inputs.iter()).enumerate()
                {
                    facts.push(OreFact::PhiInput {
                        prov: FactProvenance {
                            value: Some(value),
                            ..base_prov
                        },
                        index,
                        pred,
                        value,
                    });
                }
            }
        }
    }

    // Function-wide facts — every container is a BTreeMap, so iteration order is already the
    // required ascending-id order; nothing is re-sorted here.
    for (&inst_id, uses) in &behavior.memory().uses_by_inst {
        let block = graph.inst(inst_id).map(|inst| inst.block);
        let op_site = behavior.op_site(inst_id);
        for use_fact in uses {
            facts.push(OreFact::MemoryUse {
                prov: FactProvenance {
                    inst: Some(inst_id),
                    block,
                    op_site,
                    value: None,
                },
                object: use_fact.location.object,
                version: use_fact.version.version,
                size: use_fact.location.size,
            });
        }
    }
    for (&inst_id, defs) in &behavior.memory().defs_by_inst {
        let block = graph.inst(inst_id).map(|inst| inst.block);
        let op_site = behavior.op_site(inst_id);
        for def_fact in defs {
            facts.push(OreFact::MemoryDef {
                prov: FactProvenance {
                    inst: Some(inst_id),
                    block,
                    op_site,
                    value: None,
                },
                object: def_fact.location.object,
                previous: def_fact.previous_version.version,
                next: def_fact.next_version.version,
                size: def_fact.location.size,
            });
        }
    }

    for (&predicate_id, predicate_fact) in &behavior.predicates().predicates {
        let block = graph.block_id_for_addr(predicate_fact.block_addr);
        facts.push(OreFact::Predicate {
            prov: FactProvenance {
                inst: None,
                block,
                op_site: None,
                value: Some(predicate_fact.condition),
            },
            id: predicate_id,
            condition: predicate_fact.condition,
            comparison: predicate_fact
                .comparison
                .as_ref()
                .map(|provenance| compare_tag(provenance.kind)),
            true_target: predicate_fact.true_target,
            false_target: predicate_fact.false_target,
        });
    }

    for (&call_site_id, call_site_fact) in &behavior.calls().by_id {
        let block = graph.inst(call_site_fact.at).map(|inst| inst.block);
        let op_site = behavior.op_site(call_site_fact.at);
        facts.push(OreFact::CallSite {
            prov: FactProvenance {
                inst: Some(call_site_fact.at),
                block,
                op_site,
                value: Some(call_site_fact.target),
            },
            id: call_site_id,
            target: call_site_fact.target,
            direct_target: call_site_fact.direct_target,
        });
    }

    facts
}

/// Look up the r2il op at a `(block_addr, op_idx)` site in the SOURCE block list — the typed
/// side of the op-site join. `None` when either the block or the index does not exist in
/// `blocks` (an index-shift landing past the end of a source block, or a site whose block was
/// not part of the ingested list).
fn find_source_op(blocks: &[R2ILBlock], addr: u64, idx: usize) -> Option<&R2ILOp> {
    blocks
        .iter()
        .find(|block| block.addr == addr)
        .and_then(|block| block.ops.get(idx))
}

/// The native instruction address an ore fact came from, when the lifter recorded one.
///
/// SSA does not carry it — `SSAOp` has no address field at all — so this is the
/// `(block_addr, op_idx)` sidecar rejoin against `R2ILBlock::op_metadata`, the SAME key
/// `SsaGraph::op_inst_by_site` uses (and the same key [`FactProvenance::op_site`] already
/// carries). `None` when `prov.op_site` is `None` (a phi, a `CallDefine`, or a function-wide
/// fact with no single op site), when the site's block is not in `blocks`, or when the lifter
/// recorded no per-op metadata at that index (e.g. a single-instruction lift, where the block
/// address itself already IS the instruction address).
pub fn instruction_addr(prov: &FactProvenance, blocks: &[R2ILBlock]) -> Option<u64> {
    let (addr, idx) = prov.op_site?;
    blocks
        .iter()
        .find(|block| block.addr == addr)?
        .op_metadata
        .get(&idx)?
        .instruction_addr
}

/// Classify one CFG edge. `JUDGMENT` (undocumented upstream, no dedicated edge-kind type
/// exists): a conditional edge is classified by its role in the terminator FIRST (the more
/// specific, locally available fact); only a non-conditional successor is then checked for
/// closing a cycle, via the reverse-postorder `BlockId` ordering `SsaGraph::from_function`
/// assigns (`block_addrs()` is reverse postorder, and `BlockId`s are handed out in that same
/// order, so a successor whose id does not come strictly after the source's closes a cycle).
fn classify_edge(
    terminator: Option<&BlockTerminator>,
    succ_addr: u64,
    from: BlockId,
    to: BlockId,
) -> EdgeTag {
    if let Some(BlockTerminator::ConditionalBranch {
        true_target,
        false_target,
    }) = terminator
    {
        if succ_addr == *true_target {
            return EdgeTag::True;
        }
        if succ_addr == *false_target {
            return EdgeTag::False;
        }
    }
    if to.0 <= from.0 {
        return EdgeTag::Back;
    }
    EdgeTag::Normal
}

// ============================================================================================
// tests
// ============================================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use r2il::Varnode;

    fn reg(offset: u64, size: u32) -> Varnode {
        Varnode::register(offset, size)
    }

    fn con(value: u64, size: u32) -> Varnode {
        Varnode::constant(value, size)
    }

    /// A minimal 2-block, no-merge, no-phi, no-`CallDefine` fixture: `0x1000` computes and
    /// unconditionally branches to `0x1004`, which returns. Every SSA op-site join on this
    /// fixture is clean (source op index == SSA `.ops` index throughout).
    fn linear_fixture() -> Vec<R2ILBlock> {
        let mut b0 = R2ILBlock::new(0x1000, 4);
        b0.push(R2ILOp::IntAdd {
            dst: reg(0x00, 8),
            a: reg(0x00, 8),
            b: con(1, 8),
        });
        b0.push(R2ILOp::Branch {
            target: con(0x1004, 8),
        });

        let mut b1 = R2ILBlock::new(0x1004, 4);
        b1.push(R2ILOp::Return {
            target: reg(0x00, 8),
        });

        vec![b0, b1]
    }

    /// A single-block fixture carrying a `Custom(7)`-space operand as an op's own output
    /// varnode, plus a masked 64-bit register offset as its input — the two anti-vacuity facts
    /// §6 test 2 asks for (a `Custom` space operand is unreachable from the SSA side without
    /// parsing a display string).
    fn custom_space_fixture() -> Vec<R2ILBlock> {
        let masked = 0x1234_5678_9ABC_DEF0_u64 & 0xFFFF;
        let mut block = R2ILBlock::new(0x3000, 4);
        block.push(R2ILOp::Copy {
            dst: Varnode::new(SpaceId::Custom(7), 0x40, 8),
            src: reg(masked, 8),
        });
        block.push(R2ILOp::Return {
            target: reg(masked, 8),
        });
        vec![block]
    }

    /// Own 2-block fixture (mirrors `tests/lossless_fixtures.rs` §10 test 8's own fixture): a
    /// single-predecessor block whose r2il source is `Multiequal` then `Return`. The phi
    /// placeholder that `Multiequal` renames into is extracted out of `SSABlock::ops` into
    /// `SSABlock::phis` (`SSAFunction::from_blocks_raw`'s partition), so the surviving
    /// `.ops[0]` is `Return` while the SOURCE r2il `.ops[0]` is `Multiequal` — a genuine
    /// op-site index shift, not a contrived one.
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

    #[test]
    fn enumeration_is_deterministic() {
        let blocks_a = linear_fixture();
        let blocks_b = linear_fixture();

        let behavior_a =
            FunctionBehavior::from_blocks_raw(&blocks_a, None).expect("linear fixture ingests");
        let behavior_b =
            FunctionBehavior::from_blocks_raw(&blocks_b, None).expect("linear fixture ingests");

        let facts_a = enumerate(&behavior_a, &blocks_a);
        let facts_b = enumerate(&behavior_b, &blocks_b);
        assert_eq!(
            facts_a, facts_b,
            "enumerate over two freshly-ingested, value-identical block lists must agree \
             element by element"
        );

        // Re-running over the SAME behavior must also be stable (no interior mutability, no
        // hidden HashMap iteration leaking non-determinism between calls).
        let facts_a_again = enumerate(&behavior_a, &blocks_a);
        assert_eq!(facts_a, facts_a_again);
        assert!(!facts_a.is_empty());
    }

    #[test]
    fn operand_coordinates_come_from_the_typed_source_not_from_names() {
        let blocks = custom_space_fixture();
        let behavior =
            FunctionBehavior::from_blocks_raw(&blocks, None).expect("custom-space fixture ingests");

        let facts = enumerate(&behavior, &blocks);

        let masked = 0x1234_5678_9ABC_DEF0_u64 & 0xFFFF;
        let register_operand_seen = facts.iter().any(|fact| {
            matches!(
                fact,
                OreFact::Operand {
                    space: SpaceId::Register,
                    offset,
                    size: 8,
                    ..
                } if *offset == masked
            )
        });
        assert!(
            register_operand_seen,
            "the masked register offset must survive into an Operand row exactly, not via a \
             re-derived SSAVar name"
        );

        // Anti-vacuity (§6 test 2): a Custom(7) space operand is unreachable from the SSA side
        // without parsing a display string — this can only come from the typed r2il source.
        let custom_operand_seen = facts.iter().any(|fact| {
            matches!(
                fact,
                OreFact::Operand {
                    space: SpaceId::Custom(7),
                    offset: 0x40,
                    size: 8,
                    ..
                }
            )
        });
        assert!(
            custom_operand_seen,
            "expected an Operand row carrying SpaceId::Custom(7)"
        );
    }

    #[test]
    fn an_op_site_join_mismatch_is_enumerated_not_skipped() {
        let blocks = multiequal_fixture();
        let behavior =
            FunctionBehavior::from_blocks_raw(&blocks, None).expect("multiequal fixture ingests");

        let facts = enumerate(&behavior, &blocks);

        let join_failures: Vec<_> = facts
            .iter()
            .filter_map(|fact| match fact {
                OreFact::JoinFailure {
                    expected, found, ..
                } => Some((*expected, *found)),
                _ => None,
            })
            .collect();
        assert_eq!(
            join_failures,
            vec![(OpTag::Return, OpTag::Phi)],
            "the shifted site must be reported as expected=Return (what SSA's `.ops[0]` is, \
             the Return the Multiequal-derived phi was extracted around) found=Phi (what the \
             source r2il `.ops[0]`, the Multiequal, actually is)"
        );

        // Independently-derived expected total (see this module's doc comment for the
        // enumeration order this counts against):
        //   B0 (0x2000): Op(Branch) + Operand(Input(0), target)                    = 2
        //     Edge B0 -> B1 (Normal)                                               = 1
        //   B1 (0x2004): Op(Phi) [no operand rows: phi payload, no join target]    = 1
        //                Op(Return) + JoinFailure (no Operand rows: mismatch)      = 2
        //     (Return has no successors -> 0 Edge rows)
        //     PhiInput (1 phi, fan-in truncated to the single CFG predecessor)     = 1
        //   memory / predicates / call sites: none in this fixture                = 0
        // total                                                                    = 7
        assert_eq!(
            facts.len(),
            7,
            "independently-derived expectation: 2 (B0 op+operand) + 1 (edge) + 3 (B1 phi-op, \
             return-op, join-failure) + 1 (phi input) = 7; got {facts:#?}"
        );
    }

    #[test]
    fn instruction_addr_rejoins_op_metadata_by_op_site_and_is_none_without_it() {
        let mut blocks = linear_fixture();
        // op index 1 of block 0x1000 is the `Branch` — attach a real instruction address to it.
        blocks[0].set_op_metadata(
            1,
            r2il::OpMetadata {
                instruction_addr: Some(0xDEAD_BEEF),
                ..Default::default()
            },
        );
        let behavior =
            FunctionBehavior::from_blocks_raw(&blocks, None).expect("linear fixture ingests");
        let facts = enumerate(&behavior, &blocks);

        let branch_prov = facts
            .iter()
            .find_map(|fact| match fact {
                OreFact::Op {
                    prov,
                    opcode: OpTag::Branch,
                    ..
                } => Some(*prov),
                _ => None,
            })
            .expect("the fixture emits exactly one Branch Op row");
        assert_eq!(
            instruction_addr(&branch_prov, &blocks),
            Some(0xDEAD_BEEF),
            "op index 1 carries metadata and must rejoin to its instruction address"
        );

        // Two-sided: op index 0 (the IntAdd) has NO metadata attached — a stub that always
        // returns the same address, or that ignores op_idx, fails this half.
        let int_add_prov = facts
            .iter()
            .find_map(|fact| match fact {
                OreFact::Op {
                    prov,
                    opcode: OpTag::IntAdd,
                    ..
                } => Some(*prov),
                _ => None,
            })
            .expect("the fixture emits exactly one IntAdd Op row");
        assert_eq!(
            instruction_addr(&int_add_prov, &blocks),
            None,
            "op index 0 carries no metadata and must rejoin to None, not the sibling's address"
        );
    }
}
