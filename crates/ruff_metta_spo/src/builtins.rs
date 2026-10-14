//! The closed table of names Hyperon implements natively.
//!
//! Two sources, both in the AdaWorldAPI hyperon-experimental fork:
//!
//! - the minimal-MeTTa instructions, the `*_SYMBOL` constants in
//!   `lib/src/metta/mod.rs` (`eval`, `chain`, `unify`, ...);
//! - the grounded tokens the stdlib registers in Rust, the `register_token` /
//!   `register_function` calls under `lib/src/metta/runner/stdlib/*.rs`.
//!
//! Functions the stdlib defines in MeTTa itself (`let`, `if`, `case`, `collapse`,
//! ...) are deliberately absent: parse the stdlib with [`crate::hyperon_stdlib`]
//! and they are classified from their own definitions.
//!
//! PeTTa implements far more natively (`if`, `let`, `case`, `collapse`, ...), and
//! says so in its own Prolog registry, which [`Natives::petta`] reads.

use std::collections::BTreeMap;

/// What a native name does. Every category except [`Category::Relational`] makes a
/// rule interpreter-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    /// `match`: query a space with a pattern. The one native a relational
    /// lowering takes.
    Relational,
    /// A minimal-MeTTa instruction (`eval`, `chain`, `unify`, `function`, ...).
    Instruction,
    /// Non-deterministic results as values (`superpose`).
    Bag,
    /// Mutable state cells (`get-state`, `change-state!`).
    State,
    /// Space creation and mutation (`add-atom`, `remove-atom`, `new-space`, ...).
    Space,
    /// Numbers and comparison (`+`, `<`, `==`, `sqrt-math`, ...).
    Arithmetic,
    /// Boolean connectives (`and`, `or`, `not`, `xor`).
    Logic,
    /// Operations on atoms as values (`size-atom`, `union-atom`, `get-type`, ...).
    AtomOps,
    /// Output, modules and interpreter control (`println!`, `import!`, `pragma!`, ...).
    Effect,
}

const TABLE: &[(&str, Category)] = &[
    ("match", Category::Relational),
    // minimal MeTTa, lib/src/metta/mod.rs
    ("eval", Category::Instruction),
    ("evalc", Category::Instruction),
    ("chain", Category::Instruction),
    ("unify", Category::Instruction),
    ("decons-atom", Category::Instruction),
    ("cons-atom", Category::Instruction),
    ("function", Category::Instruction),
    ("return", Category::Instruction),
    ("collapse-bind", Category::Instruction),
    ("superpose-bind", Category::Instruction),
    ("metta", Category::Instruction),
    ("call-native", Category::Instruction),
    ("context-space", Category::Instruction),
    // grounded tokens, lib/src/metta/runner/stdlib/*.rs
    ("superpose", Category::Bag),
    ("get-state", Category::State),
    ("change-state!", Category::State),
    ("add-atom", Category::Space),
    ("remove-atom", Category::Space),
    ("new-space", Category::Space),
    ("get-atoms", Category::Space),
    ("mod-space!", Category::Space),
    ("bind!", Category::Space),
    ("+", Category::Arithmetic),
    ("-", Category::Arithmetic),
    ("*", Category::Arithmetic),
    ("/", Category::Arithmetic),
    ("%", Category::Arithmetic),
    ("<", Category::Arithmetic),
    ("<=", Category::Arithmetic),
    (">", Category::Arithmetic),
    (">=", Category::Arithmetic),
    ("==", Category::Arithmetic),
    ("abs-math", Category::Arithmetic),
    ("acos-math", Category::Arithmetic),
    ("asin-math", Category::Arithmetic),
    ("atan-math", Category::Arithmetic),
    ("ceil-math", Category::Arithmetic),
    ("cos-math", Category::Arithmetic),
    ("floor-math", Category::Arithmetic),
    ("isinf-math", Category::Arithmetic),
    ("isnan-math", Category::Arithmetic),
    ("log-math", Category::Arithmetic),
    ("pow-math", Category::Arithmetic),
    ("round-math", Category::Arithmetic),
    ("sin-math", Category::Arithmetic),
    ("sqrt-math", Category::Arithmetic),
    ("tan-math", Category::Arithmetic),
    ("trunc-math", Category::Arithmetic),
    ("and", Category::Logic),
    ("or", Category::Logic),
    ("not", Category::Logic),
    ("xor", Category::Logic),
    ("=alpha", Category::AtomOps),
    ("if-equal", Category::AtomOps),
    ("capture", Category::AtomOps),
    ("sealed", Category::AtomOps),
    ("format-args", Category::AtomOps),
    ("get-metatype", Category::AtomOps),
    ("get-type", Category::AtomOps),
    ("get-type-space", Category::AtomOps),
    ("index-atom", Category::AtomOps),
    ("size-atom", Category::AtomOps),
    ("min-atom", Category::AtomOps),
    ("max-atom", Category::AtomOps),
    ("union-atom", Category::AtomOps),
    ("intersection-atom", Category::AtomOps),
    ("subtraction-atom", Category::AtomOps),
    ("unique-atom", Category::AtomOps),
    ("_minimal-foldl-atom", Category::AtomOps),
    ("println!", Category::Effect),
    ("trace!", Category::Effect),
    ("print-alternatives!", Category::Effect),
    ("print-mods!", Category::Effect),
    ("import!", Category::Effect),
    ("include", Category::Effect),
    ("register-module!", Category::Effect),
    ("git-module!", Category::Effect),
    ("pragma!", Category::Effect),
    ("nop", Category::Effect),
];

/// The native names of one engine. A name with no rule in the program and an entry
/// here is native; everything else in call position is a rule or a constructor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Natives {
    dialect: &'static str,
    table: BTreeMap<String, Category>,
}

impl Natives {
    /// Hyperon: the minimal-MeTTa instructions and the grounded stdlib tokens.
    /// Pair it with [`crate::hyperon_stdlib`], which defines the rest in MeTTa.
    #[must_use]
    pub fn hyperon() -> Self {
        Self {
            dialect: "hyperon",
            table: TABLE.iter().map(|&(n, c)| (n.to_string(), c)).collect(),
        }
    }

    /// PeTTa: every `builtin_spec(Name/Arity, implementation(Kind), ...)` record of a
    /// `src/typecheck/builtin_registry.pl`, PeTTa's single source of builtin
    /// signatures. Names are keyed without arity (`if/2` and `if/3` are one name).
    ///
    /// A name Hyperon also implements keeps Hyperon's category, so `match` stays
    /// relational in both dialects. The others are categorised by their
    /// implementation kind: `compiler` forms as [`Category::Instruction`], `spaces`
    /// as [`Category::Space`], `external`, `foreign_promise` and `parser` as
    /// [`Category::Effect`], and `metta` as [`Category::AtomOps`].
    ///
    /// # Errors
    ///
    /// A `builtin_spec(` line that does not parse, or a file with no records.
    pub fn petta(registry: &str) -> Result<Self, String> {
        let hyperon = Self::hyperon();
        let mut table = BTreeMap::new();
        for (lineno, line) in registry.lines().enumerate() {
            let Some(rest) = line.strip_prefix("builtin_spec(") else {
                continue;
            };
            let (name, kind) =
                parse_spec(rest).ok_or_else(|| format!("line {}: {line}", lineno + 1))?;
            let category = hyperon.category(&name).unwrap_or(match kind.as_str() {
                "compiler" => Category::Instruction,
                "spaces" => Category::Space,
                "external" | "foreign_promise" | "parser" => Category::Effect,
                "metta" => Category::AtomOps,
                other => {
                    return Err(format!(
                        "line {}: unknown implementation({other})",
                        lineno + 1
                    ));
                }
            });
            table.insert(name, category);
        }
        if table.is_empty() {
            return Err("no builtin_spec records".to_string());
        }
        Ok(Self {
            dialect: "petta",
            table,
        })
    }

    /// PeTTa's registry from the AdaWorldAPI fork checkout.
    #[must_use]
    pub fn petta_fork() -> Self {
        Self::petta(include_str!(
            "../../../../petta/src/typecheck/builtin_registry.pl"
        ))
        .expect("the fork's builtin registry parses")
    }

    /// `"hyperon"` or `"petta"`.
    #[must_use]
    pub fn dialect(&self) -> &'static str {
        self.dialect
    }

    /// Number of native names.
    #[must_use]
    pub fn len(&self) -> usize {
        self.table.len()
    }

    /// Whether the table is empty. Never true for the two constructors.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    /// The category of a native name.
    #[must_use]
    pub fn category(&self, name: &str) -> Option<Category> {
        self.table.get(name).copied()
    }
}

/// `Name/Arity, implementation(Kind), ...` with `Name` a Prolog atom, quoted or not.
fn parse_spec(rest: &str) -> Option<(String, String)> {
    let (name, after) = if let Some(q) = rest.strip_prefix('\'') {
        let mut name = String::new();
        let mut chars = q.char_indices();
        let end = loop {
            let (i, c) = chars.next()?;
            match c {
                '\\' => name.push(chars.next()?.1),
                '\'' if q[i + 1..].starts_with('\'') => {
                    name.push('\'');
                    chars.next();
                }
                '\'' => break i + 1,
                c => name.push(c),
            }
        };
        (name, &q[end..])
    } else {
        let end = rest.find('/')?;
        (rest[..end].to_string(), &rest[end..])
    };
    let after = after.strip_prefix('/')?;
    let after = &after[after.find(", implementation(")? + ", implementation(".len()..];
    let kind = after[..after.find(')')?].to_string();
    if name.is_empty() {
        return None;
    }
    Some((name, kind))
}

/// Number, string and boolean tokens, plus `&self`. These are values, never calls.
pub(crate) fn is_literal(name: &str) -> bool {
    if name.starts_with('"') || matches!(name, "True" | "False" | "&self" | "PI" | "EXP") {
        return true;
    }
    let digits = name.strip_prefix(['-', '+']).unwrap_or(name);
    !digits.is_empty()
        && digits.parse::<f64>().is_ok()
        && digits.starts_with(|c: char| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals() {
        for yes in ["1", "-1", "+2", "3.5", "1e9", "\"s\"", "True", "&self"] {
            assert!(is_literal(yes), "{yes}");
        }
        for no in ["-", "+", "inf", "nan", "x1", "e9", "parent"] {
            assert!(!is_literal(no), "{no}");
        }
    }

    #[test]
    fn petta_registry_records_parse() {
        let n = Natives::petta(
            "builtin_spec('+'/2, implementation(metta), x).\n\
             builtin_spec('#\\\\='/2, implementation(metta), x).\n\
             builtin_spec('it''s'/1, implementation(external), x).\n\
             builtin_spec(if/3, implementation(compiler), x).\n\
             builtin_spec(progn/variadic, implementation(compiler), x).\n\
             builtin_spec(match/3, implementation(spaces), x).\n",
        )
        .unwrap();
        assert_eq!(n.len(), 6);
        assert_eq!(n.category("+"), Some(Category::Arithmetic));
        assert_eq!(n.category("#\\="), Some(Category::AtomOps));
        assert_eq!(n.category("it's"), Some(Category::Effect));
        assert_eq!(n.category("if"), Some(Category::Instruction));
        assert_eq!(n.category("progn"), Some(Category::Instruction));
        assert_eq!(n.category("match"), Some(Category::Relational));
    }

    #[test]
    fn a_broken_record_is_an_error() {
        assert!(Natives::petta("builtin_spec('+/2, implementation(metta)).\n").is_err());
        assert!(Natives::petta("% no records\n").is_err());
        assert!(Natives::petta("builtin_spec(f/1, implementation(magic), x).\n").is_err());
    }

    #[test]
    fn the_fork_registry_has_every_record() {
        // One builtin_spec record per line in src/typecheck/builtin_registry.pl.
        let text = include_str!("../../../../petta/src/typecheck/builtin_registry.pl");
        let records = text
            .lines()
            .filter(|l| l.starts_with("builtin_spec("))
            .count();
        let n = Natives::petta_fork();
        // 183 records, 172 distinct names: overloads (if/2 and if/3, ...) share a
        // name. Counted independently on the fork at 1d19a1d with
        //   grep '^builtin_spec(' builtin_registry.pl | sed -E "s/^builtin_spec\(('([^']|'')*'|[^/]*)\/.*/\1/" | sort -u
        assert_eq!(records, 183);
        assert_eq!(n.len(), 172);
        assert_eq!(n.category("match"), Some(Category::Relational));
    }

    #[test]
    fn table_has_no_duplicates() {
        let mut names: Vec<&str> = TABLE.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before);
    }
}
