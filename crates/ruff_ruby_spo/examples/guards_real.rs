//! Prints the contract-guard report for a directory of a real `OpenProject`
//! checkout. Skips unless `OPENPROJECT_SRC` is set.
//!
//! `OPENPROJECT_SRC=/path/to/openproject cargo run -p ruff_ruby_spo --example guards_real`

#![expect(
    clippy::print_stdout,
    reason = "measurement CLI example (mirrors ore_census)"
)]

use std::path::PathBuf;

use ruff_ruby_spo::{GuardOutcome, extract_contract_guards_with_report};

fn main() {
    let Some(src) = std::env::var_os("OPENPROJECT_SRC") else {
        println!("skip: OPENPROJECT_SRC not set");
        return;
    };
    // `app/contracts` as a whole, so predicates in `concerns/` are scanned.
    let root = PathBuf::from(src).join("app/contracts");
    let (guards, report) = extract_contract_guards_with_report(&root);
    println!("{report:?}");
    for g in guards
        .iter()
        .filter(|g| g.file.starts_with("work_packages/"))
    {
        match &g.outcome {
            GuardOutcome::Lowered(e) => {
                println!("LOWERED   {}#{} = {e:?}", g.class, g.attribute);
            }
            GuardOutcome::Unlowered { reason } => {
                println!("UNLOWERED {}#{}: {reason}", g.class, g.attribute);
            }
        }
    }
}
