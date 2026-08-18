# R2IL pass-1 harvest — the intake arm's artifact set

Produced by `crates/ruff_r2il/examples/harvest_r2il.rs`. **These artifacts are
evidence, never a re-ingest path — nothing in ruff parses them back.**

## Why two of them are gzipped

Following the MedCare-rs precedent (`AdaWorldAPI/MedCare-rs`
`.claude/harvest/README.md`): *"Committed gzipped under `.claude/` … If it ever
grows past a few MB, move it to a GitHub Release asset and keep only this
provenance file in-tree."*

Uncompressed, the ore file alone is **3.4 MB / 10,729 rows** and the convention
tree **140 KB**; together they were 75 % of this branch's diff (29,682 of 39,652
insertions), which makes the PR unreviewable and bloats the zipball for every
future clone. Gzipped they are 204 KB and 8 KB — a **17×** and **17×**
reduction.

| file | state | what it is |
|---|---|---|
| `r2il-pass1.ore.tsv.gz` | gzipped | one row per melted `FlatFact`, in `smelt` order, `at` as 32 hex chars |
| `r2il-convention.toml.gz` | gzipped | `R2ilConvention::to_toml()` — the bootstrapped drill tree, every row `unmeasured` |
| `r2il-pass1-slag.tsv` | plain | the addressed residual ledger, grouped + by-address. Kept readable: it is the artifact a reviewer actually reads |
| `r2il-pass1-census.md` | plain | per-fact-kind and per-opcode counts |
| `PROVENANCE.md` | plain | corpus manifest (FNV-1a 64 per file), r2sleigh commit pin, caps, exact invocation |
| `TRIAGE-RESULT.md` | plain | the pre-registered bars B1/B2/B3, stated **before** the measured section |

## Regenerate

```sh
cargo run --manifest-path crates/ruff_r2il/Cargo.toml --features lift \
    --example harvest_r2il

# gzip AFTER the run (the example writes plain files; `gzip -9` compresses in
# place and removes the source, so compressing first would leave the next run
# writing an uncompressed sibling next to a stale archive).
gzip -9 .claude/harvest/r2il/r2il-pass1.ore.tsv \
        .claude/harvest/r2il/r2il-convention.toml
```

To read a gzipped artifact without unpacking it: `zcat`, `zless`, or
`gzip -dc <file> | head`.

## If these grow again

The next escalation is the one MedCare-rs already names: move the ore file to a
GitHub Release asset and keep only `PROVENANCE.md` (which pins the FNV-1a of
every corpus input) in-tree. The trigger is the ore file exceeding a few MB
*compressed* — at 204 KB it is nowhere near that yet.
