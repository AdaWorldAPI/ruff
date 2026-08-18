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

use ruff_python_spo::{extract, extract_plain};

fn main() {
    let root = std::env::args().nth(1).expect("usage: plain_census <root>");
    let root = Path::new(&root);

    // The Odoo baseline over the SAME root — measured, not asserted.
    let odoo_graph = extract(root);
    println!("odoo baseline: models={}", odoo_graph.models.len());

    let graph = extract_plain(root, "py");

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
}
