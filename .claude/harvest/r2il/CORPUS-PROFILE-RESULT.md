# R2IL §12 corpus profile — measured

Resolves **O1** (`.claude/plans/r2il-behavioral-ir-v1.md`): "corpus profile
numbers (PR 1 gate for PR 2 layout)." Cite this, never re-derive it — same
discipline as `TRIAGE-RESULT.md`.

Command: `cargo run --example r2il_corpus_profile --features lift --release`
(from `crates/ruff_r2il/`). Caps: `R2IL_PROFILE_MAX_FUNCS=200`,
`R2IL_PROFILE_MAX_SECTION_BYTES=262144` (both defaults, unset).

Corpus: `r2sleigh/tests/e2e/{stress_test,stress_test_opt}` (ELF64 x86-64,
symtab present — both passes run) and `/bin/ls`, `/usr/bin/env` (ELF64
x86-64, stripped — op-level pass only, per the example's own labelling rule).

## Headline finding — the §11 layout question is settled by this corpus

**`% fitting dst+src0+src1 inline = 100.00`, `% needing Vec routing = 0.00`,
on every one of the four binaries, at both non-fitting and fitting cases
never split into two rows.** No op measured across 14158 + 12928 + 84625 +
18482 = 130193 total `Op` facts required more than one output and two inputs.

That is not a rounding artifact — the output-count and input-arity
histograms below independently confirm it: `outputs` is always `0` or `1`
(never `>1`), and `arity` is always `1` or `2` (never `>2`), across all four
binaries. The **inline** layout (fixed `dst + src0 + src1` slots, no Vec, no
descriptor indirection) is therefore the correct §11 choice for this corpus
family — a descriptor or hybrid layout would spend bytes/indirection on a
case (`>2` inputs, `>1` output) this x86-64 SLEIGH lift never produces.

**Caveat, stated plainly:** this is measured on x86-64 SLEIGH P-code from
four ELF binaries (two stress-test binaries + two stripped coreutils). It is
evidence for THIS corpus family, not a universal claim about R2IL/P-code —
a different architecture's SLEIGH spec, or an op this corpus never exercises
(the plan's own §14 lossless-fixture list includes ops like `Multiequal`
with `>2` phi inputs — see `phi_fanin` below, which is real and DOES exceed
2 on `stress_test`) needs its own measurement before the inline choice is
assumed to generalize. `Multiequal`/phi is exactly why `phi_fanin` is
measured separately, below, and is NOT covered by the
"fits dst+src0+src1" statistic (phi is not one of the ops that stat samples
in this corpus's op mix — see per-binary opcode_freq: no `multiequal` row
appears in any of the four binaries' `opcode_freq` tables, so phi's own
non-fitting shape is present in the type system and in `phi_fanin` but not
exercised as a *sampled op* in this particular corpus).

## Per-binary op-level (MEASURED EXACT)

| binary | instructions | ops_total | ops/instr (mean) | memory-op % | control-op % | arity=1 | arity=2 | outputs=0 | outputs=1 | inline fit % |
|---|---|---|---|---|---|---|---|---|---|---|
| stress_test | 3427 | 14158 | 4.13 | 15.17 | 4.26 | 5896 | 8262 | 1379 | 12779 | 100.00 |
| stress_test_opt | 2978 | 12928 | 4.34 | 6.49 | 5.00 | 4842 | 8086 | 1032 | 11896 | 100.00 |
| /bin/ls | 20764 | 84625 | 4.08 | 7.88 | 5.73 | 32979 | 51646 | 7903 | 76722 | 100.00 |
| /usr/bin/env | 5036 | 18482 | 3.67 | 9.56 | 6.69 | 7474 | 11008 | 2136 | 16346 | 100.00 |

`undecodable_instructions = 0` on all four (100% decode success on this
corpus — every window `disasm.lift`-ed cleanly).

`call_other_arity: (no samples)` on all four — this corpus exercises no
`CallOther` userop calls, so the arity distribution for that op family is
unmeasured here (a gap, not a zero finding — record honestly per the
example's own no-silent-omission rule).

Dominant opcodes (by raw count, `/bin/ls` the largest sample): `copy`
(16056), `int_and` (12361), `int_equal` (9908), `pop_count` (4821, likely a
SLEIGH lift artifact of flag-bit population-count microcode rather than the
x86 `POPCNT` instruction — not independently verified here), `int_sless`
(4956), `load` (3610), `int_zext` (3491), `int_sub` (3971), `store` (3057).

## Per-binary function/CFG-level (HEURISTIC-DERIVED, symtab binaries only)

| binary | functions | blocks/fn (mean) | ops/block (mean) | phi_fanin (mean/max) | values/fn (mean) | call_sites/fn (mean) |
|---|---|---|---|---|---|---|
| stress_test | 71 | 7.87 | 25.41 | 2.56 / 4 | 102.37 | 0.85 |
| stress_test_opt | 72 | 8.36 | 24.77 | 2.29 / 7 | 134.31 | 0.72 |

`phi_fanin` max reaches **7** on `stress_test_opt` — confirms real
`Multiequal`-shaped fan-in above 2 predecessors exists in this corpus at the
CFG level (the compiler's own phi nodes, not a sampled `Op` row per the
caveat above), which is exactly the case the plan's §14 lossless-fixture
suite tests explicitly rather than relying on corpus incidence.

## Facet projection (O3) — zero overflow observed on this corpus

| binary | facet::project ok | UnknownCustomSpace | CustomOrdinalExhausted |
|---|---|---|---|
| stress_test | 34354 | 0 | 0 |
| stress_test_opt | 31750 | 0 | 0 |

**Not a closure of O3.** Zero overflow on THIS corpus is consistent with —
but does not prove — the 16-byte `VarnodeFacet` projection never overflowing
in general; `vocab.unique_custom_spaces_from_strings` is `0` or `1` per
function on both binaries (never higher), so this corpus simply never
exercises multiple custom spaces in one function. O3's formal answer stays
the dedicated fixture the plan already calls for
(`typed_custom_space_ids_are_the_oracle_for_the_string_set` in
`src/vocab.rs`'s test list), not this corpus measurement.

## `furnace::smelt` conservation (cross-check against PR 1's harvest, different caps)

| binary | harvested | classified | residual | dropped |
|---|---|---|---|---|
| stress_test | 23608 | 10003 | 13605 | 0 |
| stress_test_opt | 30696 | 7554 | 23142 | 0 |

`dropped == 0` on both — B1 (conservation) holds under this run's caps too,
independently of the `TRIAGE-RESULT.md` run (which used `R2IL_HARVEST_MAX_FUNCS=200`
over a different, larger corpus mix and reported 143 functions / 54304
harvested). Different cap values, same invariant, both green — not the same
number and not meant to be.

## What this resolves and what it doesn't

- **Resolved (O1):** the §11 layout choice for PR 2 is **inline**
  (`dst + src0 + src1`, no Vec, no descriptor), backed by 130193 sampled ops
  across 4 binaries with 0 exceptions.
- **Not resolved by this run:** `CallOther` arity (no samples in this
  corpus — PR 2 must handle it from the type's own arity bound, not from a
  measured distribution), and O3's formal fixture (facet overflow was
  never triggered here, so this is absence-of-evidence, not evidence of
  absence, per the plan's own standing discipline on that distinction).
