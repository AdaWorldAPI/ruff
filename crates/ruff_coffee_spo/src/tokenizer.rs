//! A CoffeeScript tokenizer: just enough to find calls without being fooled
//! by comments, strings or regular expressions.
//!
//! Token spans are byte offsets into the source, lines are 1-based.
//!
//! # Known limits
//!
//! - A `/` starts a regex only after no token, a newline, an arrow, or a
//!   punctuation token other than `)`, `]` or `}`. After an identifier, a
//!   number, a string or a closing bracket it is division. So
//!   `f /re/` (an implicit call with a regex argument) and `return /re/` are
//!   read as division. A candidate that has no closing `/` on its line is
//!   an operator.
//! - Heredoc strings keep their raw content; CoffeeScript's indentation
//!   stripping is not applied. `\x` and `\u` escapes are kept verbatim.
//! - A backtick JavaScript embed is one [`TokenKind::Opaque`] token.
//! - Block regexes (`///`) are one [`TokenKind::Regex`] token; interpolation
//!   inside them is not examined.

/// The kind of a [`Token`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    /// An identifier, including a leading `@` or `@@` and `$`.
    Ident,
    /// A string literal without interpolation, escapes resolved.
    Str(String),
    /// A double-quoted string or heredoc containing `#{…}`.
    Interpolated,
    /// A numeric literal.
    Num,
    /// A regex literal, including its flags.
    Regex,
    /// A single punctuation character.
    Punct(char),
    /// `->` or `=>`.
    Arrow,
    /// A line break; `indent` is the width of the next line's leading
    /// whitespace in bytes.
    Newline { indent: usize },
    /// A backtick embed or a string that never closes.
    Opaque,
}

/// One token with its byte span and 1-based line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub start: usize,
    pub end: usize,
    pub line: u32,
    /// Whether whitespace, a comment or a line break precedes the token.
    pub space_before: bool,
}

struct Quoted {
    end: usize,
    interpolated: bool,
    terminated: bool,
    value: String,
}

/// Tokenize `src`. Comments are dropped; everything else is kept.
pub fn tokenize(src: &str) -> Vec<Token> {
    let bytes = src.as_bytes();
    let mut tokens: Vec<Token> = Vec::new();
    let mut line: u32 = 1;
    let mut space = true;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let start = i;
        let start_line = line;
        let kind = match b {
            b' ' | b'\t' | b'\r' => {
                space = true;
                i += 1;
                continue;
            }
            b'\n' => {
                let mut j = i + 1;
                while j < bytes.len() && matches!(bytes[j], b' ' | b'\t') {
                    j += 1;
                }
                i += 1;
                line += 1;
                tokens.push(Token {
                    kind: TokenKind::Newline { indent: j - i },
                    start,
                    end: i,
                    line: start_line,
                    space_before: space,
                });
                space = true;
                continue;
            }
            b'#' => {
                i = skip_comment(src, i);
                line += count_newlines(&src[start..i]);
                space = true;
                continue;
            }
            b'\'' | b'"' => {
                let quoted = scan_quoted(src, i);
                i = quoted.end;
                if !quoted.terminated {
                    TokenKind::Opaque
                } else if quoted.interpolated {
                    TokenKind::Interpolated
                } else {
                    TokenKind::Str(quoted.value)
                }
            }
            b'`' => {
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i += 2,
                        b'`' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
                i = i.min(bytes.len());
                TokenKind::Opaque
            }
            b'0'..=b'9' => {
                i = scan_number(bytes, i);
                TokenKind::Num
            }
            b'@' => {
                i += 1;
                while i < bytes.len() && bytes[i] == b'@' {
                    i += 1;
                }
                i = scan_ident_run(bytes, i);
                TokenKind::Ident
            }
            _ if is_ident_byte(b) => {
                i = scan_ident_run(bytes, i);
                TokenKind::Ident
            }
            b'-' | b'=' if bytes.get(i + 1) == Some(&b'>') => {
                i += 2;
                TokenKind::Arrow
            }
            b'/' if regex_allowed(tokens.last()) => match scan_regex(bytes, i) {
                Some(end) => {
                    i = end;
                    TokenKind::Regex
                }
                None => {
                    i += 1;
                    TokenKind::Punct('/')
                }
            },
            _ => {
                let c = src[i..].chars().next().unwrap_or('\0');
                i += c.len_utf8().max(1);
                TokenKind::Punct(c)
            }
        };
        line += count_newlines(&src[start..i]);
        tokens.push(Token {
            kind,
            start,
            end: i,
            line: start_line,
            space_before: space,
        });
        space = false;
    }
    tokens
}

fn count_newlines(text: &str) -> u32 {
    u32::try_from(text.bytes().filter(|b| *b == b'\n').count()).unwrap_or(u32::MAX)
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$' || b >= 0x80
}

fn scan_ident_run(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && (is_ident_byte(bytes[i]) || bytes[i].is_ascii_digit()) {
        i += 1;
    }
    i
}

fn scan_number(bytes: &[u8], mut i: usize) -> usize {
    if bytes[i] == b'0'
        && matches!(
            bytes.get(i + 1),
            Some(b'x' | b'X' | b'b' | b'B' | b'o' | b'O')
        )
    {
        i += 2;
        while i < bytes.len() && bytes[i].is_ascii_alphanumeric() {
            i += 1;
        }
        return i;
    }
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if bytes.get(i) == Some(&b'.') && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(bytes.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        if bytes.get(j).is_some_and(u8::is_ascii_digit) {
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        }
    }
    i
}

/// End of the comment starting at `i`: `###` (not `####`) opens a block
/// comment ending at the next `###`; anything else runs to end of line.
fn skip_comment(src: &str, i: usize) -> usize {
    let bytes = src.as_bytes();
    if src[i..].starts_with("###") && bytes.get(i + 3) != Some(&b'#') {
        return src[i + 3..]
            .find("###")
            .map_or(bytes.len(), |pos| i + 3 + pos + 3);
    }
    src[i..].find('\n').map_or(bytes.len(), |pos| i + pos)
}

fn regex_allowed(prev: Option<&Token>) -> bool {
    match prev.map(|t| &t.kind) {
        None | Some(TokenKind::Newline { .. } | TokenKind::Arrow) => true,
        Some(TokenKind::Punct(c)) => !matches!(c, ')' | ']' | '}'),
        Some(_) => false,
    }
}

/// End of the regex literal starting at `i`, or `None` when no closing `/`
/// comes on the same line (a block regex `///` may span lines).
fn scan_regex(bytes: &[u8], i: usize) -> Option<usize> {
    let mut j;
    if bytes[i..].starts_with(b"///") {
        j = i + 3;
        while j < bytes.len() && !bytes[j..].starts_with(b"///") {
            j += 1;
        }
        if j >= bytes.len() {
            return None;
        }
        j += 3;
    } else {
        j = i + 1;
        let mut in_class = false;
        loop {
            match *bytes.get(j)? {
                b'\n' => return None,
                b'\\' => j += 1,
                b'[' => in_class = true,
                b']' => in_class = false,
                b'/' if !in_class => break,
                _ => {}
            }
            j += 1;
        }
        j += 1;
    }
    while j < bytes.len() && bytes[j].is_ascii_alphabetic() {
        j += 1;
    }
    Some(j)
}

/// Scan the string or heredoc opening at `start`.
fn scan_quoted(src: &str, start: usize) -> Quoted {
    let bytes = src.as_bytes();
    let quote = bytes[start];
    let triple = bytes[start..].starts_with(&[quote, quote, quote]);
    let mut i = start + if triple { 3 } else { 1 };
    let mut value = String::new();
    let mut interpolated = false;
    loop {
        let Some(&b) = bytes.get(i) else {
            return Quoted {
                end: bytes.len(),
                interpolated,
                terminated: false,
                value,
            };
        };
        if b == b'\\' {
            let Some(c) = src.get(i + 1..).and_then(|rest| rest.chars().next()) else {
                i = bytes.len();
                continue;
            };
            match c {
                'n' => value.push('\n'),
                't' => value.push('\t'),
                'r' => value.push('\r'),
                '0' => value.push('\0'),
                '\n' => {}
                'x' | 'u' => {
                    value.push('\\');
                    value.push(c);
                }
                other => value.push(other),
            }
            i += 1 + c.len_utf8();
        } else if triple && bytes[i..].starts_with(&[quote, quote, quote]) {
            return Quoted {
                end: i + 3,
                interpolated,
                terminated: true,
                value,
            };
        } else if !triple && b == quote {
            return Quoted {
                end: i + 1,
                interpolated,
                terminated: true,
                value,
            };
        } else if quote == b'"' && b == b'#' && bytes.get(i + 1) == Some(&b'{') {
            interpolated = true;
            i = skip_interpolation(src, i + 2);
        } else {
            let c = src[i..].chars().next().unwrap_or('\0');
            value.push(c);
            i += c.len_utf8().max(1);
        }
    }
}

/// Index just past the `}` closing an interpolation whose body starts at `i`.
/// Nested braces balance, and nested strings are skipped whole so a `}` inside
/// them does not close the interpolation.
fn skip_interpolation(src: &str, mut i: usize) -> usize {
    let bytes = src.as_bytes();
    let mut depth = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            b'\'' | b'"' => {
                i = scan_quoted(src, i).end;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        tokenize(src).into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn newline_carries_next_line_indent_and_lines_advance() {
        let tokens = tokenize("a\n  b\n\n    c");
        let newlines: Vec<usize> = tokens
            .iter()
            .filter_map(|t| match t.kind {
                TokenKind::Newline { indent } => Some(indent),
                _ => None,
            })
            .collect();
        assert_eq!(newlines, vec![2, 0, 4]);
        let lines: Vec<u32> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Ident)
            .map(|t| t.line)
            .collect();
        assert_eq!(lines, vec![1, 2, 4]);
    }

    #[test]
    fn slash_is_division_after_an_identifier_and_regex_after_an_operator() {
        assert_eq!(
            kinds("a / b / c"),
            vec![
                TokenKind::Ident,
                TokenKind::Punct('/'),
                TokenKind::Ident,
                TokenKind::Punct('/'),
                TokenKind::Ident,
            ]
        );
        assert_eq!(
            kinds("x = /a\\/b/gi"),
            vec![TokenKind::Ident, TokenKind::Punct('='), TokenKind::Regex]
        );
    }

    #[test]
    fn heredocs_and_unterminated_strings() {
        assert_eq!(kinds("'''a'b'''"), vec![TokenKind::Str("a'b".into())]);
        assert_eq!(kinds("\"\"\"a#{b}\"\"\""), vec![TokenKind::Interpolated]);
        assert_eq!(kinds("'open"), vec![TokenKind::Opaque]);
    }

    #[test]
    fn spans_are_byte_offsets() {
        let src = "é = 'ü'";
        let tokens = tokenize(src);
        assert_eq!(&src[tokens[0].start..tokens[0].end], "é");
        assert_eq!(&src[tokens[2].start..tokens[2].end], "'ü'");
    }
}
