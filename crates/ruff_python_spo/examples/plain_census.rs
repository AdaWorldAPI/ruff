//! Dev probe: run the plain-Python harvest over any Python source tree and
//! report totals — alongside the Odoo baseline over the *same* root, so the
//! "Odoo gate drops this corpus" claim is measured here, not asserted.
//! Usage: `cargo run -p ruff_python_spo --example plain_census -- <root>`

#![expect(
    clippy::print_stdout,
    reason = "a dev probe's stdout report IS its deliverable"
)]

use std::collections::HashMap;
use std::path::Path;

use ruff_python_spo::{PlainResidualReason, extract, extract_plain_with_residuals};

fn main() {
    let root = std::env::args().nth(1).expect("usage: plain_census <root>");
    let root = Path::new(&root);

    // The Odoo baseline over the SAME root — measured, not asserted.
    let odoo_graph = extract(root);
    println!("odoo baseline: models={}", odoo_graph.models.len());

    let (graph, residuals) = extract_plain_with_residuals(root, "py");

    let mut functions = 0usize;
    let mut fields = 0usize;
    let mut inherits_edges = 0usize;
    let mut reads = 0usize;
    let mut writes = 0usize;
    let mut raises = 0usize;
    let mut traverses = 0usize;
    let mut guarded_writes = 0usize;
    let mut calls = 0usize;
    let mut call_histogram: HashMap<String, usize> = HashMap::new();

    for model in &graph.models {
        functions += model.functions.len();
        fields += model.fields.len();
        inherits_edges += model.inherits.len();
        for f in &model.functions {
            reads += f.reads.len();
            writes += f.writes.len();
            raises += f.raises.len();
            traverses += f.traverses.len();
            guarded_writes += f.guarded_writes.len();
            calls += f.calls.len();
            for c in &f.calls {
                *call_histogram.entry(c.clone()).or_insert(0) += 1;
            }
        }
    }

    println!(
        "plain: models={} functions={} fields={} inherits_edges={}",
        graph.models.len(),
        functions,
        fields,
        inherits_edges
    );
    println!(
        "  body facts: reads={reads} writes={writes} raises={raises} traverses={traverses} guarded_writes={guarded_writes} calls={calls}"
    );

    // Model-name collisions: `a/b.py`'s synthetic module model and a class
    // `b` in `a.py` both normalise to `a_b`. `ModelGraph::models` is a `Vec`,
    // so nothing is silently merged HERE — but any consumer that keys by name
    // (the SPO expansion, `ogar-from-ruff`'s lift to `ogar_vocab::Class`)
    // would collide. Measured rather than assumed to be zero.
    let mut name_counts: HashMap<&str, usize> = HashMap::new();
    for model in &graph.models {
        *name_counts.entry(model.name.as_str()).or_insert(0) += 1;
    }
    let mut dupes: Vec<(&str, usize)> = name_counts
        .iter()
        .filter(|&(_, &n)| n > 1)
        .map(|(&k, &v)| (k, v))
        .collect();
    dupes.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    println!(
        "  duplicate model names: {} name(s) covering {} models",
        dupes.len(),
        dupes.iter().map(|(_, n)| n).sum::<usize>()
    );
    for (name, n) in dupes.iter().take(5) {
        println!("    {n:>3}x  {name}");
    }

    let mut hist: Vec<(String, usize)> = call_histogram.into_iter().collect();
    hist.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    println!("top-10 called names:");
    for (name, count) in hist.into_iter().take(10) {
        println!("  {count:>4}  {name}");
    }

    // The residual histogram — where the harvest contributed less than it
    // could have, grouped by named reason. This is the drill signal: a
    // concentrated histogram says where a config row would pay off; a flat
    // one says there is nothing to drill toward.
    println!("residuals: {} rows", residuals.len());
    let mut by_reason: HashMap<&'static str, usize> = HashMap::new();
    let mut by_detail: HashMap<(&'static str, String), usize> = HashMap::new();
    for r in &residuals {
        *by_reason.entry(r.reason.as_str()).or_insert(0) += 1;
        if matches!(
            r.reason,
            PlainResidualReason::CurieConstant
                | PlainResidualReason::UnresolvedAnnotation
                | PlainResidualReason::NonLiteralAssign
                | PlainResidualReason::UnresolvedBase
        ) && let Some(detail) = &r.detail
        {
            *by_detail
                .entry((r.reason.as_str(), detail.clone()))
                .or_insert(0) += 1;
        }
    }
    let mut reasons: Vec<(&str, usize)> = by_reason.into_iter().collect();
    reasons.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    for (reason, count) in &reasons {
        println!("  {count:>5}  {reason}");
    }
    let mut details: Vec<((&str, String), usize)> = by_detail.into_iter().collect();
    details.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    println!("top-15 residual details (reason/detail):");
    for ((reason, detail), count) in details.into_iter().take(15) {
        println!("  {count:>5}  {reason}/{detail}");
    }
}
