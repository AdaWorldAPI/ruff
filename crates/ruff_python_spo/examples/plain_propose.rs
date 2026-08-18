//! Dev probe: run the plain-Python residual proposer over 2+ corpora and
//! print candidate config rows as TOML — data, not Rust — with each row's
//! `scope` (`generic` iff it cleared `min_support` in EVERY supplied
//! corpus, `corpus_scoped` otherwise). See `drill.rs`'s module doc for
//! exactly what this claims (grouping + classification) and what it does
//! NOT claim (a promotion gate — that needs a config-consuming
//! extractor this probe does not build).
//!
//! Usage: `cargo run -p ruff_python_spo --example plain_propose -- \
//!     <min_support> <label1>=<root1> [<label2>=<root2> ...]`
//! (2+ corpora required — see `classify_across_corpora`'s doc comment.)

#![expect(
    clippy::print_stdout,
    reason = "a dev probe's stdout report IS its deliverable"
)]

use std::path::Path;

use ruff_python_spo::{RowScope, classify_across_corpora, extract_plain_with_residuals};

fn main() {
    let mut args = std::env::args().skip(1);
    let min_support: usize = args
        .next()
        .expect("usage: plain_propose <min_support> <label>=<root> [<label>=<root> ...]")
        .parse()
        .expect("min_support must be a positive integer");

    let labeled: Vec<(String, Vec<ruff_python_spo::PlainResidual>)> = args
        .map(|arg| {
            let (label, root) = arg
                .split_once('=')
                .unwrap_or_else(|| panic!("expected <label>=<root>, got {arg:?}"));
            let (_, residuals) = extract_plain_with_residuals(Path::new(root), "py");
            (label.to_string(), residuals)
        })
        .collect();

    assert!(
        labeled.len() >= 2,
        "need 2+ corpora for a meaningful generic/corpus_scoped split; got {}",
        labeled.len()
    );

    let rows = classify_across_corpora(&labeled, min_support);
    let generic = rows.iter().filter(|r| r.scope == RowScope::Generic).count();
    let scoped = rows.len() - generic;
    println!(
        "# {} candidate rows ({generic} generic, {scoped} corpus_scoped) from {} corpora, min_support={min_support}",
        rows.len(),
        labeled.len()
    );
    for row in &rows {
        let scope = match row.scope {
            RowScope::Generic => "generic",
            RowScope::CorpusScoped => "corpus_scoped",
        };
        println!("[[candidate]]");
        println!("reason = \"{}\"", row.reason.as_str());
        println!("detail = \"{}\"", row.detail);
        println!("scope = \"{scope}\"");
        print!("per_corpus = [");
        for (i, (label, support)) in row.per_corpus.iter().enumerate() {
            if i > 0 {
                print!(", ");
            }
            print!("{{ corpus = \"{label}\", support = {support} }}");
        }
        println!("]");
        println!();
    }
}
