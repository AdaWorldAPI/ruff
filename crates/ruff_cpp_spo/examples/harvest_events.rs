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

use ruff_cpp_spo::events::{EventKind, MethodOre, Symbols, walk_tu_events};

/// Replaces tab and line-break characters in a TSV cell with spaces.
///
/// # Examples
///
/// ```
/// assert_eq!(cell("a\tb\nc\rd"), "a b c d");
/// ```
fn cell(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

/// Extracts parser-relevant flags from a compilation command.
///
/// Compilation and output options, along with the source file, are omitted.
///
/// # Examples
///
/// ```
/// let flags = args_of("clang++ -Iinclude -std=c++17 -c main.cpp -o main.o");
/// assert_eq!(flags, vec!["-Iinclude", "-std=c++17"]);
/// ```
fn args_of(cmd: &str) -> Vec<String> {
    keep_parse_flags(&shell_split(cmd))
}

/// Filters compiler arguments to retain parser-relevant include, macro, and language-standard flags.
///
/// # Examples
///
/// ```
/// let args = vec![
///     "-I".to_owned(),
///     "include".to_owned(),
///     "-DDEBUG".to_owned(),
///     "-std=c++17".to_owned(),
///     "-o".to_owned(),
///     "output.o".to_owned(),
/// ];
///
/// assert_eq!(
///     keep_parse_flags(&args),
///     vec!["-I", "include", "-DDEBUG", "-std=c++17"]
/// );
/// ```
///
/// The returned arguments include `-I`, `-isystem`, and `-include` together
/// with their following values, as well as joined `-I`, `-D`, and `-std` flags.
fn keep_parse_flags(toks: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let t = &toks[i];
        // `-I` can be written joined (`-Iinc`) or as two tokens (`-I inc`);
        // clang accepts both. Keeping only the bare `-I` would hand libclang an
        // incomplete flag and lose the include path.
        if matches!(t.as_str(), "-I" | "-isystem" | "-include") {
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

/// Splits a command string into whitespace-separated arguments while honoring single quotes, double quotes, and backslash escapes.
///
/// # Examples
///
/// ```
/// assert_eq!(
///     shell_split(r#"clang++ -I"include dir" 'source file.cpp'"#),
///     vec!["clang++", "-Iinclude dir", "source file.cpp"]
/// );
/// ```
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

/// Parses compilation database entries into source paths and parser-relevant arguments.
///
/// Supports both shell-escaped `command` strings and already-split `arguments`
/// arrays. Entries without either command form are ignored and reported.
///
/// # Examples
///
/// ```
/// let entries = parse_cc_json(
///     r#"[{"file":"src/main.cpp","arguments":["clang++","-I","include","-c","src/main.cpp"]}]"#
/// );
///
/// assert_eq!(entries.len(), 1);
/// assert_eq!(entries[0].0, std::path::PathBuf::from("src/main.cpp"));
/// assert_eq!(entries[0].1, vec!["-I", "include"]);
/// ```
fn parse_cc_json(text: &str) -> Vec<(PathBuf, Vec<String>)> {
    let mut out = Vec::new();
    let mut file: Option<String> = None;
    let mut cmd: Option<String> = None;
    let mut argv: Option<Vec<String>> = None;
    let mut collecting: Option<String> = None;
    let mut malformed = 0usize;

    for raw in text.lines() {
        let line = raw.trim();

        // Inside an `"arguments": [ ... ]` array, which may span lines. The
        // raw text is accumulated and scanned for quoted runs at the closing
        // bracket, NEVER split on commas: `-DPAIR=std::pair<int,int>` is one
        // valid argument, and splitting it would silently drop the flag.
        //
        // The array ends at the first `]` OUTSIDE a string. A bare
        // `contains(']')` stops early on `-I/tmp/sdk]` and keeps only the
        // flags before it — the same silent truncation as the comma split, one
        // level up: that one loses an argument, this one loses every argument
        // after it.
        if let Some(acc) = collecting.as_mut() {
            acc.push_str(line);
            if let Some(end) = unquoted_bracket(acc) {
                argv = Some(json_string_array(&acc[..end]));
                collecting = None;
            }
            continue;
        }

        if let Some(v) = json_string_field(line, "\"file\":") {
            file = Some(v);
        } else if let Some(v) = json_string_field(line, "\"command\":") {
            cmd = Some(v);
        } else if let Some(rest) = line.strip_prefix("\"arguments\":") {
            let rest = rest.trim_start();
            if let Some(end) = unquoted_bracket(rest) {
                argv = Some(json_string_array(&rest[..end]));
            } else {
                collecting = Some(rest.to_string());
            }
        }

        // One object ended. Emit it, or count it, but never carry it forward.
        if line.starts_with('}') {
            match (file.take(), argv.take(), cmd.take()) {
                // `arguments` wins: it is already split, so it cannot be
                // mangled by re-splitting a shell-escaped string.
                (Some(f), Some(a), _) => out.push((PathBuf::from(f), keep_parse_flags(&a))),
                (Some(f), None, Some(c)) => out.push((PathBuf::from(f), args_of(&c))),
                (Some(_), None, None) => malformed += 1,
                (None, _, _) => {}
            }
        }
    }
    if malformed > 0 {
        eprintln!("[ore] {malformed} compile_commands entries had neither command nor arguments");
    }
    out
}

/// Finds the first closing bracket outside a quoted JSON string.
///
/// # Examples
///
/// ```
/// assert_eq!(unquoted_bracket(r#"["a]"]]"#), Some(5));
/// assert_eq!(unquoted_bracket(r#"["a"]"#), None);
/// ```
fn unquoted_bracket(text: &str) -> Option<usize> {
fn unquoted_bracket(text: &str) -> Option<usize> {
    let (mut inside, mut esc) = (false, false);
    for (i, c) in text.char_indices() {
        if inside {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                inside = false;
            }
        } else if c == '"' {
            inside = true;
        } else if c == ']' {
            return Some(i);
        }
    }
    None
}

/// Extracts quoted string values from text and partially unescapes newline and tab escapes.
///
/// # Examples
///
/// ```
/// let values = json_string_array(r#"[ "one", "two\n" ]"#);
/// assert_eq!(values, vec!["one", "two\n"]);
/// ```
///
/// @param text Text containing quoted string values.
/// @returns The extracted string values in their order of appearance.
fn json_string_array(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut inside, mut esc) = (false, false);
    for c in text.chars() {
        if !inside {
            if c == '"' {
                inside = true;
                cur.clear();
            }
            continue;
        }
        if esc {
            cur.push(match c {
                'n' => '\n',
                't' => '\t',
                other => other,
            });
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == '"' {
            inside = false;
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out
}

/// Extracts a quoted string value following a matching field prefix.
///
/// Recognizes `\n` and `\t` escapes and returns `None` when the prefix or
/// quoted value is missing.
///
/// # Examples
///
/// ```
/// assert_eq!(
///     json_string_field(r#""name": "line\nvalue""#, r#""name":"#),
///     Some("line\nvalue".to_string())
/// );
/// ```
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

/// Harvests translation-unit events and writes event, scope, method, and symbol TSV files.
///
/// The corpus is selected with `ORE_CC_JSON` or `ORE_FILE`; `ORE_OUT` selects the
/// output directory. Returns an error when no corpus is selected or no translation
/// units remain after filtering.
///
/// # Examples
///
/// ```text
/// ORE_CC_JSON=compile_commands.json ORE_OUT=/tmp/ore cargo run --example harvest_events
/// ```
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
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
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
            m.n_params,
            m.overrides_target
                .as_deref()
                .map_or_else(|| "-".to_string(), cell)
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
    // A method scope emits ScopeEnter + ScopeExit, and each parameter emits one
    // Param event, so `void f(int a) {}` reaches three events with an empty
    // body. Only a non-prologue event counts as body content.
    let with_events = methods
        .iter()
        .filter(|m| {
            m.events.iter().any(|e| {
                !matches!(
                    e.kind,
                    EventKind::ScopeEnter | EventKind::ScopeExit | EventKind::Param
                )
            })
        })
        .count();
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

#[cfg(test)]
mod tests {
    use super::{json_string_array, keep_parse_flags, parse_cc_json};

    /// A comma INSIDE a JSON string is content, not a separator. Splitting on
    /// commas turns one valid flag into two invalid fragments and drops both,
    /// silently changing the flags a translation unit is parsed with.
    #[test]
    fn an_argument_containing_a_comma_survives_the_array_parse() {
        let got = json_string_array(r#"["c++", "-DPAIR=std::pair<int,int>", "-c"]"#);
        assert_eq!(got, ["c++", "-DPAIR=std::pair<int,int>", "-c"]);
        // The whole point: the comma-bearing flag is ONE element, not two.
        assert_eq!(got.len(), 3, "a comma inside a string must not split it");
    }

    /// A `]` INSIDE a JSON string is content, not the end of the array. Ending
    /// collection at the first bracket keeps only a prefix of the flags, and a
    /// truncated flag list is a partial AST rather than an error — the same
    /// silent-truncation class as the comma split above, one level up: that
    /// one splits an argument, this one drops every argument after it.
    #[test]
    fn a_bracket_inside_an_argument_does_not_end_the_array() {
        let db = r#"[
{
  "directory": "/tmp",
  "arguments": [
    "c++",
    "-std=c++17",
    "-I/tmp/sdk]",
    "-DTAIL=1",
    "-c",
    "a.cpp"
  ],
  "file": "/tmp/a.cpp"
}
]"#;
        let got = parse_cc_json(db);
        assert_eq!(got.len(), 1, "one entry");
        // `-DTAIL=1` sits AFTER the bracket-bearing argument, so it is exactly
        // what an early stop loses.
        assert_eq!(got[0].1, ["-std=c++17", "-I/tmp/sdk]", "-DTAIL=1"]);
    }

    /// The same truncation on ONE line: the array is still open after a
    /// bracket that lives inside a string, so the remaining lines belong to it.
    #[test]
    fn a_bracket_inside_an_argument_does_not_close_a_single_line_array() {
        let db = r#"[
{
  "directory": "/tmp",
  "arguments": ["c++", "-I/tmp/sdk]",
    "-DTAIL=1", "-c", "a.cpp"],
  "file": "/tmp/a.cpp"
}
]"#;
        let got = parse_cc_json(db);
        assert_eq!(got.len(), 1, "one entry");
        assert_eq!(got[0].1, ["-I/tmp/sdk]", "-DTAIL=1"]);
    }

    /// clang accepts `-I dir` as two tokens. Keeping the bare `-I` and dropping
    /// the directory hands libclang an incomplete flag and loses the include
    /// path, which shows up as a partial AST rather than an error.
    #[test]
    fn a_standalone_include_flag_keeps_its_directory() {
        let toks: Vec<String> = ["-I", "/opt/incdir", "-c", "a.cpp", "-Ijoined"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        assert_eq!(keep_parse_flags(&toks), ["-I", "/opt/incdir", "-Ijoined"]);
    }

    /// Both command forms are read, each entry is flushed at its own closing
    /// brace, and an entry carrying neither form never steals a later command.
    #[test]
    fn each_compilation_database_entry_is_parsed_independently() {
        let db = r#"[
{
  "directory": "/tmp",
  "arguments": ["c++", "-std=c++17", "-DPAIR=std::pair<int,int>", "-I", "/opt/incdir", "-c", "a.cpp"],
  "file": "/tmp/a.cpp"
},
{
  "directory": "/tmp",
  "file": "/tmp/no_command.cpp"
},
{
  "directory": "/tmp",
  "command": "c++ -std=c++17 -I/other -c b.cpp",
  "file": "/tmp/b.cpp"
}
]"#;
        let got = parse_cc_json(db);
        let files: Vec<String> = got.iter().map(|(f, _)| f.display().to_string()).collect();
        // The entry with neither form is dropped, and crucially it does NOT
        // pair with the next entry's command.
        assert_eq!(files, ["/tmp/a.cpp", "/tmp/b.cpp"]);
        assert_eq!(
            got[0].1,
            [
                "-std=c++17",
                "-DPAIR=std::pair<int,int>",
                "-I",
                "/opt/incdir"
            ]
        );
        assert_eq!(got[1].1, ["-std=c++17", "-I/other"]);
    }

    /// A multiline `arguments` array is accumulated before it is scanned, so a
    /// comma-bearing flag survives that layout too.
    #[test]
    fn a_multiline_arguments_array_is_parsed_as_one_unit() {
        let db = "[\n{\n  \"file\": \"/tmp/c.cpp\",\n  \"arguments\": [\n    \"c++\",\n    \"-DPAIR=std::pair<int,int>\",\n    \"-Iinc\"\n  ]\n}\n]";
        let got = parse_cc_json(db);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, ["-DPAIR=std::pair<int,int>", "-Iinc"]);
    }
}
