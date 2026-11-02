//! The binary rows file wired into the store: it agrees with the TSV, carries
//! the function declaration and the provenance, and its bytes are stable.
#![expect(
    clippy::disallowed_methods,
    reason = "tests write and read their own temp files; there is no ty System here"
)]

use std::path::{Path, PathBuf};

use ruff_charon::store::{SourceProv, Store};
use ruff_charon::store_bin;
use ruff_charon::{Llbc, fun_facts, load};

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn llbc(name: &str) -> Llbc {
    load(&data(name)).expect("llbc")
}

/// A fresh directory per test.
fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ruff_charon_bin_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// Every fixture with the crate name its rows are harvested under.
const FIXTURES: &[(&str, &str)] = &[
    ("cg_lib.llbc", "cg_lib"),
    ("cg_app.llbc", "cg_app"),
    ("cg_lib_rev2.llbc", "cg_lib"),
    ("cg_app_rev2.llbc", "cg_app"),
    ("fnref.llbc", "fnref"),
    ("trprobe.llbc", "trprobe"),
];

/// Red when `rows.bin` and `rows.tsv` disagree on any row of any fixture, or
/// when the fixtures stop carrying the shapes that make the comparison mean
/// something (dispatch rows, rows without a site, predicates beyond calls).
#[test]
fn bin_and_tsv_agree_row_for_row() {
    let mut all = Vec::new();
    for (file, krate) in FIXTURES {
        let store = Store::new(temp(&format!("agree_{file}")));
        store.import(&llbc(file), krate).expect("import");
        let bin = store.rows(krate).expect("bin rows");
        let tsv = store.rows_tsv(krate).expect("tsv rows");
        assert!(!bin.is_empty(), "{file}: no rows");
        assert_eq!(bin, tsv, "{file}: bin and tsv differ");
        all.extend(bin);
    }
    assert!(all.iter().any(|r| r.predicate == "MAY_DISPATCH"));
    assert!(all.iter().any(|r| r.site.is_none()));
    assert!(all.iter().any(|r| r.site.is_some()));
    assert!(
        all.iter()
            .any(|r| !matches!(r.predicate.as_str(), "DEFINES" | "CALLS_RESOLVED")),
        "no predicate beyond DEFINES and CALLS_RESOLVED in any fixture"
    );
}

/// Red when `with_tsv_export(false)` still writes a TSV, or when `rows()` then
/// needs one.
#[test]
fn tsv_export_off_writes_no_tsv() {
    let dir = temp("tsv_off");
    let store = Store::new(&dir).with_tsv_export(false);
    store
        .import(&llbc("cg_lib.llbc"), "cg_lib")
        .expect("import");
    let tsv: Vec<_> = std::fs::read_dir(&dir)
        .expect("dir")
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".rows.tsv"))
        .collect();
    assert!(tsv.is_empty(), "a TSV was written: {tsv:?}");
    assert!(dir.join("cg_lib.rows.bin").exists());
    assert!(!store.rows("cg_lib").expect("rows").is_empty());
}

/// Red if turning the export off leaves an older `rows.tsv` behind: it would
/// describe the previous revision while `rows.bin` describes the new one.
#[test]
fn turning_the_export_off_removes_a_stale_tsv() {
    let dir = temp("tsv_stale");
    Store::new(&dir)
        .import(&llbc("cg_lib.llbc"), "cg_lib")
        .expect("import rev1");
    assert!(dir.join("cg_lib.rows.tsv").exists(), "rev1 wrote a TSV");
    let store = Store::new(&dir).with_tsv_export(false);
    store
        .import(&llbc("cg_lib_rev2.llbc"), "cg_lib")
        .expect("import rev2");
    assert!(
        !dir.join("cg_lib.rows.tsv").exists(),
        "the rev1 TSV survived a rev2 import with export off"
    );
    let rev2 = store.rows("cg_lib").expect("rows");
    let rev1 = Store::new(temp("tsv_stale_ref"));
    rev1.import(&llbc("cg_lib.llbc"), "cg_lib").expect("ref");
    assert_ne!(
        rev2,
        rev1.rows("cg_lib").expect("ref rows"),
        "the two revisions must differ, or this test proves nothing"
    );
}

/// Red when a function row loses its `FunDeclId`, the CRATE row gains one, or
/// a DEFINES row names a different function than the one it was harvested from.
#[test]
fn every_function_row_carries_its_fundecl() {
    let dir = temp("fundecl");
    let l = llbc("cg_lib.llbc");
    Store::new(&dir).import(&l, "cg_lib").expect("import");
    let decoded =
        store_bin::read(&std::fs::read(dir.join("cg_lib.rows.bin")).expect("bin")).expect("read");
    let facts = fun_facts(&l.krate, "cg_lib");
    let mut defines = 0;
    for r in &decoded.rows {
        if r.predicate == "CRATE" {
            assert_eq!(r.fun, None, "the CRATE row has no function");
        } else {
            assert!(r.fun.is_some(), "{r:?} has no FunDeclId");
        }
        if r.predicate == "DEFINES" {
            defines += 1;
            let f = facts
                .iter()
                .find(|f| f.qualified == r.subject)
                .expect("a function for the subject");
            assert_eq!(r.fun, u32::try_from(f.id.index()).ok(), "{r:?}");
        }
    }
    assert!(defines > 0 && defines == facts.len());
    assert_eq!(
        decoded
            .rows
            .iter()
            .filter(|r| r.predicate == "CRATE")
            .count(),
        1
    );
}

fn header(path: &Path) -> u64 {
    let bytes = std::fs::read(path).expect("bin");
    let h = bytes
        .first_chunk::<{ store_bin::HEADER_LEN }>()
        .expect("header");
    store_bin::header_digest(h).expect("magic and major")
}

/// Red when the provenance record is missing a field, takes a wrong value, or
/// when a change of `source_revision` alone does not change the file.
#[test]
fn provenance_reaches_the_file() {
    let l = llbc("cg_lib.llbc");
    let (d1, d2) = (temp("prov_a"), temp("prov_b"));
    Store::new(&d1).import(&l, "cg_lib").expect("import");
    let prov = Store::new(&d1).provenance("cg_lib").expect("prov");
    assert_eq!(prov.len(), 1);
    let p = &prov[0];
    assert_eq!(p.charon_version.as_deref(), Some(l.charon_version.as_str()));
    assert_eq!(
        p.llbc_schema_version.as_deref(),
        Some(l.charon_version.as_str())
    );
    assert_eq!(
        p.llbc_digest.as_deref(),
        Some(format!("{:016x}", l.llbc_digest).as_str())
    );
    assert_eq!((&p.rustc_commit, &p.source_revision), (&None, &None));
    assert!(p.target_config.is_some());

    let src = SourceProv {
        rustc_commit: Some("abc123".into()),
        source_revision: Some("rev-7".into()),
    };
    Store::new(&d2)
        .import_with(&l, "cg_lib", &src)
        .expect("import");
    let p2 = &Store::new(&d2).provenance("cg_lib").expect("prov")[0];
    assert_eq!(p2.rustc_commit.as_deref(), Some("abc123"));
    assert_eq!(p2.source_revision.as_deref(), Some("rev-7"));
    assert_ne!(
        header(&d1.join("cg_lib.rows.bin")),
        header(&d2.join("cg_lib.rows.bin")),
        "provenance must be covered by the file digest"
    );

    let d3 = temp("prov_c");
    let only_rev = SourceProv {
        rustc_commit: Some("abc123".into()),
        source_revision: Some("rev-8".into()),
    };
    Store::new(&d3)
        .import_with(&l, "cg_lib", &only_rev)
        .expect("import");
    assert_ne!(
        header(&d2.join("cg_lib.rows.bin")),
        header(&d3.join("cg_lib.rows.bin")),
        "a different source_revision alone must change the digest"
    );
}

/// Red when the bytes written for `cg_lib` differ from the blessed golden
/// files. `CHARON_BLESS=1` rewrites them.
#[test]
fn golden_bytes_are_stable() {
    let dir = temp("golden");
    Store::new(&dir)
        .import_for_golden(&llbc("cg_lib.llbc"), "cg_lib")
        .expect("import");
    let golden = data("golden");
    let files = ["cg_lib.rows.bin", "cg_lib.rows.tsv"];
    if std::env::var_os("CHARON_BLESS").is_some_and(|v| v == "1") {
        std::fs::create_dir_all(&golden).expect("golden dir");
        for f in files {
            std::fs::copy(dir.join(f), golden.join(f)).expect("bless");
        }
        return;
    }
    for f in files {
        let want = std::fs::read(golden.join(f)).unwrap_or_else(|_| {
            panic!("golden file {f} is missing: run with CHARON_BLESS=1 to create the golden files")
        });
        let got = std::fs::read(dir.join(f)).expect("fresh file");
        assert!(got == want, "{f} differs from the golden file");
    }
}
