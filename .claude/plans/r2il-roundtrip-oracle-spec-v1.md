# SPEC v1 — PR2 slice: the round-trip reconstruction oracle (`oracle.rs`)

> 5+3 council spec (Phase 0). Orchestrator-authored. Savants verify/harden;
> they never design. Panel: default 5 lenses; reviewers: overclaim-auditor /
> dilution-collapse-sentinel / firewall-warden charters.
>
> Scope: the PR2 gate deliverable from `.claude/plans/r2il-behavioral-ir-v1.md`
> — "round-trip reconstruction oracle (R2IL → routes → semantic-equivalent
> R2IL; SPO explicitly NOT the oracle)". This slice follows PR #101 (sink
> trait + OfflineSink + O1) and PR #102 (v2 facts/residuals schemas + readers).

## 1. FROZEN DECISIONS

1. **SPO is NOT the oracle.** The comparison happens on typed r2il values,
   never on a triple projection. (`r2il-behavioral-ir-v1.md:282-285`, and the
   PR2 wave text `:377-380`.)
2. **Success = semantic/behavioral parity on TYPED values** — never textual,
   never binary-artifact equality. (C4, `r2il-behavioral-ir-v1.md:277-285`.)
3. **Conservation is load-bearing.** `harvested == classified + residual`,
   `dropped == 0` (`furnace.rs` `HarvestReport::is_conserved`). The oracle
   extends the same reading: every source op is either reconstructed-equal or
   ledger-accounted; a site that is neither is a FAILURE, not a footnote.
   (C3, `r2il-behavioral-ir-v1.md:270-276`.)
4. **No persistence assumption enters `furnace`/`ore`/`slag`.** (SUBSTRATE
   RULING, "PR-1 consequence" block.)
5. **`FlatFact` stays flat, `Copy`, exact 88-byte pin.** This slice adds NO
   field to it. (`furnace.rs:163-207`, const assert.)
6. **Widening classification is a `R2ilConvention` DATA change, never a new
   `smelt` match arm.** (`furnace.rs` module docs; `convention.rs:86-88`.)
7. **`ResidualReason` has no catch-all** and consumers render unknown reasons
   as raw strings. (`slag.rs:80-119` + guide §4 rule 3.)
8. **`format!("{:?}")` is FORBIDDEN as a data path.** (plan, forbidden list.)
9. **Artifact discipline is additive; readers key off `#schema` names.**
   This slice changes NO TSV schema — v2 (`sink.rs:173-177`) already carries
   facet coords + `a`/`b` + full provenance, which is sufficient for
   artifact-mediated reconstruction. (guide §4.)
10. **Falsifiability rule** (lance-graph CLAUDE.md, adopted by this crate's
    practice): every new check gets a can-fire AND a can-stay-silent test,
    plus manual disable-run verification recorded in the commit message.
11. **No model identifier in any committed artifact.**

## 2. INPUT INVENTORY (verified this session unless marked VERIFY)

- `crates/ruff_r2il/src/behavior.rs:57-70` `from_blocks_raw` (lossless
  ingest; `from_blocks` runs SCCP and must NOT be used by the oracle);
  `:188-218` provenance helpers `op_site`/`inst_at`/`value_var`.
- `crates/ruff_r2il/src/ore.rs` — `OreFact` emission (`enumerate`), operand
  coordinates taken from typed source varnodes via `R2ILOp::inputs()` /
  `output()` (`ore.rs:882-894`); `OpTag` ~85 variants with
  `as_str`/`parse` (PR #102) and `from_r2il`/`from_op`; `FactProvenance
  {inst, block, op_site, value}`.
- `crates/ruff_r2il/src/furnace.rs:112-140` `Concern`/`FactKind`;
  `:167-179` `FlatFact{id, at, concern, kind, opcode, a, b, prov}`; payload
  table (module docs `:62-77`): Op row `a`=ordinal, `b`=arity bits 0..32 |
  has_output bit 32; OperandIn `a`=input index, `b`=ValueId+1; OperandOut
  `a`=0; `smelt` `:262`; `VARIADIC_ARITY_THRESHOLD=3` `:95`; the pass-1
  ladder (module docs `:12-58`): operands melt iff facet-projects AND parent
  melted AND `conv.resolve(&facet).is_some()`.
- `crates/ruff_r2il/src/slag.rs:80-119` `ResidualReason` (11 variants,
  typed payloads); `ResidualFact{shape_id, reason, at, at_prefix,
  provenance}`; `ResidualLedger`.
- `crates/ruff_r2il/src/facet.rs:49` `VarnodeFacet([u8;16])`;
  `:222-244` `project` — discriminant map: Ram/Register/Unique/Const fixed
  + `Custom(raw)` interned via `CustomSpaceTable` from
  `CUSTOM_ORDINAL_BASE`; `FacetPrefix` `:94`. NO inverse exists today
  (VERIFY: grep). `sink.rs` `facet_from_raw` (PR #102) rebuilds facet BYTES,
  not a `Varnode`.
- `crates/ruff_r2il/src/convention.rs:80-110` `R2ilConvention` (radix rows +
  `classified_opcodes` as data); `minimal_pass_one` = exactly
  `[Copy, IntAdd, Load, Store, CBranch, Call, Return]`, ZERO rows — so under
  it `conv.resolve` is `None` everywhere and NO operand row melts
  (VERIFY consequence in smelt); `classifies` `:231`; `resolve` /
  `resolved_prefix` `:215-229`.
- `crates/ruff_r2il/src/sink.rs:173-177` FACTS v2 / RESIDUALS v2 schemas;
  `read_facts` / `read_residuals` (PR #102, round-trip tested).
- Upstream `r2sleigh/crates/r2il/src/opcode.rs:26` `R2ILOp` (~85 variants,
  `PartialEq`); `:534` `output()`, `:691` `inputs()` — varnode-only
  projections. **Non-varnode semantic state NOT covered by
  inputs()/output()**: `Load`/`Store` `space: SpaceId`; `MemoryOrdering` on
  `Fence`/`LoadLinked`/`StoreConditional`/`LoadGuarded`/`StoreGuarded`/
  `AtomicCAS` (VERIFY exact list); `CallOther` userop index (VERIFY whether
  index reaches any fact row); possibly others (S3 enumerates).
- Upstream `r2il/src/varnode.rs:18-19` `Varnode{space, offset, size}` —
  derive line shows `Debug, Clone, Serialize, Deserialize` and NOT
  `PartialEq` (VERIFY: manual impl? `R2ILOp: PartialEq` requires it).

## 3. THE PROPOSED RESOLUTION (committed)

New module `crates/ruff_r2il/src/oracle.rs` + additions below. No other
source file changes semantics.

### 3.1 `facet::unproject` (in `facet.rs`, beside `project`)

`pub fn unproject(f: &VarnodeFacet, spaces: &CustomSpaceTable) ->
Option<Varnode>` — exact inverse of `project`: discriminant → `SpaceId`
(four fixed; custom via ordinal→raw reverse lookup — add a read-only
accessor on `CustomSpaceTable` if none exists). `None` only for an unknown
discriminant (typed refusal, mirroring `project`'s refusal posture; never a
guess). Property test: `unproject(project(vn)) == vn` over all four fixed
spaces + custom + 64-bit offsets; unknown-discriminant can-fire test.

### 3.2 `OpSkeleton` — THE equivalence target

```rust
pub struct OpSkeleton { pub opcode: OpTag, pub output: Option<Varnode>,
                        pub inputs: Vec<Varnode> }
impl OpSkeleton { pub fn of(op: &R2ILOp) -> Self /* from_r2il + output() + inputs() */ }
```

Semantic equivalence for this slice = skeleton equality at each source op
site. This is exactly the projection the routes carry (frozen 2: typed
parity, not binary equality). Non-varnode attributes are OUT of the skeleton
and INTO the measured gap channel (3.4) — never silently passed.

### 3.3 `reconstruct`

`pub fn reconstruct(rows: &[FlatFact], spaces: &CustomSpaceTable) ->
Reconstruction`:

- Group `FactKind::Op` rows by `prov.op_site`; attach `OperandIn` rows (same
  `prov.inst`, ordered by `a`) + the `OperandOut` row.
- Completeness per op (from the op row's OWN payload): OperandIn count ==
  arity (`b` bits 0..32) AND OperandOut presence == has_output (`b` bit 32).
  Incomplete → `ReconstructionMiss::MissingOperands{site, have, need}` —
  reported, never skipped.
- Operand facets → `facet::unproject`; failure →
  `ReconstructionMiss::UnknownDiscriminant{site, index}`.
- Output: `Reconstruction{ ops: Vec<ReconstructedOp{site, ordinal,
  skeleton}>, misses: Vec<ReconstructionMiss> }`.

### 3.4 `judge` — the verdict

`pub fn judge(source: &[R2ILBlock], recon: &Reconstruction,
ledger: &ResidualLedger) -> OracleVerdict`:

For every source op site `(block_addr, op_idx)` exactly one of:
(a) reconstructed AND `OpSkeleton::of(source_op) == reconstructed.skeleton`
→ `matched`; (b) named by ≥1 ledger residual anchored at that site (via
`provenance.op_site`, or block-anchored for reasons that carry only
`block`) → `ledger_accounted`; (c) neither → `orphans` entry. Skeleton
inequality → `mismatches` entry carrying both skeletons.

`OracleVerdict{ matched, ledger_accounted, orphans, mismatches,
attribute_gaps }`, `fn holds() = orphans.is_empty() &&
mismatches.is_empty()`.

**Attribute-gap channel:** for each matched op whose source variant carries
non-varnode semantic state (the S3-verified list), emit
`AttributeGap{site, opcode, attribute: GapAttribute}` where `GapAttribute`
is a small typed enum (`MemorySpace`, `MemoryOrdering`, `UserOpIndex`, …).
This is deliberately NOT a `ResidualReason` — the furnace did not fail; the
schema deliberately projects. The gap census is the measured input for a
FUTURE additive widening decision (probe-first). Can-fire fixture:
Load/Fence; can-stay-silent fixture: Copy/IntAdd-only.

### 3.5 Oracle convention (measurement config, not a shipped default)

`pub fn permissive_convention(blocks: &[R2ILBlock]) -> R2ilConvention` in
`oracle.rs`: classify every `OpTag` present in `blocks` + insert
`FacetPrefix::Space` root rows for every discriminant the blocks' varnodes
project to. Pure config widening (frozen 6). Documented as the oracle's
measurement convention; `minimal_pass_one` remains the shipped default and
its stressor-slag acceptance tests are untouched.

### 3.6 Artifact-mediated arm

One test: smelt → `OfflineSink::write_harvest` → `read_facts` +
`read_residuals` → `reconstruct` + `judge` → verdict EQUAL to the in-memory
verdict. This is the load-bearing proof that the v2 schemas (PR #102) are
reconstruction-sufficient. Zero schema change (frozen 9).

### 3.7 `lift`-gated example + harvest doc

`examples/r2il_roundtrip_oracle.rs` (feature `lift`): run the oracle over
the same corpus as the §12 profile (r2sleigh e2e stress binaries; stripped
ELFs op-level), print matched / accounted / orphans / mismatches /
gap-census. Result recorded in `.claude/harvest/r2il/ORACLE-RESULT.md`
(cite-never-rederive, same footing as CORPUS-PROFILE-RESULT.md), and the
guide §1 stability table gains a row for it. Run in-session if build cost
permits; otherwise the doc records "example shipped, corpus run pending"
honestly.

## 4. NON-GOALS

- **Codebook wiring** (read `ogar_codebook`) — its own PR2 slice; different
  blast radius (cross-repo read).
- **SPO projection of semantic facts** (calls/objects) — optional per plan,
  sequenced after the oracle.
- **Closing the attribute gaps by widening FACTS/FlatFact now** — gated ON
  this oracle's measured gap census (probe-first; frozen 5 protects the pin).
- **Changing `smelt`/ladder semantics** — the oracle measures the furnace.
- **lance-graph SoA sink (backend 2)** — downstream repo, per SUBSTRATE
  RULING.
- **S3 signed PUT** — credential plumbing only remains, unchanged.

## 5. PRE-REGISTERED GATES

1. `cargo test` (crate dir) all green; total strictly above the current
   52 (40 lib + 12 integration).
2. `cargo clippy --all-targets -- -D warnings` clean; `cargo fmt --check`
   clean; `uv run --only-group dev prek run --files <changed>` clean.
3. Oracle fixture gates (each an automated test):
   - full-melt fixture under `permissive_convention`: `holds()` AND
     `matched == source op count` AND `ledger_accounted ==` exact phi +
     CallDefine count — exact numbers, not `>=`.
   - `minimal_pass_one` fixture: stressors land in ledger; still `holds()`
     (accounted, not orphaned); anti-vacuity `matched >= 1 &&
     ledger_accounted >= 1`.
   - mismatch can-fire (negative test IN the suite): corrupt one operand
     row's facet offset → exactly one mismatch reported.
   - orphan can-fire: drop one op's rows AND residuals → orphan reported.
   - gap can-fire (Load or Fence) + can-stay-silent (Copy/IntAdd only).
   - artifact-mediated verdict == in-memory verdict.
   - `unproject∘project == id` (4 fixed spaces + custom + 64-bit offsets);
     unknown discriminant → `None`.
4. Manual disable-run on ≥2 new tests (mutate → red → restore → green),
   named in the commit message.
5. Diff confinement: `oracle.rs` (new), `facet.rs` (+`unproject` +
   accessor only), `lib.rs` (module wiring), `tests/`, `examples/`,
   `.claude/harvest/r2il/ORACLE-RESULT.md`, guide §1 row, this spec's
   ratification note. NO diff in `furnace.rs`/`ore.rs`/`slag.rs`/`sink.rs`
   semantics (doc-comment cross-refs allowed).

## 6. PER-SAVANT QUESTION SETS

Output contract (every savant): ≤10 findings, each = `(question #, verdict
∈ {CONFIRMS, VIOLATES, GAP, PRIOR-ART-AT, RISK}, file:line evidence, ≤2
sentences)`. No prose essays. No redesigns — a redesign urge files one RISK
and stops. Read-only.

### S1 — prior art
1. Does any existing code in ruff_r2il or r2sleigh already implement
   facet→Varnode inversion or op-from-parts reconstruction (grep:
   `unproject`, `reconstruct`, `from_facet`, `from_parts`, `build_op`)?
2. Does r2sleigh ship an R2ILOp constructor from (tag, output, inputs) the
   skeleton could reuse instead of comparing projections?
3. Is there an existing permissive/test convention helper anywhere in
   tests/examples that 3.5 would duplicate?
4. Do the plan/guide name an artifact filename convention that
   `ORACLE-RESULT.md` should follow or that already exists?

### S2 — iron rules / repo doctrine
1. Does the design keep SPO out of the oracle path end-to-end? (frozen 1)
2. Does anything in 3.x introduce a persistence assumption into
   furnace/ore/slag? (frozen 4)
3. Any `Debug`-as-data path in skeleton compare, gap enum, or verdict
   rendering? (frozen 8)
4. Is the artifact discipline strictly additive (no schema bump, new doc +
   guide row only)? (frozen 9)
5. Does `permissive_convention` stay pure-config (frozen 6), or does any
   part of the design require a new `smelt` arm?
6. RISK check: does the AttributeGap channel (a second accounting channel
   beside the ledger) dilute the conservation ledger's authority (frozen
   3/7), or is it cleanly orthogonal (furnace-didn't-fail vs
   furnace-failed)?

### S3 — code truth (verify the spec against source)
1. Verify every file:line claim in §2, especially the payload-table
   semantics used by 3.3 (Op `b` = arity|has_output<<32; OperandIn `a` =
   index; op row and its operand rows share `prov.inst`).
2. Does `Varnode` implement `PartialEq` (derive or manual)? Cite the line.
3. Enumerate EXACTLY the R2ILOp variants whose semantic state exceeds
   `inputs()`/`output()` — variant → lost attribute(s). This list becomes
   `GapAttribute`.
4. Does `CustomSpaceTable` expose an ordinal→raw inverse today? If not, what
   is the minimal read-only accessor?
5. Under `minimal_pass_one` (zero rows), confirm from `smelt`'s operand arm
   that NO operand row melts (`conv.resolve` gate) — i.e. the oracle's
   full-melt gate REQUIRES root rows.
6. For each `ResidualReason`, which provenance anchor does its
   `ResidualFact` carry (`op_site` vs `block` vs none) — can `judge`
   re-anchor every residual to a source op site, and what is the honest
   rule for `Edge` (no prov at all)?

### S4 — cascade impact
1. Complete the mandatory-same-commit file list (§5 gate 5) — anything
   missed (plan Open-items update? STAGED-CODEGEN-GUIDE §1 table? lib.rs
   docs? README?)?
2. Which existing tests could the new module break (name any test coupling
   to facet.rs internals or convention defaults)?
3. Does adding `unproject` weaken the guide §1 caveat that the 16-byte
   facet is "provisional — treat as opaque key, do not persist as durable
   address"? What wording must the doc row carry?
4. Follow-up (not this slice) rows to file: gap-census → widening decision;
   corpus oracle run if deferred; PR3 mint implications.

### S5 — different views (no redesigns; strongest alternative + consequence)
1. Alternative: widen the schema NOW for full R2ILOp equality instead of
   skeleton + gap census. Second-order consequence of deferring vs taking?
2. Alternative: judge by FORWARD comparison (re-smelt the reconstructed
   blocks and diff row sets) instead of skeleton equality. Name each
   approach's blind spot.
3. Does an oracle that holds under `permissive_convention` prove anything
   about the shipped `minimal_pass_one`? Is the gate framing honest?
4. What does the gap census imply for PR3's classid-mint scope (one line)?
