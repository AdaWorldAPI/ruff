//! The class-constant harvest arm: `NAME = <expr>` in a class body as data.
//!
//! Rails models keep lookup tables as literal constants, such as
//! `CATEGORIES = { open: %w[new open] }.with_indifferent_access.freeze`. This
//! arm reads each such assignment as a [`ClassConstantRow`].
//!
//! # AST walk
//!
//! The class walk is the one of the class-macro arm: every `class` and
//! `module`, nested ones included. A row is a direct statement of that body of
//! the form `NAME = <expr>` with an unscoped name. `A::B = x`, `NAME ||= x`,
//! assignments inside a `def` or a block, and a nested class's constants
//! belong to no row of the enclosing class.
//!
//! # Lowering
//!
//! Method calls without arguments named in
//! [`SeedOptions::transparent_methods`] are peeled off the end of the
//! expression (`.freeze`, `.with_indifferent_access`), then the rest lowers
//! exactly as in the seed arm (shared `Lowerer`). Without the configuration
//! such a call keeps the whole value [`SeedValue::Unresolved`]. Locals are never
//! followed here, whatever [`SeedOptions::follow_locals`] says: it applies to
//! seed-row extraction only.
//!
//! # Conservation
//!
//! In every [`ClassConstantReport`]:
//! `constants_found == constants_fully_literal + constants_with_unresolved`
//! and `constants_found == rows.len()`. `unresolved_values` counts every
//! [`SeedValue::Unresolved`] and unresolved hash key in all rows.
//! `classes_seen` counts every class and module in the scanned files. A file
//! that cannot be read or parsed counts in no figure.
//!
//! No SPO triples are emitted.

use std::fs;
use std::path::Path;

use lib_ruby_parser::{Node, Parser, ParserOptions};

use crate::class_macros::walk_class_bodies;
use crate::seeds::{Lowerer, SeedOptions, SeedValue, count_unresolved};

/// One constant assignment in a class body, in source order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassConstantRow {
    /// Full constant path of the innermost enclosing class or module.
    pub class: String,
    pub name: String,
    pub file: String,
    /// 1-based line of the assignment.
    pub line: u32,
    pub value: SeedValue,
}

/// Counts for one [`extract_class_constants`] run. See the module doc for the
/// conservation law.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClassConstantReport {
    pub files_scanned: usize,
    pub classes_seen: usize,
    pub constants_found: usize,
    pub constants_fully_literal: usize,
    pub constants_with_unresolved: usize,
    pub unresolved_values: usize,
}

/// Read every file in `files` (paths relative to `root`) and return one
/// [`ClassConstantRow`] per class-body constant assignment.
#[must_use]
pub fn extract_class_constants(
    root: &Path,
    files: &[&str],
    options: &SeedOptions,
) -> (Vec<ClassConstantRow>, ClassConstantReport) {
    let mut rows = Vec::new();
    let mut report = ClassConstantReport::default();
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
        let lowerer = Lowerer::new(&src, options);
        let mut found = Vec::new();
        report.classes_seen += walk_class_bodies(&ast, &lowerer, |scope, stmt| {
            if let Node::Casgn(assign) = stmt
                && assign.scope.is_none()
                && let Some(value) = assign.value.as_deref()
            {
                found.push(ClassConstantRow {
                    class: scope.class.to_string(),
                    name: assign.name.clone(),
                    file: (*rel).to_string(),
                    line: lowerer.line_of(assign.expression_l.begin),
                    value: lowerer.lower(peel_transparent(value, &options.transparent_methods)),
                });
            }
        });
        for row in found {
            let unresolved = count_unresolved(&row.value);
            report.constants_found += 1;
            if unresolved == 0 {
                report.constants_fully_literal += 1;
            } else {
                report.constants_with_unresolved += 1;
            }
            report.unresolved_values += unresolved;
            rows.push(row);
        }
    }
    (rows, report)
}

/// Strip trailing argument-less calls named in `transparent`.
fn peel_transparent<'n>(mut node: &'n Node, transparent: &[String]) -> &'n Node {
    while let Node::Send(call) = node
        && let Some(recv) = call.recv.as_deref()
        && call.args.is_empty()
        && transparent.contains(&call.method_name)
    {
        node = recv;
    }
    node
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seeds::SeedKey;

    const STATE_TYPE: &str = "class Ticket::StateType < ApplicationModel\n  CATEGORIES = {\n    open: ['new', 'open', 'pending reminder', 'pending action'],\n    work_on: %w[new open],\n  }\n    .with_indifferent_access.freeze\n\n  def self.x\n    CATEGORIES.keys\n  end\nend\n";

    fn scan(
        case: &str,
        body: &str,
        options: &SeedOptions,
    ) -> (Vec<ClassConstantRow>, ClassConstantReport) {
        let root = std::env::temp_dir().join(format!(
            "ruff_ruby_spo_class_constants_{}_{case}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.rb"), body).unwrap();
        let result = extract_class_constants(&root, &["a.rb"], options);
        let _ = fs::remove_dir_all(&root);
        result
    }

    fn transparent() -> SeedOptions {
        SeedOptions {
            transparent_methods: vec!["freeze".to_string(), "with_indifferent_access".to_string()],
            ..Default::default()
        }
    }

    fn string(s: &str) -> SeedValue {
        SeedValue::Str(s.to_string())
    }

    #[test]
    fn state_type_categories_is_one_literal_hash() {
        let (rows, report) = scan("t1", STATE_TYPE, &transparent());
        assert_eq!(rows.len(), 1);
        assert_eq!(
            report,
            ClassConstantReport {
                files_scanned: 1,
                classes_seen: 1,
                constants_found: 1,
                constants_fully_literal: 1,
                constants_with_unresolved: 0,
                unresolved_values: 0,
            }
        );
        let row = &rows[0];
        assert_eq!(row.class, "Ticket::StateType");
        assert_eq!(row.name, "CATEGORIES");
        assert_eq!(row.line, 2);
        let SeedValue::Hash(pairs) = &row.value else {
            panic!("expected a hash, got {:?}", row.value);
        };
        assert_eq!(pairs.len(), 2);
        assert_eq!(
            pairs[1],
            (
                SeedKey::Sym("work_on".to_string()),
                SeedValue::Array(vec![string("new"), string("open")])
            )
        );
    }

    #[test]
    fn without_transparent_methods_the_value_is_unresolved() {
        let (rows, report) = scan("t2", STATE_TYPE, &SeedOptions::default());
        assert_eq!(rows.len(), 1);
        assert!(
            matches!(&rows[0].value, SeedValue::Unresolved { source } if source.ends_with(".freeze"))
        );
        assert_eq!(report.constants_with_unresolved, 1);
        assert_eq!(report.unresolved_values, 1);
    }

    #[test]
    fn only_unscoped_direct_class_body_assignments_are_rows() {
        let body = "module A\n  X = 1\n  class B\n    Y = 'y'\n    Z ||= 3\n    W::V = 4\n    [1].each { Q = 5 }\n  end\nend\n";
        let (rows, report) = scan("t3", body, &SeedOptions::default());
        let found: Vec<(&str, &str)> = rows
            .iter()
            .map(|r| (r.class.as_str(), r.name.as_str()))
            .collect();
        assert_eq!(found, vec![("A", "X"), ("A::B", "Y")]);
        assert_eq!(report.classes_seen, 2);
        assert_eq!(
            report.constants_found,
            report.constants_fully_literal + report.constants_with_unresolved
        );
    }

    #[test]
    fn constants_keep_source_order_across_nested_classes() {
        let body = "class A\n  X = 1\n  class B\n    Y = 2\n  end\n  Z = 3\nend\n";
        let (rows, _) = scan("order", body, &SeedOptions::default());
        let found: Vec<(&str, &str)> = rows
            .iter()
            .map(|r| (r.class.as_str(), r.name.as_str()))
            .collect();
        assert_eq!(found, vec![("A", "X"), ("A::B", "Y"), ("A", "Z")]);
    }

    #[test]
    fn an_absolute_class_path_ignores_the_enclosing_modules() {
        let body = "module A\n  class ::B\n    X = 1\n  end\n  module ::C\n    Y = 2\n  end\nend\n";
        let (rows, _) = scan("absolute", body, &SeedOptions::default());
        let classes: Vec<&str> = rows.iter().map(|r| r.class.as_str()).collect();
        assert_eq!(classes, vec!["B", "C"]);
    }

    #[test]
    fn a_peeled_call_with_arguments_stays_unresolved() {
        let (rows, _) = scan("t4", "class A\n  X = [1].freeze(2)\nend\n", &transparent());
        assert!(matches!(rows[0].value, SeedValue::Unresolved { .. }));
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
    /// class_constants::tests::zammad_class_constant_measurement -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs ZAMMAD_SRC"]
    #[expect(
        clippy::print_stdout,
        reason = "measurement output is the purpose of this test"
    )]
    fn zammad_class_constant_measurement() {
        let Ok(src) = std::env::var("ZAMMAD_SRC") else {
            return;
        };
        let root = Path::new(&src);
        let mut files = Vec::new();
        rb_files(&root.join("app/models"), root, &mut files);
        let refs: Vec<&str> = files.iter().map(String::as_str).collect();
        let (rows, report) = extract_class_constants(root, &refs, &transparent());
        println!("app/models: {report:?}");
        assert_eq!(rows.len(), report.constants_found);
        assert_eq!(
            report.constants_found,
            report.constants_fully_literal + report.constants_with_unresolved
        );
        let mut shapes: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for row in rows.iter().filter(|r| count_unresolved(&r.value) > 0) {
            let shape = match &row.value {
                SeedValue::Unresolved { source } => source
                    .chars()
                    .take_while(|c| !matches!(c, '(' | '.' | ' '))
                    .collect(),
                _ => "partly literal".to_string(),
            };
            *shapes.entry(shape).or_default() += 1;
        }
        let mut ranked: Vec<_> = shapes.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        for (shape, count) in ranked.iter().take(8) {
            println!("{count:5}  {shape}");
        }
        let categories = rows
            .iter()
            .find(|r| r.class == "Ticket::StateType" && r.name == "CATEGORIES")
            .expect("Ticket::StateType::CATEGORIES is a row");
        println!(
            "Ticket::StateType::CATEGORIES unresolved values: {}",
            count_unresolved(&categories.value)
        );
        assert_eq!(count_unresolved(&categories.value), 0);
    }
}
