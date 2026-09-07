//! The libclang translation-unit walker (feature `libclang`).
//!
//! Walks ONE C++ translation unit via the `clang` crate (libclang FFI) and
//! produces [`CppClass`] declarations in the frontend-local shape
//! [`crate::model_from_class`] unpacks into the shared `ModelGraph`.
//!
//! # Scope of this walker
//!
//! Extracts the rock-solid core the libclang high-level API exposes directly:
//! classes/structs (with namespace + nested-class qualification), base
//! specifiers (access + virtual), member fields, and methods with their
//! pure-virtual / noexcept / `override` / operator flags. This exercises the
//! `inherits_from`, `has_field`, `has_function`, `rdf:type`,
//! `virtually_overrides`, `defines_operator`, `is_pure_virtual`, and
//! `is_noexcept` predicates from real parsing.
//!
//! It also harvests each method's **body arm** — the recipe fingerprint
//! (`writes_field` / `reads_field` / `raises` / `calls` / `writes_if_blank`)
//! the fuzzy recipe codebook classifies on, in the same shape the Ruby and C#
//! frontends produce. See the BODY ARM section below for the measured cursor
//! shapes it matches, and [`walk_tu_configured`] to opt out of it.
//!
//! **Walker follow-ups** (the IR + predicates already exist from PR #8; only
//! the walker does not populate them yet): `constexpr`/`consteval` and
//! C++20 `requires` clauses (not surfaced by the high-level `clang` API —
//! need a token pass), templates (`template_specialises` /
//! `template_instantiates`), `friend` declarations, macro-expansion
//! provenance, and `static_assert`.
//!
//! # libclang at runtime
//!
//! The `clang` crate is built with `runtime` (dlopen), so no link-time
//! version coupling. If libclang is not on the default search path, set
//! `LIBCLANG_PATH` (e.g. `/usr/lib/llvm-18/lib`). [`Clang`] is a
//! process-singleton — call [`walk_tu`] sequentially, never from parallel
//! threads in the same process.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use clang::diagnostic::Severity;
use clang::{
    Accessibility, Clang, Entity, EntityKind, ExceptionSpecification, Index, RefQualifier,
};
use ruff_spo_triplet::{
    CppAccess, CppBase, CppField, CppFriend, CppMethod, CppRefQualifier, CppTemplate,
    CppTemplateKind,
};

use crate::{CppClass, CppEnum, CppFunction, Declaration};

/// A failure walking a translation unit.
#[derive(Debug)]
pub enum WalkError {
    /// libclang could not be loaded (missing `libclang.so` / `LIBCLANG_PATH`),
    /// or a [`Clang`] instance already exists in this process.
    Libclang(String),
    /// The translation unit failed to parse.
    Parse(String),
}

impl fmt::Display for WalkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Libclang(m) => write!(f, "libclang unavailable: {m}"),
            Self::Parse(m) => write!(f, "translation unit parse failed: {m}"),
        }
    }
}

impl std::error::Error for WalkError {}

/// Walk one C++ translation unit at `path`, returning every class/struct
/// **definition** found (forward declarations are skipped).
///
/// `args` are passed verbatim to clang (e.g. `["-std=c++17", "-x", "c++",
/// "-I/path/to/includes"]`). Function bodies ARE parsed, because each method's
/// body arm (the recipe fingerprint: what it writes, reads, throws and
/// dispatches) is harvested alongside its signature; [`walk_tu_configured`]
/// with `None` skips them for a faster signature-only walk. Parsing tolerates
/// errors (missing includes still yield a partial AST), matching how libclang
/// is used on large real corpora.
///
/// A **partial** AST is silently possible even when this returns `Ok`: see
/// [`walk_tu_with_diagnostics`] for the visibility this function alone does
/// not give a caller doing a multi-header sweep.
pub fn walk_tu(path: &Path, args: &[String]) -> Result<Vec<CppClass>, WalkError> {
    walk_tu_with_diagnostics(path, args).map(|(classes, _)| classes)
}

/// One libclang parse diagnostic at severity [`Severity::Error`] or higher —
/// the tier that can silently drop AST content (as opposed to `Warning`/
/// `Note`, which never do).
#[derive(Debug, Clone)]
pub struct ParseDiagnostic {
    /// The formatted diagnostic, including the `file:line:col:` location
    /// prefix libclang's default formatter attaches (e.g.
    /// `"scrollview.h:23:10: fatal error: 'X.h' file not found"`).
    pub message: String,
}

impl fmt::Display for ParseDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// Like [`walk_tu`], but also returns every libclang parse diagnostic at
/// [`Severity::Error`] or higher (one parse, not two — [`walk_tu`] is a thin
/// wrapper over this).
///
/// `walk_tu`'s `Ok` alone can mislead a caller into treating "the parse
/// returned `Ok`, 0 failed" as "the whole TU was captured". libclang
/// recovers from an unresolved `#include` by treating the file as
/// successfully parsed while simply DROPPING the incomplete declaration that
/// needed the missing header — no `Err`, no partial-class marker, nothing on
/// [`CppClass`] hints at the gap. This is the exact `STATS`/`scrollview.h`
/// gap found harvesting Tesseract (`statistc.h` includes `scrollview.h` for
/// GRAPHICS_DISABLED-gated declarations; without `src/viewer` on the include
/// path, `STATS`'s `CXXRecordDecl` silently never completes and the class is
/// simply ABSENT from `walk_tu`'s output —
/// `tesseract-rs/.claude/harvest/statistc-manifest.txt`). A caller doing a
/// multi-header sweep (see `examples/harvest_textord.rs`) should call this
/// instead of `walk_tu` and warn loudly when the returned diagnostic list is
/// non-empty — "0 failed" from `walk_tu` alone does NOT mean the sweep is
/// complete.
pub fn walk_tu_with_diagnostics(
    path: &Path,
    args: &[String],
) -> Result<(Vec<CppClass>, Vec<ParseDiagnostic>), WalkError> {
    walk_tu_configured(path, args, Some(&BodyArmConfig::default()))
}

/// [`walk_tu_with_diagnostics`] with explicit control over the body arm.
///
/// `arm` decides BOTH what is harvested and how the TU is parsed:
///
/// - `Some(cfg)` — parse WITH function bodies and fill each [`CppMethod`]'s
///   `writes` / `reads` / `raises` / `calls` / `guarded_writes` from the body,
///   using `cfg`'s mutator vocabulary. This is what [`walk_tu`] and
///   [`walk_tu_with_diagnostics`] do.
/// - `None` — skip function bodies (a faster parse) and leave the arm empty.
///   The signature plane is identical either way, so a consumer that only
///   reconstructs declarations (`ruff_cpp_codegen`) loses nothing by asking
///   for this.
///
/// # Errors
///
/// [`WalkError::Libclang`] if libclang fails to initialise;
/// [`WalkError::Parse`] if the TU fails to parse.
pub fn walk_tu_configured(
    path: &Path,
    args: &[String],
    arm: Option<&BodyArmConfig>,
) -> Result<(Vec<CppClass>, Vec<ParseDiagnostic>), WalkError> {
    let clang = Clang::new().map_err(WalkError::Libclang)?;
    let index = Index::new(&clang, false, false);
    let tu = index
        .parser(path)
        .arguments(args)
        .skip_function_bodies(arm.is_none())
        .parse()
        .map_err(|e| WalkError::Parse(e.to_string()))?;

    let diagnostics = tu
        .get_diagnostics()
        .into_iter()
        .filter(|d| d.get_severity() >= Severity::Error)
        .map(|d| ParseDiagnostic {
            message: d.formatter().format(),
        })
        .collect();

    let mut out = Vec::new();
    collect_classes(&tu.get_entity(), &mut out, arm);
    Ok((out, diagnostics))
}

/// Walk ONE translation unit and collect free-function DEFINITIONS with their
/// **general call graph** — the C-library dispatch structure (e.g. leptonica
/// `pixScale` → `pixScaleGeneral` → `pixScaleGrayLI`/`pixScaleAreaMap`/
/// `pixUnsharpMasking`). Unlike [`walk_tu`] this parses WITH bodies
/// (`skip_function_bodies(false)`), because the callee set is the point.
///
/// This is the missing arm for C libraries: [`walk_tu`] harvests C++ *classes*;
/// a C library (leptonica, zlib, …) is free functions on pointer buffers, so
/// the AR/OO member body-arm (`method_body_arm`) captures nothing there — but
/// the call graph IS the transcode-driving structure (which functions to port,
/// in what dispatch order). Numeric kernel BODIES remain the essential-15%
/// hand-port (the doctrine); this mints the 85% structure that classifies + orders
/// them.
///
/// # Errors
///
/// [`WalkError::Libclang`] if libclang fails to initialise (non-recoverable);
/// [`WalkError::Parse`] if the TU fails to parse.
#[cfg(feature = "libclang")]
pub fn walk_free_functions(path: &Path, args: &[String]) -> Result<Vec<CppFunction>, WalkError> {
    let clang = Clang::new().map_err(WalkError::Libclang)?;
    let index = Index::new(&clang, false, false);
    let tu = index
        .parser(path)
        .arguments(args)
        .skip_function_bodies(false)
        .parse()
        .map_err(|e| WalkError::Parse(e.to_string()))?;

    let mut out = Vec::new();
    collect_functions(&tu.get_entity(), &mut out);
    Ok(out)
}

/// Recurse the AST, emitting a [`CppFunction`] for every free-function
/// DEFINITION (recursing into namespaces). Prototypes (no body) and
/// system-header functions are skipped — a transcode wants the library's own
/// definitions.
///
/// Also captures out-of-line class-METHOD definitions (`Ret Class::method(...)
/// { ... }`) — the C++ analogue of a free function for the C-library harvest
/// arm's purpose (transcode dispatch structure). libclang's cursor tree nests
/// members by LEXICAL position, not semantic ownership: an out-of-line method
/// definition's lexical parent is the enclosing namespace/TU (where the text
/// sits), while its semantic parent is the class — so it shows up as a direct
/// child here, at the SAME recursion level as a free `FunctionDecl`, even
/// though this walker never recurses into a `ClassDecl`/`StructDecl` body. That
/// non-recursion is exactly what keeps this arm's capture correctly scoped:
/// an IN-CLASS (inline) method definition is lexically a child of the
/// `ClassDecl` cursor, which this walker never visits, so only genuinely
/// out-of-line definitions ever reach the `Method` arm below.
/// [`enclosing_scopes`] resolves the owning class as a namespace-like scope
/// component, so the harvested [`CppFunction::namespace`] is `["Widget"]` for
/// `Widget::helper`, matching how a namespaced free function is captured
/// (found via Tesseract's `Textord::compute_block_xheight` /
/// `compute_row_xheight` / `make_spline_rows`, makerow.cpp — previously
/// invisible to this harvest; `tesseract-rs/.claude/harvest/makerow-callgraph.txt`).
/// Constructors/destructors/conversion operators are deliberately NOT
/// included here (unlike [`build_class`]'s member-function set) — kept
/// scoped to the reported gap rather than expanding to every function-like
/// cursor kind.
#[cfg(feature = "libclang")]
fn collect_functions(entity: &Entity, out: &mut Vec<CppFunction>) {
    for child in entity.get_children() {
        match child.get_kind() {
            EntityKind::FunctionDecl | EntityKind::Method => {
                if child.is_definition()
                    && !in_system_header(&child)
                    && let Some(name) = child.get_name()
                {
                    // Methods are keyed CLASS-QUALIFIED (`A::reset`) so two
                    // classes' same-named methods stay distinct in the call
                    // graph (codex P2 on ruff #57); free functions keep their
                    // bare name — the banked manifests' zero-loss bar.
                    let name = if child.get_kind() == EntityKind::Method {
                        qualify_with_class(&child, name)
                    } else {
                        name
                    };
                    let mut calls = Vec::new();
                    collect_calls(&child, &mut calls);
                    calls.sort();
                    calls.dedup();
                    out.push(CppFunction {
                        namespace: enclosing_scopes(&child),
                        name,
                        calls,
                    });
                }
            }
            EntityKind::Namespace => collect_functions(&child, out),
            _ => {}
        }
    }
}

/// Recurse a function body collecting EVERY resolvable callee name (the general
/// call graph). Distinct from [`walk_body`]'s `calls` (persistence mutators
/// only): here every `CallExpr` callee is the dispatch structure a C-library
/// transcode follows.
#[cfg(feature = "libclang")]
fn collect_calls(node: &Entity, out: &mut Vec<String>) {
    for child in node.get_children() {
        if child.get_kind() == EntityKind::CallExpr
            && let Some(name) = call_callee_name(&child)
        {
            out.push(name);
        }
        collect_calls(&child, out);
    }
}

/// The callee name of a `CallExpr` cursor, with a fallback for the case
/// `child.get_name()` alone misses.
///
/// Ordinarily `CallExpr::get_name()` (`clang_getCursorSpelling`) already
/// resolves the callee — but when ANYTHING in the surrounding expression has
/// an error/dependent type (a genuinely unresolved template-dependent call,
/// OR — the concrete case found harvesting real Tesseract, makerow.cpp's
/// `make_baseline_spline`, `tesseract-rs/.claude/harvest/makerow-callgraph.txt`
/// — a call downstream of an UNRELATED parse error elsewhere in the same
/// statement: an `auto`-typed variable whose initializer references an
/// undeclared symbol becomes `<dependent type>`, and passing THAT variable as
/// an argument to an otherwise perfectly ordinary, non-overloaded function
/// forces Clang to represent the call's callee as an `UnresolvedLookupExpr`
/// instead of a resolved `DeclRefExpr`), `clang_getCursorSpelling` on the
/// `CallExpr` itself returns empty (`get_name()` → `None`) even though the
/// callee's own name is perfectly well-formed one level down: nested inside
/// an `OverloadedDeclRef` cursor (libclang's cursor-kind for the unresolved
/// lookup set), reachable via the `CallExpr`'s callee sub-expression
/// (`DeclRefExpr`/`UnexposedExpr` wrapping). Falls back to the first
/// `OverloadedDeclRef` found among the `CallExpr`'s descendants, stopping at
/// any nested `CallExpr` (an argument that is itself a call) so a broken
/// INNER call's callee is never mistaken for the OUTER one.
#[cfg(feature = "libclang")]
fn call_callee_name(call: &Entity) -> Option<String> {
    // A resolved call to a METHOD is emitted class-qualified (`A::reset`) so
    // it joins against the class-qualified definition entry and same-named
    // methods of different classes never collapse (codex P2 on ruff #57).
    // Free-function callees stay bare (zero-loss vs the banked manifests);
    // the OverloadedDeclRef fallback stays bare too — an unresolved lookup
    // set has no single owning class to name.
    if let Some(referenced) = call.get_reference() {
        if referenced.get_kind() == EntityKind::Method
            && let Some(name) = referenced.get_name()
        {
            return Some(qualify_with_class(&referenced, name));
        }
    }
    call.get_name().or_else(|| find_overloaded_decl_ref(call))
}

/// `Class::name` for a method entity, from its SEMANTIC parent (the class),
/// which is correct for out-of-line definitions whose lexical parent is the
/// namespace/TU. Falls back to the bare name when the parent is unnamed.
#[cfg(feature = "libclang")]
fn qualify_with_class(method: &Entity, name: String) -> String {
    match method.get_semantic_parent().and_then(|p| p.get_name()) {
        Some(class) => format!("{class}::{name}"),
        None => name,
    }
}

#[cfg(feature = "libclang")]
fn find_overloaded_decl_ref(node: &Entity) -> Option<String> {
    for child in node.get_children() {
        if child.get_kind() == EntityKind::OverloadedDeclRef
            && let Some(name) = child.get_name()
        {
            return Some(name);
        }
        if child.get_kind() != EntityKind::CallExpr
            && let Some(name) = find_overloaded_decl_ref(&child)
        {
            return Some(name);
        }
    }
    None
}

/// Walk ONE translation unit and collect free-standing ENUM DECLARATIONS at
/// namespace scope (`enum DawgType { ... }` / `enum class Foo : int8_t { ... }`
/// directly inside a namespace or at global scope).
///
/// Nested class-body enums are NOT collected here — they are covered by the
/// extended `build_class`, which pushes them onto the owning
/// [`CppClass::declarations`] as `Declaration::Enum` alongside its fields and
/// methods. This split mirrors [`walk_tu`] vs the class-body arm of
/// `build_class`: a free-standing enum has no owning class to attach to, so
/// it needs its own top-level collection.
///
/// # Errors
///
/// [`WalkError::Libclang`] if libclang fails to initialise (non-recoverable);
/// [`WalkError::Parse`] if the TU fails to parse.
#[cfg(feature = "libclang")]
pub fn walk_enums(path: &Path, args: &[String]) -> Result<Vec<CppEnum>, WalkError> {
    let clang = Clang::new().map_err(WalkError::Libclang)?;
    let index = Index::new(&clang, false, false);
    let tu = index
        .parser(path)
        .arguments(args)
        .skip_function_bodies(true)
        .parse()
        .map_err(|e| WalkError::Parse(e.to_string()))?;

    let mut out = Vec::new();
    collect_enums(&tu.get_entity(), &mut out);
    Ok(out)
}

/// Recurse the AST (namespaces only — class-body enums are handled by
/// [`build_class`]), emitting a [`CppEnum`] for every enum DEFINITION found
/// directly in a namespace or at global scope.
#[cfg(feature = "libclang")]
fn collect_enums(entity: &Entity, out: &mut Vec<CppEnum>) {
    for child in entity.get_children() {
        match child.get_kind() {
            EntityKind::EnumDecl => {
                if child.is_definition()
                    && !in_system_header(&child)
                    && let Some(e) = build_enum(&child)
                {
                    out.push(e);
                }
            }
            EntityKind::Namespace => collect_enums(&child, out),
            _ => {}
        }
    }
}

/// Build a [`CppEnum`] from an enum DEFINITION cursor: namespace, name
/// (`None`/empty for a truly anonymous enum — skipped, nothing to key it by),
/// scoped-ness (`enum class`), the declared underlying integer type if any,
/// and every `EnumConstantDecl` child with its resolved signed value.
#[cfg(feature = "libclang")]
fn build_enum(e: &Entity) -> Option<CppEnum> {
    let name = e.get_name().filter(|n| !n.is_empty())?;
    let namespace = enclosing_scopes(e);
    let is_class = e.is_scoped();
    let underlying_type = e
        .get_enum_underlying_type()
        .map(|t| t.get_display_name())
        .unwrap_or_default();
    let mut variants = Vec::new();
    for c in e.get_children() {
        if c.get_kind() == EntityKind::EnumConstantDecl
            && let Some(vname) = c.get_name()
            && let Some((signed, _unsigned)) = c.get_enum_constant_value()
        {
            variants.push((vname, signed));
        }
    }
    Some(CppEnum {
        namespace,
        name,
        is_class,
        underlying_type,
        variants,
    })
}

/// Coverage instrumentation for `CPP-SCHEMA-FIT`: tally the libclang
/// `EntityKind` of every DIRECT class-body child cursor across all
/// (non-system-header) class/struct definitions in the TU.
///
/// The key is the `EntityKind` `Debug` name (e.g. `"Method"`, `"FieldDecl"`,
/// `"FriendDecl"`); the value is how many times that kind appears as a direct
/// member. The caller computes the *mapped fraction* — `BaseSpecifier` +
/// `FieldDecl` + `Method` are exactly the kinds `build_class` turns into a
/// [`Declaration`] today — versus the total, so a real-corpus walk shows which
/// constructs the walker silently drops (the walker-follow-up backlog:
/// `FriendDecl`, `StaticAssert`, templates, …) rather than asserting coverage.
/// Counts only meaningful cursors; access specifiers and comments are reported
/// in the histogram like everything else so the caller can classify them.
pub fn class_body_cursor_histogram(
    path: &Path,
    args: &[String],
) -> Result<BTreeMap<String, usize>, WalkError> {
    let clang = Clang::new().map_err(WalkError::Libclang)?;
    let index = Index::new(&clang, false, false);
    let tu = index
        .parser(path)
        .arguments(args)
        .skip_function_bodies(true)
        .parse()
        .map_err(|e| WalkError::Parse(e.to_string()))?;
    let mut hist = BTreeMap::new();
    tally_class_bodies(&tu.get_entity(), &mut hist);
    Ok(hist)
}

/// The kinds `build_class` maps to a [`Declaration`] today — the "covered"
/// set for the `CPP-SCHEMA-FIT` mapped-fraction. Kept beside the walker so the
/// coverage probe and the actual extraction can never drift apart. The
/// function-like kinds (`Method` / `Constructor` / `Destructor` /
/// `ConversionFunction` / `FunctionTemplate`) become a `has_function`;
/// `FieldDecl` + `VarDecl` (static members) become `has_field`; `FriendDecl`
/// becomes `is_friend_of`; `BaseSpecifier` becomes `inherits_from`; `EnumDecl`
/// (a nested class-body enum) becomes a `Declaration::Enum`.
pub const MAPPED_CURSOR_KINDS: [&str; 10] = [
    "BaseSpecifier",
    "FieldDecl",
    "VarDecl",
    "Method",
    "Constructor",
    "Destructor",
    "ConversionFunction",
    "FunctionTemplate",
    "FriendDecl",
    "EnumDecl",
];

/// Mirror of [`collect_classes`] that tallies direct class-body child kinds
/// instead of building [`CppClass`]es (same class-selection + system-header
/// filtering, so the histogram counts exactly the bodies the walker extracts).
fn tally_class_bodies(entity: &Entity, hist: &mut BTreeMap<String, usize>) {
    for child in entity.get_children() {
        match child.get_kind() {
            // Kept in lockstep with `collect_classes`: templated classes count
            // too, so the coverage histogram reflects exactly what is harvested.
            EntityKind::ClassDecl
            | EntityKind::StructDecl
            | EntityKind::ClassTemplate
            | EntityKind::ClassTemplatePartialSpecialization => {
                if child.is_definition() && !in_system_header(&child) {
                    for member in child.get_children() {
                        *hist.entry(format!("{:?}", member.get_kind())).or_insert(0) += 1;
                    }
                }
                tally_class_bodies(&child, hist);
            }
            EntityKind::Namespace => tally_class_bodies(&child, hist),
            _ => {}
        }
    }
}

/// Recurse the AST, emitting a [`CppClass`] for every class/struct
/// definition (recursing into namespaces and nested classes).
fn collect_classes(entity: &Entity, out: &mut Vec<CppClass>, arm: Option<&BodyArmConfig>) {
    for child in entity.get_children() {
        match child.get_kind() {
            // Plain classes/structs AND templated classes. libclang FLATTENS a
            // template cursor — its direct children are the template params
            // (skipped by `build_class`'s `_` arm) + the members — so the same
            // `build_class` handles all four unchanged. The harvested name is the
            // bare template name (`GenericVector`, no `<T>`). Shape A: template
            // classes become classes; the template-relationship predicates
            // (`template_specialises` / `template_instantiates`) are a separate,
            // data-driven follow-up (ccutil measured 0 explicit specialisations).
            EntityKind::ClassDecl
            | EntityKind::StructDecl
            | EntityKind::ClassTemplate
            | EntityKind::ClassTemplatePartialSpecialization => {
                // Skip class definitions originating in system headers (the
                // std:: / __gnu_cxx:: machinery dragged in transitively) — an
                // SPO harvest of a project wants the project's own classes,
                // never the standard library's internals.
                if child.is_definition() && !in_system_header(&child) {
                    if let Some(cls) = build_class(&child, arm) {
                        out.push(cls);
                    }
                }
                // Recurse for nested classes regardless of definition state.
                collect_classes(&child, out, arm);
            }
            EntityKind::Namespace => collect_classes(&child, out, arm),
            _ => {}
        }
    }
}

/// Build a [`CppClass`] from a class/struct definition cursor by reading its
/// DIRECT member children (bases, fields, methods). Nested class decls are
/// ignored here — [`collect_classes`] emits them separately.
fn build_class(e: &Entity, arm: Option<&BodyArmConfig>) -> Option<CppClass> {
    // A `ClassTemplatePartialSpecialization` shares its primary's `get_name()`
    // (libclang spells it as the bare template name, e.g. `Foo` for
    // `template<class T> class Foo<T*>`); using that as-is collides with the
    // primary in the cross-TU `BTreeMap` dedup, dropping one of the two. Use
    // the cursor's `get_display_name()` instead — it carries the partial-spec
    // arguments (`Foo<T *>`) so the qualified name stays distinct. Codex P2 #17.
    let name = if matches!(e.get_kind(), EntityKind::ClassTemplatePartialSpecialization) {
        e.get_display_name()?
    } else {
        e.get_name()?
    };
    let namespace = enclosing_scopes(e);
    let mut declarations = Vec::new();
    for m in e.get_children() {
        match m.get_kind() {
            EntityKind::BaseSpecifier => {
                if let Some(base) = build_base(&m) {
                    declarations.push(Declaration::Base(base));
                }
            }
            // FieldDecl = a non-static data member; a VarDecl in a class body is
            // a STATIC data member (`static T x;`, libclang's distinct kind).
            // Both are data members the class HAS → has_field.
            EntityKind::FieldDecl | EntityKind::VarDecl => {
                let type_name = m
                    .get_type()
                    .map(|t| t.get_display_name())
                    .unwrap_or_default();
                // A field whose type is a template-id (`GenericVector<char>`) is a
                // template INSTANTIATION use. `cpp_field` drops `type_name`, so
                // this is otherwise invisible in the triples — surface it as
                // `template_instantiates` (Inferred: single-TU instantiation
                // visibility is incomplete by construction).
                if let Some(inst) = template_instantiation(&type_name) {
                    declarations.push(Declaration::Template(CppTemplate {
                        kind: CppTemplateKind::Instantiation,
                        name: inst,
                    }));
                }
                declarations.push(Declaration::Field(CppField {
                    name: m.get_name().unwrap_or_default(),
                    type_name,
                }));
            }
            // Constructors, destructors, conversion operators, and member
            // function templates are all member FUNCTIONS that libclang reports
            // under cursor kinds distinct from `Method`; the harvester captures
            // every one as a `has_function`. CPP-SCHEMA-FIT measured 495 such
            // cursors silently dropped across ccutil when only `Method` matched
            // (the ctor/dtor coverage gap: 82% → ~90%).
            EntityKind::Method
            | EntityKind::Constructor
            | EntityKind::Destructor
            | EntityKind::ConversionFunction
            | EntityKind::FunctionTemplate => {
                declarations.push(Declaration::Method(build_method(&m, arm)));
                collect_signature_instantiations(&m, &mut declarations);
            }
            // `friend class Foo;` / `friend Ret fn(...);` — the befriended
            // entity. CPP-SCHEMA-FIT measured 79 in ccutil; the `is_friend_of`
            // predicate + `CppFriend` IR already exist (PR #8).
            EntityKind::FriendDecl => {
                if let Some(friend) = build_friend(&m) {
                    declarations.push(Declaration::Friend(friend));
                }
            }
            // A nested (class-body) enum — e.g. Tesseract's `enum PermuterType`
            // members declared inside a class. Namespace-scope enums are
            // harvested separately via `walk_enums`, since they have no
            // owning `CppClass` to attach to.
            EntityKind::EnumDecl => {
                if let Some(en) = build_enum(&m) {
                    declarations.push(Declaration::Enum(en));
                }
            }
            _ => {}
        }
    }
    Some(CppClass {
        namespace,
        name,
        declarations,
    })
}

/// Extract the befriended entity's name from a `friend` declaration cursor.
///
/// The befriended entity is the `FriendDecl`'s child cursor (the `FriendDecl`
/// itself is anonymous). For `friend class Foo;` the child is a `TypeRef` whose
/// referenced TYPE display is the clean fully-qualified name
/// (`Tesseract::TessdataManager`) — the cursor *spelling* would carry a
/// `class `/`struct ` elaboration, so we read the type, not the spelling. For
/// `friend Ret fn(...);` the child is the friend `FunctionDecl`, whose own name
/// is what `is_friend_of` should point to.
fn build_friend(m: &Entity) -> Option<CppFriend> {
    for child in m.get_children() {
        let name = match child.get_kind() {
            EntityKind::TypeRef => child.get_type().map(|t| t.get_display_name()),
            _ => child.get_name(),
        };
        if let Some(name) = name.filter(|s| !s.is_empty()) {
            return Some(CppFriend { name });
        }
    }
    None
}

/// The template-id (`Foo<Args>`) a type display denotes, if it is a template
/// **instantiation** use — stripping a leading `const`/`volatile` and trailing
/// `*`/`&`. `int` → `None`; `const GenericVector<char> &` → `GenericVector<char>`.
/// Verbatim `Foo<Args>` form, per the `CppTemplate::name` IR convention. This is
/// a SYNTACTIC use (deterministic per-TU), not an implicit-instantiation cursor
/// (those are the per-TU-incomplete thing the Inferred provenance flags).
fn template_instantiation(type_display: &str) -> Option<String> {
    if !type_display.contains('<') {
        return None;
    }
    let mut s = type_display.trim();
    for pfx in ["const ", "volatile "] {
        s = s.strip_prefix(pfx).map(str::trim_start).unwrap_or(s);
    }
    let s = s.trim_end_matches(['*', '&', ' ']);
    (s.contains('<') && !s.is_empty()).then(|| s.to_string())
}

/// Push a `template_instantiates` declaration for every template-id in a method's
/// RETURN type or PARAMETER types — the syntactic instantiation uses in a
/// signature, which `cpp_method` does not otherwise surface. Applies to every
/// function-like cursor (ctor/dtor have no/void result + their params).
fn collect_signature_instantiations(m: &Entity, decls: &mut Vec<Declaration>) {
    let mut type_displays: Vec<String> = Vec::new();
    if let Some(ret) = m.get_result_type() {
        type_displays.push(ret.get_display_name());
    }
    if let Some(args) = m.get_arguments() {
        for arg in args {
            if let Some(t) = arg.get_type() {
                type_displays.push(t.get_display_name());
            }
        }
    }
    for ty in type_displays {
        if let Some(inst) = template_instantiation(&ty) {
            decls.push(Declaration::Template(CppTemplate {
                kind: CppTemplateKind::Instantiation,
                name: inst,
            }));
        }
    }
}

/// Whether `e` is defined in a system header (std lib, libc, …). Entities
/// with no location (rare) are treated as project entities (kept).
fn in_system_header(e: &Entity) -> bool {
    e.get_location()
        .is_some_and(|loc| loc.is_in_system_header())
}

/// The enclosing named scopes of `e` (namespaces + outer classes),
/// outermost first — the [`CppClass::namespace`] components. The class's
/// own name is excluded.
fn enclosing_scopes(e: &Entity) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = e.get_semantic_parent();
    while let Some(p) = cur {
        if matches!(
            p.get_kind(),
            EntityKind::Namespace | EntityKind::ClassDecl | EntityKind::StructDecl
        ) {
            if let Some(n) = p.get_name() {
                parts.push(n);
            }
        }
        cur = p.get_semantic_parent();
    }
    parts.reverse();
    parts
}

/// The fully-qualified name of a class-like cursor (`Namespace::Outer::Name`).
pub(crate) fn qualified_name(e: &Entity) -> String {
    let mut parts = enclosing_scopes(e);
    if let Some(n) = e.get_name() {
        parts.push(n);
    }
    parts.join("::")
}

fn build_base(m: &Entity) -> Option<CppBase> {
    let ty = m.get_type()?;
    // Prefer the resolved declaration's qualified name; fall back to the
    // type's display name (e.g. for a dependent base in a template).
    let name = ty
        .get_declaration()
        .map(|d| qualified_name(&d))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| ty.get_display_name());
    let access = match m.get_accessibility() {
        Some(Accessibility::Protected) => CppAccess::Protected,
        Some(Accessibility::Private) => CppAccess::Private,
        // Public, or unreported — default to Public (the common base form).
        _ => CppAccess::Public,
    };
    Some(CppBase {
        name,
        access,
        virtual_base: m.is_virtual_base(),
    })
}

/// Build a [`CppMethod`] from a member-function cursor. `arm` is `Some` when
/// the translation unit was parsed WITH bodies, in which case the body arm is
/// harvested from the method's DEFINITION — which for a method declared in a
/// header and defined in a `.cpp` is a different cursor from `m`, and is the
/// only one that has a body to read.
fn build_method(m: &Entity, arm: Option<&BodyArmConfig>) -> CppMethod {
    let name = m.get_name().unwrap_or_default();
    let is_noexcept = matches!(
        m.get_exception_specification(),
        Some(ExceptionSpecification::BasicNoexcept | ExceptionSpecification::ComputedNoexcept)
    );
    // libclang spells operator methods `operator==`, `operator[]`, etc. Guard
    // against an ordinary method merely named `operatorFoo` by requiring the
    // char after `operator` to not start an identifier.
    let operator_kind = (name.starts_with("operator")
        && name
            .as_bytes()
            .get(8)
            .is_none_or(|b| !(b.is_ascii_alphanumeric() || *b == b'_')))
    .then(|| name.clone());
    // `override` target → the fully-qualified base method with its overload
    // signature (`Base.method(int)`), so `virtually_overrides` joins the
    // **exact base overload** the derived method overrides — not just any
    // method with the same name. The signature suffix matches the per-overload
    // method-IRI convention `cpp_method` builds (codex P2 #17).
    let overrides = m
        .get_overridden_methods()
        .and_then(|ov| ov.into_iter().next())
        .and_then(|base_m| {
            let mname = base_m.get_name()?;
            let parent = base_m.get_semantic_parent()?;
            let params: Vec<String> = base_m
                .get_arguments()
                .into_iter()
                .flatten()
                .filter_map(|a| a.get_type().map(|t| t.get_display_name()))
                .collect();
            // The ref qualifier is part of method identity, so it has to be
            // part of the override TARGET too. Without it `Base::f() &` and
            // `Base::f() &&` both point at `Base.f()`, and neither joins to
            // the base node the IRI actually names.
            let refq = base_m
                .get_type()
                .and_then(|t| t.get_ref_qualifier())
                .map(|q| match q {
                    RefQualifier::LValue => " &",
                    RefQualifier::RValue => " &&",
                })
                .unwrap_or("");
            Some(format!(
                "{}.{mname}({}){}{refq}",
                qualified_name(&parent),
                params.join(","),
                if base_m.is_const_method() {
                    " const"
                } else {
                    ""
                }
            ))
        });
    // AST-DLL signature shape: return type (skip void/ctor/dtor) + ordered
    // parameter types, verbatim from the cursor.
    let return_type = m
        .get_result_type()
        .map(|t| t.get_display_name())
        .filter(|d| !d.is_empty() && d != "void");
    let param_types = m
        .get_arguments()
        .into_iter()
        .flatten()
        .filter_map(|a| a.get_type().map(|t| t.get_display_name()))
        .collect();
    let body = arm.map_or_else(BodyArm::default, |cfg| {
        method_body_arm(&m.get_definition().unwrap_or(*m), cfg)
    });
    CppMethod {
        name,
        is_pure_virtual: m.is_pure_virtual_method(),
        // constexpr/consteval + requires need a token pass — walker follow-up.
        constexpr_kind: None,
        is_noexcept,
        overrides,
        operator_kind,
        requires_clause: None,
        return_type,
        param_types,
        is_const: m.is_const_method(),
        is_static: m.is_static_method(),
        access: match m.get_accessibility() {
            Some(Accessibility::Protected) => CppAccess::Protected,
            Some(Accessibility::Private) => CppAccess::Private,
            // Public, or unreported (e.g. free function) — default Public.
            _ => CppAccess::Public,
        },
        writes: body.writes,
        reads: body.reads,
        raises: body.raises,
        calls: body.calls,
        guarded_writes: body.guarded_writes,
        // `void f() &` / `void f() &&`. Part of the method's identity, so it
        // has to reach the IRI: two ref-qualified overloads otherwise share
        // one node and the body-arm merge copies one's facts onto the other.
        ref_qualifier: m
            .get_type()
            .and_then(|t| t.get_ref_qualifier())
            .map(|q| match q {
                RefQualifier::LValue => CppRefQualifier::LValue,
                RefQualifier::RValue => CppRefQualifier::RValue,
            }),
    }
}

// ─────────────────────────────────────────────────────────────────────────
// BODY ARM
//
// The body-fact fingerprint the fuzzy recipe-codebook needs
// (`ruff/.claude/knowledge/fuzzy-recipe-codebook.md` §2), for C++ member
// functions — so the SAME language-agnostic recipe centroids that classify
// Rails hooks and C# handlers classify C++ setters / lifecycle overrides.
//
// The arm rides on [`CppMethod`]'s five body fields and expands to the same
// five predicates as a Ruby/Python `Function` body, with the same objects and
// the same truth tiers: `writes_field` / `raises` / `writes_if_blank` are
// Authoritative (the lvalue, the throw type and the guard shape are all
// machine-readable), `reads_field` / `calls` are Inferred (heuristic receiver,
// no scope analysis).
//
// Every cursor shape matched below was measured against libclang 18 on a
// fixture carrying each construct, not inferred from the AST documentation —
// several are counter-intuitive and the tests in `arm_tests` pin them:
//
//   `x_ = v`          BinaryOperator[ MemberRefExpr(x_), … ]
//   `p.x_ = v`        BinaryOperator[ MemberRefExpr(x_)[ DeclRefExpr(p) ], … ]
//   `this->x_ = v`    BinaryOperator[ MemberRefExpr(x_)[ ThisExpr ], … ]
//   `x_ += v`         CompoundAssignOperator[ MemberRefExpr(x_), … ]
//   `++x_`            UnaryOperator[ MemberRefExpr(x_) ]
//   `arr_[i] = v`     BinaryOperator[ ArraySubscriptExpr[ …(arr_), …(i) ], … ]
//   `repo_.Save()`    CallExpr(Save)[ MemberRefExpr(Save)[ MemberRefExpr(repo_) ] ]
//   `name_ = s`       CallExpr(operator=)[ MemberRefExpr(name_), …(operator=), …(s) ]
//                     (a class-typed member assigns through its operator, so it
//                      is a CallExpr and NOT a BinaryOperator)
//   `throw E()`       ThrowExpr[ CallExpr(E)[ TypeRef(struct E) ] ]
//
// The two shapes that make the difference between a correct fingerprint and a
// plausible-looking wrong one:
//
//   * **The operator is not on the cursor.** libclang exposes no
//     binary-operator kind, so the draft this replaces treated the LHS of
//     ANY binary operator as a write — `if (status_ == v)` recorded a write of
//     `status_`. [`binary_operator_spelling`] reads the operator from the
//     token stream instead.
//   * **An own member is one with no base cursor.** A member of another
//     object (`p.x_`) carries its base as a child, and so does a method
//     reference (`repo_.Save`), which is why neither is mistaken for this
//     class's state.

/// Which set of calls counts as a lifecycle mutator for the `calls` fact.
///
/// The C++ analogue of the Ruby `AR_MUTATORS` / C# EF sets, and configurable
/// for the same reason those are: the mutator vocabulary belongs to the
/// framework being harvested, not to the harvester. A `calls` fact fires only
/// for a match, because the signal the triage needs is "does this method
/// dispatch a writer", not a full call graph.
///
/// [`Self::default`] is the closed set that ships; a corpus with its own
/// persistence vocabulary supplies it through [`Self::with_mutators`] /
/// [`Self::with_mutator_prefixes`].
#[cfg(feature = "libclang")]
#[derive(Debug, Clone)]
pub struct BodyArmConfig {
    mutators: Vec<String>,
    mutator_prefixes: Vec<String>,
}

#[cfg(feature = "libclang")]
impl Default for BodyArmConfig {
    fn default() -> Self {
        Self {
            mutators: [
                "save", "Save", "update", "Update", "insert", "Insert", "remove", "Remove",
                "erase", "Erase", "commit", "Commit", "flush", "Flush", "destroy", "Destroy",
                "clear", "Clear", "write", "Write",
            ]
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
            mutator_prefixes: Vec::new(),
        }
    }
}

#[cfg(feature = "libclang")]
impl BodyArmConfig {
    /// Replace the exact-match mutator set.
    #[must_use]
    pub fn with_mutators(mut self, names: impl IntoIterator<Item = String>) -> Self {
        self.mutators = names.into_iter().collect();
        self
    }

    /// Add prefixes that make any method name starting with one a mutator
    /// (e.g. `"Set"` to treat every `SetFoo` as a writer).
    #[must_use]
    pub fn with_mutator_prefixes(mut self, prefixes: impl IntoIterator<Item = String>) -> Self {
        self.mutator_prefixes = prefixes.into_iter().collect();
        self
    }

    /// Does a call to `name` count as a lifecycle mutator?
    #[must_use]
    pub fn is_mutator(&self, name: &str) -> bool {
        self.mutators.iter().any(|m| m == name)
            || self.mutator_prefixes.iter().any(|p| name.starts_with(p))
    }
}

/// The recipe fingerprint of one method body, before it is folded into
/// [`CppMethod`]'s own fields.
#[cfg(feature = "libclang")]
#[derive(Debug, Default, Clone)]
pub(crate) struct BodyArm {
    /// `x_ = …` / `x_ += …` / `++x_` — an assignment to an OWN data member.
    pub writes: Vec<String>,
    /// `x_` read as a value (and the target of a compound assignment).
    pub reads: Vec<String>,
    /// `throw E(…)` — the BARE type name; the expander adds the `exc:`
    /// namespace, exactly as it does for a `Function`.
    pub raises: Vec<String>,
    /// `repo_.Save()` — a configured lifecycle mutator, as `receiver.method`.
    pub calls: Vec<String>,
    /// J1: a write under an absence test on that same member.
    pub guarded_writes: Vec<String>,
}

/// Which branch of an `if` a condition guards.
#[cfg(feature = "libclang")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuardedBranch {
    /// The condition tests for ABSENCE (`x == nullptr`, `!x`, `x.empty()`),
    /// so the THEN branch is where the default is written.
    Then,
    /// The condition tests for PRESENCE (`x != nullptr`, `x`, `!x.empty()`),
    /// so the ELSE branch is where the default is written.
    Else,
}

/// Extract the recipe fingerprint from a member function's body.
///
/// Call with the entity that HAS the body — [`Entity::get_definition`] where
/// the definition is out of line, the declaration cursor where it is inline.
/// Guard detection is deliberately local (an `IfStmt` whose condition is an
/// absence test on member `X`, threaded into the branch that writes `X`) — no
/// dominator analysis, which is what keeps `writes_if_blank` Authoritative,
/// exactly as the Ruby `detect_guarded_default` does.
#[cfg(feature = "libclang")]
pub(crate) fn method_body_arm(method: &Entity, cfg: &BodyArmConfig) -> BodyArm {
    let mut arm = BodyArm::default();
    // Parameters are skipped rather than selecting the CompoundStmt, because a
    // constructor's member-initialiser list is a SIBLING of the body, not a
    // child of it — narrowing to the body would silently drop ctor-init facts.
    //
    // This is SCOPING, not a bug fix, and the difference is measured: with this
    // skip removed, `void put(int v = C::fallback() + C::s_)` still contributed
    // no calls and no reads, because the call arm only records specific
    // receiver shapes and reads only cover own non-static members. A default
    // argument is not something the body executes, so it has no business being
    // walked — but no leak is reachable through it today, and a test asserting
    // otherwise would pass whether or not this line is here.
    for child in method.get_children() {
        if child.get_kind() == EntityKind::ParmDecl {
            continue;
        }
        walk_node(&child, &mut arm, cfg, None);
    }
    for facts in [
        &mut arm.writes,
        &mut arm.reads,
        &mut arm.raises,
        &mut arm.calls,
        &mut arm.guarded_writes,
    ] {
        facts.sort();
        facts.dedup();
    }
    arm
}

/// Walk every CHILD of `node`. `guard` is the member the enclosing branch is
/// absence-guarded on (J1), threaded down only into that branch.
#[cfg(feature = "libclang")]
fn walk_body(node: &Entity, arm: &mut BodyArm, cfg: &BodyArmConfig, guard: Option<&str>) {
    for child in node.get_children() {
        walk_node(&child, arm, cfg, guard);
    }
}

/// The LHS of an assignment, minus the member reference that IS the target.
///
/// `arr_[i] = v` must record the subscript `i` as a read and `arr_` only as a
/// write; the naive child walk records `arr_` twice, once through each role.
#[cfg(feature = "libclang")]
fn walk_lhs_skipping_target(
    lhs: &Entity,
    arm: &mut BodyArm,
    cfg: &BodyArmConfig,
    guard: Option<&str>,
) {
    let target = assignment_target(lhs);
    for child in lhs.get_children() {
        // Compared through `assignment_target`, not `own_member_name`: the
        // subscript's base arrives wrapped in an UnexposedExpr, so a bare
        // member-reference test never matches it and the array is re-read.
        if target.is_some() && assignment_target(&child) == target {
            walk_past_target(&child, arm, cfg, guard);
            continue;
        }
        walk_node(&child, arm, cfg, guard);
    }
}

/// Walk everything under the write target EXCEPT the target's own reference.
///
/// Descending with `walk_body` is not enough: the reference arrives wrapped in
/// an `UnexposedExpr`, so walking the wrapper's children lands straight back on
/// the `MemberRefExpr` and records the read this exists to avoid. Follow the
/// same descent `assignment_target` uses, and walk only what QUALIFIES the
/// member (`this`, or `p` in `p.x_`) plus any subscript indices passed on the
/// way down.
#[cfg(feature = "libclang")]
fn walk_past_target(e: &Entity, arm: &mut BodyArm, cfg: &BodyArmConfig, guard: Option<&str>) {
    let mut cur = *e;
    for _ in 0..32 {
        match cur.get_kind() {
            EntityKind::MemberRefExpr => {
                walk_body(&cur, arm, cfg, guard);
                return;
            }
            EntityKind::UnexposedExpr | EntityKind::ParenExpr => {
                let Some(next) = cur.get_children().into_iter().next() else {
                    return;
                };
                cur = next;
            }
            // A nested subscript (`a_[i][j]`): the base continues the descent,
            // and every index is a real read that must not be lost with it.
            EntityKind::ArraySubscriptExpr => {
                let mut children = cur.get_children().into_iter();
                let Some(base) = children.next() else {
                    return;
                };
                for idx in children {
                    walk_node(&idx, arm, cfg, guard);
                }
                cur = base;
            }
            _ => return,
        }
    }
}

/// Walk ONE node, matching it before descending.
#[cfg(feature = "libclang")]
fn walk_node(node: &Entity, arm: &mut BodyArm, cfg: &BodyArmConfig, guard: Option<&str>) {
    match node.get_kind() {
        EntityKind::ThrowExpr => {
            if let Some(ty) = thrown_type_name(node) {
                arm.raises.push(ty);
            }
            walk_body(node, arm, cfg, guard);
        }
        // Only `=` is an assignment. Every other binary operator (`==`, `+`,
        // `<<`, …) reads both sides — which is why the operator has to be read
        // off the tokens rather than assumed from the cursor kind.
        EntityKind::BinaryOperator => {
            let children = node.get_children();
            if let Some((lhs, rest)) = children.split_first()
                && binary_operator_spelling(node).as_deref() == Some("=")
            {
                record_write(arm, lhs, guard);
                // The LHS's own member reference is the write TARGET, not a
                // read, so descend past it — but keep whatever it contains
                // (`arr_[i]`'s subscript, `p` in `p.x_`). Walking the children
                // blindly re-reads the target through the nested member
                // reference, which is how `arr_[i] = v` recorded `arr_` as
                // both a write and a read.
                walk_lhs_skipping_target(lhs, arm, cfg, guard);
                for r in rest {
                    walk_node(r, arm, cfg, guard);
                }
            } else {
                walk_body(node, arm, cfg, guard);
            }
        }
        // `x_ += v` is both a write and a read of `x_` — a read-modify-write.
        EntityKind::CompoundAssignOperator => {
            let children = node.get_children();
            if let Some((lhs, rest)) = children.split_first() {
                record_write(arm, lhs, guard);
                if let Some(m) = assignment_target(lhs) {
                    arm.reads.push(m);
                }
                walk_body(lhs, arm, cfg, guard);
                for r in rest {
                    walk_node(r, arm, cfg, guard);
                }
            }
        }
        // `++x_` / `x_--` are read-modify-writes too; every other unary
        // operator (`!x_`, `*x_`, `-x_`) only reads.
        EntityKind::UnaryOperator => {
            let is_inc_dec = unary_operator_is_inc_dec(node);
            let children = node.get_children();
            if let Some(operand) = children.first()
                && is_inc_dec
            {
                record_write(arm, operand, guard);
                if let Some(m) = assignment_target(operand) {
                    arm.reads.push(m);
                }
                walk_body(operand, arm, cfg, guard);
            } else {
                walk_body(node, arm, cfg, guard);
            }
        }
        EntityKind::CallExpr => call_expr(node, arm, cfg, guard),
        EntityKind::IfStmt => if_stmt(node, arm, cfg, guard),
        // A member reference that is not an assignment target is a read — but
        // only when it names an OWN data member. `p.x_` and `repo_.Save` both
        // carry their base as a child and are therefore not own members;
        // descending still finds `repo_` inside the latter.
        EntityKind::MemberRefExpr => {
            if let Some(name) = own_member_name(node) {
                arm.reads.push(name);
            }
            walk_body(node, arm, cfg, guard);
        }
        _ => walk_body(node, arm, cfg, guard),
    }
}

/// A call: either an overloaded-operator assignment (a write), a configured
/// lifecycle mutator (a `calls` fact), or an ordinary call walked for its
/// arguments.
#[cfg(feature = "libclang")]
fn call_expr(node: &Entity, arm: &mut BodyArm, cfg: &BodyArmConfig, guard: Option<&str>) {
    let name = node.get_name().unwrap_or_default();
    let children = node.get_children();

    // A class-typed member assigns through `operator=`, so `name_ = s` arrives
    // as a CallExpr whose FIRST child is the assigned object (not a callee
    // reference, which is the shape an ordinary method call has).
    if let Some(op) = name.strip_prefix("operator")
        && op.ends_with('=')
        && !matches!(op, "==" | "!=" | "<=" | ">=")
        && let Some((lhs, rest)) = children.split_first()
    {
        record_write(arm, lhs, guard);
        // A compound form (`+=`, `|=`, …) also reads its target.
        if op != "="
            && let Some(m) = assignment_target(lhs)
        {
            arm.reads.push(m);
        }
        walk_body(lhs, arm, cfg, guard);
        for r in rest {
            walk_node(r, arm, cfg, guard);
        }
        return;
    }

    if cfg.is_mutator(&name) {
        arm.calls.push(format!(
            "{}.{name}",
            call_receiver(node).as_deref().unwrap_or("this")
        ));
    }
    for child in &children {
        // The callee reference names the METHOD, not a data member — walking
        // it as an ordinary node would record `Save` as a read of a field that
        // does not exist. Its children (the receiver) still matter.
        if child.get_kind() == EntityKind::MemberRefExpr && child.get_name().as_ref() == Some(&name)
        {
            walk_body(child, arm, cfg, guard);
        } else {
            walk_node(child, arm, cfg, guard);
        }
    }
}

/// An `if`: walk the condition for its reads, then each branch — threading the
/// J1 guard into whichever branch the condition proves the member ABSENT in.
#[cfg(feature = "libclang")]
fn if_stmt(node: &Entity, arm: &mut BodyArm, cfg: &BodyArmConfig, guard: Option<&str>) {
    let children = node.get_children();
    let Some((cond, branches)) = children.split_first() else {
        return;
    };
    walk_node(cond, arm, cfg, None);
    // `branches` is [then] or [then, else].
    let detected = absence_guard(cond);
    for (i, branch) in branches.iter().enumerate() {
        let branch_guard = match &detected {
            // This `if` decides the guard for both of its branches: the one
            // the condition proves the member absent in gets it, the other
            // gets nothing (an enclosing guard is dropped rather than
            // reasoned about — the safe direction, since a missed guard only
            // records a plain write).
            Some((member, GuardedBranch::Then)) => (i == 0).then_some(member.as_str()),
            Some((member, GuardedBranch::Else)) => (i == 1).then_some(member.as_str()),
            // No guard here — an enclosing one still holds in both branches.
            None => guard,
        };
        walk_node(branch, arm, cfg, branch_guard);
    }
}

/// Record an assignment to `lhs` as a write, and as a J1 guarded write when
/// the enclosing branch is absence-guarded on that same member.
#[cfg(feature = "libclang")]
fn record_write(arm: &mut BodyArm, lhs: &Entity, guard: Option<&str>) {
    if let Some(member) = assignment_target(lhs) {
        if guard == Some(member.as_str()) {
            arm.guarded_writes.push(member.clone());
        }
        arm.writes.push(member);
    }
}

/// The own data member an assignment's left-hand side ultimately names, seeing
/// through the wrappers libclang inserts plus subscripting and dereference
/// (`arr_[i] = v` and `*ptr_ = v` both write the member). `None` when the
/// target is anything else — a local, a parameter, or another object's member.
#[cfg(feature = "libclang")]
pub(crate) fn assignment_target(lhs: &Entity) -> Option<String> {
    let mut cur = *lhs;
    // Bounded: each step descends one AST level, and the depth of an lvalue
    // expression is finite. The cap only guards against a cyclic cursor graph.
    for _ in 0..32 {
        match cur.get_kind() {
            EntityKind::MemberRefExpr => return own_member_name(&cur),
            EntityKind::UnexposedExpr | EntityKind::ParenExpr | EntityKind::ArraySubscriptExpr => {
                cur = cur.get_children().into_iter().next()?;
            }
            // NOT UnaryOperator. `*ptr_ = v` changes the POINTEE; the member
            // `ptr_` itself is unchanged, so recording it as a write is wrong.
            // Increment/decrement reaches this function with the operand
            // already unwrapped, so it is unaffected.
            _ => return None,
        }
    }
    None
}

/// The member name when `e` references a data member of THIS object.
///
/// An implicit `this` base is not a visited child, and an explicit one is a
/// [`EntityKind::ThisExpr`]; any other base (`p.x_`, `repo_.Save`) means the
/// reference belongs to something else.
#[cfg(feature = "libclang")]
pub(crate) fn own_member_name(e: &Entity) -> Option<String> {
    if e.get_kind() != EntityKind::MemberRefExpr {
        return None;
    }
    match e.get_children().first().map(Entity::get_kind) {
        None | Some(EntityKind::ThisExpr) => e.get_name(),
        _ => None,
    }
}

/// The operator spelling of a binary operator, read off the token stream.
///
/// libclang exposes no binary-operator kind, so the operator is the first
/// punctuation token that starts at or after the end of the left operand.
#[cfg(feature = "libclang")]
pub(crate) fn binary_operator_spelling(node: &Entity) -> Option<String> {
    let lhs_end = node
        .get_children()
        .first()?
        .get_range()?
        .get_end()
        .get_file_location()
        .offset;
    node.get_range()?
        .tokenize()
        .into_iter()
        .find(|t| {
            t.get_kind() == clang::token::TokenKind::Punctuation
                && t.get_location().get_file_location().offset >= lhs_end
        })
        .map(|t| t.get_spelling())
}

/// Is this unary operator an increment or decrement (prefix or postfix)?
#[cfg(feature = "libclang")]
pub(crate) fn unary_operator_is_inc_dec(node: &Entity) -> bool {
    let Some(range) = node.get_range() else {
        return false;
    };
    let tokens = range.tokenize();
    let is_inc_dec = |t: Option<&clang::token::Token<'_>>| {
        t.map(clang::token::Token::get_spelling)
            .is_some_and(|s| s == "++" || s == "--")
    };
    is_inc_dec(tokens.first()) || is_inc_dec(tokens.last())
}

/// The J1 fact's condition half: does this `if` condition test ONE own member
/// for absence or presence, and which branch does that make the guarded one?
///
/// Deliberately conservative — a compound condition (`&&` / `||`), a condition
/// naming more than one own member, or a shape not in the table below yields
/// `None`, so the write is recorded as a plain write. That is the safe
/// direction: a missed guard classifies the method as `Compute`/`Normalize`,
/// never as a false schema default.
#[cfg(feature = "libclang")]
fn absence_guard(cond: &Entity) -> Option<(String, GuardedBranch)> {
    let mut members = Vec::new();
    collect_own_members(cond, &mut members);
    members.sort();
    members.dedup();
    let [member] = members.as_slice() else {
        return None;
    };

    let tokens: Vec<String> = cond
        .get_range()?
        .tokenize()
        .into_iter()
        .map(|t| t.get_spelling())
        .collect();
    let has = |t: &str| tokens.iter().any(|s| s == t);
    if has("&&") || has("||") {
        return None;
    }
    let null_literal = has("nullptr") || has("NULL") || has("0") || has("false");
    let empty_test = has("empty") || has("isEmpty") || has("IsEmpty");
    let leading_bang = tokens.first().is_some_and(|t| t == "!");
    let comparison = ["==", "!=", "<", ">", "<=", ">="].iter().any(|t| has(t));

    let branch = match () {
        () if has("==") && null_literal => GuardedBranch::Then,
        () if has("!=") && null_literal => GuardedBranch::Else,
        () if leading_bang && empty_test => GuardedBranch::Else,
        () if leading_bang => GuardedBranch::Then,
        () if empty_test => GuardedBranch::Then,
        // A bare `if (x_)` is a presence test.
        () if !comparison && !empty_test => GuardedBranch::Else,
        () => return None,
    };
    Some((member.clone(), branch))
}

/// Every own data member referenced anywhere under `node`.
#[cfg(feature = "libclang")]
fn collect_own_members(node: &Entity, out: &mut Vec<String>) {
    if let Some(name) = own_member_name(node) {
        out.push(name);
    }
    for child in node.get_children() {
        collect_own_members(&child, out);
    }
}

/// The thrown exception's type name. `throw X(...)` nests the operand under
/// wrapper cursors, so recurse for the first node yielding a concrete,
/// non-void type name.
#[cfg(feature = "libclang")]
pub(crate) fn thrown_type_name(throw: &Entity) -> Option<String> {
    fn first_typed(e: &Entity) -> Option<String> {
        if let Some(t) = e.get_type() {
            let name = bare_type_name(&t.get_display_name());
            if !name.is_empty() && name != "void" {
                return Some(name);
            }
        }
        e.get_children().iter().find_map(first_typed)
    }
    throw.get_children().iter().find_map(first_typed)
}

/// The receiver of a method call (`repo_` in `repo_.Save()`), or `None` for an
/// implicit-`this` call — the callee reference's own base.
#[cfg(feature = "libclang")]
pub(crate) fn call_receiver(call: &Entity) -> Option<String> {
    let name = call.get_name()?;
    let callee = call.get_children().into_iter().find(|c| {
        c.get_kind() == EntityKind::MemberRefExpr && c.get_name() == Some(name.clone())
    })?;
    let mut cur = callee.get_children().into_iter().next()?;
    for _ in 0..32 {
        match cur.get_kind() {
            EntityKind::MemberRefExpr | EntityKind::DeclRefExpr => return cur.get_name(),
            EntityKind::UnexposedExpr | EntityKind::ParenExpr => {
                cur = cur.get_children().into_iter().next()?;
            }
            _ => return None,
        }
    }
    None
}

/// `List<Foo>` / `foo::Bar` → a stable bare type name for the `raises` object.
#[cfg(feature = "libclang")]
pub(crate) fn bare_type_name(display: &str) -> String {
    let s = display
        .trim_start_matches("class ")
        .trim_start_matches("struct ");
    let s = s.split('<').next().unwrap_or(s);
    s.rsplit("::").next().unwrap_or(s).trim().to_string()
}

/// `clang::Clang` is a process-singleton (`Clang::new()` returns `Err` rather
/// than panicking if one is already alive, but that `Err` would surface as a
/// test failure) — serialize every test in this file that constructs one, so
/// cargo's parallel test threads never race two at once. Shared CRATE-WIDE:
/// [`arm_tests`] + [`walker_tests`] here AND `lib.rs`'s `libclang_tests`
/// (which aliases this lock) — two separate locks raced each other once
/// ("an instance of `Clang` already exists" in motherlode).
#[cfg(all(test, feature = "libclang"))]
pub(crate) static CLANG_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(all(test, feature = "libclang"))]
mod arm_tests {
    use super::*;
    use std::io::Write;

    /// Every method under `e`, by name.
    fn methods<'a>(e: &Entity<'a>, out: &mut BTreeMap<String, Entity<'a>>) {
        for c in e.get_children() {
            if matches!(
                c.get_kind(),
                EntityKind::Method | EntityKind::Constructor | EntityKind::Destructor
            ) && let Some(name) = c.get_name()
            {
                // A method seen twice (header declaration + out-of-line
                // definition) keeps the DEFINITION, which is the cursor the
                // arm needs — `build_method` resolves the same way.
                if c.is_definition() || !out.contains_key(&name) {
                    out.insert(name, c);
                }
            }
            methods(&c, out);
        }
    }

    /// Parse one inline C++ fixture WITH bodies and return every method's arm.
    ///
    /// One parse per fixture, not one per method: `Clang` is a process
    /// singleton, so each parse serialises on [`CLANG_TEST_LOCK`].
    fn arms_with(name: &str, src: &str, cfg: &BodyArmConfig) -> BTreeMap<String, BodyArm> {
        let dir = std::env::temp_dir().join(format!("cpp_arm_{name}"));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("f.cpp");

        // The write happens INSIDE the lock. Twelve tests share the `shapes`
        // fixture, so they share this path; `File::create` truncates, so a
        // write outside the lock can empty the file while another thread is
        // inside `parse()` on it. That parse then finds no methods and the
        // caller's index panics.
        let _guard = CLANG_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut fh = std::fs::File::create(&path).expect("fixture file");
        fh.write_all(src.as_bytes()).expect("write fixture");
        drop(fh);
        let clang = Clang::new().expect("libclang");
        let index = Index::new(&clang, false, false);
        let tu = index
            .parser(&path)
            .arguments(&["-std=c++17".to_string()])
            .skip_function_bodies(false)
            .parse()
            .expect("parse");
        let mut found = BTreeMap::new();
        methods(&tu.get_entity(), &mut found);
        found
            .into_iter()
            .map(|(n, e)| (n, method_body_arm(&e, cfg)))
            .collect()
    }

    fn arms(name: &str, src: &str) -> BTreeMap<String, BodyArm> {
        arms_with(name, src, &BodyArmConfig::default())
    }

    /// A class exercising every write/read shape the arm must tell apart.
    /// Declaration order matters: a type must be complete before a member
    /// uses it, or the operand's type is unresolved and the arm sees nothing.
    const SHAPES: &str = r#"
struct BadStatus {};
struct Repo { void Save(); void Peek() const; };
struct Str { bool empty() const; };
struct Patient {
    int status_;
    int arr_[4];
    int* ptr_;
    Repo repo_;
    Str name_;
    void set(int v) { status_ = v; }
    void compare(int v) { if (status_ == v) { } }
    void compound(int v) { status_ += v; }
    void increment() { ++status_; }
    void decrement() { status_--; }
    void assign_object(Str s) { name_ = s; }
    void other_object(Patient& p, int v) { p.status_ = v; }
    void explicit_this(int v) { this->status_ = v; }
    void subscript(int i, int v) { arr_[i] = v; }
    void read_only(int& out) { out = status_; }
    void persist() { repo_.Save(); }
    void peek() { repo_.Peek(); }
    void call_self() { reset(); }
    void reset() { status_ = 0; }
    void thrower() { throw BadStatus(); }
};
"#;

    #[test]
    fn a_plain_setter_writes_its_member_without_reading_it() {
        let a = &arms("shapes", SHAPES)["set"];
        assert_eq!(a.writes, ["status_"]);
        assert!(
            a.reads.is_empty(),
            "the assignment target is not a read: {:?}",
            a.reads
        );
    }

    /// `arr_[idx_] = v` writes `arr_` and reads `idx_`. Walking the LHS's
    /// children blindly also re-read `arr_` through the nested member
    /// reference, so one assignment reported the array as both written and
    /// read — and a read the method never performs changes its recipe.
    #[test]
    fn a_subscripted_assignment_reads_the_index_but_not_the_array() {
        let a = &arms(
            "subscript",
            r"
struct C {
  void put(int v) { arr_[idx_] = v; }
  int arr_[8];
  int idx_;
};",
        )["put"];
        assert_eq!(a.writes, ["arr_"]);
        assert_eq!(a.reads, ["idx_"], "the write target must not be a read too");
    }

    /// `*ptr_ = v` changes the POINTEE. The member `ptr_` still holds the same
    /// address afterwards, so recording it as a write claims a mutation that
    /// did not happen.
    #[test]
    fn a_dereference_assignment_does_not_write_the_pointer_member() {
        let a = &arms(
            "deref",
            r"
struct C {
  void put(int v) { *ptr_ = v; }
  int* ptr_;
};",
        )["put"];
        assert!(
            !a.writes.iter().any(|w| w == "ptr_"),
            "the pointer member is unchanged: {:?}",
            a.writes
        );
    }

    /// The override TARGET must carry the ref qualifier, because the method
    /// IRI does. Without it `Base::f() &` and `Base::f() &&` both point at
    /// `Base.f()`, so neither joins to the base node it actually overrides —
    /// and that pair is the only compiler-given statement of behavioural
    /// relatedness the corpus offers for free.
    #[test]
    fn an_override_target_distinguishes_the_ref_qualified_overloads() {
        let src = r"
struct Base {
    virtual void f() & ;
    virtual void f() && ;
};
struct D : Base {
    void f() & override {}
    void f() && override {}
};
";
        let dir = std::env::temp_dir().join("cpp_refq_override");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("f.cpp");
        let _guard = CLANG_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::fs::write(&path, src).expect("write fixture");
        let (classes, _) = walk_tu_configured(
            &path,
            &["-std=c++17".to_string()],
            Some(&BodyArmConfig::default()),
        )
        .expect("walk");
        let d = classes.iter().find(|c| c.name == "D").expect("class D");
        let targets: Vec<&str> = d
            .declarations
            .iter()
            .filter_map(|decl| match decl {
                Declaration::Method(m) if m.name == "f" => m.overrides.as_deref(),
                _ => None,
            })
            .collect();
        assert!(
            targets.contains(&"Base.f() &"),
            "the lvalue overload must name its own base overload: {targets:?}"
        );
        assert!(
            targets.contains(&"Base.f() &&"),
            "the rvalue overload must name its own base overload: {targets:?}"
        );
    }

    /// The defect the token-based operator lookup exists to fix: without it
    /// every binary operator's left operand read as a write, so a pure
    /// comparison reported a mutation.
    #[test]
    fn a_comparison_is_a_read_and_never_a_write() {
        let a = &arms("shapes", SHAPES)["compare"];
        assert!(a.writes.is_empty(), "comparison wrote {:?}", a.writes);
        assert_eq!(a.reads, ["status_"]);
    }

    #[test]
    fn a_read_modify_write_is_both() {
        let all = arms("shapes", SHAPES);
        for name in ["compound", "increment", "decrement"] {
            let a = &all[name];
            assert_eq!(a.writes, ["status_"], "{name} writes");
            assert_eq!(a.reads, ["status_"], "{name} reads");
        }
    }

    /// A class-typed member assigns through `operator=`, so libclang reports
    /// the assignment as a call — a shape a BinaryOperator-only walker misses
    /// entirely.
    #[test]
    fn an_overloaded_assignment_is_still_a_write() {
        let a = &arms("shapes", SHAPES)["assign_object"];
        assert_eq!(a.writes, ["name_"]);
        assert!(a.reads.is_empty(), "copy-assign read {:?}", a.reads);
    }

    #[test]
    fn another_objects_member_is_not_state_of_this_class() {
        let a = &arms("shapes", SHAPES)["other_object"];
        assert!(a.writes.is_empty(), "wrote {:?}", a.writes);
        assert!(a.reads.is_empty(), "read {:?}", a.reads);
    }

    #[test]
    fn an_explicit_this_is_the_same_member_as_an_implicit_one() {
        let all = arms("shapes", SHAPES);
        assert_eq!(all["explicit_this"].writes, all["set"].writes);
        assert_eq!(all["explicit_this"].reads, all["set"].reads);
    }

    #[test]
    fn a_subscripted_assignment_writes_the_array_member() {
        let a = &arms("shapes", SHAPES)["subscript"];
        assert_eq!(a.writes, ["arr_"]);
    }

    #[test]
    fn a_value_use_is_a_read() {
        let a = &arms("shapes", SHAPES)["read_only"];
        assert_eq!(a.reads, ["status_"]);
        assert!(a.writes.is_empty(), "wrote {:?}", a.writes);
    }

    /// The receiver is the object the mutator is called ON — not the method
    /// name, which is what the callee cursor carries.
    #[test]
    fn a_mutator_call_names_its_receiver() {
        let a = &arms("shapes", SHAPES)["persist"];
        assert_eq!(a.calls, ["repo_.Save"]);
        assert_eq!(a.reads, ["repo_"], "the receiver is read");
    }

    #[test]
    fn a_non_mutator_call_is_not_a_calls_fact() {
        let a = &arms("shapes", SHAPES)["peek"];
        assert!(a.calls.is_empty(), "calls {:?}", a.calls);
    }

    /// A method reference is not a data member, so calling one on this object
    /// must not manufacture a read of a field that does not exist.
    #[test]
    fn calling_own_method_does_not_read_a_field_named_after_it() {
        let a = &arms("shapes", SHAPES)["call_self"];
        assert!(a.reads.is_empty(), "read {:?}", a.reads);
        assert!(a.writes.is_empty(), "wrote {:?}", a.writes);
    }

    /// The IR carries the BARE type name; the `exc:` namespace is the
    /// expander's job, exactly as it is for a `Function`.
    #[test]
    fn a_throw_records_the_bare_exception_type() {
        let a = &arms("shapes", SHAPES)["thrower"];
        assert_eq!(a.raises, ["BadStatus"]);
    }

    const GUARDS: &str = r#"
struct Str { bool empty() const; };
struct Cfg {
    int* ptr_;
    Str name_;
    int other_;
    void null_guard(int* v) { if (ptr_ == nullptr) { ptr_ = v; } }
    void bang_guard(int* v) { if (!ptr_) { ptr_ = v; } }
    void empty_guard(Str s) { if (name_.empty()) { name_ = s; } }
    void present_guard(int* v) { if (ptr_ != nullptr) { } else { ptr_ = v; } }
    void bare_truth_guard(int* v) { if (ptr_) { } else { ptr_ = v; } }
    void not_empty_guard(Str s) { if (!name_.empty()) { } else { name_ = s; } }
    void unguarded(int* v) { ptr_ = v; }
    void wrong_branch(int* v) { if (ptr_ == nullptr) { } else { ptr_ = v; } }
    void other_member_guard(int* v) { if (other_ == 0) { ptr_ = v; } }
    void compound_condition(int* v, bool b) { if (ptr_ == nullptr && b) { ptr_ = v; } }
    void nested_plain_if(int* v, bool b) { if (ptr_ == nullptr) { if (b) { ptr_ = v; } } }
    void nested_regard(int* v) { if (ptr_ == nullptr) { if (other_ == 0) { ptr_ = v; } } }
};
"#;

    #[test]
    fn an_absence_test_makes_the_then_branch_write_a_guarded_write() {
        let all = arms("guards", GUARDS);
        for name in ["null_guard", "bang_guard", "empty_guard"] {
            let a = &all[name];
            assert_eq!(a.guarded_writes.len(), 1, "{name}: {:?}", a.guarded_writes);
            assert!(
                a.writes.contains(&a.guarded_writes[0]),
                "{name}: a guarded write is always a write too"
            );
        }
    }

    /// The mirror image: when the condition proves the member PRESENT, the
    /// default is written in the `else`, so that is the guarded branch.
    #[test]
    fn a_presence_test_makes_the_else_branch_write_a_guarded_write() {
        let all = arms("guards", GUARDS);
        for (name, member) in [
            ("present_guard", "ptr_"),
            ("bare_truth_guard", "ptr_"),
            ("not_empty_guard", "name_"),
        ] {
            let a = &all[name];
            assert_eq!(a.guarded_writes, [member], "{name} guarded writes");
            assert_eq!(a.writes, [member], "{name} writes");
        }
    }

    /// The silence half. Each of these writes the member and must NOT be
    /// recorded as a schema default — a guard that fires on everything
    /// carries exactly as much information as one that never fires.
    #[test]
    fn a_write_that_is_not_absence_guarded_stays_a_plain_write() {
        let all = arms("guards", GUARDS);
        for name in [
            // No conditional at all.
            "unguarded",
            // Guarded, but the write is in the branch where the member is
            // known PRESENT.
            "wrong_branch",
            // The condition tests a DIFFERENT member.
            "other_member_guard",
            // A compound condition is not analysed.
            "compound_condition",
        ] {
            let a = &all[name];
            assert!(!a.writes.is_empty(), "{name} should still write");
            assert!(
                a.guarded_writes.is_empty(),
                "{name} claimed a guarded write: {:?}",
                a.guarded_writes
            );
        }
    }

    /// An enclosing absence guard survives a nested `if` that has no guard of
    /// its own — the write is still only reachable when the member was absent.
    /// It does NOT survive a nested `if` that guards on a DIFFERENT member,
    /// because that inner condition decides its branches and the outer guard
    /// is dropped rather than reasoned about. Both are branches of the guard
    /// threading that no other test reaches.
    #[test]
    fn an_enclosing_guard_crosses_a_plain_nested_if_but_not_a_regarding_one() {
        let all = arms("guards", GUARDS);

        let crossed = &all["nested_plain_if"];
        assert_eq!(crossed.writes, ["ptr_"]);
        assert_eq!(
            crossed.guarded_writes,
            ["ptr_"],
            "a plain nested `if` does not cancel the enclosing absence guard"
        );

        let regarded = &all["nested_regard"];
        assert_eq!(regarded.writes, ["ptr_"]);
        assert!(
            regarded.guarded_writes.is_empty(),
            "an inner guard on another member drops the outer one: {:?}",
            regarded.guarded_writes
        );
    }

    #[test]
    fn the_mutator_vocabulary_is_configurable() {
        let src = r#"
struct Repo { void Persist(); void Save(); };
struct Svc { Repo repo_; void go() { repo_.Persist(); repo_.Save(); } };
"#;
        // The shipped set does not know `Persist`.
        let default = &arms("mutator_default", src)["go"];
        assert_eq!(default.calls, ["repo_.Save"]);

        // …and a corpus with its own vocabulary can say so.
        let custom = BodyArmConfig::default().with_mutators(["Persist".to_string()]);
        let configured = &arms_with("mutator_custom", src, &custom)["go"];
        assert_eq!(configured.calls, ["repo_.Persist"]);

        // Prefixes work too, and compose with the exact set.
        let prefixed = BodyArmConfig::default().with_mutator_prefixes(["Per".to_string()]);
        let both = &arms_with("mutator_prefix", src, &prefixed)["go"];
        assert_eq!(both.calls, ["repo_.Persist", "repo_.Save"]);
    }

    /// The reason `build_method` resolves through [`Entity::get_definition`]:
    /// a method declared in a class and defined out of line has NO body on the
    /// declaration cursor, which is the one `build_class` walks.
    #[test]
    fn an_out_of_line_definition_is_harvested_through_the_declaration() {
        let src = r#"
struct Repo { void Save(); };
struct Svc {
    int status_;
    Repo repo_;
    void finish(int v);
};
void Svc::finish(int v) { status_ = v; repo_.Save(); }
"#;
        let dir = std::env::temp_dir().join("cpp_arm_outofline");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("f.cpp");
        std::fs::write(&path, src).expect("write fixture");

        let _guard = CLANG_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (classes, _) = walk_tu_configured(
            &path,
            &["-std=c++17".to_string()],
            Some(&BodyArmConfig::default()),
        )
        .expect("walk");

        let svc = classes.iter().find(|c| c.name == "Svc").expect("Svc");
        let finish = svc
            .declarations
            .iter()
            .find_map(|d| match d {
                Declaration::Method(m) if m.name == "finish" => Some(m),
                _ => None,
            })
            .expect("finish");
        assert_eq!(finish.writes, ["status_"]);
        assert_eq!(finish.calls, ["repo_.Save"]);
    }

    /// libclang really does report the ref-qualifier, and each overload keeps
    /// its own body.
    ///
    /// The IR-level falsifier for this builds `CppMethod`s by hand, so it
    /// cannot tell a populated field from one that is always `None`. This
    /// parses the real thing: if `build_method` stopped reading the qualifier,
    /// all three overloads below would report `None` and the assertions fail.
    #[test]
    fn libclang_reports_the_ref_qualifier_and_each_overload_keeps_its_body() {
        let src = r#"
struct Holder {
    int lvalue_only_;
    int rvalue_only_;
    int plain_only_;
    void value() & { lvalue_only_ = 1; }
    void value() && { rvalue_only_ = 2; }
    void value() { plain_only_ = 3; }
};
"#;
        let dir = std::env::temp_dir().join("cpp_refqual");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("f.cpp");
        std::fs::write(&path, src).expect("write fixture");

        let _guard = CLANG_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (classes, _) = walk_tu_configured(
            &path,
            &["-std=c++17".to_string()],
            Some(&BodyArmConfig::default()),
        )
        .expect("walk");
        let holder = classes.iter().find(|c| c.name == "Holder").expect("Holder");
        let methods: Vec<&CppMethod> = holder
            .declarations
            .iter()
            .filter_map(|d| match d {
                Declaration::Method(m) if m.name == "value" => Some(m),
                _ => None,
            })
            .collect();
        assert_eq!(methods.len(), 3, "three overloads");

        let by_qualifier = |q: Option<CppRefQualifier>| {
            methods
                .iter()
                .find(|m| m.ref_qualifier == q)
                .unwrap_or_else(|| panic!("no overload with qualifier {q:?}"))
        };
        assert_eq!(
            by_qualifier(Some(CppRefQualifier::LValue)).writes,
            ["lvalue_only_"]
        );
        assert_eq!(
            by_qualifier(Some(CppRefQualifier::RValue)).writes,
            ["rvalue_only_"]
        );
        assert_eq!(by_qualifier(None).writes, ["plain_only_"]);
    }

    /// The opt-out: a signature-only walk parses no bodies, so every arm is
    /// empty while the signature plane is unchanged.
    ///
    /// The opt-out is enforced TWICE — the TU is parsed with
    /// `skip_function_bodies`, and `build_method` does not harvest — so
    /// disabling either mechanism alone leaves this test green. That is
    /// redundancy, not a vacuous assertion: it goes red when the parse flag is
    /// forced on, and red again when both are forced on. Anyone tempted to
    /// drop one of the two as dead weight should expect no test to notice.
    #[test]
    fn a_signature_only_walk_leaves_the_arm_empty() {
        let src = r#"
struct Svc { int status_; void set(int v) { status_ = v; } };
"#;
        let dir = std::env::temp_dir().join("cpp_arm_sigonly");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("f.cpp");
        std::fs::write(&path, src).expect("write fixture");
        let args = ["-std=c++17".to_string()];

        let _guard = CLANG_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let method_of = |arm: Option<&BodyArmConfig>| {
            let (classes, _) = walk_tu_configured(&path, &args, arm).expect("walk");
            classes
                .iter()
                .find(|c| c.name == "Svc")
                .and_then(|c| {
                    c.declarations.iter().find_map(|d| match d {
                        Declaration::Method(m) if m.name == "set" => Some(m.clone()),
                        _ => None,
                    })
                })
                .expect("set")
        };
        let with_arm = method_of(Some(&BodyArmConfig::default()));
        let without = method_of(None);

        assert_eq!(with_arm.writes, ["status_"], "the arm is the default");
        assert!(without.writes.is_empty(), "opting out harvests no body");
        // The signature plane is identical either way.
        assert_eq!(with_arm.param_types, without.param_types);
        assert_eq!(with_arm.is_const, without.is_const);
        assert_eq!(with_arm.access, without.access);
    }
}

/// Hermetic fixtures for the three `ruff_cpp_spo` harvest gaps found on real
/// Tesseract corpora (`tesseract-rs/.claude/harvest/makerow-callgraph.txt` +
/// `statistc-manifest.txt`): out-of-line class methods invisible to
/// [`walk_free_functions`], an unresolved-lookup callee reference silently
/// dropped from the call graph, and an unresolved `#include` silently
/// dropping AST content with no visible signal. No real corpus needed — each
/// fixture reproduces the exact libclang cursor shape found on the real
/// files (confirmed via `-Xclang -ast-dump` + a cursor-kind probe against
/// `/tmp/tesseract/src/textord/makerow.cpp` and `.../ccstruct/statistc.h`).
#[cfg(all(test, feature = "libclang"))]
mod walker_tests {
    use super::*;
    use std::io::Write;

    /// Write `src` to a fresh temp file under a name-scoped dir (mirrors
    /// `arm_tests::arm_of`'s fixture-writing pattern), returning its path.
    fn write_fixture(name: &str, src: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cpp_walker_{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.cpp");
        let mut fh = std::fs::File::create(&path).unwrap();
        fh.write_all(src.as_bytes()).unwrap();
        path
    }

    fn cxx_args() -> Vec<String> {
        ["-std=c++17", "-x", "c++"].map(String::from).to_vec()
    }

    /// Fix 1 + Fix 2 combined: `Widget::dispatch` is an out-of-line method
    /// that calls ANOTHER out-of-line method of the same class,
    /// `Widget::compute` — previously invisible to `walk_free_functions`,
    /// which only recursed `FunctionDecl` + `Namespace` cursors (the real gap:
    /// `Textord::compute_block_xheight` / `compute_row_xheight` /
    /// `make_spline_rows` in makerow.cpp). `compute()`'s own body exercises
    /// Fix 2: `free_helper(v)`'s callee reference becomes an unresolved
    /// `OverloadedDeclRef` (not a clean, directly-named `DeclRefExpr`) because
    /// `v`'s `auto`-deduced type is poisoned by the undeclared-identifier
    /// error on the line above — the exact shape found (via cursor-kind probe)
    /// in `make_baseline_spline`'s calls to `segment_baseline` /
    /// `linear_spline_baseline`, both of which are perfectly ordinary,
    /// non-overloaded functions.
    const OUT_OF_LINE_SRC: &str = r"
int free_helper(int x);

class Widget {
 public:
  void inline_only() {}
  void compute();
  void dispatch();
};

void Widget::compute() {
  auto v = totally_undefined_symbol_xyz();
  free_helper(v);
}

void Widget::dispatch() {
  compute();
}
";

    #[test]
    fn out_of_line_methods_are_captured_with_qualified_scope_and_dispatch() {
        let _guard = CLANG_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = write_fixture("out_of_line", OUT_OF_LINE_SRC);
        let funcs = walk_free_functions(&path, &cxx_args()).expect("libclang walk");
        let _ = std::fs::remove_file(&path);

        // Fix 1: both out-of-line methods are captured, scoped under the
        // owning class exactly like a namespaced free function
        // (`enclosing_scopes` resolves the class as the scope component).
        let compute = funcs
            .iter()
            .find(|f| f.name == "Widget::compute")
            .unwrap_or_else(|| panic!("Widget::compute missing; got {funcs:?}"));
        assert_eq!(compute.namespace, vec!["Widget".to_string()]);
        let dispatch = funcs
            .iter()
            .find(|f| f.name == "Widget::dispatch")
            .unwrap_or_else(|| panic!("Widget::dispatch missing; got {funcs:?}"));
        assert_eq!(dispatch.namespace, vec!["Widget".to_string()]);

        // Fix 1 + codex P2 (#57): in-TU dispatch between two out-of-line
        // methods resolves CLASS-QUALIFIED, so same-named methods of
        // different classes can never collapse in the call graph.
        assert!(
            dispatch.calls.contains(&"Widget::compute".to_string()),
            "dispatch must call compute: {:?}",
            dispatch.calls
        );

        // Fix 2: the unresolved (`OverloadedDeclRef`-shaped) callee reference
        // to `free_helper` is still recovered, despite `get_name()` on the
        // `CallExpr` itself returning empty.
        assert!(
            compute.calls.contains(&"free_helper".to_string()),
            "compute must call free_helper despite the unresolved-lookup shape: {:?}",
            compute.calls
        );

        // Regression: an IN-CLASS (inline) method definition must NOT be
        // captured — this walker never recurses into a ClassDecl body, and
        // neither fix changes that scoping (only out-of-line definitions,
        // lexically at namespace/TU level, ever reach the `Method` arm).
        assert!(
            !funcs.iter().any(|f| f.name.ends_with("inline_only")),
            "inline_only is a class-body definition, not out-of-line: {funcs:?}"
        );

        // Exactly the 2 out-of-line methods — no phantom extra captures.
        assert_eq!(funcs.len(), 2, "unexpected extra captures: {funcs:?}");
    }

    /// codex P2 on ruff #57, proven: two classes with the SAME method name
    /// stay distinct — definitions are keyed `A::reset` / `B::reset`, and a
    /// resolved call site to each is emitted class-qualified, so the call
    /// graph can never report a call to one as dispatching to the other.
    const SAME_NAME_TWO_CLASSES_SRC: &str = r"
class A {
 public:
  void reset();
};
class B {
 public:
  void reset();
};
void A::reset() {}
void B::reset() {}
void drive(A &a, B &b) {
  a.reset();
  b.reset();
}
";

    #[test]
    fn same_named_methods_of_different_classes_stay_distinct() {
        let _guard = CLANG_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = write_fixture("same_name_two_classes", SAME_NAME_TWO_CLASSES_SRC);
        let funcs = walk_free_functions(&path, &cxx_args()).expect("libclang walk");
        let _ = std::fs::remove_file(&path);

        assert!(funcs.iter().any(|f| f.name == "A::reset"), "{funcs:?}");
        assert!(funcs.iter().any(|f| f.name == "B::reset"), "{funcs:?}");
        let drive = funcs
            .iter()
            .find(|f| f.name == "drive")
            .unwrap_or_else(|| panic!("drive missing; got {funcs:?}"));
        assert!(
            drive.calls.contains(&"A::reset".to_string())
                && drive.calls.contains(&"B::reset".to_string()),
            "drive must reference BOTH class-qualified callees: {:?}",
            drive.calls
        );
    }

    /// A PLAIN free function (no class involved) keeps its existing
    /// zero-fallback behavior: `call_callee_name` only engages when
    /// `get_name()` is empty, so a healthy, already-resolved call is
    /// untouched by Fix 2. This is the regression bar for "existing manifests
    /// must not change for pure-C TUs" — asserted directly here as an
    /// in-crate fixture rather than only via the real leptonica corpus.
    const PLAIN_FREE_FUNCTION_SRC: &str = r"
int leaf(int x) { return x; }
int root(int x) { return leaf(x); }
";

    #[test]
    fn plain_free_function_calls_are_unaffected() {
        let _guard = CLANG_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = write_fixture("plain_free_fn", PLAIN_FREE_FUNCTION_SRC);
        let funcs = walk_free_functions(&path, &cxx_args()).expect("libclang walk");
        let _ = std::fs::remove_file(&path);

        assert_eq!(funcs.len(), 2, "got {funcs:?}");
        let root = funcs
            .iter()
            .find(|f| f.name == "root")
            .unwrap_or_else(|| panic!("root missing; got {funcs:?}"));
        assert!(root.namespace.is_empty(), "namespace {:?}", root.namespace);
        assert_eq!(root.calls, vec!["leaf".to_string()]);
    }

    /// Fix 3 (walker half): a TU with an unresolved `#include` still parses
    /// (`walk_tu` alone returns `Ok`, "0 failed") but
    /// [`walk_tu_with_diagnostics`] surfaces the severity>=Error diagnostic a
    /// caller would otherwise never see. Mirrors the real `STATS` /
    /// `scrollview.h` gap (`tesseract-rs/.claude/harvest/statistc-manifest.txt`):
    /// a class defined independently of the missing header still parses fine
    /// here (this fixture does not reproduce the FULL real-corpus cascade that
    /// drops `STATS` itself — see `lib.rs`'s `statistc_missing_viewer_include_is_now_diagnosed_and_fixed`
    /// for that end-to-end confirmation against the real file), but the
    /// diagnostic is exactly the signal that makes the caller aware the parse
    /// was imperfect, which `walk_tu` alone hides.
    const MISSING_INCLUDE_SRC: &str = r#"
#include "definitely_missing_header_xyz.h"

class Healthy {
 public:
  void method();
};
"#;

    #[test]
    fn unresolved_include_is_surfaced_as_a_diagnostic_not_silently_dropped() {
        let _guard = CLANG_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = write_fixture("missing_include", MISSING_INCLUDE_SRC);
        let (classes, diagnostics) =
            walk_tu_with_diagnostics(&path, &cxx_args()).expect("libclang walk");

        assert!(
            !diagnostics.is_empty(),
            "a missing #include must surface at least one severity>=Error diagnostic"
        );
        assert!(
            diagnostics[0]
                .message
                .contains("definitely_missing_header_xyz.h"),
            "diagnostic message must name the missing file: {}",
            diagnostics[0].message
        );
        // The class itself, defined independently of the missing header, still
        // parses ("0 failed" is not a lie here) — the diagnostic is what makes
        // the caller aware the parse was imperfect, which `walk_tu` alone hides.
        assert!(
            classes.iter().any(|c| c.name == "Healthy"),
            "Healthy must still be captured: {:?}",
            classes.iter().map(|c| &c.name).collect::<Vec<_>>()
        );

        // walk_tu (the pre-existing API, now a thin wrapper) is unaffected:
        // same class list, from the same parse-shaped TU.
        let via_walk_tu = walk_tu(&path, &cxx_args()).expect("libclang walk");
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            via_walk_tu
                .iter()
                .map(CppClass::qualified_name)
                .collect::<Vec<_>>(),
            classes
                .iter()
                .map(CppClass::qualified_name)
                .collect::<Vec<_>>(),
            "walk_tu and walk_tu_with_diagnostics must return the same class list"
        );
    }

    /// Regression: a clean TU (no missing includes, no errors) reports zero
    /// diagnostics — the happy path is unaffected by the new diagnostics arm.
    #[test]
    fn clean_tu_reports_no_diagnostics() {
        let _guard = CLANG_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = write_fixture("clean_tu", "class Healthy { public: void method(); };");
        let (classes, diagnostics) =
            walk_tu_with_diagnostics(&path, &cxx_args()).expect("libclang walk");
        let _ = std::fs::remove_file(&path);
        assert!(
            diagnostics.is_empty(),
            "clean TU must report 0 diagnostics: {diagnostics:?}"
        );
        assert_eq!(classes.len(), 1);
    }
}
