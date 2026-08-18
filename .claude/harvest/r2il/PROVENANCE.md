# R2IL pass-1 harvest — PROVENANCE

Hashes below are **FNV-1a 64** (not a cryptographic hash) over the raw file bytes, computed inline in this example — no hashing dependency.

## Corpus

| path | bytes | fnv1a64 | status |
|---|---|---|---|
| /home/user/ruff/crates/ruff_r2il/../../../r2sleigh/tests/e2e/stress_test | 52128 | d60c9fe34de17594 | harvested (71 functions) |
| /home/user/ruff/crates/ruff_r2il/../../../r2sleigh/tests/e2e/stress_test_opt | 83880 | 40d985a73341e4c8 | harvested (72 functions) |
| /bin/ls | 142312 | c3453ae463c7fa3c | skipped (no symtab) |
| /usr/bin/env | 48072 | 7c3bd066635cc085 | skipped (no symtab) |

## Environment

- `r2sleigh` commit: `60942f6`
- Architecture: `x86-64`
- `sleigh-config` = "1.0", feature `x86` (exact resolved patch pinned by the committed `Cargo.lock`)
- Convention: `R2ilConvention::from_arch(&spec, [Copy, IntAdd, Load, Store, CBranch, Call, Return])` — one convention, built once, reused for every harvested function
- Caps in force: `R2IL_HARVEST_MAX_FUNCS=200`, `R2IL_HARVEST_MAX_SECTION_BYTES=262144`

## Invocation

```sh
cargo run --manifest-path crates/ruff_r2il/Cargo.toml --features lift --example harvest_r2il
```
