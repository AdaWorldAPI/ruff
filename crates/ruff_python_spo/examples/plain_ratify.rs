//! Dev probe: run `ratify_optional_unwrap` over every `*.py` file under a
//! source tree, aggregate the report, and print it — the promotion gate
//! for the ONE drill rule wired end-to-end
//! ([`ruff_python_spo::PlainDrillConfig::unwrap_optional_annotation`]).
//! Prints every file whose ratification fails, so a non-ratifying corpus
//! is visible per-file, not just as an aggregate number.
//!
//! Usage: `cargo run -p ruff_python_spo --example plain_ratify -- <root>`

#![expect(
    clippy::print_stdout,
    reason = "a dev probe's stdout report IS its deliverable"
)]

use std::fs;
use std::path::Path;

use ruff_python_spo::ratify_optional_unwrap;

fn collect_py_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_py_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "py") {
            out.push(path);
        }
    }
}

fn main() {
    let root = std::env::args().nth(1).expect("usage: plain_ratify <root>");
    let root = Path::new(&root);

    let mut files = Vec::new();
    collect_py_files(root, &mut files);

    let mut baseline_unresolved = 0usize;
    let mut newly_resolved = 0usize;
    let mut independently_verified = 0usize;
    let mut failing_files = 0usize;

    for path in &files {
        let Ok(src) = fs::read_to_string(path) else {
            continue;
        };
        let module = path.to_string_lossy();
        let report = ratify_optional_unwrap(&src, &module);
        baseline_unresolved += report.baseline_unresolved;
        newly_resolved += report.newly_resolved_sites;
        independently_verified += report.independently_shape_verified;
        if report.baseline_unresolved > 0 && !report.ratified() {
            failing_files += 1;
            println!("NOT RATIFIED: {} -> {report:?}", path.display());
        }
    }

    println!("files scanned: {}", files.len());
    println!(
        "baseline_unresolved={baseline_unresolved} newly_resolved={newly_resolved} independently_verified={independently_verified}"
    );
    println!(
        "still_unresolved_after_rule={} (non-optional binop shapes: chained unions, `int | str`, …)",
        baseline_unresolved - newly_resolved
    );
    println!(
        "ratified: {}",
        if failing_files == 0 { "YES" } else { "NO" }
    );
}
