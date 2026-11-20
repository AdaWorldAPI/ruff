//! The call finder: every call to one dotted callee, with its arguments
//! lowered to [`CoffeeArg`].
//!
//! # What is a call
//!
//! A dotted name path (`A.B.c`, `@x.y`) that does not continue an earlier
//! path (so `x.A.B.c` is not a call to `A.B.c`), followed by
//!
//! - `(`, with or without spaces before it: arguments run to the matching
//!   `)` and may span lines; or
//! - an argument start on the same line after whitespace (a string, number,
//!   regex, identifier that is not a keyword, `[`, `{`, an arrow): an
//!   implicit call, whose arguments run to the end of the line, or to an
//!   unmatched closing bracket, `;`, or a postfix `if`, `unless`, `for`,
//!   `while`, `until`, `then`, `else`, `when`. A line ending in `,` continues
//!   on the next line.
//!
//! Arguments split at commas outside brackets.
//!
//! # Known limits
//!
//! - In an implicit call, commas inside an inline function body belong to
//!   the call's argument list.
//! - `f (x), y` is read as `f((x), y)`, which is what CoffeeScript does too.
//! - A negative number (`-1`) is an operator and a number, so it lowers to
//!   [`CoffeeArg::Other`].
//! - The words `true`, `false`, `null`, `undefined`, `yes`, `no`, `on`,
//!   `off`, `this`, `new`, `typeof`, `not` are keywords, never a
//!   [`CoffeeArg::Path`]: they lower to [`CoffeeArg::Other`].

use std::fs;
use std::path::{Path, PathBuf};

use crate::tokenizer::{Token, TokenKind, tokenize};

/// One lowered call argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoffeeArg {
    /// A string literal without interpolation.
    Str(String),
    /// A dotted identifier path such as `Router` or `App.Foo.bar`.
    Path(String),
    /// A numeric literal, source text kept.
    Num(String),
    /// Anything else, with its exact source text.
    Other { source: String },
}

/// A call to the requested callee.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoffeeCall {
    pub callee: String,
    pub file: String,
    pub line: u32,
    pub args: Vec<CoffeeArg>,
}

/// Counts over a scan; see the crate docs for the conservation law
/// `calls_found == calls_fully_literal + calls_with_other`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoffeeScanReport {
    pub files_scanned: usize,
    pub calls_found: usize,
    pub implicit_calls: usize,
    pub calls_fully_literal: usize,
    pub calls_with_other: usize,
    pub unterminated: usize,
}

impl CoffeeScanReport {
    fn absorb(&mut self, other: &CoffeeScanReport) {
        self.files_scanned += other.files_scanned;
        self.calls_found += other.calls_found;
        self.implicit_calls += other.implicit_calls;
        self.calls_fully_literal += other.calls_fully_literal;
        self.calls_with_other += other.calls_with_other;
        self.unterminated += other.unterminated;
    }
}

/// Every `*.coffee` file under `root/dir`, sorted, as paths relative to `root`.
pub fn coffee_files(root: &Path, dir: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    collect_coffee(root, &root.join(dir), &mut found);
    found.sort();
    found
}

fn collect_coffee(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_coffee(root, &path, out);
        } else if path.extension().is_some_and(|ext| ext == "coffee")
            && let Ok(relative) = path.strip_prefix(root)
        {
            out.push(relative.to_path_buf());
        }
    }
}

/// Find every call to `callee` in `files` (relative to `root`). A file that
/// cannot be read as UTF-8 text counts in no figure.
pub fn extract_calls(
    root: &Path,
    files: &[PathBuf],
    callee: &str,
) -> (Vec<CoffeeCall>, CoffeeScanReport) {
    let mut calls = Vec::new();
    let mut report = CoffeeScanReport::default();
    for file in files {
        let Ok(source) = fs::read_to_string(root.join(file)) else {
            continue;
        };
        let (found, partial) = extract_calls_from_source(&source, &file.to_string_lossy(), callee);
        calls.extend(found);
        report.absorb(&partial);
    }
    (calls, report)
}

/// Find every call to `callee` in one source text, reported under the name
/// `file`.
pub fn extract_calls_from_source(
    source: &str,
    file: &str,
    callee: &str,
) -> (Vec<CoffeeCall>, CoffeeScanReport) {
    let tokens = tokenize(source);
    let mut calls = Vec::new();
    let mut report = CoffeeScanReport {
        files_scanned: 1,
        ..CoffeeScanReport::default()
    };
    let mut i = 0;
    while i < tokens.len() {
        let continues_path = i > 0 && tokens[i - 1].kind == TokenKind::Punct('.');
        if tokens[i].kind != TokenKind::Ident || continues_path {
            i += 1;
            continue;
        }
        let (path, after) = read_path(&tokens, source, i);
        if path != callee {
            i = after;
            continue;
        }
        let line = tokens[i].line;
        let args_range = match tokens.get(after) {
            Some(t) if t.kind == TokenKind::Punct('(') => match matching_close(&tokens, after) {
                Some(close) => Some((after + 1, close, false)),
                None => {
                    report.unterminated += 1;
                    None
                }
            },
            Some(t) if starts_implicit_arg(t, source) => {
                Some((after, implicit_end(&tokens, source, after), true))
            }
            _ => None,
        };
        if let Some((from, to, implicit)) = args_range {
            let args = lower_args(&tokens[from..to], source);
            report.calls_found += 1;
            report.implicit_calls += usize::from(implicit);
            if args.iter().any(|a| matches!(a, CoffeeArg::Other { .. })) {
                report.calls_with_other += 1;
            } else {
                report.calls_fully_literal += 1;
            }
            calls.push(CoffeeCall {
                callee: path,
                file: file.to_string(),
                line,
                args,
            });
        }
        // Move past the name only: the arguments may hold more calls.
        i = after;
    }
    (calls, report)
}

/// Read `Ident (. Ident)*` from `i`; returns the joined path and the index of
/// the token after it.
fn read_path(tokens: &[Token], source: &str, i: usize) -> (String, usize) {
    let mut path = text(&tokens[i], source).to_string();
    let mut k = i + 1;
    while let (Some(dot), Some(next)) = (tokens.get(k), tokens.get(k + 1))
        && dot.kind == TokenKind::Punct('.')
        && next.kind == TokenKind::Ident
        && !dot.space_before
        && !next.space_before
    {
        path.push('.');
        path.push_str(text(next, source));
        k += 2;
    }
    (path, k)
}

fn text<'a>(token: &Token, source: &'a str) -> &'a str {
    &source[token.start..token.end]
}

const KEYWORDS: &[&str] = &[
    "true",
    "false",
    "null",
    "undefined",
    "yes",
    "no",
    "on",
    "off",
    "this",
    "new",
    "typeof",
    "not",
];

const NOT_AN_ARGUMENT: &[&str] = &[
    "if",
    "unless",
    "then",
    "else",
    "and",
    "or",
    "is",
    "isnt",
    "in",
    "of",
    "by",
    "for",
    "while",
    "until",
    "when",
    "instanceof",
    "not",
    "switch",
    "return",
];

const POSTFIX_STOP: &[&str] = &[
    "if", "unless", "for", "while", "until", "then", "else", "when",
];

fn starts_implicit_arg(token: &Token, source: &str) -> bool {
    if !token.space_before {
        return false;
    }
    match &token.kind {
        TokenKind::Str(_)
        | TokenKind::Interpolated
        | TokenKind::Num
        | TokenKind::Regex
        | TokenKind::Arrow
        | TokenKind::Opaque => true,
        TokenKind::Ident => !NOT_AN_ARGUMENT.contains(&text(token, source)),
        TokenKind::Punct(c) => matches!(c, '[' | '{'),
        TokenKind::Newline { .. } => false,
    }
}

fn is_open(kind: &TokenKind) -> bool {
    matches!(kind, TokenKind::Punct('(' | '[' | '{'))
}

fn is_close(kind: &TokenKind) -> bool {
    matches!(kind, TokenKind::Punct(')' | ']' | '}'))
}

/// Index of the bracket closing the one at `open`.
fn matching_close(tokens: &[Token], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (k, token) in tokens.iter().enumerate().skip(open) {
        if is_open(&token.kind) {
            depth += 1;
        } else if is_close(&token.kind) {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(k);
            }
        }
    }
    None
}

/// Exclusive end of the argument tokens of an implicit call starting at `from`.
fn implicit_end(tokens: &[Token], source: &str, from: usize) -> usize {
    let mut depth = 0usize;
    let mut k = from;
    while k < tokens.len() {
        let token = &tokens[k];
        if is_open(&token.kind) {
            depth += 1;
        } else if is_close(&token.kind) {
            if depth == 0 {
                return k;
            }
            depth -= 1;
        } else if depth == 0 {
            match &token.kind {
                TokenKind::Newline { .. } => {
                    let after_comma = k > from && tokens[k - 1].kind == TokenKind::Punct(',');
                    if !after_comma {
                        return k;
                    }
                }
                TokenKind::Punct(';') => return k,
                TokenKind::Ident if POSTFIX_STOP.contains(&text(token, source)) => return k,
                _ => {}
            }
        }
        k += 1;
    }
    tokens.len()
}

fn lower_args(tokens: &[Token], source: &str) -> Vec<CoffeeArg> {
    let mut args = Vec::new();
    let mut depth = 0usize;
    let mut from = 0;
    for (k, token) in tokens.iter().enumerate() {
        if is_open(&token.kind) {
            depth += 1;
        } else if is_close(&token.kind) {
            depth = depth.saturating_sub(1);
        } else if depth == 0 && token.kind == TokenKind::Punct(',') {
            push_arg(&tokens[from..k], source, &mut args);
            from = k + 1;
        }
    }
    push_arg(&tokens[from..], source, &mut args);
    args
}

fn push_arg(tokens: &[Token], source: &str, args: &mut Vec<CoffeeArg>) {
    let significant: Vec<&Token> = tokens
        .iter()
        .filter(|t| !matches!(t.kind, TokenKind::Newline { .. }))
        .collect();
    let (Some(first), Some(last)) = (significant.first(), significant.last()) else {
        return;
    };
    args.push(lower_arg(&significant, &source[first.start..last.end]));
}

fn lower_arg(tokens: &[&Token], whole: &str) -> CoffeeArg {
    let other = || CoffeeArg::Other {
        source: whole.to_string(),
    };
    if let [only] = tokens {
        return match &only.kind {
            TokenKind::Str(value) => CoffeeArg::Str(value.clone()),
            TokenKind::Num => CoffeeArg::Num(whole.to_string()),
            TokenKind::Ident if !KEYWORDS.contains(&whole) => CoffeeArg::Path(whole.to_string()),
            _ => other(),
        };
    }
    // `A.b.c`: identifiers joined by adjacent dots.
    let is_path = tokens.len() % 2 == 1
        && tokens.iter().enumerate().all(|(k, t)| {
            if k % 2 == 0 {
                t.kind == TokenKind::Ident
            } else {
                t.kind == TokenKind::Punct('.')
            }
        })
        && tokens.windows(2).all(|pair| pair[1].start == pair[0].end)
        && !tokens
            .first()
            .is_some_and(|t| KEYWORDS.contains(&&whole[..t.end - t.start]));
    if is_path {
        CoffeeArg::Path(whole.to_string())
    } else {
        other()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CALLEE: &str = "App.Config.set";

    fn scan(source: &str) -> (Vec<CoffeeCall>, CoffeeScanReport) {
        extract_calls_from_source(source, "t.coffee", CALLEE)
    }

    fn s(value: &str) -> CoffeeArg {
        CoffeeArg::Str(value.to_string())
    }

    fn p(value: &str) -> CoffeeArg {
        CoffeeArg::Path(value.to_string())
    }

    fn o(value: &str) -> CoffeeArg {
        CoffeeArg::Other {
            source: value.to_string(),
        }
    }

    #[test]
    fn four_sample_registrations() {
        let source = "\
App.Config.set('ticket/zoom/:ticket_id', TicketZoomRouter, 'Routes')
App.Config.set( 'import', Import, 'Routes' )
App.Config.set('keyboard_shortcuts', App.KeyboardShortcuts, 'Routes')
for url, Router of routes
  App.Config.set(url, Router, 'Routes')
";
        let (calls, report) = scan(source);
        assert_eq!(calls.len(), 4);
        assert_eq!(
            calls[0].args,
            vec![
                s("ticket/zoom/:ticket_id"),
                p("TicketZoomRouter"),
                s("Routes")
            ]
        );
        assert_eq!(calls[0].line, 1);
        assert_eq!(calls[1].args, vec![s("import"), p("Import"), s("Routes")]);
        assert_eq!(
            calls[2].args,
            vec![
                s("keyboard_shortcuts"),
                p("App.KeyboardShortcuts"),
                s("Routes")
            ]
        );
        assert_eq!(calls[3].args, vec![p("url"), p("Router"), s("Routes")]);
        assert_eq!(calls[3].line, 5);
        assert_eq!(
            report,
            CoffeeScanReport {
                files_scanned: 1,
                calls_found: 4,
                implicit_calls: 0,
                calls_fully_literal: 4,
                calls_with_other: 0,
                unterminated: 0,
            }
        );
    }

    #[test]
    fn calls_in_comments_and_strings_are_not_found() {
        let source = "\
# App.Config.set('x', Y, 'Routes')
### App.Config.set('x', Y, 'Routes')
App.Config.set('b', Y, 'Routes') ###
x = \"App.Config.set('x')\"
y = 'App.Config.set(\"x\")'
z = \"\"\"
App.Config.set('x')
\"\"\"
";
        let (calls, report) = scan(source);
        assert_eq!(calls.len(), 0);
        assert_eq!(report.calls_found, 0);
    }

    #[test]
    fn block_comment_hides_a_call_and_ends_at_the_closing_marker() {
        let source = "###\nApp.Config.set('hidden', Y, 'Routes')\n###\nApp.Config.set('seen', Y, 'Routes')\n";
        let (calls, _) = scan(source);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].args[0], s("seen"));
        assert_eq!(calls[0].line, 4);
    }

    #[test]
    fn interpolated_string_is_other_with_exact_source() {
        let (calls, report) = scan("App.Config.set(\"a/#{b}/c\", Y, 'Routes')");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].args[0], o("\"a/#{b}/c\""));
        assert_eq!(report.calls_with_other, 1);
        assert_eq!(report.calls_fully_literal, 0);
    }

    #[test]
    fn nested_braces_and_strings_in_interpolation_are_skipped() {
        let source = "App.Config.set(\"a#{ {k: '}'}['k'] }b\", Y, 'Routes')";
        let (calls, _) = scan(source);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].args.len(), 3);
        assert_eq!(calls[0].args[0], o("\"a#{ {k: '}'}['k'] }b\""));
        assert_eq!(calls[0].args[2], s("Routes"));
    }

    #[test]
    fn plain_double_quoted_string_is_a_literal() {
        let (calls, _) = scan("App.Config.set(\"a\\\"b\", Y, 'R\\'s')");
        assert_eq!(calls[0].args[0], s("a\"b"));
        assert_eq!(calls[0].args[2], s("R's"));
    }

    #[test]
    fn implicit_call_is_found_and_counted() {
        let (calls, report) = scan("App.Config.set 'a', B, 'Routes'\n");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].args, vec![s("a"), p("B"), s("Routes")]);
        assert_eq!(report.implicit_calls, 1);
        assert_eq!(report.calls_found, 1);
    }

    #[test]
    fn implicit_call_stops_at_end_of_line_and_postfix_condition() {
        let source = "App.Config.set 'a', B, 'Routes' if x, y\nz = 1\n";
        let (calls, _) = scan(source);
        assert_eq!(calls[0].args, vec![s("a"), p("B"), s("Routes")]);
    }

    #[test]
    fn implicit_call_continues_after_trailing_comma() {
        let source = "App.Config.set 'a',\n  B, 'Routes'\nnext = 1\n";
        let (calls, _) = scan(source);
        assert_eq!(calls[0].args, vec![s("a"), p("B"), s("Routes")]);
    }

    #[test]
    fn space_before_paren_is_still_a_call() {
        let (calls, report) = scan("App.Config.set ('a', B, 'Routes')");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].args.len(), 3);
        assert_eq!(report.implicit_calls, 0);
    }

    #[test]
    fn multi_line_call_with_nested_parens_splits_correctly() {
        let source = "\
App.Config.set(
  'a/:id',
  f(g(1), 'x'),
  { k: [1, 2], v: 'y' },
  'Routes'
)
";
        let (calls, report) = scan(source);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].line, 1);
        assert_eq!(
            calls[0].args,
            vec![
                s("a/:id"),
                o("f(g(1), 'x')"),
                o("{ k: [1, 2], v: 'y' }"),
                s("Routes"),
            ]
        );
        assert_eq!(report.calls_with_other, 1);
    }

    #[test]
    fn numbers_and_keywords_lower_as_specified() {
        let (calls, _) = scan("App.Config.set(1, 2.5e3, 0x1F, -1, true, @x.y, null)");
        assert_eq!(
            calls[0].args,
            vec![
                CoffeeArg::Num("1".into()),
                CoffeeArg::Num("2.5e3".into()),
                CoffeeArg::Num("0x1F".into()),
                o("-1"),
                o("true"),
                p("@x.y"),
                o("null"),
            ]
        );
    }

    #[test]
    fn other_callees_are_not_returned() {
        let source = "\
App.Config.get('x')
App.Config.set('keep', Y, 'Routes')
x.App.Config.set('nested', Y, 'Routes')
App.Config.setter('no', Y)
App.Config.set = 5
";
        let (calls, report) = scan(source);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].args[0], s("keep"));
        assert_eq!(report.calls_found, 1);
    }

    #[test]
    fn unterminated_call_is_counted_not_returned() {
        let (calls, report) = scan("App.Config.set('a', B, 'Routes'\nx = 1\n");
        assert_eq!(calls.len(), 0);
        assert_eq!(report.unterminated, 1);
        assert_eq!(report.calls_found, 0);
    }

    #[test]
    fn call_inside_another_calls_arguments_is_found() {
        let (calls, report) =
            scan("wrap(App.Config.set('a', B, 'Routes'))\nwrap2(App.Config.set 'b', C, 'R')");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].args, vec![s("b"), p("C"), s("R")]);
        assert_eq!(report.implicit_calls, 1);
    }

    #[test]
    fn regex_with_slash_in_class_does_not_hide_the_next_call() {
        let source = "x = /[/'\"]+/g\nApp.Config.set('a', B, 'Routes')\ny = a / b / 2\nApp.Config.set('c', D, 'Routes')\n";
        let (calls, _) = scan(source);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].line, 2);
        assert_eq!(calls[1].line, 4);
    }

    #[test]
    fn conservation_holds_on_a_mixed_file() {
        let source = "\
App.Config.set('a', B, 'Routes')
App.Config.set \"x#{y}\", B, 'Routes'
App.Config.set(url, f(1), 'Routes')
App.Config.set('open'
";
        let (calls, report) = scan(source);
        assert_eq!(calls.len(), report.calls_found);
        assert_eq!(
            report.calls_found,
            report.calls_fully_literal + report.calls_with_other
        );
        assert_eq!(report.calls_found, 3);
        assert_eq!(report.calls_fully_literal, 1);
        assert_eq!(report.calls_with_other, 2);
        assert_eq!(report.implicit_calls, 1);
        assert_eq!(report.unterminated, 1);
    }

    #[test]
    fn files_are_listed_sorted_and_scanned() {
        let root = std::env::temp_dir().join(format!("ruff_coffee_spo_{}", std::process::id()));
        let dir = root.join("app/controllers/sub");
        fs::create_dir_all(&dir).expect("create temp tree");
        fs::write(
            root.join("app/controllers/b.coffee"),
            "App.Config.set('b', B, 'Routes')\n",
        )
        .expect("write b");
        fs::write(dir.join("a.coffee"), "App.Config.set 'a', A, 'Routes'\n").expect("write a");
        fs::write(
            root.join("app/controllers/skip.js"),
            "App.Config.set('js', J, 'Routes')\n",
        )
        .expect("write js");
        let files = coffee_files(&root, "app/controllers");
        assert_eq!(
            files,
            vec![
                PathBuf::from("app/controllers/b.coffee"),
                PathBuf::from("app/controllers/sub/a.coffee"),
            ]
        );
        let (calls, report) = extract_calls(&root, &files, CALLEE);
        fs::remove_dir_all(&root).expect("remove temp tree");
        assert_eq!(report.files_scanned, 2);
        assert_eq!(report.calls_found, 2);
        assert_eq!(report.implicit_calls, 1);
        assert_eq!(calls[0].file, "app/controllers/b.coffee");
        assert_eq!(calls[1].file, "app/controllers/sub/a.coffee");
    }

    /// Run with `ZAMMAD_SRC=/path/to/zammad cargo test -p ruff_coffee_spo
    /// zammad_route_measurement -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs ZAMMAD_SRC"]
    #[expect(
        clippy::print_stdout,
        reason = "measurement output is the purpose of this test"
    )]
    fn zammad_route_measurement() {
        use std::collections::BTreeMap;

        let Ok(src) = std::env::var("ZAMMAD_SRC") else {
            return;
        };
        let root = Path::new(&src);
        let files = coffee_files(root, "app/assets/javascripts/app");
        let (calls, report) = extract_calls(root, &files, CALLEE);
        println!("{report:?}");
        assert_eq!(calls.len(), report.calls_found);
        assert_eq!(
            report.calls_found,
            report.calls_fully_literal + report.calls_with_other
        );
        let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
        let mut routes = 0;
        let mut routes_literal_first = 0;
        let mut shapes: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for call in &calls {
            let last = match call.args.last() {
                Some(CoffeeArg::Str(v)) => format!("Str({v})"),
                Some(CoffeeArg::Path(v)) => format!("Path({v})"),
                Some(CoffeeArg::Num(v)) => format!("Num({v})"),
                Some(CoffeeArg::Other { .. }) => "Other".to_string(),
                None => "(no args)".to_string(),
            };
            *kinds.entry(last).or_default() += 1;
            let site = format!("{}:{}", call.file, call.line);
            if call.args.last() == Some(&CoffeeArg::Str("Routes".to_string())) {
                routes += 1;
                match call.args.first() {
                    Some(CoffeeArg::Str(_)) => routes_literal_first += 1,
                    Some(CoffeeArg::Path(_)) => {
                        shapes
                            .entry("route first arg Path".into())
                            .or_default()
                            .push(site.clone());
                    }
                    Some(CoffeeArg::Other { source }) => {
                        shapes
                            .entry("route first arg Other".into())
                            .or_default()
                            .push(format!("{site}  {source}"));
                    }
                    _ => {}
                }
            }
            for arg in &call.args {
                if let CoffeeArg::Other { source } = arg {
                    shapes
                        .entry("any Other arg".into())
                        .or_default()
                        .push(format!("{site}  {source}"));
                    break;
                }
            }
        }
        println!("calls with last arg Str(\"Routes\"): {routes}");
        println!("  of those with a literal first arg: {routes_literal_first}");
        for (kind, count) in &kinds {
            println!("{count:5}  last arg {kind}");
        }
        for (shape, sites) in &shapes {
            println!("{shape}: {}", sites.len());
            for site in sites.iter().take(3) {
                println!("    {site}");
            }
        }
    }
}
