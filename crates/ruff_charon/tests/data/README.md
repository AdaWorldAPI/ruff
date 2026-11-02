# ruff_charon test data

- `cg_lib.llbc`, `cg_app.llbc`: Charon LLBC of the two crates of lance-graph
  `crates/sag-ra-probe/fixtures/codegraph/rev1`, produced with the AdaWorldAPI Charon fork
  (`c69e231`, toolchain `nightly-2026-09-17`, built `--no-default-features --features rustc`):

      cd rev1/cg_lib && charon cargo --no-dedup-serialized-ast --dest-file cg_lib.llbc
      cd rev1/cg_app && charon cargo --no-dedup-serialized-ast --dest-file cg_app.llbc

- `*.llbc_calls.txt`: the reference output, written by lance-graph
  `crates/sag-ra-probe/tools/llbc_calls.py <file> <crate>`, never by this crate.
