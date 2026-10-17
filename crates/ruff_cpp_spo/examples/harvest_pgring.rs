//! Harvest the PG-Ring graph query engine (Gómez-Brandón, Hogan, Navarro,
//! ISWC 2026, arXiv 2608.03840) into a structural manifest: classes, their
//! fields and method signatures, and the call graph of every function body.
//!
//! PG-Ring is GPL-3.0. This example reads the source only to extract the
//! STRUCTURE (which iterators exist, which methods they offer, who calls whom)
//! that a clean-room reimplementation is designed against. No code is copied,
//! and the corpus is never vendored here.
//!
//! Run:
//! ```sh
//! PGRING_SRC=/home/user/adaworldapi/pg-ring \
//! SDSL_INCLUDE=/home/user/adaworldapi/sdsl-lite/include \
//! LIBCLANG_PATH=/usr/lib/llvm-18/lib \
//!   cargo run -p ruff_cpp_spo --features libclang --example harvest_pgring
//! ```
//!
//! Outputs (directory `OUT_DIR`, default `/tmp/pgring_harvest`):
//! - `manifest.ndjson`: the SPO triples of every class (shared IR).
//! - `<file>.txt`: one readable summary per source file: classes with bases,
//!   fields and method signatures, then every function with its callees.

#![expect(
    clippy::print_stderr,
    reason = "manifest-emission CLI example (mirrors harvest_textord)"
)]

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use ruff_cpp_spo::{
    CppClass, Declaration, NAMESPACE, model_from_class, walk_free_functions_with_diagnostics,
    walk_tu_with_diagnostics,
};
use ruff_spo_triplet::{ModelGraph, expand, to_ndjson};

/// Every `.hpp`, `.h`, `.cpp` and `.c` under `dir`, recursively, sorted.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if p.is_dir() {
            sources(&p, out);
        } else if p
            .extension()
            .is_some_and(|e| e == "hpp" || e == "h" || e == "cpp" || e == "c")
        {
            out.push(p);
        }
    }
}

fn summarize_class(c: &CppClass, s: &mut String) {
    let _ = writeln!(s, "class {}", c.qualified_name());
    for d in &c.declarations {
        match d {
            Declaration::Base(b) => {
                let _ = writeln!(s, "  base {}", b.name);
            }
            Declaration::Field(f) => {
                let _ = writeln!(s, "  field {}: {}", f.name, f.type_name);
            }
            Declaration::Method(m) => {
                let ret = m.return_type.as_deref().unwrap_or("void");
                let flags = if m.is_static { " static" } else { "" };
                let _ = writeln!(
                    s,
                    "  method {}({}) -> {ret}{flags}",
                    m.name,
                    m.param_types.join(", ")
                );
            }
            Declaration::Template(t) => {
                let _ = writeln!(s, "  template {:?} {}", t.kind, t.name);
            }
            _ => {}
        }
    }
}

/// Library namespaces pulled in through `#include` that are not PG-Ring's own
/// code. Their declarations reach every translation unit; keeping them would
/// bury the engine under SDSL and the standard library.
const FOREIGN: [&str; 3] = ["sdsl", "std", "__gnu_cxx"];

fn is_foreign(qualified: &str) -> bool {
    qualified.starts_with("__")
        || FOREIGN
            .iter()
            .any(|ns| qualified == *ns || qualified.starts_with(&format!("{ns}::")))
}

/// The source file that DEFINES `name` (a `class name` / `struct name` not
/// followed by `;`), found by text scan. libclang reports a class in every
/// translation unit that includes it, so the walk alone cannot attribute it.
fn defining_file<'a>(name: &str, texts: &'a [(String, String)]) -> Option<&'a str> {
    texts.iter().find_map(|(rel, text)| {
        ["class ", "struct "].iter().find_map(|kw| {
            let needle = format!("{kw}{name}");
            text.match_indices(&needle).find_map(|(i, _)| {
                let rest = &text[i + needle.len()..];
                let next = rest.chars().next()?;
                if next.is_alphanumeric() || next == '_' {
                    return None;
                }
                let decl_end = rest.find(['{', ';'])?;
                (rest.as_bytes()[decl_end] == b'{').then_some(rel.as_str())
            })
        })
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(
        std::env::var("PGRING_SRC").unwrap_or_else(|_| "/home/user/adaworldapi/pg-ring".into()),
    );
    let sdsl = std::env::var("SDSL_INCLUDE")
        .unwrap_or_else(|_| "/home/user/adaworldapi/sdsl-lite/include".into());
    let out_dir =
        PathBuf::from(std::env::var("OUT_DIR").unwrap_or_else(|_| "/tmp/pgring_harvest".into()));
    std::fs::create_dir_all(&out_dir)?;

    let mut files = Vec::new();
    sources(&root.join("include"), &mut files);
    sources(&root.join("src"), &mut files);
    if files.is_empty() {
        return Err(format!("no sources under {}; set PGRING_SRC", root.display()).into());
    }

    let args = [
        "-std=c++17".to_string(),
        "-x".to_string(),
        "c++".to_string(),
        format!("-I{}", root.join("include").display()),
        format!("-I{}", root.join("libCSD").display()),
        format!("-I{sdsl}"),
    ];

    let texts: Vec<(String, String)> = files
        .iter()
        .filter_map(|f| {
            let rel = f.strip_prefix(&root).unwrap_or(f).display().to_string();
            std::fs::read_to_string(f).ok().map(|t| (rel, t))
        })
        .collect();

    let mut all: Vec<CppClass> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut functions: Vec<(String, String)> = Vec::new();
    let mut seen_fn: BTreeSet<String> = BTreeSet::new();
    let mut diag_report = String::new();
    for f in &files {
        let rel = f.strip_prefix(&root).unwrap_or(f).display().to_string();
        match walk_tu_with_diagnostics(f, &args) {
            Ok((classes, diags)) => {
                if !diags.is_empty() {
                    let _ = writeln!(diag_report, "{rel}: {} error(s)", diags.len());
                    for d in diags.iter().take(3) {
                        let _ = writeln!(diag_report, "  {d}");
                    }
                }
                for c in classes {
                    if !is_foreign(&c.qualified_name()) && seen.insert(c.qualified_name()) {
                        all.push(c);
                    }
                }
            }
            Err(e) => {
                let _ = writeln!(diag_report, "{rel}: class walk failed: {e}");
            }
        }
        match walk_free_functions_with_diagnostics(f, &args) {
            Ok((fns, _)) => {
                for func in fns {
                    let q = func.qualified_name();
                    let sig = format!("{q}({})", func.param_types.join(", "));
                    if is_foreign(&q) || !seen_fn.insert(sig.clone()) {
                        continue;
                    }
                    let ret = func.return_type.as_deref().unwrap_or("void");
                    let line = format!("fn {sig} -> {ret}\n  calls: {}\n", func.calls.join(", "));
                    // The file whose text holds `name(` attributes the
                    // definition; the walk sees it in every including TU.
                    let tail = func.name.clone();
                    let home = texts
                        .iter()
                        .find(|(_, t)| t.contains(&format!("{tail}(")))
                        .map_or_else(|| rel.clone(), |(r, _)| r.clone());
                    functions.push((home, line));
                }
            }
            Err(e) => {
                let _ = writeln!(diag_report, "{rel}: function walk failed: {e}");
            }
        }
    }

    for (rel, _) in &texts {
        let mut s = format!("# {rel}\n");
        for c in &all {
            if defining_file(&c.name, &texts) == Some(rel.as_str()) {
                summarize_class(c, &mut s);
            }
        }
        for (home, line) in &functions {
            if home == rel {
                s.push_str(line);
            }
        }
        std::fs::write(out_dir.join(format!("{}.txt", rel.replace('/', "__"))), &s)?;
    }
    let unattributed: Vec<&str> = all
        .iter()
        .filter(|c| defining_file(&c.name, &texts).is_none())
        .map(|c| c.name.as_str())
        .collect();
    let _ = writeln!(
        diag_report,
        "\nclasses with no defining file found: {unattributed:?}"
    );
    std::fs::write(out_dir.join("_diagnostics.txt"), &diag_report)?;
    let n_functions = functions.len();
    let warned = diag_report
        .lines()
        .filter(|l| l.contains(" error(s)"))
        .count();

    let mut graph = ModelGraph::new(NAMESPACE);
    for c in &all {
        graph.models.push(model_from_class(c));
    }
    let triples = expand(&graph);
    std::fs::write(out_dir.join("manifest.ndjson"), to_ndjson(&triples))?;
    eprintln!(
        "[harvest] {} files, {} unique classes, {} function definitions, {} files with parse errors, {} triples -> {}",
        files.len(),
        all.len(),
        n_functions,
        warned,
        triples.len(),
        out_dir.display()
    );
    Ok(())
}
