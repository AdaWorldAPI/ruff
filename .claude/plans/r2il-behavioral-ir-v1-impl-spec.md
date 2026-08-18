# R2IL Behavioral IR v1 — PR 1 implementation spec (`crates/ruff_r2il`)

> Implements **PR 1** of `.claude/plans/r2il-behavioral-ir-v1.md` as revised by the
> **PIVOT (2026-08-18)** and the **OPERATOR RULING (2026-08-18): the V3-shaped varnode is the
> DRILL KEY**. R2IL is an **INTAKE ARM**, not a bypass: typed input earns no exemption from
> ore → furnace → slag → proposer. Slag is evidence.
> Non-negotiables: workspace-**EXCLUDED** crate, path deps on
> `../../../r2sleigh/crates/{r2il,r2ssa}`, non-default feature `lift`.
> Every type/field/signature below was read from source this session.
> `JUDGMENT` marks a call made where the plan was silent.

**One line so a future session does not re-chase the name:** `lance-graph-arm-discovery` is
**Association Rule Mining** (Aerial+, arXiv 2504.19354), not "intake arm". It consumes a declared
`FeatureSpec` + discretised `Dataset` and emits `CandidateRule`. It provides **no** intake, ore,
furnace, slag or proposer machinery. Do not reference it as reusable here.

## 1. Framing — why there is a furnace at all

> **Operator, 2026-08-18: "Varnode in the first stage is pointer chasing stacked god objects —
> hence the ore furnace slag."**

The upstream typed truth is **good ore but structurally still stage-1 pointer chasing**:
`SSAFunction`'s private `HashMap<u64, SSABlock>`, a petgraph CFG, `BTreeMap<SSAVar, ValueId>` keyed
by String-carrying vars, facts as nested `BTreeMap`s of structs. **Typed ≠ refined.** Mistaking the
cleanliness of r2il's Rust types for refinement *is* the privileged-direct-path the pivot forbids.

- **(a) The ARM preserves that object graph untouched** — it never flattens at intake.
- **(b) The FURNACE melts it** into flat, facet-addressed, concern-separated rows
    (`FactId + VarnodeFacet + Concern` — plain flat `Vec`s, **never another object graph**).
- **(c) The SLAG is what resisted flattening**, addressed at the facet coordinate where it resisted.

| stage                            | module                      | role                                                             |
| -------------------------------- | --------------------------- | ---------------------------------------------------------------- |
| 1 · intake arm / **ore carrier** | `behavior.rs`               | lossless zero-copy assembly. **Not** an invented "contract"      |
| 2 · **ore**                      | `ore.rs`                    | deterministic typed fact enumeration over the object graph       |
| 3 · **furnace**                  | `furnace.rs`                | melt → **flat** `FlatFact` rows; conservation ledger             |
| 3b · **slag**                    | `slag.rs`                   | addressed residual rows; shape id + reason; **no `Other`, ever** |
| — · drill key + config tree      | `facet.rs`, `convention.rs` | the 16-byte address, and longest-prefix-wins config over it      |
| 4 · DTO / codebook factoring     | `vocab.rs`                  | feeds lance-graph `ogar_codebook` **read-only**                  |
| — · artifact set                 | `examples/harvest_r2il.rs`  | the deliverable, per MedCare-rs / openproject-nexgen-rs          |

Refined concern *contracts* are a later, measured furnace output. This PR does not invent them.

## 2. Verified upstream facts every worker must build against

A worker that contradicts one of these is wrong, not the source.

| Fact                                                                                                                                                                                                                                                                                                                                  | Where                                        |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------- |
| `Varnode { space: SpaceId, offset: u64, size: u32, meta: Option<VarnodeMetadata> }`; `PartialEq`/`Hash` **ignore `meta`**                                                                                                                                                                                                             | `r2il/src/varnode.rs:19,149-163`             |
| `SpaceId::{Ram, Register, Unique, Const, Custom(u32)}` (`Copy, Eq, Hash`, default `Ram`)                                                                                                                                                                                                                                              | `r2il/src/space.rs:10-23`                    |
| `R2ILBlock { addr, size, ops: Vec<R2ILOp>, switch_info, op_metadata: BTreeMap<usize, OpMetadata> }`                                                                                                                                                                                                                                   | `r2il/src/opcode.rs:1190-1203`               |
| `R2ILOp::{inputs() -> Vec<&Varnode>, output() -> Option<&Varnode>, is_control_flow/_memory_read/_memory_write}`                                                                                                                                                                                                                       | `r2il/src/opcode.rs:499-804`                 |
| **`ArchSpec` public fields** `name: String`, `addr_size: u32`, `spaces: Vec<AddressSpace>`, `registers: Vec<RegisterDef>`, `userops: Vec<UserOpDef>`                                                                                                                                                                                  | `r2il/src/serialize.rs:97-140`               |
| `RegisterDef { name: String, offset: u64, size: u32, parent: Option<String> }`; `UserOpDef { index: u32, name: String }`; `AddressSpace { id: SpaceId, name: String, … }`                                                                                                                                                             | `serialize.rs:42-51,77-82`; `space.rs:61-90` |
| `SSAFunction`: `pub name`, `pub entry`; `cfg`/`domtree`/`blocks`/`block_order` **PRIVATE** — use `blocks()`, `block_addrs()`, `get_block()`, `predecessors()`, `successors()`, `cfg()`, `num_blocks()`                                                                                                                                | `function.rs:251-268, 946-1004`              |
| `SSAFunction::from_blocks_raw(&[R2ILBlock], Option<&ArchSpec>) -> Option<Self>` — raw, **no optimization**. `from_blocks_with_arch` = raw **then constructor-time SCCP** (can rewrite ops)                                                                                                                                            | `function.rs:829, 738-753`                   |
| `SsaArtifact::{from_blocks, raw, …}` → accessors `function/graph/facts/objects/memory/predicates/call_sites/mode/with_name`; `new` **private**; `Debug + Clone`; `Deref<Target = SSAFunction>`                                                                                                                                        | `function.rs:78-234`                         |
| `SsaGraph::from_function`; public `entry, block_order, blocks, insts, values, def_of, uses_of, block_by_addr, value_by_var, op_inst_by_site, op_site_by_inst`; `GraphInst{id, block, ordinal, inputs, output, payload}`; `InstPayload::{Phi{predecessors}, Op(SSAOp)}`; `GraphBlock{id, addr, size, predecessors, successors, insts}` | `graph.rs:30-72`                             |
| `PreparedFunctionFacts::collect(&SSAFunction, &SsaGraph)` → `{objects, memory, predicates, call_sites}`                                                                                                                                                                                                                               | `semantic.rs:188-208`                        |
| `MemoryUseFact{location, version}`, `MemoryDefFact{location, previous_version, next_version}`, `MemoryLocation{object: ObjectId, offset: i64, size: u32}`, `MemoryVersion{object, version: u32}`                                                                                                                                      | `semantic.rs:73-111`                         |
| `PredicateFact{id, block_addr, condition: ValueId, comparison: Option<CompareProvenance>, true_target: u64, false_target: u64}`; `CompareKind::{Equal,NotEqual,Less,SignedLess,LessEqual,SignedLessEqual}`                                                                                                                            | `semantic.rs:113-138`                        |
| `CallSiteFact{id, at: InstId, target: ValueId, direct_target: Option<u64>, fallthrough, memory_effect}`; `ObjectKind::{StackSlot,FrameObject,Global{space:String,address},HeapAlloc,EscapedUnknown}`                                                                                                                                  | `semantic.rs:172-186, 29-36`                 |
| `InterprocFunctionInput<'a>{id, name, prepared: &'a SsaArtifact}`; `solve_interproc_summary_set(&[..], Option<&ArchSpec>, Option<Id>, &BTreeMap<..>, InterprocSolveConfig)`                                                                                                                                                           | `interproc.rs:232,451`                       |
| **`SSAVar{name: String, version: u32, size: u32}` — NO `offset`, NO `SpaceId`**                                                                                                                                                                                                                                                       | `var.rs:10-17`                               |
| `SSAOp` has **no `Multiequal`, no `Indirect`**; memory ops carry `space: String`                                                                                                                                                                                                                                                      | `op.rs:16-350`                               |
| Function-level rename stringifies spaces as **`format!("{:?}", space)`** → `"Ram"`, `"Custom(7)"`; `varnode_to_name`: `Register→"rax"\|"reg:{x}"`, `Unique→"tmp:{x}"`, `Const→"const:{x}"`, `Ram→"ram:{x}"`, `Custom(id)→"space{id}:{x}"`                                                                                             | `rename.rs:456+`; `naming.rs:120-135`        |
| `rename_op` is **total, 1 SSAOp per R2ILOp**; `Multiequal → SSAOp::Phi`, `Indirect → SSAOp::Copy`                                                                                                                                                                                                                                     | `rename.rs:433,1087,1101`                    |
| Construction partitions `SSAOp::Phi` out of `ops` into `block.phis`, **zipping sources with `cfg.predecessors(addr)`** → fan-in truncates to the predecessor count                                                                                                                                                                    | `function.rs:888-911`                        |
| `CFG::from_blocks`: **one `R2ILBlock` = one CFG node**; terminator from a **reverse** op scan (last CF op wins); `Fallthrough{next: addr+size}` when none; branch/call targets need `Const`/`Ram` to be typed                                                                                                                         | `cfg.rs:252-274, 96-160`                     |
| `PredicateFact` needs terminator `ConditionalBranch` **and** the block's **last** op `SSAOp::CBranch`                                                                                                                                                                                                                                 | `semantic.rs:670-709`                        |
| memory uses ← `{Load,LoadLinked,LoadGuarded,AtomicCAS,StoreConditional}` + calls; defs ← `{Store,StoreGuarded,StoreConditional,AtomicCAS}` + calls                                                                                                                                                                                    | `semantic.rs:397-471`                        |
| `MemoryOrdering::{Relaxed,Acquire,Release,AcqRel,SeqCst,Unknown}`                                                                                                                                                                                                                                                                     | `r2il/src/memory.rs:10-18`                   |
| `Disassembler::from_sla(&[u8], &str, &str)` (**no `new`**), `lift`, `lift_block`, `set_userop_map`; `MIN_BYTES = 16`; `build_arch_spec(&[u8], &str, &str)`; `userop_map_for_arch(&str)`                                                                                                                                               | `disasm.rs:149,261,314,301`; `sleigh.rs:106` |
| `r2ssa` has a **hard, non-optional** dep on `r2sleigh-lift` (libsla native build)                                                                                                                                                                                                                                                     | `r2ssa/Cargo.toml`                           |

### ⚠ The load-bearing consequence: the facet coordinate is NOT recoverable from SSA

`SSAVar` carries `name/version/size` and **no offset and no `SpaceId`**. The only SSA-side trace of
a varnode's offset is inside the *display name* (`"reg:10"`, `"space7:1000"`) — and parsing display
strings is forbidden as a data path. Therefore:

- **`ore::enumerate` takes BOTH the `FunctionBehavior` AND the source `&[R2ILBlock]`.** Operand
    coordinates come from the **typed** `Varnode`s in the source ops, joined to SSA instructions by
    the `(block_addr, op_idx)` key `SsaGraph::op_inst_by_site` provides — the same key §8 test 6 proves
    round-trips.
- The join is **verified, not assumed**: at each site compare `OpTag::from_op(ssa_op)` against the
    R2IL op's tag; a mismatch (the `Multiequal` index shift, or a `CallDefine` insertion) emits
    `ResidualReason::OpSiteJoinMismatch`, never a silently misattributed coordinate.
- Rows with no source varnode (phi inputs, `CallDefine`) get `at: None` and become the **named**
    residual `ResidualReason::NoFacetCoordinate`. Nothing is dropped.

**Honesty notes that must appear as doc comments in the code, not only here:**

1. `lift` does **not** buy "zero system deps by default": `r2ssa` drags `r2sleigh-lift` → `libsla` in
    unconditionally; `lift` gates only this crate's **direct** disassembler/`sleigh-config` use.
1. `Varnode.meta` and `R2ILBlock.op_metadata` **do not cross into SSA**; they stay addressable by the
    same `(block_addr, op_idx)` key. That rejoin is the contract.
1. `r2il::SwitchInfo` (struct) vs `r2ssa::SwitchInfo` (type alias) — alias on import.
1. **Do NOT copy `#![expect(clippy::print_stderr, …)]` from `ruff_cpp_spo`'s examples.** That lint
    comes from ruff's `[workspace.lints]`, which an **excluded** crate cannot inherit; the unfulfilled
    expectation would itself fail `clippy -D warnings`.

______________________________________________________________________

## 3. File inventory — `crates/ruff_r2il/`

```text
crates/ruff_r2il/
├── Cargo.toml
├── src/lib.rs                        # crate docs + module decls (W1, up front)
├── src/behavior.rs                   # stage 1 — ore carrier
├── src/facet.rs                      # the DRILL KEY: 16-byte address + config-key scheme
├── src/convention.rs                 # longest-prefix-wins config tree over facet space
├── src/ore.rs                        # stage 2 — deterministic typed fact enumeration
├── src/furnace.rs                    # stage 3 — melt to FLAT addressed rows + conservation
├── src/slag.rs                       # stage 3b — addressed residual ledger
├── src/vocab.rs                      # stage 4 — DTO/codebook factoring
├── tests/lossless_fixtures.rs        # §14 fixtures + the stressor-slag proof
├── examples/harvest_r2il.rs          # THE deliverable: the .claude/harvest artifact set
└── examples/r2il_corpus_profile.rs   # §12 corpus profile
```

`Cargo.toml`, full intended contents. Excluded ⇒ **no `{ workspace = true }` inheritance anywhere**:

```toml
[package]
name = "ruff_r2il"
version = "0.1.0"
publish = false
# Excluded from the ruff workspace (root `exclude`), so nothing here can inherit from
# [workspace.package] / [workspace.dependencies] / [workspace.lints]. edition/rust-version are
# pinned by hand to satisfy BOTH sides: ruff (edition 2024, rust-version 1.93, toolchain 1.97.1)
# and r2sleigh (edition 2024).
edition = "2024"
rust-version = "1.93"
description = "R2IL intake arm: a lossless ore carrier over r2sleigh's r2il/r2ssa object graph, a furnace that melts it into flat facet-addressed concern rows, and an addressed residual (slag) ledger. Ore in, named slag out — no catch-all, nothing dropped."

[lib]
name = "ruff_r2il"

[dependencies]
# The AdaWorldAPI fork checkout, consumed by path (P0 fork rule). DEFAULT deps: the typed
# behavioral surface is the crate's reason to exist.
r2il  = { path = "../../../r2sleigh/crates/r2il" }
r2ssa = { path = "../../../r2sleigh/crates/r2ssa" }

# Non-default `lift`, mirroring the ruff_cpp_spo shape. NOTE the honest difference: r2ssa depends
# on r2sleigh-lift unconditionally, so this feature does NOT keep libsla out of the build — it
# gates only this crate's DIRECT use of the disassembler and the .sla/.pspec data.
r2sleigh-lift = { path = "../../../r2sleigh/crates/r2sleigh-lift", optional = true }
sleigh-config = { version = "1.0", optional = true, features = ["x86"] }

[features]
default = []
lift = ["dep:r2sleigh-lift", "dep:sleigh-config"]

[[example]]
name = "harvest_r2il"
required-features = ["lift"]

[[example]]
name = "r2il_corpus_profile"
required-features = ["lift"]

[lints.rust]
unsafe_code = "forbid"
unreachable_pub = "warn"
```

- **No `license` field** (`publish = false`); no licensing statement is encoded here or in a comment.
- **No `[lints.clippy] pedantic`** — `JUDGMENT`: an excluded crate cannot inherit ruff's lint table,
    and hand-copying it creates an unmeasured lint surface for workers forbidden to compile.
- **No `[dev-dependencies]`**, no serde, no gz: fixtures and artifacts use only `r2il` + `r2ssa`.
- `crates/ruff_r2il/Cargo.lock` **is committed** (not gitignored; the excluded crate is its own
    workspace root and an arm's numbers must be reproducible).

______________________________________________________________________

## 4. `src/facet.rs` — the DRILL KEY (promoted; two roles, one shape)

Module doc must open with both roles, explicitly:

> **Role 1 (PR 1, load-bearing): the ADDRESS / CONFIG-KEY scheme.** `VarnodeFacet` is the 16-byte
> V3-shaped identity `classid(space-class) | offset_lo | offset_hi | size`, **prefix-routable by
> construction**. It is the key `convention.rs` drills on and the coordinate `slag.rs` residuals
> are addressed at.
> **Role 2 (PR 2, NOT committed here): V3 SoA persistence.** Promoting the shape as a key commits
> **no storage layout**. Same 16 bytes, two roles, no persistence decision yet.

```rust
/// Provisional container concept. The REAL mint is a canon-high slot in
/// `lance_graph_contract::ogar_codebook` (plan PR 3, the `NETWORK_LAYER = 0x0804` analog).
/// Until then this is LOCAL and provisional — never persist it as an address.
///
/// ⚠ Known tension, recorded rather than hidden: OGAR's consumer rule is "hi u16 = shared
/// concept, lo u16 = APP render prefix — NEVER a shape ordinal", and the space discriminant
/// below IS a shape ordinal in the lo half. PR 3 owns the real carving.
pub const PROVISIONAL_R2IL_VARNODE: u16 = 0x0000;
pub const SPACE_RAM: u16 = 0;   pub const SPACE_REGISTER: u16 = 1;
pub const SPACE_UNIQUE: u16 = 2; pub const SPACE_CONST: u16 = 3;
pub const CUSTOM_ORDINAL_BASE: u16 = 4;
pub const MAX_CUSTOM_ORDINAL: u16 = u16::MAX - CUSTOM_ORDINAL_BASE;   // 65531
```

**Layout — 16 bytes, all little-endian:** `0..4` `classid: u32` =
`((PROVISIONAL_R2IL_VARNODE as u32) << 16) | space_discriminant as u32`; `4..8` offset low 32;
`8..12` offset high 32; `12..16` `size: u32`. `Varnode::meta` is **documented-excluded** (advisory
plane) — matching upstream, where `Varnode`'s `PartialEq`/`Hash` already ignore it.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VarnodeFacet(pub [u8; 16]);
impl VarnodeFacet {
    pub fn space_discriminant(&self) -> u16;
    pub fn offset(&self) -> u64;     // lo | hi << 32
    pub fn size(&self) -> u32;
    /// The three prefix keys this facet resolves against, coarsest first.
    pub fn prefixes(&self) -> [FacetPrefix; 3];
}

/// A config-tree key: a facet PREFIX. Ordered coarse → fine, exactly the three levels the
/// convention drills on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FacetPrefix {
    Space { discriminant: u16 },
    SpaceOffset { discriminant: u16, offset: u64 },
    SpaceOffsetSize { discriminant: u16, offset: u64, size: u32 },
}
impl FacetPrefix { pub fn depth(&self) -> u8; /* 1, 2, 3 */ }

/// Deterministic interning of `SpaceId::Custom(u32)` raw ids → lo-u16 ordinals (sorted order).
pub struct CustomSpaceTable { ids: Vec<u32> }
impl CustomSpaceTable {
    /// `Err(CustomOrdinalExhausted)` when the set exceeds the lo-u16 budget — the
    /// `mint_factored` principle: overflow is EVIDENCE a route needs factoring, never silent
    /// truncation.
    pub fn from_ids<I: IntoIterator<Item = u32>>(ids: I) -> Result<Self, FacetOverflow>;
    /// Bootstrap from upstream data that already exists (§5's read-don't-retype rule):
    /// every `AddressSpace` in `ArchSpec::spaces` whose `id` is `SpaceId::Custom(n)`.
    pub fn from_arch(arch: &ArchSpec) -> Result<Self, FacetOverflow>;
    pub fn ordinal_of(&self, raw: u32) -> Option<u16>;
    pub fn raw_of(&self, ordinal: u16) -> Option<u32>;
    pub fn len(&self) -> usize;
}

pub enum FacetOverflow {
    /// A `Custom(raw)` the table does not know. Projection REFUSES; never `raw as u16`
    /// (65541 and 5 both truncate to 5).
    UnknownCustomSpace { raw: u32 },
    CustomOrdinalExhausted { count: usize },
}

/// Build the drill key from the TYPED r2il varnode. This is the ONLY constructor — a facet is
/// never derived from an SSAVar (which has no offset and no SpaceId; see §2's ⚠ note).
pub fn project(vn: &Varnode, spaces: &CustomSpaceTable) -> Result<VarnodeFacet, FacetOverflow>;
pub fn unproject(f: &VarnodeFacet, spaces: &CustomSpaceTable) -> Result<Varnode, FacetOverflow>;
```

**Config-key-time losslessness (the promoted `Custom(u32)` falsifier).** State in the module docs:
*a `Custom` id that overflows the interned-ordinal budget must fail **typed** at CONFIG-KEY
construction, not only at projection.* A config tree keyed by a truncated address would silently
attach rows to the wrong varnode family — the worst possible failure for a drill scheme. Hence
`CustomSpaceTable::from_ids`/`from_arch` return `Result`, and `convention.rs` (§5) propagates it.

### Tests, each two-sided

1. **`fixed_spaces_round_trip_byte_for_byte`** (+ the four classid words must be **distinct**, so a
    constant-returning impl fails).
1. **`custom_space_within_budget_round_trips`** — table `{3,7,9}`; `Custom(7)`'s lo u16 ==
    `CUSTOM_ORDINAL_BASE + 1` (sorted position).
1. **`custom_space_outside_the_table_errors_and_never_truncates`** — plan item **O3**: `Custom(5)`
    vs an empty table → `Err(UnknownCustomSpace{raw:5})`; then intern **both** `5` and `65541` (the
    pair a `raw as u16` cast would collide) and assert distinct facets, each unprojecting to its own
    raw id.
1. **`too_many_custom_spaces_is_a_typed_overflow_not_a_wrap`** — budget+2 errors; exactly budget
    succeeds. Both sides pinned.
1. **`offsets_above_u32_max_survive_the_lo_hi_split`** — `0x1234_5678_9ABC_DEF0` round-trips;
    anti-vacuity: `u32::from_le_bytes(f.0[4..8]) as u64 != offset`.
1. **`meta_is_excluded_from_the_projection`** — same varnode with/without metadata → **identical 16
    bytes**; doc comment names the upstream contract it mirrors.
1. **`prefixes_are_ordered_coarse_to_fine_and_share_their_ancestors`** — `prefixes()[0].depth()==1 … [2].depth()==3`; two facets in the same space share `prefixes()[0]` but differ at `[1]` when
    their offsets differ. Falsifies a prefix builder that ignores a component.

______________________________________________________________________

## 5. `src/convention.rs` — longest-prefix-wins config tree (NEW)

Precedent: `ruff_spo_triplet::concept_split::ConceptConvention` (caller-supplied, **zero domain
vocabulary in the module**) and OGAR codebook scoping (*"longest-prefix wins — one rule, every
level"*). **This module ships ZERO architecture vocabulary** — no register names, no opcode
semantics, no userop table. Everything arrives as data.

```rust
/// One config row, attached at a facet PREFIX.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConventionRow {
    pub at: FacetPrefix,
    pub name: Option<String>,          // e.g. a register name, a space name
    pub note: Option<String>,          // free provenance text; never branched on
    pub state: ValidationState,
}

/// Mirrors openproject-nexgen-rs's `orm-ar-backprojection.toml`
/// (`validation_states = [unmeasured|confirmed|corrected|retired]`, meta key
/// `measure_dont_claim`). EVERY row starts `Unmeasured`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationState { Unmeasured, Confirmed, Corrected, Retired }

/// The drilling convention: a radix tree over varnode identity space.
///
/// NOT a flat table. Rows attach at prefixes (space-class; space-class+offset;
/// space-class+offset+size) and resolution is LONGEST MATCHING PREFIX. Residual rows are
/// addressed (§7), so the proposer emits proposed rows AT those addresses and pass N+1 drills
/// with them — the config accumulates as a radix tree, self-scaffolding.
#[derive(Debug, Clone, Default)]
pub struct R2ilConvention {
    rows: BTreeMap<FacetPrefix, ConventionRow>,   // BTreeMap ⇒ deterministic emission order
    spaces: CustomSpaceTable,
    userops: BTreeMap<u32, String>,
    /// THE furnace ladder, as DATA. Pass 1 carries exactly seven entries. Widening the pass is
    /// a CONFIG change with a measured before/after ledger — never a code edit to a match arm.
    classified_opcodes: BTreeSet<OpTag>,
    pub arch: Option<String>,          // provenance only; never branched on
}

impl R2ilConvention {
    /// Pass 1: `[Copy, IntAdd, Load, Store, CBranch, Call, Return]`, no rows, no userops, no
    /// custom spaces. Deliberately minimal — the stressors MUST land in slag, and the ledger
    /// NAMING them is the acceptance criterion, not a bigger match.
    pub fn minimal_pass_one() -> Self;

    /// BOOTSTRAP — read, never retype. Populates from data that already exists on `ArchSpec`:
    ///  * `arch.registers: Vec<RegisterDef{name, offset, size, parent}>` → one
    ///    `FacetPrefix::SpaceOffsetSize{ SPACE_REGISTER, offset, size }` row per register,
    ///    `name = Some(reg.name)`, `state = Unmeasured`;
    ///  * one coarse `FacetPrefix::Space{ SPACE_REGISTER }` fall-through row named after the
    ///    register space, so an UNKNOWN register offset still resolves — to the space, not to
    ///    nothing;
    ///  * `arch.userops: Vec<UserOpDef{index, name}>` → the userop table;
    ///  * `arch.spaces` (`AddressSpace{id: SpaceId::Custom(n), name}`) → `CustomSpaceTable`.
    /// Errors only through `FacetOverflow` (config keys must be lossless, §4).
    pub fn from_arch(arch: &ArchSpec, classified: impl IntoIterator<Item = OpTag>)
        -> Result<Self, FacetOverflow>;

    /// Longest-prefix-wins resolution: try `SpaceOffsetSize`, then `SpaceOffset`, then `Space`;
    /// first hit wins. `None` = the convention says nothing at this address (⇒ slag).
    pub fn resolve(&self, facet: &VarnodeFacet) -> Option<&ConventionRow>;
    /// The longest prefix that DID resolve — what an addressed residual reports so the proposer
    /// knows where to attach the next row.
    pub fn resolved_prefix(&self, facet: &VarnodeFacet) -> Option<FacetPrefix>;

    pub fn classifies(&self, op: OpTag) -> bool;
    pub fn userop_name(&self, index: u32) -> Option<&str>;
    pub fn spaces(&self) -> &CustomSpaceTable;
    pub fn insert(&mut self, row: ConventionRow);     // proposer entry point
    pub fn rows(&self) -> impl Iterator<Item = &ConventionRow>;   // BTreeMap order

    /// Nested-TOML rendering, mirroring the harvest precedents. Hand-written (no serde dep):
    /// a `[meta]` table (`measure_dont_claim = true`, `validation_states = [...]`, arch,
    /// classified_opcodes), then one `[[row]]` per row in `BTreeMap` order with
    /// `prefix_depth`, `space`, `offset`, `size`, `name`, `state`. Emission only — nothing in
    /// ruff parses it back.
    pub fn to_toml(&self) -> String;
}
```

### Tests

1. **`arch_registers_bootstrap_the_register_branch`** — build an `ArchSpec` with **N = 3**
    `RegisterDef`s (e.g. `("rax",0,8)`, `("eax",0,4)`, `("rbx",8,8)` — note two share an offset and
    differ only in size, which is exactly why the finest prefix carries size). Assert each of the 3
    register varnodes resolves via `resolve()` to a row whose `name` is its own register name, and
    that every row's `state == Unmeasured`. Anti-vacuity: assert the three resolved names are
    **distinct** (a bootstrap that mapped everything to one row would otherwise pass).
1. **`an_unknown_register_offset_falls_through_to_the_space_prefix`** — resolve
    `Varnode::register(0xDEAD, 8)`: `resolve()` returns the coarse `Space{SPACE_REGISTER}` row and
    `resolved_prefix()` reports `depth() == 1`, **not** `None` and **not** a depth-3 row. Two-sided
    with test 1 (a known register must report `depth() == 3`).
1. **`longest_prefix_wins_over_a_coarser_row`** — insert a `SpaceOffset` row and a
    `SpaceOffsetSize` row at the same offset; the facet with the matching size resolves to the
    finer row, a facet with a different size resolves to the coarser one. Falsifies a first-match
    or coarsest-wins implementation.
1. **`custom_space_overflow_fails_at_config_key_time`** — `from_arch` over an `ArchSpec` whose
    custom spaces exceed the budget returns `Err(CustomOrdinalExhausted)`; a within-budget spec
    succeeds. Pins the §4 promotion.
1. **`toml_rendering_is_byte_stable_and_starts_every_row_unmeasured`** — render twice, compare;
    assert every `state = "unmeasured"` and that `measure_dont_claim` is present.

______________________________________________________________________

## 6. `src/ore.rs` — stage 2, the enumeration (NEW)

Module header carries the operator one-liner verbatim:

> **"Varnode in the first stage is pointer chasing stacked god objects — hence the ore furnace
> slag."** The upstream object graph (a private `HashMap<u64, SSABlock>`, a petgraph CFG,
> `BTreeMap<SSAVar, ValueId>` keyed by String-carrying vars, nested-`BTreeMap` facts) is GOOD ORE
> and structurally still stage-1 pointer chasing. **Typed ≠ refined.** This module *reads* that
> graph and emits typed ore rows; it does **not** flatten (that is `furnace.rs`) and it does not
> classify.

```rust
/// One variant per `SSAOp` variant. Closed, exhaustive, stable `as_str()` — the discipline
/// `ruff_spo_triplet::Predicate` enforces ("frontends MUST NOT emit raw predicate strings").
/// `from_op` is a total match; `format!("{:?}")` is FORBIDDEN.
pub enum OpTag { Phi, Copy, Load, Store, Fence, LoadLinked, StoreConditional, AtomicCAS,
                 LoadGuarded, StoreGuarded, IntAdd, IntSub, /* … one per SSAOp variant … */ }
impl OpTag {
    pub fn from_op(op: &SSAOp) -> Self;
    /// From the R2IL side, for the op-site join verification (§2 ⚠). `Multiequal → Phi`,
    /// `Indirect → Copy`, matching `rename_op`'s documented mapping.
    pub fn from_r2il(op: &R2ILOp) -> Self;
    pub fn as_str(self) -> &'static str;
}

pub enum EdgeTag { Normal, True, False, Back }
pub enum CompareTag { Equal, NotEqual, Less, SignedLess, LessEqual, SignedLessEqual }
pub enum OperandPos { Input(usize), Output }

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FactProvenance {
    pub inst: Option<InstId>, pub block: Option<BlockId>,
    pub op_site: Option<(u64, usize)>, pub value: Option<ValueId>,
}

pub enum OreFact {
    Op        { prov, opcode: OpTag, ordinal: usize, input_arity: usize, has_output: bool },
    /// The TYPED coordinate components, taken from the source `Varnode` via the verified
    /// op-site join — never from an `SSAVar` name.
    Operand   { prov, position: OperandPos, value: Option<ValueId>,
                space: SpaceId, offset: u64, size: u32 },
    Edge      { from: BlockId, to: BlockId, kind: EdgeTag },
    PhiInput  { prov, index: usize, pred: BlockId, value: ValueId },
    MemoryUse { prov, object: ObjectId, version: u32, size: u32 },
    MemoryDef { prov, object: ObjectId, previous: u32, next: u32, size: u32 },
    Predicate { prov, id: PredicateId, condition: ValueId, comparison: Option<CompareTag>,
                true_target: u64, false_target: u64 },
    CallSite  { prov, id: CallSiteId, target: ValueId, direct_target: Option<u64> },
    /// The join failed at this site — emitted so the failure is enumerated, not skipped.
    JoinFailure { prov, expected: OpTag, found: OpTag },
}

/// Enumerate the ore. Deterministic, total, lossless w.r.t. the SSA surface.
/// `blocks` is the SOURCE R2IL, required for the typed operand coordinates (§2 ⚠).
pub fn enumerate(behavior: &FunctionBehavior, blocks: &[R2ILBlock]) -> Vec<OreFact>;
```

**Enumeration order is fixed and documented** (this IS the determinism guarantee): blocks in
`SSAFunction::block_addrs()` order (reverse postorder, a `Vec` — *not* the `HashMap`); within a
block, phis then ops by `GraphInst::ordinal`; per op the `Op` row, then `Operand` rows in `inputs`
order then `Output`; then `Edge` rows from `GraphBlock::successors`; then `PhiInput`; then memory
uses/defs by `InstId`; then predicates by `PredicateId`; then call sites by `CallSiteId`. Every
container touched is ordered — **no `HashMap` iteration anywhere**.

### Tests

1. **`enumeration_is_deterministic`** — `enumerate` twice over the same behavior and over a
    freshly re-ingested identical block list; both sequences compare equal element by element.
    Falsified the moment `HashMap` order leaks in.
1. **`operand_coordinates_come_from_the_typed_source_not_from_names`** — a fixture with
    `Varnode::register(0x1234_5678_9ABC_DEF0 & 0xFFFF, 8)` and a `Custom(7)` operand: the emitted
    `Operand` rows carry the exact `SpaceId` and `offset`, and **no** `OreFact` construction path
    reads an `SSAVar::name`. Anti-vacuity: assert at least one operand has `space == SpaceId::Custom(7)` (unreachable from the SSA side without parsing).
1. **`an_op_site_join_mismatch_is_enumerated_not_skipped`** — the `Multiequal` fixture (§8 test 8)
    shifts source indices; assert at least one `OreFact::JoinFailure` is emitted with both tags
    populated, and that the total ore row count still matches the independent expectation.

______________________________________________________________________

## 7. `src/furnace.rs` — stage 3, the melt (NEW)

Module doc, mandatory and explicit:

> The furnace melts the stage-1 object graph into **flat, facet-addressed, concern-separated
> rows**. `FlatFact` is `FactId + VarnodeFacet + Concern + fixed scalar payload` — plain flat
> `Vec`s. **It must never grow back into a nested object graph.** No `Vec`, no `Box`, no map, no
> `String` inside a `FlatFact`; cardinality is handled by EMITTING MORE ROWS (a `CallOther` with
> four inputs is one `Op` row plus four `Operand` rows), never by nesting a collection. A
> `FlatFact` refers to another only by `FactId`.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactId(pub u32);

/// The route the row belongs to — r2sleigh's own decomposition, named.
pub enum Concern { Control, Values, Objects, Memory, Predicates, Calls }
pub enum FactKind { Op, OperandIn, OperandOut, Edge, PhiInput, MemUse, MemDef,
                    Predicate, CallSite }

/// ONE flat row. `const _: () = assert!(size_of::<FlatFact>() <= 64);` — the compile-time guard
/// against the nested-object-graph regression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlatFact {
    pub id: FactId,
    /// The drill key. Operand rows carry their own varnode's facet; rows with no varnode of
    /// their own are BLOCK-ANCHORED — `project(&Varnode::ram(block_addr, 0))` — which is a
    /// documented convention, never an implicit default.
    pub at: VarnodeFacet,
    pub concern: Concern,
    pub kind: FactKind,
    pub opcode: OpTag,
    /// Two fixed typed payload slots (e.g. version/previous/next, index/arity, target addr).
    /// Their meaning per `kind` is documented in a table in the module docs.
    pub a: u64,
    pub b: u64,
    pub prov: FactProvenance,
}

/// Melt one function. `Ok` rows are flat and addressed; everything else is addressed slag.
pub fn smelt(behavior: &FunctionBehavior, blocks: &[R2ILBlock], conv: &R2ilConvention)
    -> (Vec<FlatFact>, ResidualLedger, HarvestReport);

/// Conservation ledger — the `harvested N / classified X / residual Y / dropped 0` line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HarvestReport { pub harvested: usize, pub classified: usize,
                           pub residual: usize, pub dropped: usize }
impl HarvestReport {
    /// `harvested == classified + residual + dropped` AND `dropped == 0`.
    pub fn is_conserved(&self) -> bool;
}
/// Per-fact-kind and per-`OpTag` counts — the census (the "108,548 triples by predicate"
/// analog). `BTreeMap`, so the census artifact is byte-stable across runs.
pub struct Census { pub by_fact_kind: BTreeMap<&'static str, usize>,
                    pub by_opcode: BTreeMap<&'static str, usize> }
pub fn census(rows: &[FlatFact]) -> Census;
```

**Pass-1 ladder (the whole thing — nothing else classifies).** An `OreFact::Op` melts iff
`conv.classifies(opcode)`. An `Operand` melts iff its parent op melted **and** `facet::project`
succeeds **and** `conv.resolve(&facet)` is `Some`. `Edge`/`PhiInput`/`MemoryUse`/`MemoryDef`/
`Predicate`/`CallSite` melt iff their parent op (by `prov.inst`) melted and their own condition
holds (`CallSite` needs `direct_target` `Some`; `PhiInput` needs `index < predecessors`;
`MemoryUse`/`Def` need a non-`EscapedUnknown` object). `JoinFailure` never melts. Everything else
becomes a **named, addressed** residual. There is no third outcome, so `dropped == 0` holds by
construction of the `Result` signature.

### Tests

1. **`a_flat_fact_stays_flat`** — `assert_eq!(size_of::<FlatFact>(), <the pinned constant>)` and
    `assert!(FlatFact: Copy)`. Adding a `Vec`/`String`/`Box` breaks both. This is the
    never-a-nested-graph guard, mechanised.
1. **`cardinality_is_rows_not_nesting`** — a `CallOther` with four inputs produces **1** `Op` row
    and **4** `OperandIn` rows sharing its `FactId` via `prov.inst`; assert the counts exactly.
1. **`melt_is_conserved_and_drops_nothing`** — over the §8 fixture: `harvested == classified + residual`, `dropped == 0`, and `harvested` equals an independently recomputed expectation.
    Anti-vacuity: `harvested >= 50`.
1. **`the_convention_is_the_knob_not_the_code`** — with `minimal_pass_one()` every melted `Op`'s
    opcode is one of the seven and `classified > 0`; then add exactly `OpTag::AtomicCAS` to the
    convention and assert `classified` rises by **exactly** the AtomicCAS fact count and `residual`
    falls by the same. Proves widening happens in data.
1. **`block_anchored_rows_are_addressed_not_defaulted`** — every emitted `FlatFact` has an `at`
    whose `space_discriminant()` and `offset()` are meaningful; no row carries the all-zero facet
    unless its varnode genuinely is `Ram:0/size 0`. Falsifies a lazy `VarnodeFacet::default()`.

______________________________________________________________________

## 8. `src/slag.rs` — stage 3b, the ADDRESSED residual ledger (NEW)

```rust
/// FNV-1a 64 over the residual's SHAPE — the reason discriminant and its typed payload only,
/// NEVER the provenance and NEVER the address. Identical shapes at different sites therefore
/// group, exactly as MedCare-rs's `fnv1a:` class fingerprints do.
pub struct ShapeId(pub u64);

/// Why the CURRENT convention could not melt a fact.
///
/// The slag doctrine (`concept_split.rs` § "The slag doctrine"): the residual is not waste, it
/// is the empirical boundary of the current convention; a recurring reason NAMES the next
/// convention row to add.
///
/// HARD RULE — there is NO catch-all. No `Other`, no `Opaque`, no `Unknown`, no `_ =>` arm that
/// manufactures a reason. A shape that fits no variant means ADD A VARIANT (and record the
/// before/after counts), never widen an existing one to swallow it.
pub enum ResidualReason {
    OpcodeNotInConvention { opcode: OpTag },
    NoConventionRowAtAddress,                 // resolve() returned None at every prefix
    UserOpNotInConvention { userop: u32 },
    CustomSpaceNotInConvention { raw: u32 },
    FacetOverflowAtKey { raw: u32 },          // config keys must be lossless (§4)
    VariadicArity { arity: usize },
    PhiFanInExceedsPredecessors { inputs: usize, predecessors: usize },
    MemoryObjectEscaped,
    IndirectTarget,
    NoFacetCoordinate,                        // phi input / CallDefine — no source varnode
    OpSiteJoinMismatch { expected: OpTag, found: OpTag },
}
impl ResidualReason {
    /// Every variant, for the no-catch-all test. Adding a variant without listing it here is
    /// exactly what that test exists to catch.
    pub const ALL: &'static [&'static str] = &[ /* one stable snake_case name per variant */ ];
    pub fn as_str(&self) -> &'static str;
    pub fn shape_id(&self) -> ShapeId;
}

/// An ADDRESSED residual: it records WHERE in varnode identity space the melt failed, so the
/// proposer can emit a proposed `ConventionRow` AT that address and pass N+1 drills with it.
pub struct ResidualFact {
    pub shape_id: ShapeId,
    pub reason: ResidualReason,
    /// The facet coordinate where it occurred. `None` only for `NoFacetCoordinate`.
    pub at: Option<VarnodeFacet>,
    /// The longest convention prefix that DID resolve — where the proposer attaches the next,
    /// finer row. `None` = nothing resolved, so the proposal attaches at `Space`.
    pub at_prefix: Option<FacetPrefix>,
    pub provenance: FactProvenance,
}

pub struct ResidualLedger { rows: Vec<ResidualFact> }
impl ResidualLedger {
    pub fn push(&mut self, fact: ResidualFact);
    pub fn len(&self) -> usize;
    pub fn rows(&self) -> &[ResidualFact];
    /// Grouped and counted, sorted by count DESC then `shape_id` ASC — deterministic artifact
    /// order. Each group reports one example address so the proposal has a coordinate.
    pub fn grouped(&self) -> Vec<(ShapeId, &'static str, usize, Option<VarnodeFacet>)>;
    /// Grouped by (shape, resolved prefix) — the proposer's actual work queue.
    pub fn by_address(&self) -> Vec<(Option<FacetPrefix>, ShapeId, usize)>;
    /// The largest group's share. A ledger where one shape absorbs everything is a catch-all
    /// wearing a reason's clothes; the pre-registered bar (§10) tests this.
    pub fn dominant_share(&self) -> f64;
}
```

**Rule that must be written into the module docs:** `residual` is NOT to be driven to 0 by widening
a match arm. It falls only when the **convention** gains a row, and every such widening lands with
its measured before/after counts in the harvest ledger.

### Tests

1. **`shape_id_groups_identical_shapes_across_addresses`** — two residuals with the same reason
    payload but different `at`/`provenance` share a `shape_id`; a different payload
    (`OpcodeNotInConvention{AtomicCAS}` vs `{CallOther}`) differs. Two-sided.
1. **`residuals_carry_the_address_they_failed_at`** — every pushed residual except
    `NoFacetCoordinate` has `at.is_some()`; `by_address()` groups two same-shape residuals at
    different prefixes into **two** entries, and two at the same prefix into **one** with count 2.
1. **`grouping_is_exact_and_deterministically_ordered`** — counts 3/1/2 group to exactly
    `[3, 2, 1]` in that order, twice in a row; the total equals `len()`.
1. **`there_is_no_catch_all_reason`** — `ResidualReason::ALL` has no duplicates, its length equals
    the variant count exercised by an exhaustive `match` in the test, and no entry matches
    `"other" | "opaque" | "unknown" | "misc"`. Falsified by a future catch-all.

______________________________________________________________________

## 9. `src/behavior.rs` — stage 1, the ORE CARRIER

Doc comments frame it as the **ore carrier**: a truthful, lossless, zero-copy assembly of upstream
values that **never flattens at intake**. It invents no vocabulary and decides nothing.

```rust
pub struct FunctionIdentity { pub entry: u64, pub name: Option<String>, pub arch: Option<String> }
// entry ← SSAFunction::entry; name ← SSAFunction::name; arch ← ArchSpec::name (advisory
// provenance, never an address; None ⇒ registers carry `reg:<hex>` names).

/// The ORE CARRIER for one function. NOT a "behavioral contract" — nothing here is refined,
/// classified or proposed. It holds r2sleigh's own `SSAFunction` / `SsaGraph` /
/// `PreparedFunctionFacts` (as one `SsaArtifact`) and NAMES that decomposition through borrowed
/// accessors. Refined concern contracts are a later, measured furnace output; this type must
/// never grow into one.
pub struct FunctionBehavior { identity: FunctionIdentity, artifact: SsaArtifact,
                              summary: Option<FunctionSemanticSummary> }
```

**`JUDGMENT` — one `SsaArtifact` instead of three sibling fields.** The plan sketches
`{identity, ssa, graph, facts, summary}`. `SsaArtifact` *is* that triple upstream
(`function.rs:79-96`), its constructor `new` is **private**, and `InterprocFunctionInput.prepared`
requires `&SsaArtifact` — a hand-assembled triple could never produce the `summary` the same struct
declares. Holding the artifact keeps all five concerns verbatim as accessors. Nothing is copied.

```rust
/// RAW ingest: `SsaArtifact::raw(blocks, arch)` → `SSAFunction::from_blocks_raw` →
/// `SsaGraph::from_function` → `PreparedFunctionFacts::collect`. `None` exactly when upstream
/// is: empty `blocks`, or `CFG::from_blocks` fails.
pub fn from_blocks_raw(blocks: &[R2ILBlock], arch: Option<&ArchSpec>) -> Option<Self>;
/// GENERIC ingest (`SsaArtifact::from_blocks`) — applies constructor-time SCCP and can REWRITE
/// ops. Never use it for anything claiming losslessness.
pub fn from_blocks(blocks: &[R2ILBlock], arch: Option<&ArchSpec>) -> Option<Self>;
pub fn with_name(self, name: impl Into<String>) -> Self;
pub fn with_summary(self, summary: FunctionSemanticSummary) -> Self;
/// One `InterprocFunctionInput{id, name, prepared: self.artifact()}` →
/// `solve_interproc_summary_set(&[input], arch, Some(id), &BTreeMap::new(), Default::default())`
/// → `set.summaries.remove(&id)`. Single-function scope: `has_unknown_calls` true whenever the
/// function calls anything is CORRECT, not a defect.
pub fn solve_summary(&mut self, id: InterprocFunctionId, arch: Option<&ArchSpec>);
```

`from_blocks_raw` body, spelled out:

```rust
let artifact = SsaArtifact::raw(blocks, arch)?;
let function = artifact.function();
let identity = FunctionIdentity { entry: function.entry, name: function.name.clone(),
                                  arch: arch.map(|spec| spec.name.clone()) };
Some(Self { identity, artifact, summary: None })
```

**Borrowed route accessors** (no clones, no wrappers): `control() -> &SSAFunction` (CONTROL — CFG,
`BlockTerminator`, `CFGEdge`, block order), `values() -> &SsaGraph` (VALUES + DEF/USE +
PROVENANCE), `objects() -> &ObjectModel`, `memory() -> &MemorySSAFacts`,
`predicates() -> &PredicateFacts`, `calls() -> &CallSiteFacts`. Plus `identity()`, `ssa()` (alias
of `control`, kept because the plan names the field `ssa`), `graph()`, `facts()`, `artifact()`,
`summary()`, and provenance helpers `op_site(InstId) -> Option<(u64, usize)>`,
`inst_at(u64, usize) -> Option<InstId>`, `value_var(ValueId) -> Option<&SSAVar>`,
`def_inst(ValueId)`, `use_sites(ValueId) -> &[UseSite]`.

**Tests.** 1. **`from_blocks_raw_names_the_upstream_decomposition`** — `identity().entry == 0x1000`;
`control().num_blocks() == 2` (exact); `values().insts.len()` equals the source op count; values
non-empty. 2. **`empty_block_list_is_none_not_a_panic`** — `&[]` is `None`, a one-block list is
`Some`. 3. **`op_site_round_trips_through_the_graph_provenance_map`** — for every `Op` inst,
`inst_at(op_site(inst)?) == Some(inst)`; anti-vacuity: checked count `>= 3`.

______________________________________________________________________

## 10. `tests/lossless_fixtures.rs`

Hand-built `R2ILBlock`s through `FunctionBehavior::from_blocks_raw(&blocks, None)` — **raw**,
because the generic path runs SCCP and a losslessness claim must not go through an optimizer.
Helpers `fn reg(off: u64, size: u32)`, `fn con(v: u64, size: u32)` over `Varnode::{register, constant}`.

### `fn fixture_function() -> Vec<R2ILBlock>` — 4 blocks, 3-way merge

**B0 @ `0x1000` size 8** → `ConditionalBranch{true: 0x1018, false: 0x1008}`

```text
0 IntAdd  { dst: reg(0x00,8), a: reg(0x00,8), b: con(0x10,8) }
1 IntSub  { dst: reg(0x58,8), a: reg(0x00,8), b: con(4,8) }
2 IntLess { dst: reg(0x40,1), a: reg(0x00,8), b: con(0x100,8) }
3 CBranch { target: con(0x1018,8), cond: reg(0x40,1) }        // MUST be the last op
```

**B1 @ `0x1008` size 8** → `ConditionalBranch{true: 0x1018, false: 0x1010}`

```text
0 Load { dst: reg(0x08,8), space: Ram, addr: reg(0x00,8) }
1 Store { space: Ram, addr: reg(0x00,8), val: reg(0x08,8) }
2 Copy { dst: reg(0x00,8), src: reg(0x08,8) }                 // 2nd def of reg:0
3 IntSLess { dst: reg(0x44,1), a: reg(0x08,8), b: con(0,8) }
4 CBranch { target: con(0x1018,8), cond: reg(0x44,1) }
```

**B2 @ `0x1010` size 8** → `Fallthrough{next: 0x1018}` — **the stressor block**

```text
0  AtomicCAS { dst: reg(0x00,8), space: Ram, addr: reg(0x20,8), expected: con(0,8),
               replacement: con(1,8), ordering: SeqCst }
1  StoreConditional { result: Some(reg(0x28,1)), space: Ram, addr: reg(0x20,8),
                      val: reg(0x00,8), ordering: Release }
2  StoreConditional { result: None, space: Ram, addr: reg(0x20,8), val: reg(0x00,8),
                      ordering: Relaxed }                       // optional output = None
3  LoadGuarded  { dst: reg(0x30,8), space: SpaceId::Custom(7), addr: reg(0x20,8),
                  guard: reg(0x28,1), ordering: Acquire }       // CUSTOM SPACE
4  StoreGuarded { space: SpaceId::Custom(7), addr: reg(0x20,8), val: reg(0x30,8),
                  guard: reg(0x28,1), ordering: AcqRel }
5  CallOther { output: Some(reg(0x38,8)), userop: 42,
               inputs: vec![reg(0x30,8), reg(0x28,1), con(1,4), con(2,4)] }   // 4 inputs
6  CallOther { output: None, userop: 42, inputs: vec![reg(0x30,8)] }
7  Insert { dst: reg(0x48,8), src: reg(0x30,8), value: reg(0x38,8), position: con(3,4) }
8  Load   { dst: reg(0x50,8), space: Ram, addr: Varnode::ram(0x1234_5678_9ABC_DEF0, 8) }
9  Fence  { ordering: MemoryOrdering::Unknown }
10 Copy   { dst: reg(0x00,8), src: reg(0x48,8) }                // 3rd def of reg:0
```

B2 also carries **op metadata** at index 0 — `set_op_metadata(0, OpMetadata{memory_ordering: Some(SeqCst), atomic_kind: Some(AtomicKind::CompareExchange), ..Default::default()})` — and
**varnode metadata** on op 3's `dst` (`ScalarKind::UnsignedInt` + `PointerHint::PointerLike`).

**B3 @ `0x1018` size 8** — merge, 3 predecessors, terminator `Return` (the reverse scan hits
`Return` before `Call`; correct and intended)

```text
0 Call   { target: con(0x2000,8) }
1 Return { target: reg(0x00,8) }        // uses reg:0 → the merged phi is live
```

### Tests

1. **`every_mandated_op_shape_survives_ingest_as_a_typed_ssa_op`** — `OpTag::from_op` over
    `values().insts`, **set equality** with `{Phi, Copy, Load, Store, Fence, StoreConditional, AtomicCAS, LoadGuarded, StoreGuarded, IntAdd, IntSub, IntLess, IntSLess, CBranch, Call, Return, CallOther, Insert}` — a missing op fails AND an unexpected extra fails.
1. **`op_order_within_a_block_is_preserved_in_ordinal_order`** — for B2 (no `Multiequal`, so the
    1:1 rename rule holds): `ordinal` strictly increasing and the `OpTag` sequence equals the R2IL
    source sequence element by element. Anti-vacuity: length `== 11`.
1. **`facts_are_populated_and_counted_against_upstreams_own_classification`** —
    `calls().by_id.len() == 1`, `direct_target == Some(0x2000)`; `predicates().predicates.len() == 2`
    and the comparison-kind set equals `{Less, SignedLess}`; memory counts equal an expectation
    computed **from upstream's own rule** (uses ← `{Load,LoadLinked,LoadGuarded,AtomicCAS, StoreConditional}` + calls; defs ← `{Store,StoreGuarded,StoreConditional,AtomicCAS}` + calls);
    `objects().global_objects` contains `address == 0x1234_5678_9ABC_DEF0`. Anti-vacuity:
    `expected_uses >= 5`. Do **not** assert the `Global.space` string (Debug-formatted upstream) —
    record the observed value in a doc comment as a stage-4 measurement.
1. **`phi_fan_in_equals_the_predecessor_count_at_the_three_way_merge`** —
    `control().predecessors(0x1018).len() == 3` (exact); ≥1 phi there; **every** phi has
    `inputs.len() == 3` and `predecessors.len() == 3`.
1. **`custom_space_and_every_memory_ordering_survive_into_typed_ssa_ops`** — `LoadGuarded` /
    `StoreGuarded` share a non-empty space string; their `ordering` is `Acquire`/`AcqRel` (typed);
    the set of `MemoryOrdering` values across all SSA ops equals
    `{Relaxed, Acquire, Release, AcqRel, SeqCst, Unknown}` — exact set equality.
1. **`op_metadata_rejoins_by_op_site_even_though_ssa_does_not_carry_it`** — `inst_at(0x1010, 0)` is
    `Some(i)`; `op_site(i) == Some((0x1010, 0))`; that inst is the `AtomicCAS`; the source block's
    `op_metadata(0)` equals the constructed value.
1. **`varnode_metadata_is_advisory_and_does_not_change_ingest`** — fixture with and without the
    `with_meta` → identical `(name, version, size)` value sequences and `insts.len()`.
1. **`multiequal_ingest_becomes_a_phi_zipped_to_the_predecessor_count`** — own 2-block fixture:
    B0 `@0x2000 sz 4` = `Branch{target: con(0x2004,8)}`; B1 `@0x2004 sz 4` = `Multiequal{dst: reg(0x00,8), inputs: vec![reg(0x08,8), reg(0x10,8), reg(0x18,8)]}` then `Return{target: reg(0x00,8)}`. Assert phi count `== 1`; `inputs.len() == control().predecessors(0x2004).len()`
    (**state the rule, not the number**); no `Op`-payload inst in that block is phi-shaped.
1. **`sixty_four_bit_offsets_are_not_truncated_on_ingest`** — the global object address is exactly
    `0x1234_5678_9ABC_DEF0`, **and** no `SSAVar::name` equals `"ram:9abcdef0"` (the name a 32-bit
    truncation would produce). Two-sided anti-truncation.
1. **`stressors_land_in_slag_under_pass_one_and_are_named_and_addressed`** — ⭐ **the proof the
    loop works.** `furnace::smelt(&behavior, &blocks, &R2ilConvention::minimal_pass_one())`. Assert
    `dropped == 0`, `is_conserved()`, `residual > 0`; the ledger's reason set **contains**
    `OpcodeNotInConvention{AtomicCAS}`, `{CallOther}`, `{StoreGuarded}`, `{Insert}`,
    `CustomSpaceNotInConvention{raw: 7}` and `VariadicArity{arity: 4}` — each asserted individually
    so no single absorbing group can satisfy the test; and **every** such residual carries
    `at.is_some()` (the addressed-slag rule).
1. **`widening_the_convention_moves_a_stressor_out_of_slag`** — the two-sided partner: add
    `OpTag::AtomicCAS`; its residual rows disappear, `classified` rises by exactly that count and
    `residual` falls by the same. Proves the ledger tracks the convention, not noise.
1. **`a_bootstrapped_convention_resolves_register_operands_that_pass_one_cannot`** — smelt once
    with `minimal_pass_one()` (no rows ⇒ register operands are `NoConventionRowAtAddress`) and once
    with `R2ilConvention::from_arch(&spec, seven)` over an `ArchSpec` naming `reg 0x00/8` and
    `reg 0x08/8`: those operand rows move from residual to classified, and an *unnamed* offset
    (`0x58`) still resolves — to the coarse `Space` row, at `depth() == 1`. Ties §5 to the melt.

______________________________________________________________________

## 11. `examples/harvest_r2il.rs` (feature `lift`) — THE artifact set

Mirrors `ruff_cpp_spo/examples/harvest_network.rs` (env-driven output path, stderr progress, file
emission). Writes into **`/home/user/ruff/.claude/harvest/r2il/`** (create it; it does not exist
yet), overridable via `R2IL_HARVEST_OUT`.

**Format (`JUDGMENT`):** deterministic line-oriented **TSV** with `#schema` and `#version` header
lines for the row files, and hand-written **nested TOML** for the convention — **not** ndjson/serde.
Reasons: no serde dep; the plan forbids `R2IL→JSON→Ruff` and an ndjson ore invites exactly that
confusion; TSV/TOML diff cleanly. State in every header: **these artifacts are evidence, never a
re-ingest path — nothing in ruff parses them back.**

| file                   | content                                                                                                                                  |
| ---------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| `r2il-pass1.ore.tsv`   | one row per `FlatFact`, columns per `#schema`, in `smelt` order, `at` rendered as 32 hex chars                                           |
| `r2il-pass1-census.md` | `by_fact_kind` + `by_opcode` tables (BTreeMap order ⇒ byte-stable)                                                                       |
| `r2il-pass1-slag.tsv`  | `ResidualLedger::grouped()` — `shape_id`, `reason`, `count`, example facet — **plus** a `by_address` section (the proposer's work queue) |
| `r2il-convention.toml` | `R2ilConvention::to_toml()` — the bootstrapped tree, every row `unmeasured`                                                              |
| `PROVENANCE.md`        | see below                                                                                                                                |
| `TRIAGE-RESULT.md`     | the pre-registered bar (stated first) + the measured outcome                                                                             |

`PROVENANCE.md` pins, so the next session does not re-derive: each corpus path with byte length and
an **FNV-1a 64** of its bytes (labelled FNV, *not* called a sha — no hashing dep); `r2sleigh` commit
**`60942f6`**; arch `x86-64`; `sleigh-config` version + feature; the convention used
(`minimal_pass_one` and/or `from_arch`); env caps in force; and the **EXACT** invocation:

```sh
cargo run --manifest-path crates/ruff_r2il/Cargo.toml --features lift --example harvest_r2il
```

### The PRE-REGISTERED bar — stated in `TRIAGE-RESULT.md` BEFORE the first run

MedCare-rs's `TRIAGE-RESULT.md` states "recoverable ≥85% PASS, \<50% KILL" before running. The R2IL
analog cannot be a rate over *all* facts — pass 1 classifies only seven opcodes by design, so a low
overall rate is the intended result, not a failure. Three bars, each falsifiable:

- **B1 — conservation (absolute).** `dropped == 0` and `harvested == classified + residual`. Any
    violation **KILLS** the pass: the enumerator, not the corpus, is wrong. Not a percentage.
- **B2 — coverage of the declared seven.** Of ore facts whose parent opcode is one of
    `{Copy, IntAdd, Load, Store, CBranch, Call, Return}`, **≥99% classify → PASS; \<90% → KILL.**
    Those seven are fully specified here; a shortfall means the classifier is wrong, not that the
    corpus is hard. The 90–99% band is INVESTIGATE (expected causes: operand rows with no convention
    row at their address, `CallSite` rows with no `direct_target` — both legitimate slag under a
    parent that classified).
- **B3 — the slag is named and addressed, not lumped.** `residual > 0`, distinct `shape_id` count
    **≥ 5**, `dominant_share() < 0.60`, and **every** residual except `NoFacetCoordinate` carries
    `at.is_some()`. A ledger where one shape absorbs everything is a catch-all wearing a reason's
    clothes; an unaddressed ledger cannot feed the proposer. `residual == 0` is a **KILL** too — it
    means someone widened the ladder.

Also **pre-register a prediction that is NOT a bar** (so it can be wrong without moving a
goalpost): on an x86-64 corpus `Copy/IntAdd/Load/Store` dominate, so pass 1 is expected to classify
roughly **60–80%** of all `Op` facts. Record the measured figure either way.

Caps echoed in the header: `R2IL_HARVEST_MAX_FUNCS` (default 200),
`R2IL_HARVEST_MAX_SECTION_BYTES` (default 262144).

______________________________________________________________________

## 12. `examples/r2il_corpus_profile.rs` (feature `lift`) — §12 profile

Plain text to stdout. No serde, no file writes, no `unwrap`/`panic` on corpus input.

**Corpus**, each skipped with a printed note if absent: `$R2IL_CORPUS` (colon-separated) or CLI args
→ `concat!(env!("CARGO_MANIFEST_DIR"), "/../../../r2sleigh/tests/e2e/stress_test")` and `…_opt`
(verified ELF64 x86-64, **not stripped** — symtab present) → `/bin/ls`, `/usr/bin/env` (**stripped**
— op-level only). Nothing is copied into ruff.

**`mod elf` — minimal ELF64 LE reader (~80 lines, no deps).** Every read bounds-checked; anything
malformed returns `None` and the binary is skipped with a note. Header: magic `\x7FELF` @0,
`EI_CLASS==2` @4, `EI_DATA==1` @5, `e_machine u16 @18 == 62`, `e_shoff u64 @40`,
`e_shentsize u16 @58`, `e_shnum u16 @60`, `e_shstrndx u16 @62`. Section header (64 B):
`sh_name u32 @0`, `sh_type u32 @4`, `sh_flags u64 @8`, `sh_addr u64 @16`, `sh_offset u64 @24`,
`sh_size u64 @32`, `sh_link u32 @40`, `sh_entsize u64 @56`; names from section `e_shstrndx` at
`sh_offset + sh_name`, NUL-terminated; executable = `sh_flags & 0x4 != 0 && sh_type != 8`.
Symbols: section `sh_type == 2 (SHT_SYMTAB)`, strtab = section `sh_link`; `Elf64_Sym` (24 B)
`st_name u32 @0`, `st_info u8 @4`, `st_value u64 @8`, `st_size u64 @16`; functions =
`st_info & 0xF == 2 (STT_FUNC) && st_size > 0` inside an exec section.

**Setup:** `build_arch_spec(SLA_X86_64, PSPEC_X86_64, "x86-64")`, `Disassembler::from_sla(…)`,
`set_userop_map(userop_map_for_arch("x86-64"))`. libsla needs **≥16 bytes**, so every lift gets a
16-byte zero-padded window.

**Pass 1 — `== MEASURED EXACT (op level) ==`.** Linear sweep: `disasm.lift(&window, addr)`; on `Ok`
advance `block.size.max(1)`, on `Err` count `undecodable_instructions` and advance 1 byte. Metrics
from `R2ILOp`'s own accessors (never `Display`): opcode frequency; ops per native instruction
(mean/min/median/max); input-arity histogram; output-count histogram; memory-op %, control-op %,
atomic %; `CallOther` arity distribution; **% fitting `dst+src0+src1` inline**
(`output_count <= 1 && inputs().len() <= 2`) and **% needing Vec routing** (complement).

**Pass 2 — `== HEURISTIC-DERIVED (function / CFG level) ==`**, symtab binaries only. Per `STT_FUNC`
symbol: sweep `[st_value, st_value+st_size)`; leaders = `{st_value}` ∪ intra-function const branch
targets ∪ `{addr after any control-flow instruction}`; basic blocks = maximal ranges between
leaders; one `disasm.lift_block(&padded, bb_addr, bb_len)` per block →
`FunctionBehavior::from_blocks_raw(&bbs, Some(&spec))`. Metrics: blocks/fn, ops/block, phi fan-in
distribution, values/fn, call sites/fn, predicates/fn, plus `vocab::VocabHarvest::stats()`, a
`facet::project` sweep (facet count, `FacetOverflow` count by variant), and the `furnace::smelt`
conservation line — feeding open items **O1** and **O3**.

**Labelling rule (non-negotiable in the output):** `STT_FUNC` boundaries are **exact**; the leader
set is an approximation (indirect branches / jump tables unresolved), so every CFG-derived row
prints under the heuristic heading and the header states `function_boundaries: symtab (exact) | leaders: intra-function const targets (approximate)`. A stripped binary prints
`function-level stats: skipped (no symtab)` — never a silent omission. Caps
`R2IL_PROFILE_MAX_FUNCS` (200) / `R2IL_PROFILE_MAX_SECTION_BYTES` (262144) are echoed.

______________________________________________________________________

## 13. `src/vocab.rs` — stage 4 (DTO / codebook factoring)

Feeds lance-graph's `ogar_codebook`; we **read** that codebook and never construct a parallel one.
PR 1 produces the local interning table and the measurement; PR 2/3 wire the real mint.

```rust
pub struct VocabId(pub u32);
/// Deterministic, ORDER-INDEPENDENT interning: built from a BTreeSet, ids assigned in sorted
/// order, so the table is a pure function of the name SET. `JUDGMENT`: there is deliberately NO
/// public incremental `intern(&mut self)` in PR 1 — two build paths would make ids depend on
/// which one ran.
pub struct VocabTable { by_name: BTreeMap<String, VocabId>, names: Vec<String> }
// from_names / id_of / name_of / len / is_empty / iter / name_bytes

pub struct VocabHarvest {
    pub ssa_names: VocabTable,        // SSAVar::name over SsaGraph::values
    pub op_spaces: VocabTable,        // SSAOp `space: String` (Debug-formatted upstream)
    pub object_spaces: VocabTable,    // ObjectKind::Global{space}
    pub userops: BTreeMap<u32, usize>,          // numeric already — COUNTED, not interned
    pub custom_spaces_from_strings: BTreeSet<u32>,
}
impl VocabHarvest { pub fn from_behavior(&FunctionBehavior) -> Self; pub fn stats(&self) -> VocabStats; }

/// The TYPED oracle for custom space ids: walks `Varnode::space` and each op's `space: SpaceId`
/// on the R2IL side. No parsing, no strings.
pub fn custom_space_ids_from_blocks(blocks: &[R2ILBlock]) -> BTreeSet<u32>;
/// Recover a custom space id from an upstream space STRING (`"Custom(7)"` from the function
/// rename, `"space_7"` from `block.rs::space_name`).
/// ⚠ MEASUREMENT ONLY — never a data path. The plan forbids parsing display strings for
/// behavioral truth; this exists solely so `VocabStats` can put a NUMBER on the loss by
/// comparing against `custom_space_ids_from_blocks`.
pub fn parse_custom_space_id(space: &str) -> Option<u32>;
pub fn custom_space_id_from_var_name(name: &str) -> Option<u32>;

pub struct VocabStats { pub unique_ssa_names: usize, pub unique_op_spaces: usize,
    pub unique_object_spaces: usize, pub unique_userops: usize, pub userop_mentions: usize,
    pub unique_custom_spaces_from_strings: usize, pub total_values: usize,
    pub ssa_name_bytes: usize, pub interned_id_bytes: usize }
```

**Tests.** 1. **`vocab_table_is_order_independent`** — the same 5 names in two orders (one with
duplicates) build equal tables; anti-vacuity: `len() == 5` and at least two ids differ.
2\. **`harvest_counts_every_userop_mention_but_interns_names_once`** — two `CallOther`s sharing
`userop: 7` plus one with `9` → `unique_userops == 2`, `userop_mentions == 3` (both exact), reused
name interned once. 3. *(recommended)* **`typed_custom_space_ids_are_the_oracle_for_the_string_set`**
— `custom_space_ids_from_blocks == {7}`; the string-recovered set equals it **on this fixture**,
with a doc comment recording that the equality is a measurement, not a guarantee.

______________________________________________________________________

## 14. Worker split — DISJOINT files, Sonnet fleet

| worker  | owns (exclusively)                            | spec section                       |
| ------- | --------------------------------------------- | ---------------------------------- |
| **W1**  | `Cargo.toml`, `src/lib.rs`, `src/behavior.rs` | §3, §9                             |
| **W2**  | `src/facet.rs`                                | §4                                 |
| **W3**  | `src/convention.rs`                           | §5 (consumes W2's types as spec'd) |
| **W4**  | `src/ore.rs`                                  | §6                                 |
| **W5**  | `src/furnace.rs`                              | §7                                 |
| **W6**  | `src/slag.rs`                                 | §8                                 |
| **W7**  | `src/vocab.rs`                                | §13                                |
| **W8**  | `tests/lossless_fixtures.rs`                  | §10                                |
| **W9**  | `examples/harvest_r2il.rs`                    | §11                                |
| **W10** | `examples/r2il_corpus_profile.rs`             | §12                                |

**W1 writes every module declaration up front** — `src/lib.rs` contains `pub mod behavior; pub mod convention; pub mod facet; pub mod furnace; pub mod ore; pub mod slag; pub mod vocab;` from its
first commit, so no later worker ever edits a shared file. W2–W10 code against the APIs **as
written in this spec**, never against another worker's output; if a signature here is wrong, STOP
and report — do not improvise a different one. Dependency direction: `facet → convention → {ore, furnace}`, `furnace → slag`. All type shapes are spec'd here, so no worker blocks on another.

**Verbatim guardrail block — paste into EVERY worker brief:**

> Do NOT run cargo (build/check/test/clippy/fmt) — the orchestrator compiles centrally in the
> shared target/. Do NOT run git. Do NOT create worktrees. Do NOT write to any .claude/ board file.
> Edit ONLY your assigned files. Do not claim it compiles or that tests pass — you did not run it.

**Orchestrator (Opus), after the fleet lands:**

1. Add the exclusion to `/home/user/ruff/Cargo.toml`:

    ```toml
    [workspace]
    members = ["crates/*"]
    exclude = ["crates/ruff_r2il"]
    resolver = "2"
    ```

    (`exclude` wins over the `crates/*` glob; the root `Cargo.lock` must stay untouched.)

1. Run every gate in §15 centrally, once. Fix cross-file fallout itself — do not re-fan-out.

1. Run the harvest example; commit the artifact set under `.claude/harvest/r2il/`, with
    `TRIAGE-RESULT.md`'s bar section written **before** the run and the measured section after.

1. Commit on `claude/ruff-r2il-lancegraph-3tdt8d` with the board update in the **same** commit:
    plan open items **O1** and **O3** get measured values; the four honesty notes (§2) are recorded.
    Push and open the PR.

______________________________________________________________________

## 15. Gates and definition of done

```sh
cargo fmt    --manifest-path crates/ruff_r2il/Cargo.toml --check
cargo clippy --manifest-path crates/ruff_r2il/Cargo.toml --all-targets -- -D warnings
cargo test   --manifest-path crates/ruff_r2il/Cargo.toml
cargo clippy --manifest-path crates/ruff_r2il/Cargo.toml --features lift --all-targets -- -D warnings
cargo run    --manifest-path crates/ruff_r2il/Cargo.toml --features lift --example harvest_r2il
cargo run    --manifest-path crates/ruff_r2il/Cargo.toml --features lift --example r2il_corpus_profile

# workspace-isolation proof — the exclusion must be real, not asserted
cargo check -p ruff_graph                                          # ruff workspace still builds
cargo metadata --no-deps --format-version 1 | grep -c ruff_r2il    # must print 0
git status --porcelain Cargo.lock                                  # must be EMPTY
```

`cargo check` is not run separately — clippy compiles. The first `--features lift` build pays the
libsla native compile (~43 s measured upstream); if the sandbox is offline add `--offline`.

### Definition of done for PR 1

1. `crates/ruff_r2il/` matches the §3 inventory; root `Cargo.toml` carries `exclude`; root
    `Cargo.lock` unchanged; `crates/ruff_r2il/Cargo.lock` committed.
1. All gates green, including the three isolation checks.
1. Test names match §4–§10 and §13. For every guarded assertion (the stressor-slag proof, the
    convention-widening partner, the register bootstrap + fall-through pair, the flat-row size
    guard, facet overflow, the metadata exclusion, the phi zip, the anti-vacuity counts) the
    orchestrator ran the **disable-the-guard** check and recorded that it went red.
1. `.claude/harvest/r2il/` contains all six artifacts; `TRIAGE-RESULT.md` states the §11 bar
    **before** the measured section; the conservation line `harvested N / classified X / residual Y / dropped 0` is present with `dropped == 0`.
1. **B1 holds absolutely.** B2 and B3 are reported PASS / INVESTIGATE / KILL by their own stated
    thresholds — a KILL is recorded honestly and blocks PR 2, never argued away.
1. The residual ledger contains **no** catch-all row, **every** residual (bar `NoFacetCoordinate`)
    carries its facet address, and `residual` was reduced only by a recorded convention change with
    before/after counts — never by widening a match arm.
1. The doc framing is in the code, not only here: `ore.rs`'s header carries the operator one-liner
    ("Varnode in the first stage is pointer chasing stacked god objects — hence the ore furnace
    slag") plus **typed ≠ refined**; `furnace.rs` states the flat-rows constraint; `behavior.rs`
    frames `FunctionBehavior` as the **ore carrier**, never a contract; `facet.rs` states both roles
    (config key now, V3 persistence not committed); the four §2 honesty notes appear as doc comments.
1. No new type duplicates an upstream one; no `serde_json`; no `Display`/`Debug` parsing on any data
    path (`vocab::parse_custom_space_id` is measurement-only and says so in its doc comment).
