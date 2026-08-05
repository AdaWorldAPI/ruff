//! Dev probe: run the Odoo Klickweg harvest over a real addon.
//! Usage: `cargo run -p ruff_python_spo --example odoo_nav_probe -- <addon_root>`

#![expect(
    clippy::print_stdout,
    reason = "a dev probe's stdout report IS its deliverable"
)]

use ruff_python_spo::{NavVocab, extract_odoo_nav_edges_with_report};

fn main() {
    let root = std::env::args()
        .nth(1)
        .expect("usage: odoo_nav_probe <addon_root>");
    // Vocab: the account-addon core screens (normalized model names).
    let screens = [
        "account_move",
        "account_move_line",
        "res_partner",
        "res_company",
        "account_account",
        "account_journal",
        "account_payment",
        "account_bank_statement",
        "account_analytic_line",
        "account_tax",
    ];
    let vocab = NavVocab {
        screens: screens.iter().map(|s| (*s).to_string()).collect(),
    };
    let (edges, report) = extract_odoo_nav_edges_with_report(std::path::Path::new(&root), &vocab);
    println!(
        "py={} xml={} raw_act_window={} files_with_edges={} edges={}",
        report.py_files,
        report.xml_files,
        report.raw_act_window_refs,
        report.files_with_edges,
        edges.len()
    );
    for e in &edges {
        println!("  {} -> {}  [{}]  {}", e.source, e.target, e.via, e.file);
    }
}
