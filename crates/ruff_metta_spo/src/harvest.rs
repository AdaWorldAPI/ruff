//! Parse one MeTTa file into its rules, type declarations, facts and queries.

use hyperon::metta::text::{SExprParser, Tokenizer};
use hyperon_atom::Atom;

/// One `(= lhs rhs)` rule.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    /// The left-hand side, e.g. `(grandparent $x)`.
    pub lhs: Atom,
    /// The right-hand side, the body that is evaluated.
    pub rhs: Atom,
}

impl Rule {
    /// The symbol the rule defines: the head of an expression left-hand side, or a
    /// bare symbol left-hand side. `None` when the head is a variable or an
    /// expression, which no call site can name.
    #[must_use]
    pub fn head_name(&self) -> Option<&str> {
        match &self.lhs {
            Atom::Symbol(s) => Some(s.name()),
            Atom::Expression(e) => match e.children().first() {
                Some(Atom::Symbol(s)) => Some(s.name()),
                _ => None,
            },
            _ => None,
        }
    }
}

/// One `(: name type)` declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeDecl {
    /// The declared symbol. Declarations whose subject is not a symbol are skipped.
    pub name: String,
    /// The declared type, e.g. `(-> Atom Atom)`.
    pub ty: Atom,
}

/// The harvest of one file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Module {
    /// The module name, normally the file stem.
    pub name: String,
    /// `=` rules, in source order.
    pub rules: Vec<Rule>,
    /// `:` declarations, in source order.
    pub types: Vec<TypeDecl>,
    /// Top-level atoms that are neither rules nor declarations: they are added to
    /// the space as facts.
    pub facts: Vec<Atom>,
    /// Atoms preceded by `!`: evaluated when the file runs.
    pub queries: Vec<Atom>,
    /// The parser error, if the file did not parse to the end. Everything parsed
    /// before the error is kept.
    pub parse_error: Option<String>,
}

/// Parse MeTTa source text.
///
/// The tokenizer is empty on purpose: numbers, strings and builtin names stay
/// symbols, because the harvest wants the syntax, not grounded values.
#[must_use]
pub fn parse_module(name: &str, text: &str) -> Module {
    let tokenizer = Tokenizer::new();
    let mut parser = SExprParser::new(text);
    let mut module = Module {
        name: name.to_string(),
        ..Module::default()
    };
    let mut bang = false;
    loop {
        match parser.parse(&tokenizer) {
            Ok(Some(atom)) => {
                if bang {
                    bang = false;
                    module.queries.push(atom);
                } else if is_symbol(&atom, "!") {
                    bang = true;
                } else {
                    file_top_level(&mut module, atom);
                }
            }
            Ok(None) => break,
            Err(e) => {
                module.parse_error = Some(e);
                break;
            }
        }
    }
    module
}

fn file_top_level(module: &mut Module, atom: Atom) {
    if let Atom::Expression(e) = &atom {
        match e.children() {
            [eq, lhs, rhs] if is_symbol(eq, "=") => {
                module.rules.push(Rule {
                    lhs: lhs.clone(),
                    rhs: rhs.clone(),
                });
                return;
            }
            [colon, Atom::Symbol(s), ty] if is_symbol(colon, ":") => {
                module.types.push(TypeDecl {
                    name: s.name().to_string(),
                    ty: ty.clone(),
                });
                return;
            }
            _ => {}
        }
    }
    module.facts.push(atom);
}

fn is_symbol(atom: &Atom, name: &str) -> bool {
    matches!(atom, Atom::Symbol(s) if s.name() == name)
}

/// The stdlib that ships with the hyperon-experimental fork, as a module named
/// `stdlib`. Classifying it alongside a program resolves `let`, `if`, `case` and
/// the other library functions from their definitions.
#[must_use]
pub fn hyperon_stdlib() -> Module {
    parse_module(
        "stdlib",
        include_str!("../../../../hyperon-experimental/lib/src/metta/runner/stdlib/stdlib.metta"),
    )
}
