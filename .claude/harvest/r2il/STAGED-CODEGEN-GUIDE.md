# Staged codegen off the R2IL harvest — without breaking what exists

> **Audience:** the sibling session that consumes this arc's output (the Ghidra
> console work, and any codegen/target-profile work downstream).
> **Status of the substrate:** PR 1 shipped the intake arm; PR 2 (routes → V3)
> has NOT landed. Everything below is written so you can start staging now and
> not have to unpick it when PR 2 changes the physicalization.

## 0. The one rule

**Every export is additive. A consumer written against version N must keep
working, unread, against version N+1.**

That is not a style preference — it is the same rule the substrate already runs
on. lance-graph's V3 canon says *"RESERVE, DON'T RECLAIM: a zero tier means
`not consulted`, never `compacted away`"*, and `ruff_spo_triplet::ir::ModelGraph`
is schema-locked at the top level with per-language growth confined to sibling
`Vec`s carrying `skip_serializing_if`. Copy that discipline; do not invent a
migration story you will then have to run.

## 1. What you may consume today, and what is still moving

| artifact | stability | use it for |
|---|---|---|
| `TRIAGE-RESULT.md` | **stable** | the bars and their verdicts. Read `B1` before trusting anything else — if conservation ever fails, the run is void. |
| `PROVENANCE.md` | **stable** | corpus identity (FNV-1a per input), r2sleigh pin, exact invocation. Cite this, never re-derive it. |
| `r2il-pass1-slag.tsv` | **stable shape** | the residual work queue. New `reason` values WILL appear; treat an unknown reason as "not yet classified", never as an error. |
| `r2il-pass1-census.md` | **stable shape** | counts per fact-kind / opcode. |
| `r2il-pass1.ore.tsv.gz` | **shape stable, columns additive** | the melted rows. Read by the `#schema` header, never by column position. |
| `r2il-convention.toml.gz` | **stable** | the drill tree. Every row is `unmeasured` until something measures it. |
| `FlatFact`'s two payload slots (`a`, `b`) | ⚠ **NOT stable** | their per-kind meaning is documented in `furnace.rs` and may be re-carved in PR 2. Do not hardcode the bit layout; go through the accessor or re-read the module table. |
| `OpTag::as_str()` opcode tags | **stable, with one correction** | one tag shipped briefly as `int_scary` — a spellchecker rewrite of `int_scarry` (P-code `INT_SCARRY`, signed carry) that reached the enum's `as_str`. Corrected; the pass-1 artifacts never carried it (no SCARRY op classified in the corpus). If you pinned the misspelling, repin. |
| the 16-byte `VarnodeFacet` **as an address** | ⚠ **provisional** | `PROVISIONAL_R2IL_VARNODE = 0x0000` is a local placeholder. The real classid is minted in `ogar_codebook` (PR 3). Treat the facet as an opaque key today; do not persist it as a durable address. |

## 2. Staging order

Stage in this order; each step is independently useful and none of them blocks
on PR 2.

```text
S1  read the ledger        slag + census only. Answers "what does the arm not
                           yet explain?" Needs no codegen at all.
S2  read the ore rows      per-function fact rows, joined back to native
                           addresses via ore::instruction_addr. Enables
                           navigation and evidence display.
S3  emit into a landing    generated code goes to an ADDITIVE landing zone (a
    zone, not in place      new module/crate), never edited into existing files.
                           openproject-nexgen-rs's `op-generated` is the worked
                           precedent: 16 structs emitted beside hand code, with
                           a `// @generated` header.
S4  wire ONE consumer      prove the seam on a single real call site before
                           scaling. That is what "no wave scales out before
                           P-REHOST is green" means in a2ui-rs.
S5  target profiles        only here does "ordinary Java vs Valhalla/Panama
                           Java" become a real fork. Until S4 is green it is a
                           design conversation, not a code path.
```

**Do not skip to S3.** The MedCare and OpenProject transcodes both earned their
numbers by measuring at S1/S2 first (`99.6 %` recoverable, `98.4 %` recipe
coverage) — those figures are what made the later codegen defensible.

## 3. The old/new SoA and `Va*` format question

There are two independent axes here and conflating them is the failure mode.

**Axis 1 — the physical row.** The V3 512-byte `NodeRow` (`16 | 16 | 480`) is
CANON and unchanged. What is V1-legacy is the *reading* of two fields: the
`NodeGuid` u24 tail (new mints go through the 4+12 content-blind facet) and the
`EdgeBlock`'s 12+4 carving (resolve `ClassView::edge_codec_flavor`). Neither
requires an `ENVELOPE_LAYOUT_VERSION` bump, and neither is something this
harvest emits.

**Axis 2 — the `Va*` carrier family** (`Vsa16kF32` / `Vsa16kBF16` / `Vsa16kF16`
/ `Vsa16kI8` / `Binary16K`). These are *compute* formats, selected per workload,
not a schema. The relevant standing rulings: `Vsa16kF32` is deprecated **as a
cross-boundary carrier** (it never crosses a mailbox boundary), and VSA is
demoted to its `I-VSA-IDENTITIES` niche — lossless role superposition of
**identities**, `N ≤ √d/4 ≈ 32`, never of content or of quantized codes.

**What that means for you concretely:** the R2IL harvest emits *neither*. It
emits flat facet-addressed rows and a residual ledger. If you find yourself
about to bundle R2IL facts into a `Va*` carrier, run the four
`I-VSA-IDENTITIES` tests first — in particular Test 0 (register laziness: does
this thing have a natural id? then use the id) and Test 1 (bundle size). R2IL
facts have natural ids (`FactId`, `InstId`, `ValueId`), so Test 0 short-circuits
and the answer is almost certainly "not a VSA workload".

## 4. Additive export — the mechanics

When you extend the export (and you will), obey these five:

1. **Version the header, never the reader's assumptions.** Every row artifact
   carries `#version N` and a `#schema` line naming its columns. Bump `N` when
   you ADD; a reader that keys off `#schema` names needs no change at all.
2. **Append columns; never reorder or remove.** A column that becomes
   meaningless gets an explicit empty value, not deletion — the same
   RESERVE-DON'T-RECLAIM rule the node key follows.
3. **New enum variants are expected, not exceptional.** `ResidualReason` grows
   as the furnace learns; a consumer must render an unknown reason as its raw
   string and carry on. Never `match` exhaustively across a process boundary.
4. **Never widen a field to fit one outlier.** That is what route-local
   overflow is for (`mint_factored`'s base-255 cascade; `MAX_SIBLINGS_PER_TIER
   = 255` is a design smell, *not* a storage ceiling). One 900-way phi does not
   get to change the ABI for everyone.
5. **A new pass gets a NEW release tag.** Harvest assets are immutable
   evidence. Re-running and overwriting `r2il-harvest-pass1` in place destroys
   the ability to diff pass N against pass N+1 — which is the whole point of
   keeping the failing B2 run in history.

## 5. What "without breaking existing" means in practice

- **Generated code lands beside hand code, never inside it.** A `// @generated`
  header and its own module. If a generated item must be specialised, the
  specialisation lives in hand code that *calls* it.
- **The SPO projection stays optional and lossy.** `ModelGraph → expand() →
  Vec<Triple>` is a projection of *semantic* facts (calls, reads, writes). It is
  not the behavioral roundtrip oracle and must never become the source you
  reconstruct R2IL from.
- **Conservation is your regression test.** If a staged consumer starts
  dropping facts, `harvested == classified + residual` with `dropped == 0` is
  the invariant that catches it. Assert it in your own pipeline too, not just
  in ours.
- **When the arm can't explain something, that is data.** Do not paper a gap
  with a catch-all so your codegen compiles. A named residual with an address
  is worth more than a generated stub that silently means nothing.

## 6. Where things live

- **Canonical bulk evidence:** GitHub Release `r2il-harvest-pass1`
  (`r2il-pass1.ore.tsv.gz`, `r2il-convention.toml.gz`).
- **Scratch mirror + config backup:** `s3://$AWS_S3_BUCKET_NAME/r2il-arc/`
  (`harvest/` and `config-backup/<repo>/`), and this guide at
  `r2il-arc/harvest/STAGED-CODEGEN-GUIDE.md`. The MedCare-rs backup is the one
  thing NOT under `r2il-arc/` — it belongs to that repo and lives at
  `MedCare-rs/harvest/2026-08-18/`, datestamped like the `bakes/` siblings
  beside it. Credentials from `AWS_*` env only — the bucket is **shared** with
  other work (`q2`, `MedCare-rs`, `OSM`, `ontologies`), so stay inside your own
  prefix and never write at the root.
- **In-tree:** `.claude/harvest/r2il/` keeps the small readable artifacts;
  the plan and impl spec are in `.claude/plans/`.
