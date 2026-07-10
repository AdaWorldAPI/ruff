//! Harvest the Stockfish **NNUE** method-resolution manifest from real
//! Stockfish source — the `ruff>OGAR` sink-in for the `stockfish-rs` NNUE
//! transcode (the chess sibling of the Tesseract `harvest_network` arc).
//!
//! Walks the NNUE headers (the `FeatureTransformer`, the `AffineTransform`/
//! `ClippedReLU`/`SqrClippedReLU`/`AffineTransformSparseInput` layer templates,
//! the `HalfKAv2_hm`/`FullThreats` feature sets, and `Network`) via libclang
//! and emits the SPO manifest (`has_function` / `inherits_from` /
//! `virtually_overrides`) the `stockfish-rs` transcode resolves against. The
//! numeric propagate/accumulate BODIES are the doctrine's hand-ported 15%
//! (int8 GEMM via `ndarray::simd`); this harvest is the minted 85% — the layer
//! dimension chain + the read_parameters/propagate method table.
//!
//! Unlike Tesseract's polymorphic `Network` vtable, NNUE composition is by
//! TEMPLATE nesting (`AffineTransform<Prev, Out>`), not virtual override — so
//! the interesting manifest is the per-class METHOD SET (`propagate`,
//! `read_parameters`, `write_parameters`, `get_weight_index`,
//! `OutputDimensions`/`InputDimensions` constants) rather than an override set.
//! We print both; `virtually_overrides` is expected empty here (a real finding
//! about NNUE's shape, recorded, not a bug).
//!
//! Uses `walk_tu_with_diagnostics` (not `walk_tu`) so a silently-dropped class
//! from an unresolved `#include` is LOUD (the Tesseract `STATS`/`scrollview.h`
//! lesson): a "0 failed" that hides a missing class is the trap.
//!
//! Run:
//! ```sh
//! STOCKFISH_SRC=/home/user/Stockfish LIBCLANG_PATH=/usr/lib/llvm-18/lib \
//!   cargo run -p ruff_cpp_spo --features libclang --example harvest_nnue
//! ```

#![expect(
    clippy::print_stderr,
    reason = "manifest-emission CLI example (mirrors harvest_network)"
)]

use std::collections::BTreeSet;
use std::path::Path;

use ruff_cpp_spo::{
    CppClass, Declaration, NAMESPACE, model_from_class, walk_tu_with_diagnostics,
};
use ruff_spo_triplet::{ModelGraph, expand, to_ndjson};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::var("STOCKFISH_SRC").unwrap_or_else(|_| "/home/user/Stockfish".to_string());
    let root = Path::new(&root);
    let nnue = root.join("src/nnue");
    if !nnue.join("network.h").exists() {
        return Err(format!("{} not found; set STOCKFISH_SRC", nnue.display()).into());
    }

    // Stockfish is `-std=c++17`, includes are relative to `src/`. Tolerate any
    // unresolved transitive include (libclang still surfaces the class decls);
    // `-fno-exceptions` mirrors the build so template SFINAE resolves as it does
    // in-tree.
    let args = [
        "-std=c++17".to_string(),
        "-fno-exceptions".to_string(),
        "-x".to_string(),
        "c++".to_string(),
        format!("-I{}", root.join("src").display()),
        format!("-I{}", root.join("src/nnue").display()),
    ];

    // The NNUE headers (each declares one or more NNUE class/template).
    let headers = [
        "nnue_common.h",
        "nnue_architecture.h",
        "nnue_accumulator.h",
        "nnue_feature_transformer.h",
        "network.h",
        "features/half_ka_v2_hm.h",
        "features/full_threats.h",
        "layers/affine_transform.h",
        "layers/affine_transform_sparse_input.h",
        "layers/clipped_relu.h",
        "layers/sqr_clipped_relu.h",
    ];

    let mut all: Vec<CppClass> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut any_diag = false;
    for h in headers {
        let path = nnue.join(h);
        if !path.exists() {
            eprintln!("[harvest] skip missing {h}");
            continue;
        }
        match walk_tu_with_diagnostics(&path, &args) {
            Ok((classes, diags)) => {
                if !diags.is_empty() {
                    any_diag = true;
                    eprintln!("[harvest] {h}: {} error-severity diagnostic(s):", diags.len());
                    for d in diags.iter().take(3) {
                        eprintln!("    {d}");
                    }
                }
                for c in classes {
                    if seen.insert(c.qualified_name()) {
                        all.push(c);
                    }
                }
            }
            Err(e) => eprintln!("[harvest] walk {h} failed: {e}"),
        }
    }
    eprintln!(
        "[harvest] {} unique classes across {} headers{}",
        all.len(),
        headers.len(),
        if any_diag { " (SOME headers had error diagnostics — see above; a dropped class is silent)" } else { "" }
    );

    // The NNUE class manifest — per class, the method set + any override set.
    // (Override set expected empty: NNUE composes by template nesting, not
    // virtual dispatch — the shape finding, recorded.)
    let targets = [
        "FeatureTransformer",
        "Network",
        "HalfKAv2_hm",
        "FullThreats",
        "AffineTransform",
        "AffineTransformSparseInput",
        "ClippedReLU",
        "SqrClippedReLU",
    ];
    eprintln!("\n[nnue] class -> method manifest (the layer dimension + propagate table):");
    for t in targets {
        if let Some(c) = all.iter().find(|c| c.name == t) {
            let methods: Vec<&_> = c
                .declarations
                .iter()
                .filter_map(|d| match d {
                    Declaration::Method(m) => Some(m),
                    _ => None,
                })
                .collect();
            let names: Vec<&String> = methods.iter().map(|m| &m.name).collect();
            let overrides: Vec<&String> = methods
                .iter()
                .filter(|m| m.overrides.is_some())
                .map(|m| &m.name)
                .collect();
            eprintln!(
                "  {t:26} {:2} methods  overrides={:?}\n      methods={:?}",
                methods.len(),
                overrides,
                names
            );
        } else {
            eprintln!("  {t:26} NOT FOUND");
        }
    }

    // Emit the full ndjson manifest — what lance-graph's SPO store + the
    // stockfish-rs codegen/hand-port classifier consume.
    let mut graph = ModelGraph::new(NAMESPACE);
    for c in &all {
        graph.models.push(model_from_class(c));
    }
    let triples = expand(&graph);
    let ndjson = to_ndjson(&triples);
    let out =
        std::env::var("MANIFEST_OUT").unwrap_or_else(|_| "/tmp/nnue_manifest.ndjson".to_string());
    std::fs::write(&out, &ndjson)?;
    eprintln!(
        "\n[harvest] {} models -> {} triples, {} ndjson bytes -> {out}",
        graph.models.len(),
        triples.len(),
        ndjson.len()
    );
    Ok(())
}
