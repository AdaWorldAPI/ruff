# SPEC — PR2 slice: the round-trip reconstruction oracle (`oracle.rs`)

> 5+3 council spec. **RATIFIED v3** (v1 and v2 are the prior git revisions
> of this file). Council run: 5 savants (prior-art / iron-rules /
> code-truth / cascade / views, all Sonnet) → consolidation → 3 reviewers
> (overclaim / dilution-collapse / firewall charters, Sonnet) → fix.
> Reviewer verdicts on v2: R3 all-PASS 0 findings; R2 five-PASS +
> FIX(P2)×2; R1 four-PASS + FIX(P1)×1 + FIX(P2)×2; ZERO BLOCK. All five
> FIX findings applied — §7 rows 15-18. This document is the executable
> spec; implementation follows it without further design.
> The change ledger (§7) records every finding and its disposition,
> including losing findings (anti-collapse).
>
> Scope: the PR2 gate deliverable from `.claude/plans/r2il-behavioral-ir-v1.md`
> — "round-trip reconstruction oracle (R2IL → routes → semantic-equivalent
> R2IL; SPO explicitly NOT the oracle)". Follows PR #101 (sink trait +
> OfflineSink + O1) and PR #102 (v2 facts/residuals schemas + readers).

## 1. FROZEN DECISIONS

1. **SPO is NOT the oracle.** The comparison happens on typed r2il values,
   never on a triple projection. (`r2il-behavioral-ir-v1.md:282-285`,
   `:377-380`.) [S2 Q1: CONFIRMS]
2. **Success = semantic/behavioral parity on TYPED values** — never textual,
   never binary-artifact equality. (C4, `r2il-behavioral-ir-v1.md:277-285`.)
3. **Conservation is load-bearing.** `harvested == classified + residual`,
   `dropped == 0` (`furnace.rs` `HarvestReport::is_conserved`). The oracle
   extends the same reading over its own universe (§3.4): every SOURCE OP
   SITE is reconstructed-equal or ledger-accounted; neither is a FAILURE.
   (C3.)
4. **No persistence assumption enters `furnace`/`ore`/`slag`.** (SUBSTRATE
   RULING.) [S2 Q2: CONFIRMS]
5. **`FlatFact` stays flat, `Copy`, exact 88-byte pin.** This slice adds NO
   field. (`furnace.rs:206-207` const assert.)
6. **Widening classification is `R2ilConvention` DATA, never a new `smelt`
   arm.** (`furnace.rs` module docs; `convention.rs:88/205`.)
   [S2 Q5: CONFIRMS — 3.5 uses only data-driven constructors]
7. **No catch-all discipline** extends to the NEW `GapAttribute` enum: it
   gets `ALL`, `as_str`, and an exhaustiveness test mirroring
   `ResidualReason` (`slag.rs:124+`). [S2 Q6 RISK, absorbed as a gate]
8. **`format!("{:?}")` is FORBIDDEN as a data path** — and this now
   explicitly covers verdict/skeleton RENDERING: the example's printed
   report and `ORACLE-RESULT.md` numbers are produced from typed fields and
   `as_str()` only. `Debug` remains legal solely inside test-assertion
   failure messages (a diagnostic on a failing test is not a data path).
   [S2 Q3 VIOLATES, fixed here]
9. **Artifact discipline additive; no TSV schema change.** v2 schemas
   (`sink.rs:173-177`) are sufficient (proven by §3.6's arm). [S2 Q4]
10. **Falsifiability rule**: can-fire + can-stay-silent per guard; manual
    disable-runs recorded in the commit message.
11. **No model identifier in any committed artifact.**

## 2. INPUT INVENTORY (savant-verified; corrections from v1 marked ✎)

- `behavior.rs:57-70` `from_blocks_raw` (lossless; `from_blocks` runs SCCP —
  forbidden for the oracle); `:188-218` provenance helpers.
- `ore.rs:841-899` — Op row and its Operand rows share the identical
  `base_prov` (`inst: Some(inst_id)`) [S3 Q1 CONFIRMS]; phi facts hardcode
  `op_site: None` (`ore.rs:824-829, 941-949`) [S3 Q6].
- `furnace.rs:280-408, 791-793` — payload semantics CONFIRMED: Op `a` =
  ordinal, `b` = `input_arity | (has_output << 32)` via `pack_op_metadata`;
  OperandIn `a` = index, `b` = `ValueId.0+1`. Operand melt gate
  `conv.resolve(&facet).is_some()` at `:350`; unresolved →
  `NoConventionRowAtAddress` (`:371-377`).
- `slag.rs:80-118` `ResidualReason` (11 variants). ✎ Provenance anchors per
  reason [S3 Q6]: all op-derived reasons carry the parent op's full `prov`
  (with `op_site: Some`); `PhiFanInExceedsPredecessors` and phi/CallDefine
  `NoFacetCoordinate` are block-anchored only (`op_site: None`); the Edge
  no-facet case carries a furnace-SYNTHESIZED `prov{block: Some(from)}`
  (`furnace.rs:421-426`). ✎ `UserOpNotInConvention` is NEVER constructed by
  the current ladder (dead variant) — recorded, out of scope to fix.
- ✎ `facet.rs:246-261` — **`unproject(f, spaces) -> Result<Varnode,
  FacetOverflow>` ALREADY EXISTS**, with the exact property tests v1
  proposed (`fixed_spaces_round_trip_byte_for_byte`,
  `custom_space_within_budget_round_trips`,
  `custom_space_outside_the_table_errors_and_never_truncates`,
  `offsets_above_u32_max_survive_the_lo_hi_split`). `CustomSpaceTable::
  raw_of` (`facet.rs:196-206`) is the ordinal→raw inverse. v1's "no inverse
  exists" was WRONG. [S1/S3/S4 unanimous]
- `convention.rs:98-114` `minimal_pass_one` = 7 opcodes, ZERO rows ⇒
  `resolve` always `None` ⇒ NO operand row ever melts under it [S3 Q5
  CONFIRMS]. No existing test couples to its row contents [S4 CONFIRMS].
- `sink.rs:173-177` FACTS/RESIDUALS v2 + `read_facts`/`read_residuals`.
- `r2il/src/opcode.rs:26-495` `R2ILOp` (`PartialEq`); `:534 output()`,
  `:691 inputs()`. ✎ `varnode.rs:149-155`: `Varnode` has a MANUAL
  `PartialEq` over `space`/`offset`/`size` only (excludes `meta`) — so
  `unproject`'s output compares correctly. [S1/S3]
- ✎ **The verified attribute-gap enumeration — 12 variants, 5 attribute
  kinds** [S3 Q3, replaces v1's 7-variant guess]:

  | variant(s) | lost attribute(s) |
  |---|---|
  | `Load`, `Store` | `space: SpaceId` |
  | `Fence` | `ordering: MemoryOrdering` — AND zero varnode fields: its skeleton is `(Fence, None, [])`, trivially equal; the gap channel carries ALL its semantics |
  | `LoadLinked`, `StoreConditional`, `AtomicCAS`, `LoadGuarded`, `StoreGuarded` | `space` AND `ordering` |
  | `CallOther` | `userop: u32` (`opcode.rs:425`) |
  | `Subpiece` | `offset: u32` (`opcode.rs:301`) |
  | `PtrAdd`, `PtrSub` | `element_size: u32` (`opcode.rs:457/465`) |

- No op-builder `(tag, output, inputs) -> R2ILOp` exists anywhere
  [S1 Q2 GAP] — projection comparison is the only viable equivalence path,
  which independently validates §3.2's design.
- `*-RESULT.md` naming convention confirmed (`CORPUS-PROFILE-RESULT.md:5`)
  [S1 Q4].

## 3. THE RESOLUTION (committed; v1 deltas marked ✎)

New module `crates/ruff_r2il/src/oracle.rs`. `facet.rs` gets **no code
change** (✎ — at most a doc-comment cross-reference).

### 3.1 ✎ Facet inversion: CONSUME `facet::unproject`, build nothing

The oracle uses the shipped `facet::unproject` and `CustomSpaceTable::
raw_of` as-is. `FacetOverflow` from `unproject` maps to
`ReconstructionMiss::FacetInversion{site, index, raw}` (carrying the
overflow's own payload) — no bespoke `Option` shape, no duplicate inverse,
no new accessor. The already-shipped round-trip property tests stand as the
inversion gates; the oracle adds none.

### 3.2 `OpSkeleton` — THE equivalence target

```rust
pub struct OpSkeleton { pub opcode: OpTag, pub output: Option<Varnode>,
                        pub inputs: Vec<Varnode> }
impl OpSkeleton { pub fn of(op: &R2ILOp) -> Self /* from_r2il + output().cloned() + inputs() cloned */ }
```

Semantic equivalence for this slice = skeleton equality at each source op
site (Varnode's own `PartialEq`: space/offset/size). Non-varnode attributes
are OUT of the skeleton and INTO the measured gap channel (3.4).

### 3.3 `reconstruct`

`pub fn reconstruct(rows: &[FlatFact], spaces: &CustomSpaceTable) ->
Reconstruction`:

- Group `FactKind::Op` rows by `prov.op_site`; attach `OperandIn` rows
  (same `prov.inst`, ordered by `a`) + the `OperandOut` row.
- Completeness per op (from the op row's own payload, `b` = arity |
  has_output<<32): OperandIn count == arity AND OperandOut presence ==
  has_output; incomplete → `ReconstructionMiss::MissingOperands{site,
  have, need}` — reported, never skipped.
- Operand facets → `facet::unproject`; `Err(FacetOverflow)` →
  `ReconstructionMiss::FacetInversion` (✎ per 3.1).
- Output: `Reconstruction{ ops: Vec<ReconstructedOp{site, ordinal,
  skeleton}>, misses: Vec<ReconstructionMiss> }`.

### 3.4 `judge` — the verdict, with a PINNED universe ✎

**Universe = source op sites** `(block_addr, op_idx)` enumerated from the
input `&[R2ILBlock]`. For each site, a TRUE 4-way partition, evaluated in
this precedence order [R2 F1 / R1 F2]:

1. reconstructed (complete) AND `OpSkeleton::of(source_op) == skeleton` →
   `matched`;
2. reconstructed (complete) AND unequal → `mismatches` (both skeletons
   carried as typed values) — checked BEFORE the ledger criterion: a
   mismatch is never excused by a coincident residual at the same site;
3. not reconstructed — including every site whose op appears only via a
   `ReconstructionMiss` (`MissingOperands` / `FacetInversion` mean the site
   is NOT reconstructed) — AND ≥1 ledger residual whose
   `provenance.op_site` equals the site → `ledger_accounted`. This is the
   expected home of incomplete ops: the very operands that failed to melt
   produced op_site-anchored residuals;
4. else → `orphans`. An incomplete op with NO residual at its site orphans
   — that is the correct failure signal, not a gap to paper over.

✎ **Ledger rows OUTSIDE the universe** — residuals with `op_site: None`
(phi inputs, CallDefine, the Edge no-facet case; S3 Q6's anchor table) —
account for SSA-level facts that have no source op site. They are counted
as `ssa_only_residuals: usize` in the verdict, never errors and never
silently dropped. `holds()` = `orphans.is_empty() && mismatches.is_empty()`.

**Attribute-gap channel:** for each matched op whose variant appears in
§2's 12-variant table, emit `AttributeGap{site, opcode, attribute}` with

```rust
pub enum GapAttribute { MemorySpace, MemoryOrdering, UserOpIndex,
                        SubpieceOffset, PtrElementSize }
```

✎ Discipline mirrors `ResidualReason` (frozen 7): `GapAttribute::ALL`,
`as_str`, a no-catch-all test, AND the variant→gap mapping lives as ONE
total `fn gaps_of(tag: OpTag) -> &'static [GapAttribute]` match whose
completeness over the 12 variants is asserted by test (each of the 12
returns non-empty; a spot-check set of non-gap tags returns empty). Not a
`ResidualReason` — the furnace did not fail; the schema deliberately
projects. The census is the measured input for a FUTURE additive widening
decision, tracked as plan item O6 (§5 gate 5).

### 3.5 Oracle convention (measurement config, not a shipped default)

`pub fn permissive_convention(blocks: &[R2ILBlock]) -> R2ilConvention` in
`oracle.rs`: classify every `OpTag` present + insert `FacetPrefix::Space`
root rows for every discriminant the blocks' varnodes project to. Pure
config (frozen 6; S2 Q5 CONFIRMS). No prior art duplicated (S1 Q3).

✎ **Honest framing (normative, from S5 Q3):** a verdict that holds under
`permissive_convention` proves the reconstruction MECHANISM (facet
inversion + grouping + skeleton compare) — it says nothing about the
shipped `minimal_pass_one`'s coverage, under which no operand melts and
accounting dominates. `oracle.rs` module docs and `ORACLE-RESULT.md` MUST
state this, and the corpus example reports BOTH conventions' numbers so the
census shows what the shipped default actually covers.

### 3.6 Artifact-mediated arm

smelt → `OfflineSink::write_harvest` → `read_facts` + `read_residuals` →
`reconstruct` + `judge` → verdict EQUAL to the in-memory verdict. Proves
the v2 schemas are reconstruction-sufficient. Zero schema change.

### 3.7 `lift`-gated example + harvest doc

`examples/r2il_roundtrip_oracle.rs` (feature `lift`; ✎ requires its own
`[[example]] required-features` stanza in `Cargo.toml` — S4): corpus run
per §3.5's dual-convention rule; results →
`.claude/harvest/r2il/ORACLE-RESULT.md`; run in-session if build cost
permits, else the doc records "example shipped, corpus run pending" and
plan item O6 tracks it (✎ S4 Q4).

## 4. NON-GOALS (unchanged from v1)

- Codebook wiring (`ogar_codebook`) — own PR2 slice.
- SPO projection of semantic facts — after the oracle.
- Closing attribute gaps by widening FACTS/FlatFact now — gated on O6's
  census (S5 Q1: the census MUST be tracked or the loss goes permanent;
  hence O6 is mandatory-same-commit).
- Changing `smelt`/ladder semantics; fixing the dead `UserOpNotInConvention`
  variant (recorded, untouched).
- lance-graph SoA sink; S3 signed PUT.

## 5. PRE-REGISTERED GATES

1. `cargo test` (crate dir) all green; total strictly above 52.
2. `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`,
   `uv run --only-group dev prek run --files <changed>` — all clean.
3. Oracle test gates (automated):
   - full-melt fixture under `permissive_convention`: `holds()`, `matched
     == source op count` (exact), `ledger_accounted == 0` (exact),
     `ssa_only_residuals ==` exact phi+CallDefine count.
   - `minimal_pass_one` fixture: still `holds()` (accounted, not
     orphaned); `matched == 0` EXACT — satisfiable and pinned, because all
     7 classified opcodes carry ≥1 operand and no operand melts under zero
     rows, so no op can fully reconstruct (the v2 draft's `matched >= 1`
     conjunct was UNSATISFIABLE — R1 F1, P1); `ledger_accounted >= 1`
     anti-vacuity; PLUS `matched(minimal) < matched(permissive)` on the
     same blocks (the two conventions measure different things — S5 Q3).
   - mismatch can-fire: corrupt one operand row's facet → exactly one
     mismatch. ✎ swap can-fire (S5 Q2): swap two operand rows' facets
     ACROSS two ops → BOTH sites report mismatches (cross-row corruption
     is visible, not cancelled).
   - orphan can-fire: drop one op's rows AND its residuals → orphan.
   - gap can-fire: Load (MemorySpace) and Fence (MemoryOrdering, empty
     skeleton); can-stay-silent: Copy/IntAdd-only → zero gaps.
   - `gaps_of` completeness: all 12 table variants non-empty; non-gap tags
     empty; `GapAttribute::ALL` no-catch-all test (frozen 7).
   - artifact-mediated verdict == in-memory verdict.
   - ✎ NO new facet round-trip tests (they exist; re-running them is the
     gate).
4. Manual disable-run on ≥2 new tests, named in the commit message.
5. ✎ Diff confinement (corrected + completed per S4): `oracle.rs` (new),
   `lib.rs` (module wiring + module-table row), `Cargo.toml`
   (`[[example]]` stanza), `tests/` (new oracle fixture file), `examples/
   r2il_roundtrip_oracle.rs`, `.claude/harvest/r2il/ORACLE-RESULT.md`
   (new), `.claude/harvest/r2il/STAGED-CODEGEN-GUIDE.md` (§1: ONE new row
   for ORACLE-RESULT; the `VarnodeFacet ⚠ provisional` row is NOT edited —
   S4 Q3: unproject recovers a typed Varnode, not a durable address, so
   the persistence caveat is orthogonal), `.claude/harvest/r2il/README.md`
   (entry for the new example/artifact pair), `.claude/plans/
   r2il-behavioral-ir-v1.md` (new Open item **O6**: gap-census → widening
   decision; corpus-run-pending state; PR3 mint-scope note per S5 Q4),
   this spec (ratification note). `facet.rs`: **ZERO code diff** (same
   commitment as §3.1's "no code change"); at most a doc-comment
   cross-reference [R1 F5]. NO semantic diff in
   `furnace.rs`/`ore.rs`/`slag.rs`/`sink.rs`.

## 6. PER-SAVANT QUESTION SETS — retired (Phase 1 complete)

Question sets from v1 were answered; findings and dispositions in §7.

## 7. CHANGE LEDGER v1 → v2 (every finding, its disposition)

| # | savant, verdict | finding | disposition |
|---|---|---|---|
| 1 | S1/S3/S4 VIOLATES | `unproject` + `raw_of` already shipped with the exact tests v1 proposed | §3.1 rewritten to CONSUME; facet.rs code diff = zero; gates drop the duplicate tests |
| 2 | S1 RISK | real signature is `Result<_, FacetOverflow>`, not `Option` | §3.1/§3.3: `FacetInversion` miss maps the real error |
| 3 | S1 GAP/CONFIRMS | no op-builder exists; skeleton comparison is the only path | design validated; noted in §2 |
| 4 | S2 VIOLATES | no-Debug rule not stated for verdict rendering | frozen 8 extended; rendering rule normative |
| 5 | S2 RISK | `GapAttribute` lacked ResidualReason-style exhaustiveness | §3.4: `ALL` + `as_str` + `gaps_of` totality test; gate 3 |
| 6 | S3 GAP | gap enumeration is 12 variants/5 kinds, incl. `Subpiece`/`PtrAdd`/`PtrSub`/`Fence`-empty-skeleton | §2 table replaces v1 guess; `GapAttribute` gains `SubpieceOffset`, `PtrElementSize` |
| 7 | S3 CONFIRMS | payload semantics, shared `prov.inst`, Varnode manual `PartialEq`, minimal-pass-one no-operand-melt | inventory marked verified |
| 8 | S3 RISK | provenance anchors: phi/Edge block-only; `UserOpNotInConvention` dead | §3.4 universe pinned; `ssa_only_residuals` channel; dead variant recorded in §4 |
| 9 | S4 GAP ×4 | Cargo.toml stanza, README entry, plan O6, guide row missing from file list | gate 5 completed |
| 10 | S4 Q1 (partial) vs S4 Q3 | Q1 said the provisional facet row needs in-place edit; Q3 (deeper) says orthogonal | **Q3 wins** — `unproject` recovers a typed `Varnode`, not a durable address, so the row's persistence caveat is untouched by this slice. Q1's ONE salvageable facet — a discoverability pointer toward the oracle's use of `unproject` — is granted as the `facet.rs` doc-comment cross-reference (§3.1, §5 gate 5); no other facet of Q1 survives unaddressed [R2 F2] |
| 11 | S5 RISK | deferring widening is safe only if census is tracked | O6 mandatory-same-commit (§5) |
| 12 | S5 RISK | forward re-smelt oracle tests furnace against itself; skeleton misses cross-row swaps | direction confirmed; swap can-fire added to gate 3 |
| 13 | S5 RISK | permissive pass ≠ shipped-coverage proof | §3.5 honest-framing normative + dual-convention reporting + minimal<permissive assertion |
| 14 | S5 GAP | gap census feeds PR3 mint scope | folded into O6 text |
| 15 | R1 F1 FIX(P1) + v2→v3 | `matched >= 1` under `minimal_pass_one` was unsatisfiable (all 7 classified opcodes carry ≥1 operand; none melts under zero rows) | gate restated: `matched == 0` exact + `ledger_accounted >= 1` + the comparative inequality carries the signal |
| 16 | R1 F2 / R2 F1 FIX(P2) | §3.4's "(a)/(b)/(c)" partition omitted the mismatch bucket and the `ReconstructionMiss` routing | true 4-way partition with precedence; misses route to rule 3 or orphan |
| 17 | R1 F5 FIX(P2) | gate-5 facet.rs phrasing weaker than §3.1's "no code change" | aligned: ZERO code diff both places |
| 18 | R2 F2 FIX(P2) | ledger row 10 not self-contained | row 10 now names Q1's one salvageable facet and where it is granted |
