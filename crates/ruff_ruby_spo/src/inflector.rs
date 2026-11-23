//! `ActiveSupport`'s English pluralization, as `OpenProject` vendors it
//! (`activesupport-8.1.4.1`).
//!
//! `has_and_belongs_to_many` names its internal middle association
//! `[<class>.downcase.pluralize, <assoc>].sort.join("_")`
//! (`activerecord-8.1.4.1/lib/active_record/associations/builder/has_and_belongs_to_many.rb:59-61`),
//! so the callbacks Rails generates for it can be named only with the
//! inflector. This module is that inflector and nothing else: no singular
//! rules, no `underscore`, no `camelize`.
//!
//! # Rules
//!
//! The rule table is `active_support/inflections.rb`: the plural rules at
//! lines 13-33, the irregular words at 63-68 (each `irregular` adds two plural
//! rules, `inflector/inflections.rb:191-203`) and the uncountable words at 70.
//! `Inflections#plural` PREPENDS (`inflector/inflections.rb:171`), and
//! `apply_inflections` stops at the first rule that matches
//! (`inflector/methods.rb:366`), so the rules are tried newest first: the
//! irregulars (last defined first, each one's plural-form rule before its
//! singular-form rule), then lines 33 down to 13. A word that is uncountable
//! (`/\b(?:equipment|...)\Z/i`, `inflector/inflections.rb:68-74`) is returned
//! as it is.
//!
//! # Not guessed
//!
//! [`pluralize`] decides only for ASCII input, where Ruby's `downcase` and the
//! case-insensitive rules above are plain ASCII operations. Any other word is
//! `None`: the caller reports it undecided. The application can add rules of
//! its own (`config/initializers/inflections.rb`); this table is the framework
//! default, so a caller whose application adds plural, irregular or
//! uncountable rules must say so before trusting it. `OpenProject` adds only
//! acronyms, which `pluralize` never reads.

/// Source of the rule table, relative to the gem directory.
pub const RULES: &str = "activesupport-8.1.4.1/lib/active_support/inflections.rb:13-33,63-68,70";

/// The words `inflect.uncountable` lists (`inflections.rb:70`).
const UNCOUNTABLE: [&str; 10] = [
    "equipment",
    "information",
    "rice",
    "money",
    "species",
    "series",
    "fish",
    "sheep",
    "jeans",
    "police",
];

/// `inflect.irregular(singular, plural)` pairs, in definition order
/// (`inflections.rb:63-68`). All six share their first letter, so each adds
/// `plural(/(s0)srest$/i, '\1prest')` and `plural(/(p0)prest$/i, '\1prest')`
/// (`inflector/inflections.rb:201-203`).
const IRREGULAR: [(&str, &str); 6] = [
    ("person", "people"),
    ("man", "men"),
    ("child", "children"),
    ("sex", "sexes"),
    ("move", "moves"),
    ("zombie", "zombies"),
];

/// The plural of `word`, or `None` when this table cannot decide it (the word
/// is not ASCII).
#[must_use]
pub fn pluralize(word: &str) -> Option<String> {
    if !word.is_ascii() {
        return None;
    }
    if word.is_empty() || uncountable(word) {
        return Some(word.to_string());
    }
    Some(first_rule(word))
}

/// `/\b(?:m1|m2|...)\Z/i`: the word ends in an uncountable word that starts
/// the word or follows a non-word character.
fn uncountable(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    UNCOUNTABLE.iter().any(|m| {
        lower.strip_suffix(m).is_some_and(|before| {
            before
                .as_bytes()
                .last()
                .is_none_or(|b| !(b.is_ascii_alphanumeric() || *b == b'_'))
        })
    })
}

/// Whether `word` ends in `suffix`, ignoring ASCII case.
fn ends(word: &str, suffix: &str) -> bool {
    word.len() >= suffix.len()
        && word.as_bytes()[word.len() - suffix.len()..].eq_ignore_ascii_case(suffix.as_bytes())
}

/// `word` without its last `n` bytes, then `tail`.
fn swap(word: &str, n: usize, tail: &str) -> String {
    format!("{}{tail}", &word[..word.len() - n])
}

fn is_vowel_or_y(b: u8) -> bool {
    matches!(
        b.to_ascii_lowercase(),
        b'a' | b'e' | b'i' | b'o' | b'u' | b'y'
    )
}

/// The first matching rule, newest first.
///
/// A rule's replacement keeps only what its groups captured and writes the
/// rest of the suffix as the rule spells it, lowercase: `^(m|l)ice$` ->
/// `'\1ice'` turns `MICE` into `Mice`. A rule whose group captures the whole
/// match (`(hive)$`, `^(oxen)$`) keeps the word's own case. The catch-all `/$/ -> "s"`
/// (`inflections.rb:13`) always matches, so there is always a result.
#[allow(clippy::too_many_lines)] // one arm per rule, in table order
fn first_rule(w: &str) -> String {
    // Irregulars, newest definition first; the plural-form rule before the
    // singular-form rule (the later `plural` call is tried first).
    for (singular, plural) in IRREGULAR.iter().rev() {
        for form in [plural, singular] {
            if ends(w, form) {
                // `(first letter)rest$` -> `\1` + the plural's rest.
                return swap(w, form.len() - 1, &plural[1..]);
            }
        }
    }
    let n = w.len();
    let lower = w.to_ascii_lowercase();
    let l = lower.as_str();
    // inflections.rb:33 `(quiz)$`
    if l.ends_with("quiz") {
        return format!("{w}zes");
    }
    // :32 `^(oxen)$`
    if l == "oxen" {
        return w.to_string();
    }
    // :31 `^(ox)$`
    if l == "ox" {
        return format!("{w}en");
    }
    // :30 `^(m|l)ice$`
    if l == "mice" || l == "lice" {
        return swap(w, 3, "ice");
    }
    // :29 `^(m|l)ouse$`
    if l == "mouse" || l == "louse" {
        return swap(w, 4, "ice");
    }
    // :28 `(matr|vert|ind)(?:ix|ex)$`
    for stem in ["matr", "vert", "ind"] {
        for tail in ["ix", "ex"] {
            if l.ends_with(&format!("{stem}{tail}")) {
                return swap(w, 2, "ices");
            }
        }
    }
    // :27 `(x|ch|ss|sh)$`
    if ["x", "ch", "ss", "sh"].iter().any(|s| l.ends_with(s)) {
        return format!("{w}es");
    }
    // :26 `([^aeiouy]|qu)y$`
    if l.ends_with('y') && n >= 2 {
        let before = &w.as_bytes()[..n - 1];
        if l[..n - 1].ends_with("qu") || !is_vowel_or_y(before[before.len() - 1]) {
            return swap(w, 1, "ies");
        }
    }
    // :25 `(hive)$`
    if l.ends_with("hive") {
        return format!("{w}s");
    }
    // :24 `(?:([^f])fe|([lr])f)$`
    if l.ends_with("fe") && n >= 3 && !l[..n - 2].ends_with('f') {
        return swap(w, 2, "ves");
    }
    if l.ends_with('f') && n >= 2 && matches!(l.as_bytes()[n - 2], b'l' | b'r') {
        return swap(w, 1, "ves");
    }
    // :23 `sis$`
    if l.ends_with("sis") {
        return swap(w, 3, "ses");
    }
    // :22 `([ti])a$` -> `\1a`: the word, its `a` respelled
    if l.len() >= 2 && l.ends_with('a') && matches!(l.as_bytes()[n - 2], b't' | b'i') {
        return swap(w, 1, "a");
    }
    // :21 `([ti])um$`
    if l.len() >= 3 && l.ends_with("um") && matches!(l.as_bytes()[n - 3], b't' | b'i') {
        return swap(w, 2, "a");
    }
    // :20 `(buffal|tomat)o$`
    if l.ends_with("buffalo") || l.ends_with("tomato") {
        return swap(w, 1, "oes");
    }
    // :19 `(bu)s$`
    if l.ends_with("bus") {
        return swap(w, 1, "ses");
    }
    // :18 `(alias|status)$`
    if l.ends_with("alias") || l.ends_with("status") {
        return format!("{w}es");
    }
    // :17 `(octop|vir)i$` -> `\1i`: the word, its `i` respelled
    if l.ends_with("octopi") || l.ends_with("viri") {
        return swap(w, 1, "i");
    }
    // :16 `(octop|vir)us$`
    if l.ends_with("octopus") || l.ends_with("virus") {
        return swap(w, 2, "i");
    }
    // :15 `^(ax|test)is$`
    if l == "axis" || l == "testis" {
        return swap(w, 2, "es");
    }
    // :14 `s$` -> "s": the word, its `s` respelled
    if l.ends_with('s') {
        return swap(w, 1, "s");
    }
    // :13 `$` -> "s"
    format!("{w}s")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(w: &str) -> String {
        pluralize(w).unwrap_or_else(|| panic!("{w} undecided"))
    }

    /// Pairs Rails' own `inflector_test_cases.rb` lists, plus the class names
    /// `habtm` meets (`workpackage`, `project`).
    #[test]
    fn the_default_rules_give_the_pairs_rails_documents() {
        for (singular, plural) in [
            ("workpackage", "workpackages"),
            ("project", "projects"),
            ("status", "statuses"),
            ("box", "boxes"),
            ("church", "churches"),
            ("class", "classes"),
            ("dish", "dishes"),
            ("category", "categories"),
            ("quiz", "quizzes"),
            ("matrix", "matrices"),
            ("vertex", "vertices"),
            ("index", "indices"),
            ("wolf", "wolves"),
            ("knife", "knives"),
            ("life", "lives"),
            ("hive", "hives"),
            ("day", "days"),
            ("analysis", "analyses"),
            ("datum", "data"),
            ("tomato", "tomatoes"),
            ("buffalo", "buffaloes"),
            ("bus", "buses"),
            ("alias", "aliases"),
            ("axis", "axes"),
            ("testis", "testes"),
            ("octopus", "octopi"),
            ("virus", "viri"),
            ("ox", "oxen"),
            ("mouse", "mice"),
            ("louse", "lice"),
            ("person", "people"),
            ("man", "men"),
            ("child", "children"),
            ("sex", "sexes"),
            ("move", "moves"),
            ("zombie", "zombies"),
            ("news", "news"),
            ("leaf", "leafs"),
        ] {
            assert_eq!(p(singular), plural, "{singular}");
        }
    }

    #[test]
    fn an_uncountable_word_is_returned_unchanged() {
        for w in [
            "equipment",
            "information",
            "rice",
            "money",
            "species",
            "series",
            "fish",
            "sheep",
            "jeans",
            "police",
        ] {
            assert_eq!(p(w), w);
        }
        // `\b` before the word: a trailing word after `::` is uncountable,
        // one glued on by `_` or a letter is not.
        assert_eq!(p("foo::fish"), "foo::fish");
        assert_eq!(p("foo_fish"), "foo_fishes");
        assert_eq!(p("goldfish"), "goldfishes");
    }

    /// The irregular rules are not anchored at the start of the word
    /// (`/(m)an$/i`), so they apply to compounds: Rails' own behaviour,
    /// including `human` -> `humen`.
    #[test]
    fn irregular_rules_apply_to_a_word_that_merely_ends_in_one() {
        assert_eq!(p("woman"), "women");
        assert_eq!(p("human"), "humen");
        assert_eq!(p("salesperson"), "salespeople");
        assert_eq!(p("remove"), "removes");
    }

    #[test]
    fn case_of_the_captured_part_is_kept_and_the_suffix_is_lowercase() {
        assert_eq!(p("Category"), "Categories");
        assert_eq!(p("PERSON"), "People");
        assert_eq!(p("Status"), "Statuses");
        assert_eq!(p("Quiz"), "Quizzes");
        // Each rule writes its literal suffix as written, even where the match
        // already ended in those letters. Expected values are the output of
        // `ActiveSupport::Inflector.pluralize` from activesupport-8.1.4.1.
        for (word, plural) in [
            ("MICE", "Mice"),
            ("LICE", "Lice"),
            ("MOUSE", "Mice"),
            ("DATA", "DATa"),
            ("MEDIA", "MEDIa"),
            ("DATUM", "DATa"),
            ("OCTOPI", "OCTOPi"),
            ("VIRI", "VIRi"),
            ("OCTOPUS", "OCTOPi"),
            ("BUS", "BUses"),
            ("TOMATO", "TOMAToes"),
            ("BUFFALO", "BUFFALoes"),
            ("POSTS", "POSTs"),
            ("OXEN", "OXEN"),
            ("OX", "OXen"),
            ("HIVE", "HIVEs"),
            ("STATUS", "STATUSes"),
            ("ANALYSIS", "ANALYses"),
            ("MATRIX", "MATRices"),
            ("SHEEP", "SHEEP"),
        ] {
            assert_eq!(p(word), plural, "{word}");
        }
    }

    #[test]
    fn a_word_this_table_cannot_decide_is_none() {
        assert_eq!(pluralize("caf\u{e9}"), None);
        assert_eq!(pluralize("\u{5de5}\u{5355}"), None);
        assert_eq!(pluralize(""), Some(String::new()));
    }

    /// `([^aeiouy]|qu)y$`: a vowel before the `y` keeps it, `qu` does not.
    #[test]
    fn y_after_a_vowel_is_kept_and_after_qu_is_replaced() {
        assert_eq!(p("day"), "days");
        assert_eq!(p("key"), "keys");
        assert_eq!(p("soliloquy"), "soliloquies");
        assert_eq!(p("party"), "parties");
        assert_eq!(p("y"), "ys");
    }
}
