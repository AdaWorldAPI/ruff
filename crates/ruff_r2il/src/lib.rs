//! # `ruff_r2il` — the R2IL intake arm
//!
//! > **Operator, 2026-08-18: "Varnode in the first stage is pointer chasing stacked god objects —
//! > hence the ore furnace slag."**
//!
//! `ruff_r2il` is an **INTAKE ARM**, not a bypass: typed input earns no exemption from
//! ore → furnace → slag → proposer. Slag is evidence.
//!
//! The upstream typed truth (`r2il`/`r2ssa`) is **good ore but structurally still stage-1
//! pointer chasing**: `SSAFunction`'s private `HashMap<u64, SSABlock>`, a petgraph CFG,
//! `BTreeMap<SSAVar, ValueId>` keyed by String-carrying vars, facts as nested `BTreeMap`s of
//! structs. **Typed ≠ refined.** Mistaking the cleanliness of r2il's Rust types for refinement
//! *is* the privileged-direct-path the pivot forbids.
//!
//! - **(a) The ARM preserves that object graph untouched** — it never flattens at intake.
//! - **(b) The FURNACE melts it** into flat, facet-addressed, concern-separated rows
//!   (`FactId + VarnodeFacet + Concern` — plain flat `Vec`s, **never another object graph**).
//! - **(c) The SLAG is what resisted flattening**, addressed at the facet coordinate where it
//!   resisted.
//!
//! | stage | module | role |
//! |---|---|---|
//! | 1 · intake arm / **ore carrier** | [`behavior`] | lossless zero-copy assembly. **Not** an invented "contract" |
//! | 2 · **ore** | [`ore`] | deterministic typed fact enumeration over the object graph |
//! | 3 · **furnace** | [`furnace`] | melt → **flat** `FlatFact` rows; conservation ledger |
//! | 3b · **slag** | [`slag`] | addressed residual rows; shape id + reason; **no `Other`, ever** |
//! | — · drill key + config tree | [`facet`], [`convention`] | the 16-byte address, and longest-prefix-wins config over it |
//! | 4 · DTO / codebook factoring | [`vocab`] | feeds lance-graph `ogar_codebook` **read-only** |
//! | 5 · sink | [`sink`] | where refined truth lands — trait + offline backend; lance-graph SoA is implemented downstream, not here |
//! | 6 · **oracle** | [`oracle`] | round-trip reconstruction: routes → [`oracle::OpSkeleton`] equality per source op site; SPO is NOT the oracle |
//! | — · artifact set | `examples/harvest_r2il.rs` | the deliverable, per MedCare-rs / openproject-nexgen-rs |
//!
//! Refined concern *contracts* are a later, measured furnace output. This crate does not invent
//! them.
//!
//! ## One line so a future session does not re-chase the name
//!
//! `lance-graph-arm-discovery` is **Association Rule Mining** (Aerial+, arXiv 2504.19354), not
//! "intake arm". It consumes a declared `FeatureSpec` + discretised `Dataset` and emits
//! `CandidateRule`. It provides **no** intake, ore, furnace, slag or proposer machinery. Do not
//! reference it as reusable here.
//!
//! ## Honesty notes (crate-scoped — every one of these is load-bearing)
//!
//! 1. **`lift` does *not* buy "zero system deps by default".** `r2ssa` drags `r2sleigh-lift` →
//!    `libsla` in unconditionally; the `lift` feature on *this* crate gates only its own
//!    **direct** disassembler / `sleigh-config` use, not the transitive native build.
//! 2. **`Varnode::meta` and `R2ILBlock::op_metadata` do not cross into SSA.** They stay
//!    addressable by the same `(block_addr, op_idx)` key that [`behavior::FunctionBehavior`]
//!    exposes via its provenance helpers. That rejoin is the contract — never a parsed display
//!    string.
//! 3. **`r2il::SwitchInfo` (a struct) and `r2ssa::SwitchInfo` (a type alias) are different
//!    shapes with the same name.** Alias explicitly on import; never assume they unify.
//! 4. **Do NOT copy `#![expect(clippy::print_stderr, …)]` from `ruff_cpp_spo`'s examples.** That
//!    lint comes from ruff's `[workspace.lints]`, which this **excluded** crate cannot inherit —
//!    the unfulfilled expectation would itself fail `clippy -D warnings`.
//!
//! ## Verified upstream facts
//!
//! Every type and signature this crate builds against was read from the `r2sleigh` source this
//! session (commit `60942f6`), not inferred. See
//! `.claude/plans/r2il-behavioral-ir-v1-impl-spec.md` §2 for the full ledger. A worker or
//! reviewer who finds a signature here contradicting the actual upstream source should treat the
//! source as authoritative and file a correction — never silently improvise a replacement shape.

pub mod absref;
pub mod behavior;
pub mod convention;
pub mod facet;
pub mod furnace;
pub mod mgra;
pub mod oracle;
pub mod ore;
pub mod sink;
pub mod slag;
pub mod vocab;
