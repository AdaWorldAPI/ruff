//! Harvest an arbitrary ladybug (KuzuDB-fork) header **subtree**'s C++ class
//! manifest via libclang, recursively, and emit the SPO manifest plus a full
//! breakdown (`inherits_from` trees, `has_function` top-25, pure-virtual
//! classes, `virtually_overrides` count, field/template/static-assert
//! counts, and a body-arm recipe-centroid census) — the `ruff>OGAR`
//! structure feeding the ladybug-rs transcode.
//!
//! Walks every `*.h` header under `src/include/<SUBTREE>` (recursively) via
//! [`walk_tu_with_diagnostics`] (not [`walk_tu`](ruff_cpp_spo::walk_tu)) and
//! warns loudly when a header's diagnostic list is non-empty. Per that
//! function's own docs: "0 failed" from `walk_tu` alone does NOT mean the
//! sweep is complete — libclang can recover from an unresolved `#include` by
//! treating the rest of the file as best-effort, silently dropping the
//! declaration that needed the missing header (see `examples/harvest_textord.rs`'s
//! `src/viewer` note for the concrete failure mode this was found against).
//!
//! Also reports the **body arm** — `writes` / `reads` / `raises` / `calls` /
//! `guarded_writes` fact totals plus a `RecipeCentroid` census via
//! `ruff_spo_triplet::classify` — the cross-frontend recipe-centroid
//! comparison the Ruby and C# legs already produce, now surfaced for C++.
//! This example walks HEADERS only (no `.cpp` translation units), so most
//! bodies it sees are inline accessors: expect the non-`Empty` fraction to
//! read low relative to a whole-tree harvest — that is this example's scope,
//! not a harvest gap.
//!
//! Expect `guarded_writes` to read **0** on this corpus, and read that as the
//! corpus rather than the detector: a search of the whole `ladybug/src` tree
//! finds three absence tests on a member, and all three are early returns or
//! an assertion — none is the lazy-init `if (!x_) x_ = …` shape the J1 fact
//! records. A corpus that uses that shape is what a J1 hit-rate needs.
//!
//! Env:
//!   `LADYBUG_SRC`   default /home/user/ladybug
//!   `SUBTREE`       relative to src/include, e.g. "storage" or "processor" (default "storage")
//!   `MANIFEST_OUT`  ndjson output path (default /tmp/ladybug_<subtree>_manifest.ndjson)
//!
//! Run:
//! ```sh
//! LADYBUG_SRC=/home/user/ladybug LIBCLANG_PATH=/usr/lib/llvm-18/lib SUBTREE=storage \
//!   cargo run -p ruff_cpp_spo --features libclang --example harvest_ladybug
//! ```
//!
//! This corpus needs `-std=c++20` (already the value baked into the clang
//! `args` below, not overridable via env): under `-std=c++17`,
//! `common/assert.h` fails to parse `std::format` and libclang silently
//! drops the declarations downstream of that failure — measured to cost
//! roughly a third of the body-arm facts (13.0% vs 20.7% of methods
//! reaching a non-`Empty` centroid). This is exactly the partial-AST
//! failure the `walk_tu_with_diagnostics` warning above exists to catch.

#![expect(
    clippy::print_stderr,
    reason = "manifest-emission CLI example (mirrors harvest_textord)"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ruff_cpp_spo::{CppClass, Declaration, NAMESPACE, model_from_class, walk_tu_with_diagnostics};
use ruff_spo_triplet::{ModelGraph, RecipeCentroid, classify, expand, to_ndjson};

/// Recursively collects regular `.h` files beneath a directory, skipping symlinks.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// let mut headers = Vec::new();
/// collect_headers(Path::new("src/include"), &mut headers)?;
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// Returns an error if a directory entry cannot be read or traversed.
fn collect_headers(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        // Ask the ENTRY for its type rather than the path: `Path::is_dir`
        // follows symlinks, so a directory symlink pointing at an ancestor
        // recurses forever and the harvest never finishes. Symlinks are
        // skipped entirely — a header tree's translation units are real
        // files. This is the same guard, for the same reason, that
        // `ruff_cpp_spo::collect_cpp_files` already carries.
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_headers(&path, out)?;
        } else if file_type.is_file() && path.extension().is_some_and(|x| x == "h") {
            out.push(path);
        }
    }
    Ok(())
}

/// Harvests C++ class metadata from the configured Ladybug include subtree and writes an NDJSON SPO manifest.
///
/// The source root, subtree, and manifest path are configured with `LADYBUG_SRC`, `SUBTREE`, and
/// `MANIFEST_OUT`. Missing or unreadable input directories and manifest-write failures are reported
/// as errors; individual header parse failures are collected and reported without aborting the
/// harvest.
///
/// # Examples
///
/// ```no_run
/// let result = main();
/// assert!(result.is_ok());
/// ```
///
/// # Errors
///
/// Returns an error if the configured subtree does not exist, header traversal fails, or the
/// manifest cannot be written.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::var("LADYBUG_SRC").unwrap_or_else(|_| "/home/user/ladybug".to_string());
    let root = Path::new(&root);
    let subtree = std::env::var("SUBTREE").unwrap_or_else(|_| "storage".to_string());
    let sub_dir = root.join("src/include").join(&subtree);
    if !sub_dir.exists() {
        return Err(format!("{} not found; set LADYBUG_SRC/SUBTREE", sub_dir.display()).into());
    }

    let args = [
        "-std=c++20".to_string(),
        "-x".to_string(),
        "c++".to_string(),
        format!("-I{}", root.join("src/include").display()),
    ];

    let mut headers: Vec<PathBuf> = Vec::new();
    collect_headers(&sub_dir, &mut headers)?;
    headers.sort();

    let mut all: Vec<CppClass> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut failed: Vec<(String, String)> = Vec::new();
    // Headers where the parse itself succeeded ("0 failed") but libclang
    // still reported severity>=Error diagnostics — the silent-drop signature
    // documented on `walk_tu_with_diagnostics`. Tracked separately from
    // `failed` because a diagnostic-bearing header still yields a (possibly
    // partial) class list here, unlike a hard `WalkError`.
    let mut warned: Vec<(String, usize)> = Vec::new();
    for h in &headers {
        let name = h.strip_prefix(&sub_dir).unwrap_or(h).display().to_string();
        match walk_tu_with_diagnostics(h, &args) {
            Ok((classes, diagnostics)) => {
                for c in classes {
                    if seen.insert(c.qualified_name()) {
                        all.push(c);
                    }
                }
                if !diagnostics.is_empty() {
                    eprintln!(
                        "[harvest:{subtree}] WARNING: {} unresolved-include error(s) in {name} — class list may be incomplete",
                        diagnostics.len(),
                    );
                    for d in diagnostics.iter().take(5) {
                        eprintln!("    {d}");
                    }
                    warned.push((name, diagnostics.len()));
                }
            }
            Err(e) => failed.push((name, e.to_string())),
        }
    }
    eprintln!(
        "[harvest:{subtree}] {} headers walked, {} unique classes, {} failed, {} with unresolved-include diagnostics",
        headers.len(),
        all.len(),
        failed.len(),
        warned.len(),
    );
    for (name, err) in failed.iter().take(20) {
        eprintln!("[harvest:{subtree}] walk failed in {name}: {err}");
    }
    if failed.len() > 20 {
        eprintln!(
            "[harvest:{subtree}] ... {} more failures omitted",
            failed.len() - 20
        );
    }
    if !warned.is_empty() {
        eprintln!(
            "\n[harvest:{subtree}] headers with unresolved-include diagnostics (class list may be incomplete):"
        );
        for (name, n) in &warned {
            eprintln!("  {name}: {n} error(s)");
        }
    }

    // ── inherits_from edges, grouped by base (root -> subclasses) ──
    eprintln!("\n[{subtree}] inherits_from edges (derived : base):");
    let mut base_to_subs: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for c in &all {
        let bases: Vec<&str> = c
            .declarations
            .iter()
            .filter_map(|d| match d {
                Declaration::Base(b) => Some(b.name.as_str()),
                _ => None,
            })
            .collect();
        if !bases.is_empty() {
            eprintln!("  {} : {}", c.qualified_name(), bases.join(", "));
            for b in bases {
                base_to_subs
                    .entry(b.to_string())
                    .or_default()
                    .push(c.qualified_name());
            }
        }
    }

    eprintln!("\n[{subtree}] base -> subclass counts (sorted by count desc):");
    let mut base_counts: Vec<(&String, &Vec<String>)> = base_to_subs.iter().collect();
    base_counts.sort_by_key(|a| std::cmp::Reverse(a.1.len()));
    for (base, subs) in &base_counts {
        eprintln!("  {base}: {} subclasses", subs.len());
    }

    // ── has_function top-25 ──
    eprintln!("\n[{subtree}] has_function top-25 by method count:");
    let mut counts: Vec<(String, usize, usize)> = all
        .iter()
        .map(|c| {
            let methods: Vec<&_> = c
                .declarations
                .iter()
                .filter_map(|d| match d {
                    Declaration::Method(m) => Some(m),
                    _ => None,
                })
                .collect();
            let overrides = methods.iter().filter(|m| m.overrides.is_some()).count();
            (c.qualified_name(), methods.len(), overrides)
        })
        .collect();
    counts.sort_by_key(|c| std::cmp::Reverse(c.1));
    for (name, n, ov) in counts.iter().take(25) {
        eprintln!("  {name}: {n} methods, {ov} overrides");
    }

    // ── is_pure_virtual classes, ranked by pure-virtual method count ──
    eprintln!("\n[{subtree}] pure-virtual-heavy classes (top 20):");
    let mut pv: Vec<(String, usize)> = all
        .iter()
        .map(|c| {
            let n = c
                .declarations
                .iter()
                .filter_map(|d| match d {
                    Declaration::Method(m) => Some(m),
                    _ => None,
                })
                .filter(|m| m.is_pure_virtual)
                .count();
            (c.qualified_name(), n)
        })
        .filter(|(_, n)| *n > 0)
        .collect();
    pv.sort_by_key(|p| std::cmp::Reverse(p.1));
    for (name, n) in pv.iter().take(20) {
        eprintln!("  {name}: {n} pure-virtual methods");
    }

    // ── virtually_overrides count ──
    let virt_overrides: usize = all
        .iter()
        .flat_map(|c| c.declarations.iter())
        .filter_map(|d| match d {
            Declaration::Method(m) => Some(m),
            _ => None,
        })
        .filter(|m| m.overrides.is_some())
        .count();
    eprintln!("\n[{subtree}] total virtually_overrides edges: {virt_overrides}");

    // ── fields / templates / static_asserts / friends ──
    let n_fields: usize = all
        .iter()
        .flat_map(|c| c.declarations.iter())
        .filter(|d| matches!(d, Declaration::Field(_)))
        .count();
    let n_templates: usize = all
        .iter()
        .flat_map(|c| c.declarations.iter())
        .filter(|d| matches!(d, Declaration::Template(_)))
        .count();
    let n_static_asserts: usize = all
        .iter()
        .flat_map(|c| c.declarations.iter())
        .filter(|d| matches!(d, Declaration::StaticAssert(_)))
        .count();
    let n_friends: usize = all
        .iter()
        .flat_map(|c| c.declarations.iter())
        .filter(|d| matches!(d, Declaration::Friend(_)))
        .count();
    eprintln!(
        "\n[{subtree}] has_field={n_fields} template_specialises/instantiates={n_templates} static_asserts={n_static_asserts} is_friend_of={n_friends}"
    );

    // top-10 classes by template decl count (heavy specialisation signal)
    let mut tmpl_counts: Vec<(String, usize)> = all
        .iter()
        .map(|c| {
            let n = c
                .declarations
                .iter()
                .filter(|d| matches!(d, Declaration::Template(_)))
                .count();
            (c.qualified_name(), n)
        })
        .filter(|(_, n)| *n > 0)
        .collect();
    tmpl_counts.sort_by_key(|t| std::cmp::Reverse(t.1));
    if !tmpl_counts.is_empty() {
        eprintln!("\n[{subtree}] top classes by template decl count:");
        for (name, n) in tmpl_counts.iter().take(10) {
            eprintln!("  {name}: {n} template decls");
        }
    }

    // ── body arm: writes / reads / raises / calls / guarded_writes ──
    // This example walks HEADERS only (no `.cpp` translation units), so most
    // bodies visible here are inline accessors — a low non-Empty fraction is
    // this example's scope, not a harvest gap (see the module doc header).
    let all_methods: Vec<&_> = all
        .iter()
        .flat_map(|c| c.declarations.iter())
        .filter_map(|d| match d {
            Declaration::Method(m) => Some(m),
            _ => None,
        })
        .collect();
    let with_body_facts = all_methods
        .iter()
        .filter(|m| {
            !m.writes.is_empty()
                || !m.reads.is_empty()
                || !m.raises.is_empty()
                || !m.calls.is_empty()
                || !m.guarded_writes.is_empty()
        })
        .count();
    eprintln!(
        "\n[{subtree}] body arm: {} methods, {with_body_facts} with at least one body fact",
        all_methods.len(),
    );

    let n_writes: usize = all_methods.iter().map(|m| m.writes.len()).sum();
    let n_reads: usize = all_methods.iter().map(|m| m.reads.len()).sum();
    let n_raises: usize = all_methods.iter().map(|m| m.raises.len()).sum();
    let n_calls: usize = all_methods.iter().map(|m| m.calls.len()).sum();
    let n_guarded_writes: usize = all_methods.iter().map(|m| m.guarded_writes.len()).sum();
    eprintln!(
        "[{subtree}] body-arm fact counts: writes={n_writes} reads={n_reads} raises={n_raises} calls={n_calls} guarded_writes={n_guarded_writes}"
    );

    // Recipe-centroid census — the headline: the same classifier the Ruby and
    // C# legs already run against `Function`, here run against `CppMethod`
    // via the shared `BodyFacts` trait (`ruff_spo_triplet::recipe`).
    let centroids = [
        RecipeCentroid::Compensate,
        RecipeCentroid::Cascade,
        RecipeCentroid::Guard,
        RecipeCentroid::WriteRaise,
        RecipeCentroid::Default,
        RecipeCentroid::Compute,
        RecipeCentroid::Normalize,
        RecipeCentroid::Observe,
        RecipeCentroid::Empty,
    ];
    let classified: Vec<RecipeCentroid> = all_methods.iter().map(|m| classify(*m)).collect();
    let mut census: Vec<(RecipeCentroid, usize)> = centroids
        .iter()
        .map(|&c| (c, classified.iter().filter(|&&x| x == c).count()))
        .collect();
    census.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    eprintln!("\n[{subtree}] recipe-centroid census (most common first):");
    for (centroid, n) in &census {
        eprintln!("  {centroid:?}: {n}");
    }

    // top-10 classes by body-fact count (same shape as the template-decl list
    // above — sum of writes/reads/raises/calls/guarded_writes per class).
    let mut body_fact_counts: Vec<(String, usize)> = all
        .iter()
        .map(|c| {
            let methods: Vec<&_> = c
                .declarations
                .iter()
                .filter_map(|d| match d {
                    Declaration::Method(m) => Some(m),
                    _ => None,
                })
                .collect();
            let n: usize = methods
                .iter()
                .map(|m| {
                    m.writes.len()
                        + m.reads.len()
                        + m.raises.len()
                        + m.calls.len()
                        + m.guarded_writes.len()
                })
                .sum();
            (c.qualified_name(), n)
        })
        .filter(|(_, n)| *n > 0)
        .collect();
    body_fact_counts.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    if !body_fact_counts.is_empty() {
        eprintln!("\n[{subtree}] top classes by body-fact count:");
        for (name, n) in body_fact_counts.iter().take(10) {
            eprintln!("  {name}: {n} body facts");
        }
    }

    // Emit the full ndjson manifest — what lance-graph's SPO store + the
    // ladybug-rs codegen consume.
    let mut graph = ModelGraph::new(NAMESPACE);
    for c in &all {
        graph.models.push(model_from_class(c));
    }
    let triples = expand(&graph);
    let ndjson = to_ndjson(&triples);
    let out_path = std::env::var("MANIFEST_OUT")
        .unwrap_or_else(|_| format!("/tmp/ladybug_{subtree}_manifest.ndjson"));
    std::fs::write(&out_path, &ndjson)?;
    eprintln!(
        "\n[harvest:{subtree}] {} models -> {} triples, {} ndjson bytes -> {out_path}",
        graph.models.len(),
        triples.len(),
        ndjson.len()
    );

    Ok(())
}
