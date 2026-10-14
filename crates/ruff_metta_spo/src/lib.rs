//! `ruff_metta_spo` — the MeTTa frontend for `ruff_spo_triplet`.
//!
//! MeTTa source is parsed with the Hyperon parser from the AdaWorldAPI fork, so this
//! crate never re-implements the grammar. A file becomes a [`Module`]: its `=` rules,
//! `:` type declarations, plain facts and `!` queries.
//!
//! [`classify`] then gives every rule head a [`Verdict`]:
//!
//! - **Lowerable**: every clause of the head only matches patterns, builds data and
//!   calls other lowerable heads. These are the rules a relational lowering
//!   (mask-risc / Quack in lance-graph) can take.
//! - **Interpreter-only**, with the first [`Reason`]: a minimal-MeTTa instruction
//!   (`chain`, `unify`, `eval`, ...), a grounded operation (`+`, `println!`,
//!   `add-atom`, ...), an expression headed by a variable (a call or a tuple, decided
//!   at run time, so counted conservatively), or a call to another
//!   interpreter-only head.
//!
//! The verdict is computed over the whole call graph, to a fixpoint. A rule that
//! names no builtin itself but calls one that does is interpreter-only, and a
//! recursive cycle stays lowerable unless one of its members is tainted.
//!
//! What counts as a call follows Hyperon's evaluation order. The pattern of a
//! `match` is matched, never evaluated, so names inside it are data. Everything
//! else in a rule body may be evaluated, including a `match` template and arguments
//! declared as `Atom` (the callee may return them for evaluation). An expression
//! head with no rule and no builtin is a data constructor; those are collected in
//! [`Classification::constructors`] so an unknown function shows up instead of
//! silently counting as lowerable.
//!
//! Native names are per engine ([`Natives`]). For Hyperon they are the minimal
//! instructions and grounded tokens, and [`hyperon_stdlib`] parses the stdlib that
//! ships with the fork, so `let`, `case`, `collapse` and the rest are classified
//! from their own definitions. For PeTTa they are read from its Prolog builtin
//! registry, where `if`, `let`, `case` and many more are native. The same rule can
//! therefore be lowerable under one engine and interpreter-only under the other.
//!
//! [`to_model_graph`] projects modules into the shared `ModelGraph` (one model per
//! module, one function per rule head). The classification and call edges are not
//! projected: the shared predicate vocabulary has no `calls` predicate that means a
//! MeTTa call, and borrowing one meant for ORM methods would mislabel the edge.

mod builtins;
mod classify;
mod harvest;
mod project;

pub use builtins::{Category, Natives};
pub use classify::{Classification, Reason, Verdict, classify};
pub use harvest::{Module, Rule, TypeDecl, hyperon_stdlib, parse_module};
pub use project::to_model_graph;
