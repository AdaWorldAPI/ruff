//! Rows v2 on the codegraph fixture (lance-graph
//! `crates/sag-ra-probe/fixtures/codegraph/rev1`): `PUBLIC`, `IMPLEMENTS`,
//! `SYNTHETIC`, `REFERENCES`, and the method-qualified `MAY_DISPATCH` object.

use std::path::{Path, PathBuf};

use ruff_charon::{Relation, kythe_entries, load, relations};

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn lib_rows() -> (Vec<Relation>, ruff_charon::Receipt) {
    let llbc = load(&data("cg_lib.llbc")).expect("llbc");
    let (rows, _, receipt) = relations(&llbc.krate, "cg_lib");
    (rows, receipt)
}

fn count(rows: &[Relation], predicate: &str) -> usize {
    rows.iter().filter(|r| r.predicate == predicate).count()
}

/// Fails when a `pub fn` is not marked public, or a private item is. The
/// fixture has seven `pub fn`s and the private `private_unused`; Charon also
/// marks the two `Shape::area` impl methods public (a trait impl's methods
/// take the trait's visibility), and this transcribes Charon's flag as is.
#[test]
fn public_functions_are_marked() {
    let (rows, _) = lib_rows();
    let mut public: Vec<_> = rows
        .iter()
        .filter(|r| r.predicate == "PUBLIC")
        .map(|r| r.subject.as_str())
        .collect();
    public.sort_unstable();
    assert_eq!(
        public,
        [
            "cg_lib::area_dyn",
            "cg_lib::area_of",
            "cg_lib::macro_use::checked",
            "cg_lib::shapes::impl_Shape_for_Rect::area",
            "cg_lib::shapes::impl_Shape_for_Square::area",
            "cg_lib::shapes::{Square}::side",
            "cg_lib::util::double",
            "cg_lib::util::triple",
            "cg_lib::util::unit",
        ]
    );
}

/// Fails when a function used only as a function pointer has no reference
/// row (it would read as dead), or when a call through a pointer argument is
/// named instead of left unresolved.
#[test]
fn a_function_pointer_use_is_a_reference() {
    let llbc = load(&data("fnref.llbc")).expect("llbc");
    let (rows, residual, receipt) = relations(&llbc.krate, "fnref");
    assert!(receipt.conserved(), "{receipt:?}");
    assert!(residual.is_empty(), "{residual:?}");
    let refs: Vec<_> = rows
        .iter()
        .filter(|r| r.predicate == "REFERENCES")
        .map(|r| (r.subject.as_str(), r.object.as_str()))
        .collect();
    assert_eq!(refs, [("fnref::take", "fnref::helper")]);
    let unresolved: Vec<_> = rows
        .iter()
        .filter(|r| r.predicate == "UNRESOLVED_CALL")
        .map(|r| r.subject.as_str())
        .collect();
    assert_eq!(unresolved, ["fnref::apply"]);
}

/// Fails when a trait-impl method does not name the trait method it
/// implements, or when the trait's method name is dropped.
#[test]
fn impl_methods_name_the_trait_method() {
    let (rows, _) = lib_rows();
    let implements: Vec<_> = rows
        .iter()
        .filter(|r| r.predicate == "IMPLEMENTS")
        .collect();
    assert_eq!(implements.len(), 2, "{implements:#?}");
    assert!(
        implements
            .iter()
            .all(|r| r.object == "cg_lib::shapes::Shape::area")
    );
}

/// Fails when the compiler-made `{promoted_const}` item of `checked` is not
/// flagged, or when ordinary identifier and impl paths are.
#[test]
fn synthetic_items_are_flagged() {
    let (rows, _) = lib_rows();
    assert_eq!(count(&rows, "SYNTHETIC"), 1, "{rows:#?}");
}

/// Fails when the dispatch object loses the method name, or when the Kythe
/// target stops being the plain trait name.
#[test]
fn may_dispatch_names_the_method_and_keeps_the_kythe_target() {
    let (rows, _) = lib_rows();
    let dispatch: Vec<_> = rows
        .iter()
        .filter(|r| r.predicate == "MAY_DISPATCH")
        .collect();
    assert_eq!(dispatch.len(), 1, "{dispatch:#?}");
    assert_eq!(dispatch[0].object, "cg_lib::shapes::Shape::area via Clause");
    assert_eq!(
        dispatch[0].kythe_target.as_deref(),
        Some("cg_lib::shapes::Shape")
    );
    assert!(
        rows.iter()
            .filter(|r| r.predicate != "MAY_DISPATCH")
            .all(|r| r.kythe_target.is_none())
    );
}

/// Fails when a called function is reported as merely referenced: the
/// fixture takes no function as a value.
#[test]
fn called_functions_are_not_references() {
    let (rows, receipt) = lib_rows();
    assert_eq!(count(&rows, "REFERENCES"), 0, "{rows:#?}");
    assert!(receipt.conserved(), "{receipt:?}");
}

/// Fails when the v2 predicates change a single Kythe entry.
#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "a test reading its own data file; the ty `System` abstraction does not apply here"
)]
fn kythe_output_is_unchanged_by_v2() {
    for (file, krate) in [("cg_lib", "cg_lib"), ("cg_app", "cg_app")] {
        let llbc = load(&data(&format!("{file}.llbc"))).expect("llbc");
        let want = std::fs::read_to_string(data(&format!("{file}.kythe.golden"))).expect("golden");
        // The `kythe` command's defaults: corpus `lance-graph`, nothing stripped,
        // one entry per line.
        let mut got = kythe_entries(&llbc.krate, krate, "lance-graph", "").join("\n");
        got.push('\n');
        assert_eq!(got, want, "{file}");
    }
}
