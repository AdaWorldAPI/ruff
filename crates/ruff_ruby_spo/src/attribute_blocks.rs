//! Validator calls inside `attribute :x do … end` blocks of a contract.
//!
//! `OpenProject`'s `BaseContract#attribute` registers a trailing block as a
//! validation (`app/contracts/base_contract.rb:83-90`):
//!
//! ```ruby
//! def attribute(attribute, options = {}, &block)
//!   property attribute, options.slice(:readable)
//!   add_writable(attribute, options[:writable])
//!   attribute_permission(attribute, options[:permission])
//!   validate(attribute, &block) if block
//! end
//! ```
//!
//! so the block runs on every `valid?`, and what it calls under which
//! condition is part of the contract:
//!
//! ```ruby
//! attribute :schedule_manually do
//!   validate_has_predecessors_or_children if model.schedule_automatically?
//! end
//! ```
//!
//! [`crate::extract_class_functions`] already reports such a block as a
//! [`crate::FunctionKind::Block`] whose `self_calls` list
//! `validate_has_predecessors_or_children`, but it flattens the body: the
//! `if` modifier is read for facts and then forgotten. This arm keeps the
//! pairing of each call with the condition it runs under.
//!
//! # What is read
//!
//! A block attached to a receiver-less `attribute` call that is a direct
//! statement of a class or module body. Each statement of the block body is
//! read as follows:
//!
//! - A receiver-less call, or a call on `self`, whose name is a method
//!   identifier (`foo`, `foo?`, `foo!`) is an [`AttributeBlockCall`]. Setters
//!   and operators are not.
//! - `stmt if cond`, `stmt unless cond`, and the block forms `if` / `unless`
//!   (with `else`) apply to the statements beneath them. Nested conditions
//!   combine with `And`; `unless` and an `else` branch negate.
//! - Anything else (a loop, a `case`, `errors.add …`, a call on another
//!   receiver) is not read. It is recorded in [`AttributeBlock::unread`] with
//!   its line and source, never dropped silently.
//!
//! # Conditions
//!
//! A condition lowers to the [`GuardExpr`] the `writable:` arm uses: `&&` /
//! `and`, `||` / `or`, `!` / `not`, parentheses, `true` / `false`, and a call
//! chain without arguments rooted at `model` or `user`
//! (`model.schedule_automatically?`) as a [`GuardExpr::Fact`] in the same
//! canonical text. A receiver-less call (`leaf?`) is not inlined here — the
//! method lives in a class this arm does not resolve — so it is
//! [`GuardOutcome::Unlowered`] with that reason, as is every other shape. The
//! condition's source text is always kept.
//!
//! # Conservation
//!
//! In every [`AttributeBlockReport`]: `blocks_found == blocks.len()`,
//! `calls_found == calls_unconditional + calls_conditional`, and
//! `calls_conditional == conditions_lowered + conditions_unlowered`.
//! `attribute_calls` counts every receiver-less `attribute` call that is a
//! direct statement of a class or module body, with or without a block, so
//! `blocks_found <= attribute_calls`. A file that cannot be read or parsed
//! counts in no figure. No SPO triples are emitted.

use std::fs;
use std::path::Path;

use lib_ruby_parser::{Node, Parser, ParserOptions};

use crate::class_macros::walk_class_bodies;
use crate::guards::{GuardExpr, GuardOutcome, flat_source};
use crate::seeds::{Lowerer, SeedOptions};

/// The condition a call runs under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockCondition {
    /// The condition as written, whitespace-flattened. `unless c` is
    /// `!(c)`; nested conditions read `(outer) && (inner)`.
    pub source_text: String,
    /// The lowered expression, or why it could not be lowered.
    pub outcome: GuardOutcome,
}

/// One validator call in an `attribute … do` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributeBlockCall {
    /// The method called, as written (`validate_has_predecessors_or_children`).
    pub name: String,
    /// Each argument as written, whitespace-flattened.
    pub args: Vec<String>,
    /// 1-based line of the call.
    pub line: u32,
    /// `None` when the call runs on every `valid?`.
    pub condition: Option<BlockCondition>,
}

/// A block statement this arm does not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadStatement {
    pub line: u32,
    pub reason: String,
    pub source_text: String,
}

/// The block of one `attribute :name do … end`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributeBlock {
    /// Full constant path of the class or module.
    pub class: String,
    /// The first symbol or string argument of `attribute`, `_` when none.
    pub attribute: String,
    /// Path as given to [`extract_attribute_blocks`].
    pub file: String,
    /// 1-based line of the `attribute` call.
    pub line: u32,
    /// Validator calls in source order.
    pub calls: Vec<AttributeBlockCall>,
    /// Statements that were not read, in source order.
    pub unread: Vec<UnreadStatement>,
}

/// Counts for one [`extract_attribute_blocks`] run. See the module doc for the
/// conservation law.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AttributeBlockReport {
    pub files_scanned: usize,
    /// Files skipped because the parser reported an error (a recovered AST
    /// may be partial) or produced no AST.
    pub files_unparsed: usize,
    pub classes_seen: usize,
    pub attribute_calls: usize,
    pub blocks_found: usize,
    pub calls_found: usize,
    pub calls_unconditional: usize,
    pub calls_conditional: usize,
    pub conditions_lowered: usize,
    pub conditions_unlowered: usize,
    pub unread_statements: usize,
}

/// Read every file in `files` (paths relative to `root`) and return one
/// [`AttributeBlock`] per `attribute … do` block in a class or module body.
#[must_use]
pub fn extract_attribute_blocks(
    root: &Path,
    files: &[&str],
) -> (Vec<AttributeBlock>, AttributeBlockReport) {
    let mut blocks = Vec::new();
    let mut report = AttributeBlockReport::default();
    let seed_options = SeedOptions::default();
    for rel in files {
        let Ok(src) = fs::read_to_string(root.join(rel)) else {
            continue;
        };
        let parser_options = ParserOptions {
            buffer_name: (*rel).to_string(),
            ..Default::default()
        };
        let result = Parser::new(src.as_bytes().to_vec(), parser_options).do_parse();
        // A recovered AST after a syntax error can omit whole class bodies, so
        // harvesting it would report incomplete facts as complete. Skip the
        // file and count it instead (the same rule as `guards::parse`).
        if result
            .diagnostics
            .iter()
            .any(lib_ruby_parser::Diagnostic::is_error)
        {
            report.files_unparsed += 1;
            continue;
        }
        let Some(ast) = result.ast else {
            report.files_unparsed += 1;
            continue;
        };
        report.files_scanned += 1;
        let lowerer = Lowerer::new(&src, &seed_options);
        report.classes_seen += walk_class_bodies(&ast, &lowerer, |scope, stmt| {
            let (send, block) = match stmt {
                Node::Send(send) => (send, None),
                Node::Block(blk) => match blk.call.as_ref() {
                    Node::Send(send) => (send, Some(blk.body.as_deref())),
                    _ => return,
                },
                _ => return,
            };
            if send.recv.is_some() || send.method_name != "attribute" {
                return;
            }
            report.attribute_calls += 1;
            let Some(body) = block else { return };
            let attribute = send
                .args
                .iter()
                .find_map(symbol_or_string)
                .unwrap_or_else(|| "_".to_string());
            let mut read = Read {
                src: &src,
                lowerer: &lowerer,
                calls: Vec::new(),
                unread: Vec::new(),
            };
            read.statements(body, &mut Vec::new());
            blocks.push(AttributeBlock {
                class: scope.class.to_string(),
                attribute,
                file: (*rel).to_string(),
                line: lowerer.line_of(send.expression_l.begin),
                calls: read.calls,
                unread: read.unread,
            });
        });
    }
    for block in &blocks {
        report.blocks_found += 1;
        report.unread_statements += block.unread.len();
        for call in &block.calls {
            report.calls_found += 1;
            match &call.condition {
                None => report.calls_unconditional += 1,
                Some(cond) => {
                    report.calls_conditional += 1;
                    match cond.outcome {
                        GuardOutcome::Lowered(_) => report.conditions_lowered += 1,
                        GuardOutcome::Unlowered { .. } => report.conditions_unlowered += 1,
                    }
                }
            }
        }
    }
    (blocks, report)
}

fn symbol_or_string(node: &Node) -> Option<String> {
    match node {
        Node::Sym(s) => Some(s.name.to_string_lossy()),
        Node::Str(s) => Some(s.value.to_string_lossy()),
        _ => None,
    }
}

/// A condition on the way down: source text and lowering.
type Cond = (String, Result<GuardExpr, String>);

struct Read<'a> {
    src: &'a str,
    lowerer: &'a Lowerer<'a>,
    calls: Vec<AttributeBlockCall>,
    unread: Vec<UnreadStatement>,
}

impl Read<'_> {
    /// The statements of a block body: one node, or the children of a `begin`.
    fn statements(&mut self, body: Option<&Node>, conds: &mut Vec<Cond>) {
        match body {
            Some(Node::Begin(b)) => {
                for stmt in &b.statements {
                    self.statement(stmt, conds);
                }
            }
            Some(node) => self.statement(node, conds),
            None => {}
        }
    }

    fn statement(&mut self, stmt: &Node, conds: &mut Vec<Cond>) {
        match stmt {
            Node::If(i) => self.branch(&i.cond, i.if_true.as_deref(), i.if_false.as_deref(), conds),
            Node::IfMod(i) => {
                self.branch(&i.cond, i.if_true.as_deref(), i.if_false.as_deref(), conds);
            }
            Node::Send(s)
                if matches!(s.recv.as_deref(), None | Some(Node::Self_(_)))
                    && is_method_ident(&s.method_name) =>
            {
                self.calls.push(AttributeBlockCall {
                    name: s.method_name.clone(),
                    args: s.args.iter().map(|a| flat_source(self.src, a)).collect(),
                    line: self.lowerer.line_of(s.expression_l.begin),
                    condition: combine(conds),
                });
            }
            other => self.unread.push(UnreadStatement {
                line: self.lowerer.line_of(other.expression().begin),
                reason: format!("statement kind `{}` is not read", other.str_type()),
                source_text: flat_source(self.src, other),
            }),
        }
    }

    /// `if_true` runs under `cond`, `if_false` under its negation. The parser
    /// puts an `unless` body in `if_false` with no `if_true`.
    fn branch(
        &mut self,
        cond: &Node,
        if_true: Option<&Node>,
        if_false: Option<&Node>,
        conds: &mut Vec<Cond>,
    ) {
        let text = flat_source(self.src, cond);
        let lowered = self.lower(cond);
        for (body, negate) in [(if_true, false), (if_false, true)] {
            if body.is_none() {
                continue;
            }
            let entry = if negate {
                (
                    format!("!({text})"),
                    lowered.clone().map(|e| GuardExpr::Not(Box::new(e))),
                )
            } else {
                (text.clone(), lowered.clone())
            };
            conds.push(entry);
            self.statements(body, conds);
            conds.pop();
        }
    }

    fn lower(&self, node: &Node) -> Result<GuardExpr, String> {
        match node {
            Node::True(_) => Ok(GuardExpr::Const(true)),
            Node::False(_) => Ok(GuardExpr::Const(false)),
            Node::Begin(b) if b.statements.len() == 1 => self.lower(&b.statements[0]),
            Node::And(a) => Ok(GuardExpr::And(
                Box::new(self.lower(&a.lhs)?),
                Box::new(self.lower(&a.rhs)?),
            )),
            Node::Or(o) => Ok(GuardExpr::Or(
                Box::new(self.lower(&o.lhs)?),
                Box::new(self.lower(&o.rhs)?),
            )),
            Node::Send(s) if s.method_name == "!" && s.args.is_empty() => match s.recv.as_deref() {
                Some(recv) => Ok(GuardExpr::Not(Box::new(self.lower(recv)?))),
                None => Err("`!` without an operand".to_string()),
            },
            Node::Send(_) | Node::CSend(_) => self.chain(node).map(GuardExpr::Fact),
            other => Err(format!(
                "condition kind `{}` is not lowered: {}",
                other.str_type(),
                flat_source(self.src, other)
            )),
        }
    }

    /// Canonical text of an argument-less call chain rooted at `model` or
    /// `user`, each step keeping its own `.` or `&.`.
    fn chain(&self, node: &Node) -> Result<String, String> {
        let (recv, name, args, dot) = match node {
            Node::Send(s) => (s.recv.as_deref(), s.method_name.as_str(), &s.args, "."),
            Node::CSend(c) => (Some(&*c.recv), c.method_name.as_str(), &c.args, "&."),
            other => {
                return Err(format!(
                    "call chain rooted in `{}` is not lowered: {}",
                    other.str_type(),
                    flat_source(self.src, other)
                ));
            }
        };
        let unsupported = |why: &str| Err(format!("{why}: {}", flat_source(self.src, node)));
        if !args.is_empty() {
            return unsupported("call with arguments is not lowered");
        }
        match recv {
            None if matches!(name, "model" | "user") => Ok(name.to_string()),
            None => {
                unsupported("receiver-less call is not inlined (its `def` is not resolved here)")
            }
            Some(r @ (Node::Send(_) | Node::CSend(_))) => {
                Ok(format!("{}{dot}{name}", self.chain(r)?))
            }
            Some(_) => unsupported("call chain is not rooted at `model` or `user`"),
        }
    }
}

/// The conjunction of the conditions on the way down, `None` when empty.
fn combine(conds: &[Cond]) -> Option<BlockCondition> {
    let (first, rest) = conds.split_first()?;
    let mut text = first.0.clone();
    let mut expr = first.1.clone();
    for (t, e) in rest {
        text = format!("({text}) && ({t})");
        expr = match (expr, e) {
            (Ok(a), Ok(b)) => Ok(GuardExpr::And(Box::new(a), Box::new(b.clone()))),
            (Err(reason), _) => Err(reason),
            (_, Err(reason)) => Err(reason.clone()),
        };
    }
    Some(BlockCondition {
        source_text: text,
        outcome: match expr {
            Ok(e) => GuardOutcome::Lowered(e),
            Err(reason) => GuardOutcome::Unlowered { reason },
        },
    })
}

/// A method name as Ruby spells a call: an identifier, optionally ending in
/// `?` or `!`. Setters and operators are not.
fn is_method_ident(name: &str) -> bool {
    let core = name.strip_suffix(['?', '!']).unwrap_or(name);
    let mut chars = core.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use GuardExpr::{And, Fact, Not};

    const CREATE_CONTRACT: &str = "module WorkPackages
  class CreateContract < BaseContract
    attribute :author_id,
              writable: -> { default_attributes_admin_writable? }
    attribute :schedule_manually do
      validate_has_predecessors_or_children if model.schedule_automatically?
    end
  end
end
";

    fn scan(case: &str, body: &str) -> (Vec<AttributeBlock>, AttributeBlockReport) {
        let root = std::env::temp_dir().join(format!(
            "ruff_ruby_spo_attribute_blocks_{}_{case}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.rb"), body).unwrap();
        let result = extract_attribute_blocks(&root, &["a.rb"]);
        let _ = fs::remove_dir_all(&root);
        result
    }

    fn lowered(call: &AttributeBlockCall) -> &GuardExpr {
        match &call.condition.as_ref().expect("conditional").outcome {
            GuardOutcome::Lowered(e) => e,
            GuardOutcome::Unlowered { reason } => panic!("unlowered: {reason}"),
        }
    }

    #[test]
    fn create_contract_schedule_manually_block() {
        let (blocks, report) = scan("create", CREATE_CONTRACT);
        assert_eq!(blocks.len(), 1, "{blocks:?}");
        let block = &blocks[0];
        assert_eq!(block.class, "WorkPackages::CreateContract");
        assert_eq!(block.attribute, "schedule_manually");
        assert_eq!(block.line, 5);
        assert!(block.unread.is_empty());
        assert_eq!(block.calls.len(), 1);
        let call = &block.calls[0];
        assert_eq!(call.name, "validate_has_predecessors_or_children");
        assert!(call.args.is_empty());
        assert_eq!(call.line, 6);
        let cond = call.condition.as_ref().expect("guarded by the modifier");
        assert_eq!(cond.source_text, "model.schedule_automatically?");
        assert_eq!(
            cond.outcome,
            GuardOutcome::Lowered(Fact("model.schedule_automatically?".to_string()))
        );
        assert_eq!(
            report,
            AttributeBlockReport {
                files_scanned: 1,
                files_unparsed: 0,
                classes_seen: 2,
                attribute_calls: 2,
                blocks_found: 1,
                calls_found: 1,
                calls_unconditional: 0,
                calls_conditional: 1,
                conditions_lowered: 1,
                conditions_unlowered: 0,
                unread_statements: 0,
            }
        );
    }

    #[test]
    fn attributes_without_a_block_yield_no_facts() {
        let body = "class C < BaseContract
  attribute :x
  attribute :y, writable: false
  attribute :z, writable: -> { model.leaf? }
end
";
        let (blocks, report) = scan("silence", body);
        assert!(blocks.is_empty(), "{blocks:?}");
        assert_eq!(report.attribute_calls, 3);
        assert_eq!(report.blocks_found, 0);
        assert_eq!(report.calls_found, 0);
    }

    #[test]
    fn unless_modifier_negates_and_a_block_may_call_twice() {
        let body = "class C < BaseContract
  attribute :due_date do
    validate_not_in_past unless model.milestone?
    validate_after_start(:start_date)
  end
end
";
        let (blocks, report) = scan("unless", body);
        let calls = &blocks[0].calls;
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "validate_not_in_past");
        assert_eq!(
            lowered(&calls[0]),
            &Not(Box::new(Fact("model.milestone?".to_string())))
        );
        assert_eq!(
            calls[0].condition.as_ref().unwrap().source_text,
            "!(model.milestone?)"
        );
        assert_eq!(calls[1].name, "validate_after_start");
        assert_eq!(calls[1].args, [":start_date"]);
        assert!(calls[1].condition.is_none());
        assert_eq!(report.calls_found, 2);
        assert_eq!(report.calls_unconditional, 1);
        assert_eq!(report.calls_conditional, 1);
    }

    #[test]
    fn block_if_else_and_nesting_combine_conditions() {
        let body = "class C < BaseContract
  attribute :a do
    if model.new_record?
      validate_new
      validate_both if model.leaf?
    else
      validate_old
    end
  end
end
";
        let (blocks, _) = scan("nested", body);
        let calls = &blocks[0].calls;
        let names: Vec<&str> = calls.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["validate_new", "validate_both", "validate_old"]);
        let new_record = || Fact("model.new_record?".to_string());
        assert_eq!(lowered(&calls[0]), &new_record());
        assert_eq!(
            lowered(&calls[1]),
            &And(
                Box::new(new_record()),
                Box::new(Fact("model.leaf?".to_string()))
            )
        );
        assert_eq!(
            calls[1].condition.as_ref().unwrap().source_text,
            "(model.new_record?) && (model.leaf?)"
        );
        assert_eq!(lowered(&calls[2]), &Not(Box::new(new_record())));
    }

    #[test]
    fn unresolvable_conditions_and_statements_are_reported_not_dropped() {
        let body = "class C < BaseContract
  attribute :a do
    validate_x if leaf_or_manually_scheduled?
    model.children.each { |c| validate_child(c) }
    errors.add(:a, :invalid)
  end
end
";
        let (blocks, report) = scan("unread", body);
        let block = &blocks[0];
        assert_eq!(block.calls.len(), 1);
        let cond = block.calls[0].condition.as_ref().unwrap();
        assert_eq!(cond.source_text, "leaf_or_manually_scheduled?");
        match &cond.outcome {
            GuardOutcome::Unlowered { reason } => {
                assert!(reason.contains("not inlined"), "{reason}");
            }
            GuardOutcome::Lowered(e) => panic!("lowered: {e:?}"),
        }
        assert_eq!(block.unread.len(), 2, "{:?}", block.unread);
        assert_eq!(block.unread[0].line, 4);
        assert_eq!(block.unread[1].line, 5);
        assert_eq!(report.unread_statements, 2);
        assert_eq!(report.conditions_unlowered, 1);
        assert_eq!(report.conditions_lowered, 0);
    }

    #[test]
    fn a_file_with_a_syntax_error_is_skipped_not_harvested() {
        // The block parses on its own; the stray `end` after the class makes
        // the file a syntax error, which the parser recovers from.
        let body =
            "class C < BaseContract\n  attribute :a do\n    check_a if model.x?\n  end\nend\nend\n";
        let (blocks, report) = scan("syntax_error", body);
        assert!(blocks.is_empty(), "{blocks:?}");
        assert_eq!(report.files_unparsed, 1);
        assert_eq!(report.files_scanned, 0);
    }
}
