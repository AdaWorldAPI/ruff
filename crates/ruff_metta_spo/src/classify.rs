//! Lowerable versus interpreter-only, decided over the whole call graph.

use std::collections::{BTreeMap, BTreeSet};

use hyperon_atom::Atom;

use crate::builtins::{self, Category, Natives};
use crate::harvest::{Module, Rule};

/// Why a rule head is interpreter-only. The first reason found, in clause order and
/// then in evaluation order within the clause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// The body uses a native name other than `match`.
    Builtin {
        /// The native name, as written.
        name: String,
        /// What the native name does.
        category: Category,
    },
    /// The body has an expression headed by a variable, `($f $x)`. If the variable
    /// binds to a function this is a call through it; if it binds to data the
    /// expression is a tuple, as in `(= (flip ($a $b)) ($b $a))`. Which one is decided
    /// at run time, so the verdict is conservative: such a rule may in fact be
    /// lowerable.
    VariableHead,
    /// The body calls a head that is interpreter-only. For a cycle this names the
    /// first tainted callee in the clause.
    Calls {
        /// The interpreter-only callee.
        callee: String,
    },
}

/// The classification of one rule head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Every clause only matches, builds data and calls lowerable heads.
    Lowerable,
    /// At least one clause needs the interpreter.
    InterpreterOnly(Reason),
}

/// The result of [`classify`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Classification {
    /// One verdict per rule head, across all modules.
    pub verdicts: BTreeMap<String, Verdict>,
    /// Calls between rule heads, from caller to callee.
    pub calls: BTreeMap<String, BTreeSet<String>>,
    /// Expression heads with no rule and no native implementation: data
    /// constructors, or functions this harvest cannot see.
    pub constructors: BTreeSet<String>,
    /// Rules whose head is not a symbol, e.g. `(= ($f $x) ...)`. No call site can
    /// name them, so they get no verdict.
    pub unnamed_rules: usize,
}

/// What one clause body does directly.
#[derive(Default)]
struct Body {
    taint: Option<Reason>,
    callees: Vec<String>,
}

/// Classify every rule head in `modules` against one engine's native names. Names
/// resolve across all modules, as they would in one space. For Hyperon, pass
/// [`crate::hyperon_stdlib`] among the modules to resolve the stdlib.
#[must_use]
pub fn classify(natives: &Natives, modules: &[Module]) -> Classification {
    let mut clauses: BTreeMap<&str, Vec<&Rule>> = BTreeMap::new();
    let mut out = Classification::default();
    for rule in modules.iter().flat_map(|m| &m.rules) {
        match rule.head_name() {
            Some(name) => clauses.entry(name).or_default().push(rule),
            None => out.unnamed_rules += 1,
        }
    }

    let mut bodies: BTreeMap<&str, Body> = BTreeMap::new();
    for (&head, rules) in &clauses {
        let mut body = Body::default();
        for rule in rules {
            walk(
                &rule.rhs,
                natives,
                &clauses,
                &mut body,
                &mut out.constructors,
            );
        }
        out.calls
            .insert(head.to_string(), body.callees.iter().cloned().collect());
        bodies.insert(head, body);
    }

    // Fixpoint on the tainted set first, then name a reason. Naming reasons while
    // the set still grows would make the named callee depend on map order.
    let mut tainted: BTreeSet<&str> = bodies
        .iter()
        .filter(|(_, b)| b.taint.is_some())
        .map(|(&h, _)| h)
        .collect();
    loop {
        let grown: Vec<&str> = bodies
            .iter()
            .filter(|(h, b)| {
                !tainted.contains(*h) && b.callees.iter().any(|c| tainted.contains(c.as_str()))
            })
            .map(|(&h, _)| h)
            .collect();
        if grown.is_empty() {
            break;
        }
        tainted.extend(grown);
    }

    for (&head, body) in &bodies {
        let verdict = if let Some(reason) = &body.taint {
            Verdict::InterpreterOnly(reason.clone())
        } else if let Some(callee) = body.callees.iter().find(|c| tainted.contains(c.as_str())) {
            Verdict::InterpreterOnly(Reason::Calls {
                callee: callee.clone(),
            })
        } else {
            Verdict::Lowerable
        };
        out.verdicts.insert(head.to_string(), verdict);
    }
    out
}

/// Walk an evaluated position. Records the first taint, every callee in order of
/// first occurrence, and unresolved expression heads.
fn walk(
    atom: &Atom,
    natives: &Natives,
    clauses: &BTreeMap<&str, Vec<&Rule>>,
    body: &mut Body,
    constructors: &mut BTreeSet<String>,
) {
    match atom {
        // A bare symbol is evaluated too: `(= answer 42)` makes `answer` a call.
        Atom::Symbol(s) => {
            if clauses.contains_key(s.name()) {
                callee(body, s.name());
            }
        }
        Atom::Expression(e) => {
            let children = e.children();
            let Some(head) = children.first() else {
                return;
            };
            let args = &children[1..];
            match head {
                Atom::Symbol(s) => {
                    let name = s.name();
                    match name {
                        // (match space pattern template): the pattern is matched,
                        // never evaluated. The template is evaluated afterwards.
                        "match" if args.len() == 3 => {
                            walk(&args[0], natives, clauses, body, constructors);
                            walk(&args[2], natives, clauses, body, constructors);
                            return;
                        }
                        // `quote` stops evaluation of its argument.
                        "quote" => return,
                        _ => {}
                    }
                    if clauses.contains_key(name) {
                        callee(body, name);
                    } else if let Some(category) = natives.category(name) {
                        if category != Category::Relational && body.taint.is_none() {
                            body.taint = Some(Reason::Builtin {
                                name: name.to_string(),
                                category,
                            });
                        }
                    } else if !builtins::is_literal(name) {
                        constructors.insert(name.to_string());
                    }
                }
                Atom::Variable(_) => {
                    if body.taint.is_none() {
                        body.taint = Some(Reason::VariableHead);
                    }
                }
                other => walk(other, natives, clauses, body, constructors),
            }
            for arg in args {
                walk(arg, natives, clauses, body, constructors);
            }
        }
        Atom::Variable(_) | Atom::Grounded(_) => {}
    }
}

fn callee(body: &mut Body, name: &str) {
    if !body.callees.iter().any(|c| c == name) {
        body.callees.push(name.to_string());
    }
}
