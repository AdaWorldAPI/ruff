//! The offline store on the codegraph fixture: rev1 → rev2 deletes `triple`
//! and `area_dyn` from `cg_lib` and drops the `area_dyn` call from `cg_app`.
#![expect(
    clippy::disallowed_methods,
    reason = "tests write and read their own temp files; there is no ty System here"
)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ruff_charon::store::{Store, crate_files, with_dependents};
use ruff_charon::{Llbc, load};

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
    let dir = std::env::temp_dir().join(format!("ruff_charon_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn store_of(name: &str, llbcs: &[(&str, &str)]) -> Store {
    let store = Store::new(temp(name));
    for (file, krate) in llbcs {
        store.import(&llbc(file), krate).expect("import");
    }
    store
}

const REV1: &[(&str, &str)] = &[("cg_lib.llbc", "cg_lib"), ("cg_app.llbc", "cg_app")];
const REV2: &[(&str, &str)] = &[
    ("cg_lib_rev2.llbc", "cg_lib"),
    ("cg_app_rev2.llbc", "cg_app"),
];

/// Refreshing every changed crate gives exactly the rows of a clean import.
#[test]
fn incremental_refresh_equals_a_clean_import() {
    let inc = store_of("inc", REV1);
    let before = inc.all_rows().expect("rows");
    for (file, krate) in REV2 {
        inc.import(&llbc(file), krate).expect("refresh");
    }
    let clean = store_of("clean", REV2);
    let after = inc.all_rows().expect("rows");
    assert_ne!(
        before, after,
        "rev1 and rev2 must differ, or this compares nothing"
    );
    assert_eq!(after, clean.all_rows().expect("rows"));
}

/// Deleted functions leave no definition and no call row behind.
#[test]
fn deleted_functions_leave_nothing() {
    let inc = store_of("deleted", REV1);
    let names = |s: &Store| -> BTreeSet<String> {
        s.all_rows()
            .expect("rows")
            .into_iter()
            .flat_map(|r| [r.subject, r.object])
            .collect()
    };
    for gone in ["cg_lib::util::triple", "cg_lib::area_dyn"] {
        assert!(names(&inc).contains(gone), "{gone} must be in rev1");
    }
    for (file, krate) in REV2 {
        inc.import(&llbc(file), krate).expect("refresh");
    }
    for gone in ["cg_lib::util::triple", "cg_lib::area_dyn"] {
        assert!(!names(&inc).contains(gone), "{gone} survived the refresh");
    }
    assert!(inc.dangling().expect("dangling").is_empty());
}

/// Skipping a dependent crate leaves exactly the edge to the deleted function;
/// the dependency rule names the dependent.
#[test]
fn skipping_a_dependent_leaves_a_dangling_edge() {
    let inc = store_of("dangling", REV1);
    assert!(
        inc.dangling().expect("dangling").is_empty(),
        "rev1 is consistent"
    );
    inc.import(&llbc("cg_lib_rev2.llbc"), "cg_lib")
        .expect("refresh");
    let edges: Vec<_> = inc
        .dangling()
        .expect("dangling")
        .into_iter()
        .map(|r| (r.subject, r.object))
        .collect();
    assert_eq!(
        edges,
        [("cg_app::run".to_owned(), "cg_lib::area_dyn".to_owned())]
    );
    let deps = [("cg_app".to_owned(), "cg_lib".to_owned())];
    let todo = with_dependents(&BTreeSet::from(["cg_lib".to_owned()]), &deps);
    assert_eq!(
        todo,
        BTreeSet::from(["cg_app".to_owned(), "cg_lib".to_owned()])
    );
    assert_eq!(
        with_dependents(&BTreeSet::from(["cg_app".to_owned()]), &deps).len(),
        1
    );
}

/// Write the files Charon read (their contents ride in the LLBC) under `root`.
fn materialize(root: &Path, llbcs: &[(&str, &str)]) {
    for (file, krate) in llbcs {
        for f in llbc(file)
            .krate
            .files
            .iter()
            .filter(|f| f.crate_name == *krate)
        {
            if let (charon_lib::ast::FileName::Local(p), Some(text)) = (&f.name, &f.contents) {
                let path = root.join(p);
                std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
                std::fs::write(path, text).expect("write");
            }
        }
    }
}

/// A crate is stale exactly when one of its recorded files changed or vanished.
#[test]
fn stale_names_the_crates_whose_files_changed() {
    let store = store_of("stale", REV1);
    let root = temp("stale_root");
    materialize(&root, REV1);
    assert!(store.stale(&root).expect("stale").is_empty());
    // One cg_lib file edited by one byte.
    let util = root.join("cg_lib/src/util.rs");
    let mut text = std::fs::read(&util).expect("util");
    text.push(b'\n');
    std::fs::write(&util, &text).expect("write");
    assert_eq!(
        store.stale(&root).expect("stale"),
        BTreeSet::from(["cg_lib".to_owned()])
    );
    // The whole rev2 tree: both crates' files changed.
    materialize(&root, REV2);
    assert_eq!(
        store.stale(&root).expect("stale"),
        BTreeSet::from(["cg_app".to_owned(), "cg_lib".to_owned()])
    );
    // A recorded file that vanished.
    let fresh = temp("stale_root_missing");
    materialize(&fresh, REV1);
    std::fs::remove_file(fresh.join("cg_app/src/lib.rs")).expect("rm");
    assert_eq!(
        store.stale(&fresh).expect("stale"),
        BTreeSet::from(["cg_app".to_owned()])
    );
}

/// The digests cover every source file of the crate and nothing of another crate.
#[test]
fn files_are_the_crates_own() {
    let lib = crate_files(&llbc("cg_lib.llbc"), "cg_lib");
    let paths: Vec<_> = lib.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        paths,
        [
            "cg_lib/src/lib.rs",
            "cg_lib/src/macro_use.rs",
            "cg_lib/src/shapes.rs",
            "cg_lib/src/util.rs"
        ]
    );
    assert!(
        crate_files(&llbc("cg_app.llbc"), "cg_app")
            .iter()
            .all(|(p, _)| p.starts_with("cg_app/"))
    );
}

/// Readers refuse an artifact newer than they know, or one missing a column.
#[test]
fn readers_refuse_unknown_versions_and_missing_columns() {
    let store = store_of("reader", &[("cg_lib.llbc", "cg_lib")]);
    let rows = store.rows("cg_lib").expect("rows");
    assert!(!rows.is_empty());
    let path = std::env::temp_dir()
        .join(format!("ruff_charon_reader_{}", std::process::id()))
        .join("cg_lib.rows.tsv");
    let text = std::fs::read_to_string(&path).expect("rows file");
    std::fs::write(&path, text.replacen("#version 1", "#version 2", 1)).expect("write");
    assert!(
        store.rows("cg_lib").is_err(),
        "a newer version must be refused"
    );
    std::fs::write(&path, text.replacen("\tgrade\t", "\tlevel\t", 1)).expect("write");
    assert!(
        store.rows("cg_lib").is_err(),
        "a renamed column must be refused"
    );
    std::fs::write(&path, &text).expect("restore");
    assert_eq!(store.rows("cg_lib").expect("rows"), rows);
}

/// A call resolved through a known trait impl names the trait, not a
/// function, so a consistent store must not report it as dangling, while a
/// call to a function nobody defines still is.
#[test]
fn trait_impl_calls_are_not_dangling() {
    let dir = temp("traitimpl");
    let header = "#version 1\n#schema predicate\tsubject\tobject\tgrade\tfile\tstart\tend\n";
    std::fs::write(
        dir.join("x.rows.tsv"),
        format!(
            "{header}DEFINES\tx::f\tx::f\tCOMPILER\t-\t-\t-\n\
             CALLS_RESOLVED\tx::f\tx::Shape via TraitImpl\tCOMPILER\t-\t-\t-\n"
        ),
    )
    .expect("rows");
    std::fs::write(dir.join("x.files.tsv"), "#version 1\n#schema path\tfnv64\n").expect("files");
    let store = Store::new(&dir);
    assert!(store.dangling().expect("dangling").is_empty());
    // The check is still live for function targets.
    std::fs::write(
        dir.join("x.rows.tsv"),
        format!(
            "{header}DEFINES\tx::f\tx::f\tCOMPILER\t-\t-\t-\n\
             CALLS_RESOLVED\tx::f\tx::Shape via TraitImpl\tCOMPILER\t-\t-\t-\n\
             CALLS_RESOLVED\tx::f\tx::gone\tCOMPILER\t-\t-\t-\n"
        ),
    )
    .expect("rows");
    let objects: Vec<_> = store
        .dangling()
        .expect("dangling")
        .into_iter()
        .map(|r| r.object)
        .collect();
    assert_eq!(objects, ["x::gone"]);
}

/// Concurrent imports of one crate each write through their own temporary
/// file: all succeed, and the store ends with a whole, correct artifact.
#[test]
fn concurrent_imports_of_one_crate_do_not_collide() {
    let dir = temp("concurrent");
    let lib = std::sync::Arc::new(llbc("cg_lib.llbc"));
    let clean = store_of("concurrent_clean", &[("cg_lib.llbc", "cg_lib")]);
    for _round in 0..10 {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let (dir, lib) = (dir.clone(), lib.clone());
                std::thread::spawn(move || Store::new(dir).import(&lib, "cg_lib").map(|_| ()))
            })
            .collect();
        for h in handles {
            h.join().expect("thread").expect("import");
        }
    }
    assert_eq!(
        Store::new(&dir).all_rows().expect("rows"),
        clean.all_rows().expect("rows")
    );
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .expect("dir")
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "temporary files left behind: {leftovers:?}"
    );
}
