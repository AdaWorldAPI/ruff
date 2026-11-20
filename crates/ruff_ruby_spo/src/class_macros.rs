//! The class-macro harvest arm: receiver-less calls in a class body as data.
//!
//! Rails declares much of its configuration as calls directly in a class
//! body, such as `permit! %i[create], to: 'admin'` in a policy or
//! `prepend_before_action :authentication_check, only: %i[create]` in a
//! controller. This arm reads each such call as a [`ClassMacroRow`].
//!
//! # AST walk
//!
//! Every `class` and `module` is visited, nested ones included. A call is a
//! row when it has no receiver, its method name is one of the requested
//! names, and it is a direct statement of that class or module body. Calls
//! inside a `def`, inside a block, or in a nested class's methods are not
//! rows; a nested class's own direct statements belong to that class.
//! A class's path joins its enclosing modules and classes with the (possibly
//! scoped) name it declares, so `module A; class B::C` is `A::B::C`.
//!
//! # Arguments and blocks
//!
//! Arguments lower exactly as in the seed arm (shared `Lowerer`): a lambda
//! such as `-> { authorize! }` is [`SeedValue::Unresolved`] with its source.
//! A block attached to the call (`do … end` or `{ … }`) is not an argument;
//! its source text, delimiters included, is [`ClassMacroRow::block`].
//!
//! # Conservation
//!
//! In every [`ClassMacroReport`]:
//! `calls_found == calls_fully_literal + calls_with_unresolved` and
//! `calls_found == rows.len()`. `unresolved_values` counts every
//! [`SeedValue::Unresolved`] and unresolved hash key in the arguments of all
//! rows (a block is not counted). `classes_seen` counts every class and
//! module in the scanned files. A file that cannot be read or parsed counts
//! in no figure.
//!
//! No SPO triples are emitted.

use std::fs;
use std::path::Path;

use lib_ruby_parser::nodes::{Block, Class as ClassNode, Module as ModuleNode};
use lib_ruby_parser::traverse::visitor::{Visitor, visit_class, visit_module};
use lib_ruby_parser::{Node, Parser, ParserOptions};

use crate::seeds::{Lowerer, SeedOptions, SeedValue, const_path, count_unresolved};

/// One macro call in a class body, in source order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassMacroRow {
    /// Full constant path of the innermost enclosing class or module.
    pub class: String,
    /// Constant path of `< X` when the class declares a literal one.
    pub superclass: Option<String>,
    pub file: String,
    /// 1-based line of the call.
    pub line: u32,
    pub name: String,
    /// Positional arguments; a trailing keyword-argument list is one `Hash`.
    pub args: Vec<SeedValue>,
    /// Source of an attached block, delimiters included.
    pub block: Option<String>,
}

/// Counts for one [`extract_class_macros`] run. See the module doc for the
/// conservation law.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClassMacroReport {
    pub files_scanned: usize,
    pub classes_seen: usize,
    pub calls_found: usize,
    pub calls_fully_literal: usize,
    pub calls_with_unresolved: usize,
    pub unresolved_values: usize,
}

/// Read every file in `files` (paths relative to `root`) and return one
/// [`ClassMacroRow`] per class-body call named in `names`.
#[must_use]
pub fn extract_class_macros(
    root: &Path,
    files: &[&str],
    names: &[&str],
    options: &SeedOptions,
) -> (Vec<ClassMacroRow>, ClassMacroReport) {
    let mut rows = Vec::new();
    let mut report = ClassMacroReport::default();
    for rel in files {
        let Ok(src) = fs::read_to_string(root.join(rel)) else {
            continue;
        };
        let parser_options = ParserOptions {
            buffer_name: (*rel).to_string(),
            ..Default::default()
        };
        let Some(ast) = Parser::new(src.as_bytes().to_vec(), parser_options)
            .do_parse()
            .ast
        else {
            continue;
        };
        report.files_scanned += 1;
        let mut collector = Collector {
            lowerer: Lowerer { src: &src, options },
            file: rel,
            names,
            path: Vec::new(),
            classes_seen: 0,
            rows: Vec::new(),
        };
        collector.visit(&ast);
        report.classes_seen += collector.classes_seen;
        for row in collector.rows {
            let unresolved = row.args.iter().map(count_unresolved).sum::<usize>();
            report.calls_found += 1;
            if unresolved == 0 {
                report.calls_fully_literal += 1;
            } else {
                report.calls_with_unresolved += 1;
            }
            report.unresolved_values += unresolved;
            rows.push(row);
        }
    }
    (rows, report)
}

struct Collector<'a> {
    lowerer: Lowerer<'a>,
    file: &'a str,
    names: &'a [&'a str],
    /// Full paths of the enclosing classes and modules, outermost first.
    path: Vec<String>,
    classes_seen: usize,
    rows: Vec<ClassMacroRow>,
}

impl Visitor for Collector<'_> {
    fn on_class(&mut self, node: &ClassNode) {
        let superclass = node.superclass.as_deref().and_then(const_path);
        self.enter(&node.name, superclass.as_ref(), node.body.as_deref());
        visit_class(self, node);
        self.path.pop();
    }

    fn on_module(&mut self, node: &ModuleNode) {
        self.enter(&node.name, None, node.body.as_deref());
        visit_module(self, node);
        self.path.pop();
    }
}

impl Collector<'_> {
    /// Push the declared name onto the path and harvest the body's direct
    /// statements; the caller pops after visiting the children.
    fn enter(&mut self, name: &Node, superclass: Option<&String>, body: Option<&Node>) {
        let declared = const_path(name).unwrap_or_else(|| self.lowerer.source_of(name));
        let full = match self.path.last() {
            Some(outer) => format!("{outer}::{declared}"),
            None => declared,
        };
        self.path.push(full);
        self.classes_seen += 1;
        match body {
            Some(Node::Begin(b)) => {
                for stmt in &b.statements {
                    self.statement(stmt, superclass);
                }
            }
            Some(stmt) => self.statement(stmt, superclass),
            None => {}
        }
    }

    fn statement(&mut self, stmt: &Node, superclass: Option<&String>) {
        match stmt {
            Node::Send(_) => self.row(stmt, None, superclass),
            Node::Block(Block {
                call,
                begin_l,
                end_l,
                ..
            }) => {
                let block = self
                    .lowerer
                    .src
                    .as_bytes()
                    .get(begin_l.begin..end_l.end)
                    .map(|b| String::from_utf8_lossy(b).into_owned());
                self.row(call, block, superclass);
            }
            _ => {}
        }
    }

    fn row(&mut self, call: &Node, block: Option<String>, superclass: Option<&String>) {
        let Node::Send(send) = call else { return };
        if send.recv.is_some() || !self.names.contains(&send.method_name.as_str()) {
            return;
        }
        let Some(class) = self.path.last() else {
            return;
        };
        self.rows.push(ClassMacroRow {
            class: class.clone(),
            superclass: superclass.cloned(),
            file: self.file.to_string(),
            line: self.lowerer.line_of(send.expression_l.begin),
            name: send.method_name.clone(),
            args: send.args.iter().map(|a| self.lowerer.lower(a)).collect(),
            block,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seeds::SeedKey;
    use std::path::PathBuf;

    fn scan(case: &str, body: &str, names: &[&str]) -> (Vec<ClassMacroRow>, ClassMacroReport) {
        let root: PathBuf = std::env::temp_dir().join(format!(
            "ruff_ruby_spo_class_macros_{}_{case}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.rb"), body).unwrap();
        let result = extract_class_macros(&root, &["a.rb"], names, &SeedOptions::default());
        let _ = fs::remove_dir_all(&root);
        result
    }

    fn sym(s: &str) -> SeedValue {
        SeedValue::Sym(s.to_string())
    }

    fn string(s: &str) -> SeedValue {
        SeedValue::Str(s.to_string())
    }

    #[test]
    fn policy_permit_rows() {
        let body = "class Controllers::TicketsControllerPolicy < Controllers::ApplicationControllerPolicy\n  permit! %i[import_example import_start], to: 'admin'\n  permit! %i[ticket_create create], to: ['ticket.agent', 'ticket.customer']\nend\n";
        let (rows, report) = scan("t1", body, &["permit!", "default_permit!"]);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            report,
            ClassMacroReport {
                files_scanned: 1,
                classes_seen: 1,
                calls_found: 2,
                calls_fully_literal: 2,
                calls_with_unresolved: 0,
                unresolved_values: 0,
            }
        );
        assert_eq!(rows[1].class, "Controllers::TicketsControllerPolicy");
        assert_eq!(
            rows[1].superclass,
            Some("Controllers::ApplicationControllerPolicy".to_string())
        );
        assert_eq!(rows[1].line, 3);
        assert_eq!(
            rows[1].args,
            vec![
                SeedValue::Array(vec![sym("ticket_create"), sym("create")]),
                SeedValue::Hash(vec![(
                    SeedKey::Sym("to".to_string()),
                    SeedValue::Array(vec![string("ticket.agent"), string("ticket.customer")])
                )]),
            ]
        );
    }

    #[test]
    fn controller_before_action_rows() {
        let body = "class TicketsController < ApplicationController\n  prepend_before_action -> { authorize! }, only: %i[create import_example]\n  prepend_before_action :authentication_check\nend\n";
        let (rows, report) = scan("t2", body, &["prepend_before_action"]);
        assert_eq!(rows.len(), 2);
        assert_eq!(report.calls_with_unresolved, 1);
        assert_eq!(report.calls_fully_literal, 1);
        assert_eq!(report.unresolved_values, 1);
        assert_eq!(
            rows[0].args[0],
            SeedValue::Unresolved {
                source: "-> { authorize! }".to_string()
            }
        );
        assert_eq!(
            rows[0].args[1],
            SeedValue::Hash(vec![(
                SeedKey::Sym("only".to_string()),
                SeedValue::Array(vec![sym("create"), sym("import_example")])
            )])
        );
        assert_eq!(rows[1].args, vec![sym("authentication_check")]);
    }

    #[test]
    fn calls_in_def_or_block_are_not_rows() {
        let body = "class A\n  def x\n    permit! :a\n  end\n  [1].each { permit! :b }\n  permit! :c\nend\n";
        let (rows, report) = scan("t3", body, &["permit!"]);
        assert_eq!(rows.len(), 1);
        assert_eq!(report.calls_found, 1);
        assert_eq!(rows[0].args, vec![sym("c")]);
    }

    #[test]
    fn nested_names_join_into_a_full_path() {
        let body = "module A\n  class B::C\n    permit! :x, to: 'y'\n  end\nend\n";
        let (rows, report) = scan("t4", body, &["permit!"]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].class, "A::B::C");
        assert_eq!(rows[0].superclass, None);
        assert_eq!(report.classes_seen, 2);
    }

    #[test]
    fn an_attached_block_is_not_an_argument() {
        let body = "class A\n  prepend_before_action { authorize! }\nend\n";
        let (rows, report) = scan("t5", body, &["prepend_before_action"]);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].args.is_empty());
        assert_eq!(rows[0].block, Some("{ authorize! }".to_string()));
        assert_eq!(report.unresolved_values, 0);
    }

    #[test]
    fn unlisted_names_and_receivers_are_not_rows() {
        let body = "class A\n  include Foo\n  self.permit! :a\n  permit! :b\nend\n";
        let (rows, _) = scan("t6", body, &["permit!"]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].args, vec![sym("b")]);
        let (none, report) = scan("t6b", "class A\n  include Foo\nend\n", &["permit!"]);
        assert!(none.is_empty());
        assert_eq!(report.calls_found, 0);
    }

    fn rb_files(dir: &Path, root: &Path, out: &mut Vec<String>) {
        let mut entries: Vec<_> = fs::read_dir(dir).unwrap().flatten().collect();
        entries.sort_by_key(fs::DirEntry::path);
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                rb_files(&path, root, out);
            } else if path.extension().is_some_and(|e| e == "rb") {
                out.push(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }

    /// Run with `ZAMMAD_SRC=/path/to/zammad cargo test -p ruff_ruby_spo
    /// class_macros::tests::zammad_class_macro_measurement -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs ZAMMAD_SRC"]
    #[expect(
        clippy::print_stdout,
        reason = "measurement output is the purpose of this test"
    )]
    fn zammad_class_macro_measurement() {
        let Ok(src) = std::env::var("ZAMMAD_SRC") else {
            return;
        };
        let root = Path::new(&src);
        let targets: [(&str, &[&str]); 2] = [
            ("app/policies/controllers", &["permit!", "default_permit!"]),
            (
                "app/controllers",
                &[
                    "before_action",
                    "prepend_before_action",
                    "append_before_action",
                    "skip_before_action",
                ],
            ),
        ];
        for (dir, names) in targets {
            let mut files = Vec::new();
            rb_files(&root.join(dir), root, &mut files);
            let refs: Vec<&str> = files.iter().map(String::as_str).collect();
            let (rows, report) = extract_class_macros(root, &refs, names, &SeedOptions::default());
            let classes: std::collections::BTreeSet<&str> =
                rows.iter().map(|r| r.class.as_str()).collect();
            println!("{dir}: {report:?}");
            println!("{dir}: {} distinct classes with rows", classes.len());
            assert_eq!(rows.len(), report.calls_found);
            assert_eq!(
                report.calls_found,
                report.calls_fully_literal + report.calls_with_unresolved
            );
            if dir == "app/controllers" {
                let mut shapes: std::collections::BTreeMap<String, usize> =
                    std::collections::BTreeMap::new();
                for row in &rows {
                    let shape = match (row.args.first(), &row.block) {
                        (Some(SeedValue::Sym(_)), _) => "symbol".to_string(),
                        (Some(SeedValue::Unresolved { source }), _) => {
                            if source.starts_with("->") || source.starts_with("lambda") {
                                "lambda".to_string()
                            } else {
                                format!("other: {source}")
                            }
                        }
                        (Some(SeedValue::Hash(_)), Some(_)) => "block + options".to_string(),
                        (Some(other), _) => format!("other: {other:?}"),
                        (None, Some(_)) => "block".to_string(),
                        (None, None) => "none".to_string(),
                    };
                    *shapes.entry(shape).or_default() += 1;
                }
                let mut ranked: Vec<_> = shapes.into_iter().collect();
                ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                for (shape, count) in ranked {
                    println!("{count:5}  {shape}");
                }
            }
        }
    }
}
