//! Census of a MeTTa corpus: rules, declarations, and how many rule heads are
//! lowerable or interpreter-only, by reason.
//!
//! ```text
//! cargo run --manifest-path crates/ruff_metta_spo/Cargo.toml --example metta_census -- \
//!     [--dialect hyperon|petta] [--no-stdlib] <dir-or-file>...
//! ```
//!
//! Every `.metta` file under the given paths is one module; all modules are
//! classified together against one engine's native names. With `--dialect hyperon`
//! (the default) the Hyperon stdlib is classified alongside, unless `--no-stdlib`;
//! only the corpus's own heads are counted. With `--dialect petta` the natives come
//! from PeTTa's builtin registry and no stdlib is added: pass PeTTa's `lib/` among
//! the paths if the corpus imports it.

// ruff/clippy.toml's disallowed-methods list is directory-scoped and aimed at the
// ty crates, which must go through their `System` abstraction. This example reads a
// corpus from disk and has no `System`.
#![expect(
    clippy::disallowed_methods,
    reason = "a corpus reader with no ty System; see the comment above"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ruff_metta_spo::{
    Category, Module, Natives, Reason, Verdict, classify, hyperon_stdlib, parse_module,
};

fn collect(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        entries.sort();
        for p in entries {
            collect(&p, out);
        }
    } else if path.extension().is_some_and(|e| e == "metta") {
        out.push(path.to_path_buf());
    }
}

fn main() {
    let mut with_stdlib = true;
    let mut natives = Natives::hyperon();
    let mut files = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--no-stdlib" => with_stdlib = false,
            "--dialect" => match args.next().as_deref() {
                Some("hyperon") => natives = Natives::hyperon(),
                Some("petta") => {
                    natives = Natives::petta_fork();
                    with_stdlib = false;
                }
                other => panic!("--dialect hyperon|petta, got {other:?}"),
            },
            _ => collect(Path::new(&arg), &mut files),
        }
    }
    let mut modules: Vec<Module> = Vec::new();
    let mut parse_errors = 0;
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
        let name = f.to_string_lossy().replace('/', "::");
        let m = parse_module(&name, &text);
        if let Some(e) = &m.parse_error {
            parse_errors += 1;
            eprintln!("parse error in {}: {e}", f.display());
        }
        modules.push(m);
    }
    let corpus_heads: std::collections::BTreeSet<String> = modules
        .iter()
        .flat_map(|m| {
            m.rules
                .iter()
                .filter_map(|r| r.head_name().map(str::to_string))
        })
        .collect();
    let rules: usize = modules.iter().map(|m| m.rules.len()).sum();
    let types: usize = modules.iter().map(|m| m.types.len()).sum();
    let facts: usize = modules.iter().map(|m| m.facts.len()).sum();
    let queries: usize = modules.iter().map(|m| m.queries.len()).sum();
    if with_stdlib {
        modules.insert(0, hyperon_stdlib());
    }
    let c = classify(&natives, &modules);

    println!(
        "files {} (parse errors {parse_errors}), rules {rules}, rule heads {}, `:` declarations {types}, facts {facts}, `!` queries {queries}, unnamed-head rules {}",
        files.len(),
        corpus_heads.len(),
        c.unnamed_rules
    );
    let mut lowerable = 0;
    let mut by_reason: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_native: BTreeMap<(Category, String), usize> = BTreeMap::new();
    for head in &corpus_heads {
        match &c.verdicts[head] {
            Verdict::Lowerable => lowerable += 1,
            Verdict::InterpreterOnly(r) => {
                let key = match r {
                    Reason::Builtin { name, category } => {
                        *by_native.entry((*category, name.clone())).or_default() += 1;
                        format!("native {category:?}")
                    }
                    Reason::VariableHead => {
                        "variable head (call or tuple, decided at run time)".to_string()
                    }
                    Reason::Calls { .. } => "calls an interpreter-only head".to_string(),
                };
                *by_reason.entry(key).or_default() += 1;
            }
        }
    }
    println!(
        "dialect {} ({} natives), heads lowerable {lowerable}, interpreter-only {} (stdlib {})",
        natives.dialect(),
        natives.len(),
        corpus_heads.len() - lowerable,
        if with_stdlib { "included" } else { "excluded" }
    );
    for (reason, n) in &by_reason {
        println!("  {n:>6}  {reason}");
    }
    let mut natives: Vec<_> = by_native.into_iter().collect();
    natives.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    println!("most frequent direct natives:");
    for ((cat, name), n) in natives.iter().take(12) {
        println!("  {n:>6}  {name} ({cat:?})");
    }
    let stdlib_heads: std::collections::BTreeSet<String> = if with_stdlib {
        modules[0]
            .rules
            .iter()
            .filter_map(|r| r.head_name().map(str::to_string))
            .collect()
    } else {
        Default::default()
    };
    let constructors: Vec<&String> = c
        .constructors
        .iter()
        .filter(|n| !stdlib_heads.contains(*n))
        .collect();
    println!(
        "unresolved expression heads (constructors or functions this harvest cannot see): {}",
        constructors.len()
    );
}
