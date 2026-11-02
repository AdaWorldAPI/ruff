//! `ruff_charon calls|rows|kythe <file.llbc> <crate_name> [--corpus C] [--strip PREFIX]`
//!
//! - `calls`: the `llbc_calls.py` report, byte for byte.
//! - `rows`: `relation subject object grade file start end`, then `residual` rows and one
//!   `receipt` row. Exit 3 if the receipt does not conserve.
//! - `kythe`: Kythe entries, one JSON object per line.
//!
//! The offline store (`ruff_charon::store`):
//! - `import <dir> <file.llbc> <crate_name>`: import or refresh one crate;
//! - `stale <dir> <workspace root>`: the crates with a changed or missing file;
//! - `dangling <dir>`: call rows whose callee no stored crate defines. Exit 1 if any.

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = store_mode(&args) {
        return code;
    }
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

fn store_mode(args: &[String]) -> Option<ExitCode> {
    use ruff_charon::store::Store;
    let fail = |e: std::io::Error| {
        eprintln!("ruff_charon: {e}");
        ExitCode::from(2)
    };
    match (
        args.get(1).map(String::as_str),
        args.get(2),
        args.get(3),
        args.get(4),
    ) {
        (Some("import"), Some(dir), Some(file), Some(krate)) => {
            let llbc = match ruff_charon::load(Path::new(file)) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("ruff_charon: {e:#}");
                    return Some(ExitCode::from(2));
                }
            };
            Some(match Store::new(dir).import(&llbc, krate) {
                Ok(r) => {
                    println!(
                        "imported\t{krate}\tharvested={}\trepresented={}\tresidual={}",
                        r.harvested, r.represented, r.residual
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            })
        }
        (Some("stale"), Some(dir), Some(root), _) => {
            Some(match Store::new(dir).stale(Path::new(root)) {
                Ok(set) => {
                    for c in set {
                        println!("stale\t{c}");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            })
        }
        (Some("dangling"), Some(dir), _, _) => Some(match Store::new(dir).dangling() {
            Ok(rows) => {
                for r in &rows {
                    println!("dangling\t{}\t{}\t{}", r.predicate, r.subject, r.object);
                }
                if rows.is_empty() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(1)
                }
            }
            Err(e) => fail(e),
        }),
        _ => None,
    }
}
