//! THE `OpenGGS` signature harvest — the C-library arm wired end to end, from
//! libclang to rendered Rust, with the round-trip oracle as the gate.
//!
//! `OpenGGS` (<https://github.com/bugix/OpenGGS>) is plain C: 25 file-scope
//! structs and ~181 free functions, no classes. So the class plane
//! (`walk_tu` → `CppClass` → `model_from_class`) harvests NOTHING from it, and
//! the C-library arm ([`walk_free_functions`]) is the whole story.
//!
//! ```text
//!   OpenGGS/src/*.cpp --(walk_free_functions)--> CppFunction
//!     --(one Model per TU)--> ModelGraph
//!     --(expand)--> Vec<Triple>                      the SPO plane
//!     --(ruff_cpp_codegen::project)--> ClassManifest
//!     --(decompile ∘ project == expand | signature)--> THE ORACLE
//!     --(render)--> Rust `&[MethodSig]` const tables
//! ```
//!
//! # A translation unit is the Model
//!
//! `project` keys manifests by `Model::name`, and a C library has no class to
//! key on. The translation unit is the honest unit: it is what the C programmer
//! used for grouping and for internal linkage (`static` = TU-private), so
//! `GAME_ENVIRONMENT.cpp`'s functions become the `GAME_ENVIRONMENT` manifest.
//! Inventing one synthetic class for the whole corpus would throw that grouping
//! away; inventing one per function would make the manifest layer meaningless.
//!
//! # What this generates, and what it does NOT
//!
//! Signature manifests — names, ordered parameter types, return types, linkage.
//! **Not bodies.** That is the 85/15 split: this mints the mechanical structure
//! that classifies and orders the transcode, and the function bodies remain the
//! essential-15% hand-port. A generated `MethodSig` table is a map, not a port.
//!
//! Env:
//!   `GGS_SRC`         corpus directory (default `/home/user/OpenGGS/src`)
//!   `GGS_ARGS`        extra clang args, space-separated
//!                     (default `-I/usr/include/SDL2`)
//!   `GGS_OUT`         artifact directory (default `.claude/harvest/openggs`)
//!
//! A missing corpus is a printed skip, never a failure, so this runs on any
//! checkout. The corpus is read locally and never redistributed.
//!
//! ```sh
//! LIBCLANG_PATH=/usr/lib/llvm-18/lib \
//!   cargo run -p ruff_cpp_spo --features libclang --example harvest_openggs
//! ```

#![expect(
    clippy::print_stderr,
    reason = "manifest-emission CLI example (mirrors harvest_ladybug)"
)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use ruff_cpp_spo::{NAMESPACE, walk_free_functions_with_diagnostics};
use ruff_spo_triplet::{CppMethod, Model, ModelGraph, Triple, expand, to_ndjson};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// The translation units of the corpus, sorted so the harvest is reproducible.
fn translation_units(src: &PathBuf) -> std::io::Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(src)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "cpp"))
        .collect();
    out.sort();
    Ok(out)
}

/// Run the harvest.
///
/// Two failure postures, deliberately different: a MISSING corpus is a printed
/// skip and `Ok`, so this example runs on any checkout; a FAILED ORACLE is a
/// hard error that writes nothing, because a manifest whose IRIs disagree with
/// the triples is worse than no manifest. A translation unit that parses but
/// emits an error diagnostic is skipped rather than harvested — see the gate in
/// the loop for why the oracle cannot catch that case.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let src = PathBuf::from(env_or("GGS_SRC", "/home/user/OpenGGS/src"));
    if !src.is_dir() {
        eprintln!("[ggs] skip: no corpus at {}", src.display());
        return Ok(());
    }
    let mut args: Vec<String> = env_or("GGS_ARGS", "-I/usr/include/SDL2")
        .split_whitespace()
        .map(str::to_string)
        .collect();
    // The corpus's own headers are beside its sources; every TU includes
    // `globals.h` by bare name.
    args.push(format!("-I{}", src.display()));

    let units = translation_units(&src)?;
    // The namespace lives on the GRAPH, not the model, and `decompile` is told
    // the same one below. Building the graph with `default()` leaves it empty,
    // which keys every manifest IRI differently from the expanded triples — the
    // oracle caught exactly that on the first run.
    let mut graph = ModelGraph::new(NAMESPACE);
    let (mut n_functions, mut n_failed, mut n_diagnostics) = (0usize, 0usize, 0usize);

    for tu in &units {
        let stem = tu
            .file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        let (funcs, diagnostics) = match walk_free_functions_with_diagnostics(tu, &args) {
            Ok(fd) => fd,
            Err(e) => {
                eprintln!("[ggs] {stem}: parse failed: {e}");
                n_failed += 1;
                continue;
            }
        };
        // A TU that parsed but emitted an error diagnostic is NOT usable ore.
        // libclang recovers from an unresolved #include by dropping the
        // declarations that needed it, with no Err and no marker — so the
        // functions are simply absent, and the round-trip oracle below cannot
        // see the loss (it compares a projection of the graph against an
        // expansion of the SAME graph; both are equally short). Refuse the unit
        // rather than mint a manifest from a partial parse.
        if !diagnostics.is_empty() {
            eprintln!(
                "[ggs] {stem}: SKIPPED — {} error diagnostic(s); first: {}",
                diagnostics.len(),
                diagnostics[0]
            );
            for d in diagnostics.iter().skip(1).take(2) {
                eprintln!("[ggs]     also: {d}");
            }
            n_diagnostics += 1;
            continue;
        }
        if funcs.is_empty() {
            continue;
        }
        // A function DEFINED in a header reachable from several TUs is harvested
        // once per TU. Keeping the first sighting inside one model is a corpus
        // decision, not a dedup of occurrences.
        //
        // The key is (name, ordered parameter types), NOT the name alone: this
        // corpus is compiled as C++, so `f(int)` and `f(double)` are two
        // distinct functions that a name-only key would collapse into one,
        // silently dropping the second before it ever reached the graph. The
        // oracle cannot catch that either — it compares a projection of the
        // graph against an expansion of the SAME graph, and both are equally
        // short. This key matches the identity the method IRI already uses.
        let mut seen = BTreeSet::new();
        let mut model = Model::new(&stem);
        for f in funcs {
            if !seen.insert((f.name.clone(), f.param_types.clone())) {
                continue;
            }
            n_functions += 1;
            // `CppFunction::is_static` (C internal linkage) is deliberately NOT
            // mapped onto `CppMethod::is_static`. That predicate means "a
            // class-level member with no implicit `this`", and a free function
            // is not a class member at all, so neither value is a true claim
            // about it: `true` would assert class membership, `false` would
            // assert an implicit receiver these functions do not have. The
            // expander emits `is_static` only when the field is true, so
            // leaving it at the default makes NO claim — which is the honest
            // encoding. Linkage stays on `CppFunction` for a consumer reading
            // the harvest directly; representing it in the shared IR would need
            // its own predicate, and that is a deliberate ontology change, not
            // something to smuggle in by overloading an existing one.
            model.methods.push(CppMethod {
                name: f.name,
                return_type: f.return_type,
                param_types: f.param_types,
                ..CppMethod::default()
            });
        }
        graph.models.push(model);
    }

    let triples = expand(&graph);

    // ── THE ORACLE ───────────────────────────────────────────────────────────
    // `decompile(project(g))` must equal `expand(g)` restricted to the
    // signature plane. This is not a self-golden: the two sides are computed by
    // different code (the expander vs the manifest projection), so a manifest
    // that drops a method or reorders a parameter list fails here. A run that
    // cannot prove it refuses to write anything.
    let manifests = ruff_cpp_codegen::project(&graph);
    let round_tripped: BTreeSet<String> = ruff_cpp_codegen::decompile(&manifests, NAMESPACE)
        .iter()
        .map(key)
        .collect();
    let expected: BTreeSet<String> = triples
        .iter()
        .filter(|t| ruff_cpp_codegen::is_signature_plane(t))
        .map(key)
        .collect();

    let missing: Vec<&String> = expected.difference(&round_tripped).collect();
    let extra: Vec<&String> = round_tripped.difference(&expected).collect();
    if !missing.is_empty() || !extra.is_empty() {
        eprintln!(
            "[ggs] ORACLE FAILED: {} signature triples lost, {} invented",
            missing.len(),
            extra.len()
        );
        for k in missing.iter().take(5) {
            eprintln!("  lost:     {k}");
        }
        for k in extra.iter().take(5) {
            eprintln!("  invented: {k}");
        }
        return Err("signature round-trip mismatch — nothing written".into());
    }

    let out = PathBuf::from(env_or("GGS_OUT", ".claude/harvest/openggs"));
    std::fs::create_dir_all(&out)?;
    // This corpus's own parity status, not the Tesseract arm's. `render` would
    // stamp "operator-blocked: leptonica" onto an artifact that has nothing to
    // do with leptonica — an artifact asserting something false about its own
    // verification is worse than one that says nothing.
    let rendered = ruff_cpp_codegen::render_with_parity(
        &manifests,
        "UNRUN (no execution oracle: signatures only, bodies are hand-port)",
    );
    std::fs::write(out.join("openggs_methods.rs"), &rendered)?;

    std::fs::write(out.join("triples.ndjson"), to_ndjson(&triples))?;

    eprintln!(
        "[ggs] {} TUs ({n_failed} failed to parse, {n_diagnostics} skipped on error \
         diagnostics), {} models, {n_functions} functions",
        units.len(),
        graph.models.len()
    );
    eprintln!(
        "[ggs] {} triples ({} on the signature plane)",
        triples.len(),
        expected.len()
    );
    eprintln!("[ggs] ORACLE PASSED: decompile(project(g)) == expand(g) | signature plane");
    eprintln!("[ggs] artifacts -> {}", out.display());
    Ok(())
}

/// A triple's identity for set comparison. Provenance is deliberately excluded:
/// the oracle is about WHICH facts survive the projection, not about how each
/// side labelled its own derivation.
fn key(t: &Triple) -> String {
    format!("{}\t{}\t{}", t.s, t.p, t.o)
}
