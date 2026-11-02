# Golden rows files

`cg_lib.rows.bin` and `cg_lib.rows.tsv` are the store's output for `tests/data/cg_lib.llbc`, imported with `Store::import_for_golden` (no Charon commit, rustc commit or source revision in the provenance record, so the bytes do not depend on the machine). `tests/store_bin_store.rs` writes them when run with `CHARON_BLESS=1` and otherwise requires a fresh import to match them byte for byte. lance-graph's codegraph tests copy these files (PR B) to check their reader against this writer. They change only with a deliberate re-bless after a deliberate format or fixture change.
