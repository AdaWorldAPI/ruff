//! `ruff_charon calls|rows|kythe <file.llbc> <crate_name> [--corpus C] [--strip PREFIX]`
//!
//! - `calls`: the `llbc_calls.py` report, byte for byte.
//! - `rows`: `relation subject object grade file start end`, then `residual` rows and one
//!   `receipt` row. Exit 3 if the receipt does not conserve.
//! - `kythe`: Kythe entries, one JSON object per line.

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let (Some(mode), Some(file), Some(krate)) = (args.get(1), args.get(2), args.get(3)) else {
        eprintln!(
            "usage: ruff_charon calls|rows|kythe <file.llbc> <crate_name> [--corpus C] [--strip PREFIX]"
        );
        return ExitCode::from(2);
    };
    let llbc = match ruff_charon::load(Path::new(file)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("ruff_charon: {e:#}");
            return ExitCode::from(2);
        }
    };
    match mode.as_str() {
        "calls" => print!("{}", ruff_charon::calls_text(&llbc, krate)),
        "rows" => {
            let (rows, residual, receipt) = ruff_charon::relations(&llbc.krate, krate);
            for r in &rows {
                let (f, s, e) = r
                    .site
                    .as_ref()
                    .map_or(("-".into(), String::new(), String::new()), |s| {
                        (s.file.clone(), s.start.to_string(), s.end.to_string())
                    });
                println!(
                    "{}\t{}\t{}\t{}\t{f}\t{s}\t{e}",
                    r.predicate,
                    r.subject,
                    r.object,
                    ruff_charon::GRADE
                );
            }
            for r in &residual {
                println!("residual\t{}\t{}", r.reason, r.subject);
            }
            println!(
                "receipt\tharvested={}\trepresented={}\tresidual={}\tconserved={}\thas_errors={}",
                receipt.harvested,
                receipt.represented,
                receipt.residual,
                receipt.conserved(),
                llbc.has_errors
            );
            if !receipt.conserved() {
                return ExitCode::from(3);
            }
        }
        "kythe" => {
            let corpus = flag("--corpus").unwrap_or_else(|| "lance-graph".into());
            let strip = flag("--strip").unwrap_or_default();
            for e in ruff_charon::kythe_entries(&llbc.krate, krate, &corpus, &strip) {
                println!("{e}");
            }
        }
        _ => {
            eprintln!("unknown mode {mode}");
            return ExitCode::from(2);
        }
    }
    ExitCode::SUCCESS
}
