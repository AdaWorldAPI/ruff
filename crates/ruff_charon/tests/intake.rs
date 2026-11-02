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
    assert_eq!(bound[0].object, "cg_lib::shapes::Shape::area via Clause");
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
    // 11 local functions, 5 call statements (llbc_calls.py's count), and
    // the rows v2 facts: 9 PUBLIC, 2 IMPLEMENTS, 1 SYNTHETIC.
    assert_eq!(receipt.harvested, 28);
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

/// Rewrite every call in `fun` from `FnPtrKind::Fun(from)` into a call through
/// `TraitImpl(impl_id)` of trait `shape`, method `method`. Returns how many
/// calls were rewritten.
fn rewrite_to_trait_impl(
    llbc: &mut ruff_charon::Llbc,
    fun: &str,
    from: charon_lib::ast::FunDeclId,
    impl_id: charon_lib::ast::TraitImplId,
    shape: charon_lib::ast::TraitDeclId,
    method: charon_lib::ast::TraitMethodId,
) -> usize {
    use charon_lib::ast::llbc_ast::{Statement, StatementKind};
    use charon_lib::ast::visitor::AstVisitable;
    use charon_lib::ast::*;
    let id = llbc
        .krate
        .fun_decls
        .iter()
        .find(|f| path_name(&f.item_meta.name) == fun)
        .expect("fun")
        .def_id;
    let decl = llbc.krate.fun_decls.get_mut(id).expect("decl");
    let Body::Structured(body) = &mut decl.body else {
        panic!("{fun} has no structured body")
    };
    let mut n = 0;
    body.body.dyn_visit_mut(|st: &mut Statement| {
        if let StatementKind::Call { call, .. } = &mut st.kind
            && let FnOperand::Regular(ptr) = &mut call.func
            && matches!(ptr.kind.as_ref(), FnPtrKind::Fun(f) if *f == from)
        {
            let tr = TraitRef::new(
                TraitRefKind::TraitImpl(TraitImplRef {
                    id: impl_id,
                    generics: Box::new(GenericArgs::empty()),
                }),
                TraitDeclRef {
                    id: shape,
                    generics: Box::new(GenericArgs::empty()),
                },
            );
            *ptr.kind = FnPtrKind::Trait(tr, method);
            n += 1;
        }
    });
    n
}

/// A call through a known impl names the function it resolves to: the
/// impl's own method, or the trait's default when the impl lacks it. An impl
/// that is not in the LLBC is a residual, never a row pointing at the trait.
#[test]
fn a_call_through_a_known_impl_names_the_resolved_function() {
    let mut llbc = load(&data("trprobe.llbc")).expect("llbc");
    let k = &llbc.krate;
    let fun_id = |p: &str| {
        k.fun_decls
            .iter()
            .find(|f| path_name(&f.item_meta.name) == p)
            .unwrap_or_else(|| panic!("{p}"))
            .def_id
    };
    let (area_impl, twice) = (
        fun_id("trprobe::Impl::area"),
        fun_id("trprobe::Shape::twice"),
    );
    let shape = k
        .trait_decls
        .iter()
        .find(|t| path_name(&t.item_meta.name) == "trprobe::Shape")
        .expect("Shape")
        .def_id;
    let method = |name: &str| {
        k.trait_decls
            .get(shape)
            .expect("decl")
            .methods
            .iter_indexed()
            .find(|(_, m)| m.skip_binder.name.0.as_str() == name)
            .expect("method")
            .0
    };
    let (area, twice_m) = (method("area"), method("twice"));
    let impl_id = k
        .trait_impls
        .iter()
        .find(|i| i.impl_trait.id == shape)
        .expect("impl")
        .def_id;
    let names = |llbc: &ruff_charon::Llbc| {
        (
            qualified_name(
                &llbc.krate,
                &llbc
                    .krate
                    .fun_decls
                    .get(area_impl)
                    .expect("f")
                    .item_meta
                    .name,
            ),
            qualified_name(
                &llbc.krate,
                &llbc.krate.fun_decls.get(twice).expect("f").item_meta.name,
            ),
        )
    };
    let (area_name, twice_name) = names(&llbc);
    assert_eq!(
        rewrite_to_trait_impl(
            &mut llbc,
            "trprobe::method_call",
            area_impl,
            impl_id,
            shape,
            area
        ),
        1
    );
    assert_eq!(
        rewrite_to_trait_impl(
            &mut llbc,
            "trprobe::default_method",
            twice,
            impl_id,
            shape,
            twice_m
        ),
        1
    );
    // An impl id the crate does not have.
    let missing = charon_lib::ast::TraitImplId::new(10_000);
    assert_eq!(
        rewrite_to_trait_impl(
            &mut llbc,
            "trprobe::qualified_call",
            area_impl,
            missing,
            shape,
            area
        ),
        1
    );
    let (rows, residual, receipt) = relations(&llbc.krate, "trprobe");
    let objects = |subject: &str| -> Vec<String> {
        find(&rows, "CALLS_RESOLVED", subject)
            .iter()
            .map(|r| r.object.clone())
            .collect()
    };
    assert_eq!(
        objects("trprobe::method_call"),
        [area_name],
        "the impl's own method"
    );
    assert_eq!(
        objects("trprobe::default_method"),
        std::slice::from_ref(&twice_name),
        "the trait's default method"
    );
    assert!(objects("trprobe::qualified_call").is_empty());
    assert!(
        rows.iter().all(|r| !r.object.contains(" via TraitImpl")),
        "{rows:#?}"
    );
    assert_eq!(
        residual.iter().map(|r| r.reason).collect::<Vec<_>>(),
        ["TRAIT_IMPL_METHOD_NOT_FOUND"]
    );
    assert!(receipt.conserved(), "{receipt:?}");

    // Charon lists an inherited default method in the impl's own table, so
    // the trait-default fallback is only reached when that entry is missing.
    let table = &mut llbc
        .krate
        .trait_impls
        .get_mut(impl_id)
        .expect("impl")
        .methods;
    assert!(
        table.remove(twice_m).is_some(),
        "the impl lists the default method"
    );
    let (rows, _, _) = relations(&llbc.krate, "trprobe");
    let objects: Vec<_> = find(&rows, "CALLS_RESOLVED", "trprobe::default_method")
        .iter()
        .map(|r| r.object.clone())
        .collect();
    assert_eq!(
        objects,
        std::slice::from_ref(&twice_name),
        "falls back to the trait's default method"
    );
}

/// A local function without a structured body is one definition (a DEFINES
/// row) plus one body nobody can read (a residual): the receipt still
/// conserves, and `rows` would not exit 3.
#[test]
fn an_opaque_body_keeps_the_receipt_conserved() {
    let mut llbc = load(&data("cg_lib.llbc")).expect("llbc");
    let (_, _, before) = relations(&llbc.krate, "cg_lib");
    let id = llbc
        .krate
        .fun_decls
        .iter()
        .find(|f| path_name(&f.item_meta.name) == "cg_lib::util::unit")
        .expect("unit")
        .def_id;
    llbc.krate.fun_decls.get_mut(id).expect("unit").body = charon_lib::ast::Body::Opaque;
    let (rows, residual, receipt) = relations(&llbc.krate, "cg_lib");
    assert_eq!(residual.len(), 1);
    assert_eq!(residual[0].reason, "BODY_NOT_STRUCTURED");
    assert!(
        find(&rows, "DEFINES", &residual[0].subject).len() == 1,
        "the definition stays represented"
    );
    assert!(receipt.conserved(), "{receipt:?}");
    assert_eq!(
        receipt.harvested,
        before.harvested + 1,
        "the body is harvested as its own item"
    );
}
