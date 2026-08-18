# R2IL pass-1 harvest — the intake arm's artifact set

Produced by `crates/ruff_r2il/examples/harvest_r2il.rs`. **These artifacts are
evidence, never a re-ingest path — nothing in ruff parses them back.**

## Where the bulk artifacts live (NOT in git)

Following the escalation MedCare-rs's `.claude/harvest/README.md` already
names — *"If it ever grows past a few MB, move it to a GitHub Release asset and
keep only this provenance file in-tree"* — the two bulk files are out of the
tree entirely. Uncompressed they were **3.4 MB / 10,729 rows**, which made
generated data **75 % of this branch's diff** (29,682 of 39,652 insertions).

**Canonical:** GitHub Release
[`r2il-harvest-pass1`](https://github.com/AdaWorldAPI/ruff/releases/tag/r2il-harvest-pass1)

```sh
curl -sL -o r2il-pass1.ore.tsv.gz \
  https://github.com/AdaWorldAPI/ruff/releases/download/r2il-harvest-pass1/r2il-pass1.ore.tsv.gz
curl -sL -o r2il-convention.toml.gz \
  https://github.com/AdaWorldAPI/ruff/releases/download/r2il-harvest-pass1/r2il-convention.toml.gz
zcat r2il-pass1.ore.tsv.gz | head        # read without unpacking
```

**Scratch mirror:** S3 (Tigris), `s3://$AWS_S3_BUCKET_NAME/ruff-r2il/harvest/pass1/`
— the full set including the small files, for cross-session scratch. Read via
`AWS_ENDPOINT_URL` / `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` from the
environment; never hardcode an endpoint or a key. The Release is authoritative;
S3 is a working mirror that may be pruned.

Both are gitignored, so a regenerate run leaves the tree clean.

## What IS in git

| file | why it stays |
|---|---|
| `TRIAGE-RESULT.md` | the pre-registered bars B1/B2/B3, stated **before** the measured section — the point of the whole run |
| `r2il-pass1-slag.tsv` | the addressed residual ledger; the artifact a reviewer actually reads |
| `r2il-pass1-census.md` | per-fact-kind and per-opcode counts |
| `PROVENANCE.md` | corpus manifest (FNV-1a 64 per input), r2sleigh commit pin, caps, exact invocation |

Together under 32 KB. The Release assets are reproducible from these plus the
pinned corpus; these are not reproducible from the Release.

## Regenerate

```sh
cargo run --manifest-path crates/ruff_r2il/Cargo.toml --features lift \
    --example harvest_r2il

# gzip AFTER the run: `gzip -9` compresses in place and removes its source, so
# compressing first would leave the next run writing an uncompressed sibling
# beside a stale archive.
gzip -9 .claude/harvest/r2il/r2il-pass1.ore.tsv \
        .claude/harvest/r2il/r2il-convention.toml
```

Then re-upload to the Release (new tag per pass — assets are immutable
evidence, never overwritten in place) and to the S3 prefix.

## Measured, this pass

143 functions across 4 x86-64 binaries (2 with symtab; 2 stripped and skipped
with a printed note). Conservation `54304 / 17557 / 36747 / 0`.
**B1 PASS · B2 91.30 % INVESTIGATE · B3 PASS.** The entire remaining B2 gap is
one named reason — `memory_object_escaped`, 1670 rows — which is legitimate
slag, not a defect. See `TRIAGE-RESULT.md`.
