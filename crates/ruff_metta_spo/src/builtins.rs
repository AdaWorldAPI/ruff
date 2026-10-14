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

/// The category of a native name, or `None` if Hyperon does not implement it natively.
pub(crate) fn lookup(name: &str) -> Option<(&'static str, Category)> {
    TABLE
        .iter()
        .find(|(n, _)| *n == name)
        .map(|&(n, c)| (n, c))
}

/// Number, string and boolean tokens, plus `&self`. These are values, never calls.
pub(crate) fn is_literal(name: &str) -> bool {
    if name.starts_with('"') || matches!(name, "True" | "False" | "&self" | "PI" | "EXP") {
        return true;
    }
    let digits = name.strip_prefix(['-', '+']).unwrap_or(name);
    !digits.is_empty() && digits.parse::<f64>().is_ok() && digits.starts_with(|c: char| c.is_ascii_digit())
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
    fn table_has_no_duplicates() {
        let mut names: Vec<&str> = TABLE.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before);
    }
}
