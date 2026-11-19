//! What may stand in a TSV cell.
//!
//! Subjects and names are identities: a control character in one is refused,
//! never repaired, because a repaired name is a different name. Free text
//! (option values, DSL arguments, call text as written) is escaped instead,
//! so a multi-line hash argument does not stop a run.

use std::fmt::Write as _;

use crate::error::Error;

/// Refuse an empty name, a name with a control character (tab and newline
/// included) and a name with a `?` path segment, which the reader treats as
/// the placeholder for an unresolved name.
pub fn name<'a>(what: &'static str, value: &'a str) -> Result<&'a str, Error> {
    let bad = value.is_empty()
        || value.chars().any(char::is_control)
        || value.split("::").any(|segment| segment == "?");
    if bad {
        return Err(Error::BadCell {
            what,
            value: value.to_owned(),
        });
    }
    Ok(value)
}

/// Escape free text so it holds no tab, newline or other control character.
/// Backslash is escaped too, so the mapping can be undone.
pub fn text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    if out.is_empty() {
        out.push('-');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_refuses_control_characters_and_placeholders() {
        assert!(name("subject", "shop::Ticket").is_ok());
        assert!(name("subject", "shop::valid?").is_ok());
        for bad in ["", "a\tb", "a\nb", "a\rb", "a\u{7}b", "shop::?::x"] {
            assert!(name("subject", bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn text_escapes_and_stays_reversible() {
        assert_eq!(text("a\tb\nc\\d"), "a\\tb\\nc\\\\d");
        assert_eq!(text("plain"), "plain");
        assert_eq!(text("\u{1}"), "\\u{1}");
        assert!(!text("x\ty\nz\r").contains(['\t', '\n', '\r']));
    }
}
