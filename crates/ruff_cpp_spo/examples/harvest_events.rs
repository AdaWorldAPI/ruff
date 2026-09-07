//! Emit the ordered behavioral ore for a C++ corpus — the four TSVs described
//! in `.claude/plans/behavioral-ore-v1.md`.
//!
//! ONE extractor, many consumers: a frequency tokenizer, a behavioral
//! tokenizer and any downstream representation all read these same files. No
//! consumer re-extracts, so extraction differences can never masquerade as
//! representation differences.
//!
//! Env:
//!   `ORE_OUT`        output directory (default `/tmp/ore`)
//!   `ORE_CC_JSON`    a `compile_commands.json`; each entry's own `-I`/`-D`/
//!                    `-std`/`-isystem` flags are used, so the parse sees what
//!                    the compiler sees instead of a guessed argument set
//!   `ORE_TU_FILTER`  substring filter on the TU path
//!   `ORE_TU_LIMIT`   max TUs
//!   `ORE_FILE`       a single source file (alternative to `ORE_CC_JSON`)
//!   `ORE_ARGS_FILE`  one clang argument per line, for `ORE_FILE`
//!
//! ```sh
//! ORE_CC_JSON=build/compile_commands.json ORE_TU_LIMIT=50 ORE_OUT=/tmp/ore \
//!   LIBCLANG_PATH=/usr/lib/llvm-18/lib \
//!   cargo run -p ruff_cpp_spo --features libclang --example harvest_events
//! ```

#![expect(
    clippy::print_stderr,
    reason = "manifest-emission CLI example (mirrors harvest_ladybug)"
)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use ruff_cpp_spo::events::{MethodOre, Symbols, walk_tu_events};

/// A TSV cell can carry no tab and no newline. C++ identifiers and type
/// spellings contain neither, but a malformed one would silently shift every
/// later column, so the guard is unconditional.
fn cell(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

/// Pull the compile flags out of one `compile_commands.json` entry. Only the
/// flags that change what the parser SEES are kept — `-o`, `-c` and the input
/// file itself would make libclang re-drive a compilation.
fn args_of(cmd: &str) -> Vec<String> {
    let toks: Vec<String> = shell_split(cmd);
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let t = &toks[i];
        if t == "-isystem" || t == "-include" {
            if let Some(v) = toks.get(i + 1) {
                out.push(t.clone());
                out.push(v.clone());
            }
            i += 2;
            continue;
        }
        if t.starts_with("-I") || t.starts_with("-D") || t.starts_with("-std") {
            out.push(t.clone());
        }
        i += 1;
    }
    out
}

/// Minimal POSIX-ish splitter: enough for the quoting cmake emits.
fn shell_split(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote = None::<char>;
    let mut esc = false;
    for c in s.chars() {
        if esc {
            cur.push(c);
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                cur.push(c);
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
        } else if c.is_whitespace() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `compile_commands.json` without a JSON dependency: the file is a flat array
/// of objects with string values, and only two keys are needed.
fn parse_cc_json(text: &str) -> Vec<(PathBuf, Vec<String>)> {
    let mut out = Vec::new();
    let (mut file, mut cmd) = (None::<String>, None::<String>);
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(v) = json_string_field(line, "\"file\":") {
            file = Some(v);
        } else if let Some(v) = json_string_field(line, "\"command\":") {
            cmd = Some(v);
        }
        if let (Some(f), Some(c)) = (&file, &cmd) {
            out.push((PathBuf::from(f), args_of(c)));
            file = None;
            cmd = None;
        }
    }
    out
}

fn json_string_field(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?.trim_start();
    let body = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut esc = false;
    for c in body.chars() {
        if esc {
            out.push(match c {
                'n' => '\n',
                't' => '\t',
                other => other,
            });
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == '"' {
            return Some(out);
        } else {
            out.push(c);
        }
    }
    None
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(std::env::var("ORE_OUT").unwrap_or_else(|_| "/tmp/ore".into()));
    std::fs::create_dir_all(&out_dir)?;

    let mut units: Vec<(PathBuf, Vec<String>)> = Vec::new();
    if let Ok(cc) = std::env::var("ORE_CC_JSON") {
        let text = std::fs::read_to_string(&cc)?;
        units = parse_cc_json(&text);
        if let Ok(f) = std::env::var("ORE_TU_FILTER") {
            units.retain(|(p, _)| p.display().to_string().contains(&f));
        }
        if let Ok(n) = std::env::var("ORE_TU_LIMIT") {
            let n: usize = n.parse().unwrap_or(usize::MAX);
            units.truncate(n);
        }
    } else if let Ok(f) = std::env::var("ORE_FILE") {
        let args = std::env::var("ORE_ARGS_FILE").map_or_else(
            |_| Vec::new(),
            |p| {
                std::fs::read_to_string(p)
                    .map_or_else(|_| Vec::new(), |t| t.lines().map(str::to_string).collect())
            },
        );
        units.push((PathBuf::from(f), args));
    } else {
        return Err("set ORE_CC_JSON or ORE_FILE — this example never invents a corpus".into());
    }
    if units.is_empty() {
        return Err("no translation units selected".into());
    }

    let mut syms = Symbols::default();
    let mut methods: Vec<MethodOre> = Vec::new();
    let (mut bad, mut failed) = (0usize, 0usize);
    for (i, (path, args)) in units.iter().enumerate() {
        match walk_tu_events(path, args, &mut syms) {
            Ok(tu) => {
                if !tu.diagnostics.is_empty() {
                    bad += 1;
                    if bad <= 5 {
                        eprintln!(
                            "[ore] {} error diagnostic(s) in {} — partial AST, facts may be missing\n    {}",
                            tu.diagnostics.len(),
                            path.display(),
                            tu.diagnostics[0]
                        );
                    }
                }
                methods.extend(tu.methods);
            }
            Err(e) => {
                failed += 1;
                if failed <= 5 {
                    eprintln!("[ore] FAILED {}: {e}", path.display());
                }
            }
        }
        if (i + 1) % 25 == 0 {
            eprintln!(
                "[ore] {}/{} TUs, {} methods, {} symbols",
                i + 1,
                units.len(),
                methods.len(),
                syms.len()
            );
        }
    }

    // A method defined in a header reachable from several TUs is harvested
    // once per TU. Keep the FIRST sighting: dropping duplicates here is a
    // corpus decision, not a dedup of occurrences — the event stream inside
    // each kept method is untouched.
    let mut seen = BTreeMap::new();
    for m in methods {
        seen.entry(m.iri.clone()).or_insert(m);
    }
    let methods: Vec<MethodOre> = seen.into_values().collect();

    let (mut ev, mut sc, mut me) = (String::new(), String::new(), String::new());
    let mut hist: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut n_events = 0usize;
    for m in &methods {
        for e in &m.events {
            *hist.entry(e.kind.as_str()).or_default() += 1;
            n_events += 1;
            writeln!(
                ev,
                "{}\t{}\t{}\t{}\t{}\tc{}\t{}\t{}\t{}\t{}\t{}",
                cell(&m.iri),
                e.seq,
                e.kind,
                e.subj.as_deref().unwrap_or("-"),
                e.obj.as_deref().unwrap_or("-"),
                e.scope,
                e.parent
                    .map_or_else(|| "-".to_string(), |p| format!("c{p}")),
                e.anchor,
                e.control.as_deref().unwrap_or("-"),
                e.type_rel.as_deref().unwrap_or("-"),
                e.prov.as_str()
            )?;
        }
        for s in &m.scopes {
            writeln!(
                sc,
                "{}\tc{}\t{}\t{}\t{}\t{}\t{}",
                cell(&m.iri),
                s.id,
                s.parent
                    .map_or_else(|| "-".to_string(), |p| format!("c{p}")),
                s.kind.as_str(),
                s.depth,
                s.enter_seq,
                s.exit_seq
            )?;
        }
        writeln!(
            me,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            cell(&m.iri),
            cell(&m.tu),
            cell(&m.class),
            m.events.len(),
            m.scopes.len(),
            m.centroid,
            m.set_reads,
            m.set_writes,
            m.set_raises,
            m.set_calls,
            m.set_guarded,
            u8::from(m.is_const),
            u8::from(m.is_static),
            u8::from(m.is_virtual),
            u8::from(m.overrides),
            m.mkind,
            m.access,
            m.n_params
        )?;
    }
    let mut sy = String::new();
    for (id, kind, role, name, prov) in syms.rows() {
        writeln!(
            sy,
            "{id}\t{}\t{}\t{}\t{}",
            kind.as_str(),
            role.as_str(),
            cell(name),
            prov.as_str()
        )?;
    }

    std::fs::write(out_dir.join("events.tsv"), &ev)?;
    std::fs::write(out_dir.join("scopes.tsv"), &sc)?;
    std::fs::write(out_dir.join("methods.tsv"), &me)?;
    std::fs::write(out_dir.join("symbols.tsv"), &sy)?;

    eprintln!(
        "\n[ore] {} TUs ({} with error diagnostics, {} failed to parse)",
        units.len(),
        bad,
        failed
    );
    eprintln!(
        "[ore] {} methods, {n_events} events, {} symbols -> {}",
        methods.len(),
        syms.len(),
        out_dir.display()
    );
    let with_events = methods.iter().filter(|m| m.events.len() > 2).count();
    let a0_facts: u32 = methods
        .iter()
        .map(|m| m.set_reads + m.set_writes + m.set_raises + m.set_calls)
        .sum();
    eprintln!(
        "[ore] methods with a body beyond the empty function scope: {with_events}; \
         A0 five-set facts over the same corpus: {a0_facts}"
    );
    eprintln!("[ore] event-kind histogram:");
    let mut h: Vec<_> = hist.into_iter().collect();
    h.sort_by_key(|a| std::cmp::Reverse(a.1));
    for (k, n) in h {
        eprintln!("  {k:<11} {n:>8}");
    }
    Ok(())
}
