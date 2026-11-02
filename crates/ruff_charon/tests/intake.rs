//! The intake checked against the codegraph fixture (lance-graph
//! `crates/sag-ra-probe/fixtures/codegraph/rev1`). The LLBC files and the
//! `llbc_calls.py` reference outputs are in `tests/data`; see its README.

use std::path::{Path, PathBuf};

use ruff_charon::{
    Relation, calls_text, kythe_entries, load, path_name, qualified_name, relations,
};

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn rows(file: &str, krate: &str) -> Vec<Relation> {
    let llbc = load(&data(file)).expect("llbc");
    relations(&llbc.krate, krate).0
}

fn find<'a>(rows: &'a [Relation], predicate: &str, subject: &str) -> Vec<&'a Relation> {
    rows.iter()
        .filter(|r| r.predicate == predicate && r.subject == subject)
        .collect()
}

/// The Rust reader reproduces the reference script byte for byte.
#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "a test reading its own data file; the ty `System` abstraction does not apply here"
)]
fn calls_report_equals_the_reference_script() {
    for (file, krate) in [("cg_lib.llbc", "cg_lib"), ("cg_app.llbc", "cg_app")] {
        let llbc = load(&data(file)).expect("llbc");
        let want = std::fs::read_to_string(data(&format!("{file}_calls.txt"))).expect("reference");
        assert_eq!(calls_text(&llbc, krate), want, "{file}");
    }
}

/// A call through a generic bound is a possible dispatch, a call through
/// `dyn` is unresolved, and neither is a confirmed caller of an impl.
#[test]
fn dispatch_is_never_a_confirmed_call() {
    let rows = rows("cg_lib.llbc", "cg_lib");
    let bound = find(&rows, "MAY_DISPATCH", "cg_lib::area_of");
    assert_eq!(bound.len(), 1, "{rows:#?}");
    assert_eq!(bound[0].object, "cg_lib::shapes::Shape via Clause");
    assert!(find(&rows, "CALLS_RESOLVED", "cg_lib::area_of").is_empty());
    let dynamic = find(&rows, "UNRESOLVED_CALL", "cg_lib::area_dyn");
    assert_eq!(dynamic.len(), 1);
    assert!(find(&rows, "CALLS_RESOLVED", "cg_lib::area_dyn").is_empty());
}

/// The call inside `assert_eq!`, through the alias `t3`, is witnessed with
/// the exact span of `t3(1)` (rust-analyzer's call hierarchy misses it).
#[test]
fn the_call_inside_a_macro_is_witnessed() {
    let rows = rows("cg_lib.llbc", "cg_lib");
    let hit: Vec<_> = find(&rows, "CALLS_RESOLVED", "cg_lib::macro_use::checked")
        .into_iter()
        .filter(|r| r.object == "cg_lib::util::triple")
        .collect();
    assert_eq!(hit.len(), 1);
    let s = hit[0].site.as_ref().expect("site");
    assert_eq!(
        (s.file.as_str(), s.start, s.end),
        ("cg_lib/src/macro_use.rs", 73, 78)
    );
}

/// The two `Shape::area` impls print the same as paths but are distinct
/// items, and a cross-crate caller names the one it calls.
#[test]
fn impl_methods_keep_distinct_identities_across_crates() {
    let llbc = load(&data("cg_lib.llbc")).expect("llbc");
    let areas: Vec<_> = llbc
        .krate
        .fun_decls
        .iter()
        .filter(|f| path_name(&f.item_meta.name) == "cg_lib::shapes::Impl::area")
        .map(|f| qualified_name(&llbc.krate, &f.item_meta.name))
        .collect();
    assert_eq!(areas.len(), 2);
    assert_ne!(areas[0], areas[1]);
    let app = rows("cg_app.llbc", "cg_app");
    let objects: Vec<_> = find(&app, "CALLS_RESOLVED", "cg_app::run")
        .iter()
        .map(|r| r.object.clone())
        .collect();
    assert!(
        objects.contains(&"cg_lib::shapes::impl_Shape_for_Square::area".to_owned()),
        "{objects:?}"
    );
    assert!(!objects.iter().any(|o| o.contains("Rect")), "{objects:?}");
}

/// Every function and every call is represented or counted as residual.
#[test]
fn receipt_conserves() {
    let llbc = load(&data("cg_lib.llbc")).expect("llbc");
    let (rows, residual, receipt) = relations(&llbc.krate, "cg_lib");
    assert!(receipt.conserved(), "{receipt:?}");
    assert_eq!(receipt.represented, rows.len());
    assert_eq!(receipt.residual, residual.len());
    // 11 local functions, 5 call statements (llbc_calls.py's count).
    assert_eq!(receipt.harvested, 16);
}

/// The Kythe entries carry the macro call as a `ref/call` anchor and the
/// file text Kythe needs to serve it.
#[test]
fn kythe_entries_carry_the_call_and_its_file() {
    let llbc = load(&data("cg_lib.llbc")).expect("llbc");
    let entries = kythe_entries(&llbc.krate, "cg_lib", "lance-graph", "");
    let call = entries.iter().find(|e| {
        e.contains("\"edge_kind\":\"/kythe/edge/ref/call\"")
            && e.contains("\"signature\":\"@73:78\"")
            && e.contains("\"signature\":\"cg_lib::util::triple\"")
    });
    assert!(call.is_some(), "no ref/call anchor for t3(1)");
    assert!(
        entries
            .iter()
            .any(|e| e.contains("\"fact_name\":\"/kythe/text\"")
                && e.contains("\"path\":\"cg_lib/src/macro_use.rs\""))
    );
}
