//! Load + validate a harvested ndjson triple dump, then classify every
//! function through `ruff_spo_triplet::recipe::classify` and report the
//! metal/slag centroid distribution.
//!
//! Corpus-agnostic: takes the ndjson path (and namespace) as CLI args, so it
//! runs against any harvested corpus — not a fixture, not a hardcoded path.
//! The load step doubles as the schema gate: a harvester bug that emits a
//! predicate outside the closed `Predicate` vocabulary fails here, not later.
//!
//! ```sh
//! cargo run -p ruff_csharp_spo --example recipe_census -- \
//!   triples.ndjson --ns medcare --exclude-prefix Crypt --exclude-prefix Mime
//! ```

#![allow(clippy::print_stdout, clippy::print_stderr)]

use ruff_spo_triplet::{RecipeCentroid, classify, is_recoverable, reassemble_model_graph};
use std::collections::BTreeMap;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: recipe_census <triples.ndjson> [--ns NAME] [--exclude-prefix PREFIX]...");
        std::process::exit(2);
    };

    let mut namespace = "csharp".to_string();
    let mut exclude_prefixes: Vec<String> = Vec::new();
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--ns" => namespace = args.next().expect("--ns needs a value"),
            "--exclude-prefix" => {
                exclude_prefixes.push(args.next().expect("--exclude-prefix needs a value"));
            }
            other => {
                eprintln!("unknown flag: {other}");
                std::process::exit(2);
            }
        }
    }

    let ndjson = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!("failed to read {path}: {e}");
        std::process::exit(1);
    });
    let triples = ruff_csharp_spo::load(&ndjson).unwrap_or_else(|e| {
        eprintln!("schema-invalid harvest: {e:?}");
        std::process::exit(1);
    });
    println!("loaded {} triples, schema-valid", triples.len());

    let graph = reassemble_model_graph(&triples, &namespace);
    let is_excluded = |name: &str| {
        exclude_prefixes
            .iter()
            .any(|p| name.starts_with(p.as_str()))
    };

    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut examples: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    let mut recoverable = 0usize;
    let mut essential = 0usize;
    let mut total = 0usize;
    let mut excluded_models = 0usize;

    for model in &graph.models {
        if is_excluded(&model.name) {
            excluded_models += 1;
            continue;
        }
        for f in model.functions.iter().chain(model.helpers.iter()) {
            total += 1;
            let c = classify(f);
            let name = centroid_name(c);
            *counts.entry(name).or_insert(0) += 1;
            if is_recoverable(c) {
                recoverable += 1;
            } else {
                essential += 1;
            }
            let ex = examples.entry(name).or_default();
            if ex.len() < 3 {
                ex.push(format!("{}::{}", model.name, f.name));
            }
        }
    }

    if !exclude_prefixes.is_empty() {
        println!("excluded {excluded_models} model(s) matching {exclude_prefixes:?}");
    }
    println!();
    println!("total functions classified: {total}");
    println!("recoverable (codegen-eligible): {recoverable}");
    println!("essential (hand-keep/slag): {essential}");
    println!();
    println!("centroid distribution:");
    for (name, n) in &counts {
        #[allow(clippy::cast_precision_loss)]
        // intentional: percentage display, precision loss immaterial at these counts
        let pct = 100.0 * (*n as f64) / (total.max(1) as f64);
        println!("  {n:6}  ({pct:5.1}%)  {name}");
        if let Some(ex) = examples.get(name) {
            for e in ex {
                println!("           e.g. {e}");
            }
        }
    }
}

fn centroid_name(c: RecipeCentroid) -> &'static str {
    match c {
        RecipeCentroid::Compensate => "Compensate",
        RecipeCentroid::Cascade => "Cascade",
        RecipeCentroid::Guard => "Guard",
        RecipeCentroid::WriteRaise => "WriteRaise",
        RecipeCentroid::Default => "Default",
        RecipeCentroid::Compute => "Compute",
        RecipeCentroid::Normalize => "Normalize",
        RecipeCentroid::Observe => "Observe",
        RecipeCentroid::Empty => "Empty",
    }
}
