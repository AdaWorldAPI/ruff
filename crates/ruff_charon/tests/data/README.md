# ruff_charon test data

- `cg_lib.llbc`, `cg_app.llbc`: Charon LLBC of the two crates of lance-graph `crates/sag-ra-probe/fixtures/codegraph/rev1`.
- `cg_lib_rev2.llbc`, `cg_app_rev2.llbc`: the same for `rev2`, which deletes `triple` and `area_dyn` from `cg_lib` and drops the `area_dyn` call from `cg_app` (the store tests).
- `trprobe.llbc`: Charon LLBC of `trprobe.rs` (a one-file crate with a trait, a default method and one impl), used to build calls through a known impl, which Charon does not emit for any of these sources.
- `*.llbc_calls.txt`: the reference output, written by lance-graph `crates/sag-ra-probe/tools/llbc_calls.py <file> <crate>`, never by this crate.

The LLBC was produced with the AdaWorldAPI Charon fork (`c69e231`, toolchain `nightly-2026-09-17`, built `--no-default-features --features rustc`), from the fixture revision's directory:

```sh
cd cg_lib && charon cargo --no-dedup-serialized-ast --dest-file cg_lib.llbc
cd cg_app && charon cargo --no-dedup-serialized-ast --dest-file cg_app.llbc
```

- `fnref.llbc`: Charon LLBC of `fnref.rs` (a function used only as a function pointer, an unused private function, and a call through a pointer argument), produced with `charon rustc --no-dedup-serialized-ast --dest-file fnref.llbc -- --crate-type=lib fnref.rs`. rustc reports `dead_code` for `unused_private` only.
- `derefw.llbc`: Charon LLBC of `derefw.rs` (writes through argument-rooted and local-rooted dereferences, and a function that assigns only locals), produced the same way as `fnref.llbc`: `charon rustc --no-dedup-serialized-ast --dest-file derefw.llbc -- --crate-type=lib derefw.rs`.
- `cg_lib.kythe.golden`, `cg_app.kythe.golden`: `ruff_charon kythe <file> <crate>` on rows v1 (before the v2 predicates); the v2 Kythe output must equal them.
- `cg_broken` (rev1) produces no LLBC: rustc stops on E0425 and E0432 before Charon translates, so a `CRATE … PARTIAL` row cannot come from a crate that does not compile.
