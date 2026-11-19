use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use ruff_codegraph::{Relation, project, relation};

const USAGE: &str = "usage:
  ruff_codegraph project <config.toml> <out_dir>
  ruff_codegraph order <store_dir> <ns> <scope> <a> <b>";

fn run(args: &[String]) -> Result<String, String> {
    match args {
        [cmd, config, out] if cmd == "project" => {
            let receipt = project(Path::new(config), Path::new(out)).map_err(|e| e.to_string())?;
            Ok(format!(
                "projected {} files, {} relation rows",
                receipt.snapshot.files,
                receipt.cardinality.rows.values().sum::<u32>()
            ))
        }
        [cmd, dir, ns, scope, a, b] if cmd == "order" => {
            let answer = relation(Path::new(dir), ns, scope, a, b).map_err(|e| e.to_string())?;
            Ok(match answer {
                Relation::Before | Relation::After | Relation::Unknown => {
                    answer.as_str().to_owned()
                }
            })
        }
        _ => Err(USAGE.to_owned()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(line) => {
            let _ = writeln!(std::io::stdout(), "{line}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            let _ = writeln!(std::io::stderr(), "ruff_codegraph: {message}");
            ExitCode::FAILURE
        }
    }
}
