# R2IL round-trip reconstruction oracle — measured

Resolves the PR-2 gate deliverable from `.claude/plans/r2il-behavioral-ir-v1.md`
§14 ("R2IL → routes → semantic-equivalent R2IL; SPO explicitly NOT the oracle")
and opens **O6a**'s census. Cite this, never re-derive it — same discipline as
`TRIAGE-RESULT.md` and `CORPUS-PROFILE-RESULT.md`.

Command: `cargo run --release --example r2il_roundtrip_oracle --features lift`
(from `crates/ruff_r2il/`). Caps: `R2IL_ORACLE_MAX_SECTION_BYTES=262144`,
`R2IL_ORACLE_CHUNK_BLOCKS=24`, `R2IL_ORACLE_MAX_CHUNKS=200` (all defaults).
Module: `src/oracle.rs`; spec: `.claude/plans/r2il-roundtrip-oracle-spec-v1.md`
(council-ratified v3).

Corpus: identical to the §12 profile — `r2sleigh/tests/e2e/{stress_test,
stress_test_opt}` plus `/bin/ls`, `/usr/bin/env` (all ELF64 x86-64).

## Headline finding — ZERO mismatches on every binary

| binary | chunks | matched | ledger_accounted | ssa_only | orphans | **mismatches** |
|---|---|---|---|---|---|---|
| stress_test | 143 | 8053 | 0 | 2253 | 6105 | **0** |
| stress_test_opt | 125 | 7761 | 0 | 3058 | 5167 | **0** |
| /bin/ls | 200 | 9743 | 0 | 3703 | 7224 | **0** |
| /usr/bin/env | 200 | 10389 | 0 | 2321 | 7230 | **0** |

(permissive convention; `minimal_pass_one` produces the identical orphan and
`ssa_only` counts with `matched = 0` and the same totals moved wholesale into
`ledger_accounted` — see "Both conventions" below.)

**Across 35,946 matched op sites in four binaries, the reconstruction never
produced a skeleton that differed from its source op.** Reconstruction is
`facet::unproject` + row grouping + `OpSkeleton` comparison; a mismatch would
mean the routes carried an op that decodes back to something else. None did.

This is the mechanism claim and nothing more. It does NOT claim the routes
carry every op (they do not — see the gap census), nor that the shipped default
convention achieves this coverage (it does not — see below).

## The orphan count is a CHUNKING artifact — measured, not assumed

`orphans` is large (5167–7230) and that number is explained entirely by how the
corpus is fed in, not by the oracle:

| binary | blocks reaching the CFG | source blocks in chunks |
|---|---|---|
| stress_test | 1835 | 3427 |
| stress_test_opt | 1579 | 2978 |
| /bin/ls | 2377 | 4800 |
| /usr/bin/env | 2607 | 4800 |

A chunk is a window of 24 consecutive linear-sweep blocks, so many of its
blocks branch to targets OUTSIDE the window and are unreachable from the
chunk's entry. `CFG::from_blocks` drops them; their ops therefore never reach
`ore::enumerate`, produce neither a fact row nor a residual, and land in
`orphans` by construction. Roughly 46–50 % of blocks are dropped this way,
and the op-level arithmetic matches exactly: for `stress_test`,
`8053 matched + 6105 orphans = 14158`, which is precisely that binary's
`ops_total` in `CORPUS-PROFILE-RESULT.md`.

**Two things this is and is not.** It is NOT evidence of a reconstruction
defect — the in-repo fixtures (`tests/oracle_roundtrip.rs`), which use coherent
CFGs, report zero orphans. It IS evidence that the oracle refuses to hide a
gap: an implementation that quietly skipped unreachable blocks would have
reported `holds()` on every chunk and looked perfect. Getting a loud, countable
orphan population from a deliberately lossy input is the behaviour the
conservation reading (plan C3) exists to produce.

A symtab-driven function decomposition (the §12 profiler's Pass 2 shape) would
shrink this dramatically; that is O6a's natural next refinement, not a defect
to fix here.

## Both conventions, never conflated (spec §3.5)

`minimal_pass_one` — the SHIPPED default — has zero convention rows, so
`R2ilConvention::resolve` returns `None` for every facet and no operand row
melts. Measured consequence on all four binaries: `matched = 0`, with every one
of those op sites moving into `ledger_accounted` (8053 / 7761 / 9743 / 10389
respectively) and `holds` unchanged.

So the honest reading is:
- **permissive** proves the reconstruction MECHANISM is faithful;
- **minimal_pass_one** shows the shipped default currently round-trips nothing
  through matching — it round-trips through ACCOUNTING, which is a real but
  different property.

Anyone quoting the mismatch-free result must quote which convention produced
it. The example prints both columns for exactly this reason.

## O6 — the attribute-gap census (the schema-widening input)

Twelve `R2ILOp` variants carry semantic state beyond the `inputs()`/`output()`
varnode projection the fact rows represent. Matched ops of those variants emit a
typed `AttributeGap` rather than passing silently. Measured:

| binary | memory_space | subpiece_offset | memory_ordering | userop_index | ptr_element_size |
|---|---|---|---|---|---|
| stress_test | 1273 | 20 | 0 | 0 | 0 |
| stress_test_opt | 536 | 56 | 0 | 0 | 0 |
| /bin/ls | 741 | 25 | 0 | 0 | 0 |
| /usr/bin/env | 1036 | 12 | 0 | 0 | 0 |

**`MemorySpace` is the dominant gap by an order of magnitude** (3586 total
against 113 `SubpieceOffset`). Every `Load`/`Store` that round-trips today
round-trips WITHOUT which address space it touched — the fact rows carry the
address varnode but not the `space: SpaceId` field. That is the single
highest-value candidate for an additive widening, and it is now measured rather
than assumed.

**Three gap kinds recorded ZERO on this corpus** — `MemoryOrdering`,
`UserOpIndex`, `PtrElementSize`. That is absence of evidence, not evidence of
absence: this corpus exercises no atomics/fences (consistent with
`CORPUS-PROFILE-RESULT.md`'s own `call_other_arity: (no samples)` finding), so
those three remain unmeasured here. A corpus with atomics is what would move
them, and the `gaps_of` totality test (`tests/oracle_roundtrip.rs`) pins that
all twelve variants CAN fire regardless of whether this corpus makes them.

## What this resolves and what it does not

- **Resolved:** the §14 round-trip oracle exists, runs on the real corpus, and
  reports zero reconstruction mismatches over 35,946 matched op sites.
- **Resolved (O6, first census):** `MemorySpace` dominates the attribute gap;
  `SubpieceOffset` is real but small; three kinds are unmeasured on this corpus.
- **NOT resolved:** whether to widen the schema for `MemorySpace` (that is the
  O6 decision this census feeds, and it must stay additive — the `FlatFact`
  88-byte pin holds); the symtab-driven decomposition that would collapse the
  chunking orphans (O6a refinement); and the shipped `minimal_pass_one`
  convention's own coverage, which this run measures as accounting-only.
