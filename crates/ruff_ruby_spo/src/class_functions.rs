//! The class-function harvest arm: method facts for the code a class body
//! declares, in `def`s and in the lambdas and blocks handed to class macros.
//!
//! Rails apps put behaviour in class-body macros as well as in methods:
//!
//! ```ruby
//! class BaseContract < ModelContract
//!   attribute :start_date, writable: ->(*) { leaf_or_manually_scheduled? } do
//!     validate_after_soonest_start(:start_date)
//!   end
//!
//!   def leaf_or_manually_scheduled?
//!     model.leaf? || model.schedule_manually
//!   end
//! end
//! ```
//!
//! This arm turns each of those bodies into a [`Function`], using the same
//! body walker as [`crate::functions`], so the lambda and the block above
//! are described by the same facts as the `def`.
//!
//! # What is a function
//!
//! - [`FunctionKind::Def`]: every instance `def` that is a direct statement
//!   of the class body, `private def x` and defs after a bare `private`
//!   included. A `def` inside a block (`included do … end`) or a `def self.x`
//!   is not one.
//! - [`FunctionKind::Lambda`]: every `->`, `lambda`, `proc` or `Proc.new`
//!   argument of a class-body call named in [`FunctionOptions::lambda_macros`],
//!   positional or the value of a keyword. Named `<macro>:<subject>:<slot>`:
//!   `subject` is the first symbol or string argument (`_` when there is
//!   none), `slot` the keyword (`writable`) or `arg<i>`, the argument's
//!   0-based position.
//! - [`FunctionKind::Block`]: the block attached to such a call, named
//!   `<macro>:<subject>:block`.
//!
//! Relation traversals are not recorded: the arm does not see the class's
//! associations.
//!
//! # Conservation
//!
//! In every [`ClassFunctionReport`]: `functions_found == defs_found +
//! lambdas_found + blocks_found`, and `functions_found` is the number of
//! [`ClassFunction`]s returned over all classes. `classes_seen` counts every
//! class and module in the scanned files, including those that yield no
//! function (they are not returned). A file that cannot be read or parsed
//! counts in no figure. No SPO triples are emitted.

use std::fs;
use std::path::Path;

use lib_ruby_parser::nodes::Send as SendNode;
use lib_ruby_parser::{Node, Parser, ParserOptions};
use ruff_spo_triplet::Function;

use crate::class_macros::walk_class_bodies;
use crate::functions::{BodyContext, function_from_body};
use crate::seeds::{Lowerer, SeedOptions};

/// What to read out of the class bodies.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FunctionOptions {
    /// Bare local or method names whose sends are recorded as
    /// [`Function::receiver_reads`] (`["model"]` for `model.leaf?`).
    pub receivers: Vec<String>,
    /// Class-body calls whose lambda arguments and block are functions
    /// (`["attribute"]`).
    pub lambda_macros: Vec<String>,
    /// Class-body calls whose block's statements are read as direct statements
    /// of the class (`["included"]`: a concern's `included do … end` defines its
    /// `def`s on every includer).
    pub transparent_blocks: Vec<String>,
}

/// Where a [`ClassFunction`] came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionKind {
    Def,
    /// A lambda or proc argument of the macro call `macro_name`.
    Lambda {
        macro_name: String,
        subject: String,
        slot: String,
    },
    /// The block attached to the macro call `macro_name`.
    Block {
        macro_name: String,
        subject: String,
    },
}

/// One function of a class.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassFunction {
    /// The method name for a [`FunctionKind::Def`], else
    /// `<macro>:<subject>:<slot>` or `<macro>:<subject>:block`.
    pub name: String,
    pub kind: FunctionKind,
    /// 1-based line of the `def`, lambda or block.
    pub line: u32,
    /// The method facts; `function.name == name`.
    pub function: Function,
}

/// The functions of one class or module in one file, in source order. A class
/// reopened in the same file is one entry.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassFunctions {
    /// Full constant path of the class or module.
    pub class: String,
    /// Constant path of `< X` when the class declares a literal one.
    pub superclass: Option<String>,
    /// Ruby's `Module.nesting` at the class, innermost first: the class itself,
    /// then each enclosing module or class. The superclass is evaluated in
    /// `nesting[1..]`; constants in the body resolve in `nesting`.
    pub nesting: Vec<String>,
    pub file: String,
    pub functions: Vec<ClassFunction>,
}

/// Counts for one [`extract_class_functions`] run. See the module doc for the
/// conservation law.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClassFunctionReport {
    pub files_scanned: usize,
    pub classes_seen: usize,
    pub defs_found: usize,
    pub lambdas_found: usize,
    pub blocks_found: usize,
    pub functions_found: usize,
}

/// Read every file in `files` (paths relative to `root`) and return the
/// functions of each class that has any.
#[must_use]
pub fn extract_class_functions(
    root: &Path,
    files: &[&str],
    options: &FunctionOptions,
) -> (Vec<ClassFunctions>, ClassFunctionReport) {
    let mut classes: Vec<ClassFunctions> = Vec::new();
    let mut report = ClassFunctionReport::default();
    let seed_options = SeedOptions::default();
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
        let lowerer = Lowerer::new(&src, &seed_options);
        let ctx = BodyContext {
            known_relations: &[],
            receivers: &options.receivers,
        };
        let mut file_classes: Vec<ClassFunctions> = Vec::new();
        let seen = walk_class_bodies(&ast, &lowerer, |scope, stmt| {
            let mut found = Vec::new();
            collect_statement(stmt, options, &ctx, &lowerer, &mut found);
            if found.is_empty() {
                return;
            }
            let at = match file_classes.iter().position(|c| c.class == scope.class) {
                Some(at) => at,
                None => {
                    file_classes.push(ClassFunctions {
                        class: scope.class.to_string(),
                        superclass: scope.superclass.cloned(),
                        nesting: scope.module_nesting(),
                        file: (*rel).to_string(),
                        functions: Vec::new(),
                    });
                    file_classes.len() - 1
                }
            };
            file_classes[at].functions.extend(found);
        });
        report.classes_seen += seen;
        for function in file_classes.iter().flat_map(|c| &c.functions) {
            match function.kind {
                FunctionKind::Def => report.defs_found += 1,
                FunctionKind::Lambda { .. } => report.lambdas_found += 1,
                FunctionKind::Block { .. } => report.blocks_found += 1,
            }
        }
        classes.extend(file_classes);
    }
    report.functions_found = report.defs_found + report.lambdas_found + report.blocks_found;
    (classes, report)
}

/// The functions one direct statement of a class body declares.
fn collect_statement(
    stmt: &Node,
    options: &FunctionOptions,
    ctx: &BodyContext<'_>,
    lowerer: &Lowerer<'_>,
    out: &mut Vec<ClassFunction>,
) {
    match stmt {
        Node::Def(_) => push_def(stmt, ctx, lowerer, out),
        // `private def x … end` and its siblings.
        Node::Send(send)
            if send.recv.is_none()
                && matches!(
                    send.method_name.as_str(),
                    "private" | "protected" | "public"
                ) =>
        {
            for arg in &send.args {
                if matches!(arg, Node::Def(_)) {
                    push_def(arg, ctx, lowerer, out);
                }
            }
        }
        Node::Send(send) => collect_macro(send, None, options, ctx, lowerer, out),
        Node::Block(blk) => {
            if let Node::Send(send) = blk.call.as_ref() {
                if send.recv.is_none() && options.transparent_blocks.contains(&send.method_name) {
                    for inner in block_statements(blk.body.as_deref()) {
                        collect_statement(inner, options, ctx, lowerer, out);
                    }
                } else {
                    let block = Some((blk.body.as_deref(), blk.expression_l.begin));
                    collect_macro(send, block, options, ctx, lowerer, out);
                }
            }
        }
        Node::Numblock(blk) => {
            if let Node::Send(send) = blk.call.as_ref() {
                let block = Some((Some(blk.body.as_ref()), blk.expression_l.begin));
                collect_macro(send, block, options, ctx, lowerer, out);
            }
        }
        _ => {}
    }
}

/// The statements of a block body: one node, or the children of a `begin`.
fn block_statements(body: Option<&Node>) -> &[Node] {
    match body {
        Some(Node::Begin(b)) => &b.statements,
        Some(node) => std::slice::from_ref(node),
        None => &[],
    }
}

fn push_def(
    def: &Node,
    ctx: &BodyContext<'_>,
    lowerer: &Lowerer<'_>,
    out: &mut Vec<ClassFunction>,
) {
    let Node::Def(d) = def else { return };
    out.push(ClassFunction {
        name: d.name.clone(),
        kind: FunctionKind::Def,
        line: lowerer.line_of(d.expression_l.begin),
        function: function_from_body(&d.name, d.body.as_deref(), ctx),
    });
}

/// A macro call's lambda arguments, then its block, if the call is named in
/// [`FunctionOptions::lambda_macros`]. `block` is the attached block's body
/// and start offset.
fn collect_macro(
    send: &SendNode,
    block: Option<(Option<&Node>, usize)>,
    options: &FunctionOptions,
    ctx: &BodyContext<'_>,
    lowerer: &Lowerer<'_>,
    out: &mut Vec<ClassFunction>,
) {
    if send.recv.is_some() || !options.lambda_macros.contains(&send.method_name) {
        return;
    }
    let macro_name = send.method_name.as_str();
    let subject = send
        .args
        .iter()
        .find_map(symbol_or_string)
        .unwrap_or_else(|| "_".to_string());
    let mut push_lambda = |slot: String, lambda: &Lambda<'_>| {
        let name = format!("{macro_name}:{subject}:{slot}");
        out.push(ClassFunction {
            function: function_from_body(&name, lambda.body, ctx),
            kind: FunctionKind::Lambda {
                macro_name: macro_name.to_string(),
                subject: subject.clone(),
                slot,
            },
            line: lowerer.line_of(lambda.begin),
            name,
        });
    };
    for (i, arg) in send.args.iter().enumerate() {
        if let Some(lambda) = lambda_of(arg) {
            push_lambda(format!("arg{i}"), &lambda);
        } else if let Node::Hash(h) = arg {
            keyword_lambdas(&h.pairs, i, &mut push_lambda);
        } else if let Node::Kwargs(k) = arg {
            keyword_lambdas(&k.pairs, i, &mut push_lambda);
        }
    }
    if let Some((body, begin)) = block {
        let name = format!("{macro_name}:{subject}:block");
        out.push(ClassFunction {
            function: function_from_body(&name, body, ctx),
            kind: FunctionKind::Block {
                macro_name: macro_name.to_string(),
                subject,
            },
            line: lowerer.line_of(begin),
            name,
        });
    }
}

fn keyword_lambdas(pairs: &[Node], arg_index: usize, push: &mut impl FnMut(String, &Lambda<'_>)) {
    for pair in pairs {
        let Node::Pair(pair) = pair else { continue };
        if let Some(lambda) = lambda_of(&pair.value) {
            let slot = symbol_or_string(&pair.key).unwrap_or_else(|| format!("arg{arg_index}"));
            push(slot, &lambda);
        }
    }
}

/// The body and start offset of a lambda or proc literal.
struct Lambda<'n> {
    body: Option<&'n Node>,
    begin: usize,
}

/// `->(…) { … }`, `lambda { … }`, `proc { … }` or `Proc.new { … }`.
fn lambda_of(node: &Node) -> Option<Lambda<'_>> {
    let (call, body, begin) = match node {
        Node::Block(b) => (b.call.as_ref(), b.body.as_deref(), b.expression_l.begin),
        Node::Numblock(b) => (b.call.as_ref(), Some(b.body.as_ref()), b.expression_l.begin),
        _ => return None,
    };
    let is_lambda = match call {
        Node::Lambda(_) => true,
        Node::Send(s) => match s.recv.as_deref() {
            None => matches!(s.method_name.as_str(), "lambda" | "proc"),
            Some(Node::Const(c)) => c.name == "Proc" && s.method_name == "new",
            Some(_) => false,
        },
        _ => false,
    };
    is_lambda.then_some(Lambda { body, begin })
}

fn symbol_or_string(node: &Node) -> Option<String> {
    match node {
        Node::Sym(s) => Some(s.name.to_string_lossy()),
        Node::Str(s) => Some(s.value.to_string_lossy()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTRACT: &str = "module WorkPackages
  class BaseContract < ::ModelContract
    attribute :start_date, writable: ->(*) { leaf_or_manually_scheduled? } do
      validate_after_soonest_start(:start_date)
    end
    attribute :status_id, writable: ->(*) { model.status_id_change || !closed_version_and_status? }

    private

    def leaf_or_manually_scheduled?
      model.leaf? || model.schedule_manually
    end
  end
end
";

    fn scan(
        case: &str,
        body: &str,
        receivers: &[&str],
    ) -> (Vec<ClassFunctions>, ClassFunctionReport) {
        let root = std::env::temp_dir().join(format!(
            "ruff_ruby_spo_class_functions_{}_{case}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.rb"), body).unwrap();
        let options = FunctionOptions {
            receivers: receivers.iter().map(|r| (*r).to_string()).collect(),
            lambda_macros: vec!["attribute".to_string()],
            transparent_blocks: Vec::new(),
        };
        let result = extract_class_functions(&root, &["a.rb"], &options);
        let _ = fs::remove_dir_all(&root);
        result
    }

    fn names(functions: &[String]) -> Vec<&str> {
        functions.iter().map(String::as_str).collect()
    }

    fn only_class(classes: &[ClassFunctions]) -> &ClassFunctions {
        assert_eq!(classes.len(), 1, "{classes:?}");
        &classes[0]
    }

    fn by_name<'c>(class: &'c ClassFunctions, name: &str) -> &'c ClassFunction {
        class
            .functions
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("no function `{name}` in {:?}", class.functions))
    }

    #[test]
    fn contract_lambdas_blocks_and_defs() {
        let (classes, report) = scan("contract", CONTRACT, &["model"]);
        let class = only_class(&classes);
        assert_eq!(class.class, "WorkPackages::BaseContract");
        assert_eq!(class.superclass.as_deref(), Some("ModelContract"));
        assert_eq!(
            report,
            ClassFunctionReport {
                files_scanned: 1,
                classes_seen: 2,
                defs_found: 1,
                lambdas_found: 2,
                blocks_found: 1,
                functions_found: 4,
            }
        );
        let listed: Vec<&str> = class.functions.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            listed,
            [
                "attribute:start_date:writable",
                "attribute:start_date:block",
                "attribute:status_id:writable",
                "leaf_or_manually_scheduled?",
            ]
        );

        let lambda = by_name(class, "attribute:start_date:writable");
        assert_eq!(
            lambda.kind,
            FunctionKind::Lambda {
                macro_name: "attribute".to_string(),
                subject: "start_date".to_string(),
                slot: "writable".to_string(),
            }
        );
        assert_eq!(lambda.line, 3);
        assert_eq!(
            names(&lambda.function.self_calls),
            ["leaf_or_manually_scheduled?"]
        );
        assert!(lambda.function.receiver_reads.is_empty());

        let block = by_name(class, "attribute:start_date:block");
        assert_eq!(
            block.kind,
            FunctionKind::Block {
                macro_name: "attribute".to_string(),
                subject: "start_date".to_string(),
            }
        );
        assert_eq!(
            names(&block.function.self_calls),
            ["validate_after_soonest_start"]
        );

        let status = by_name(class, "attribute:status_id:writable");
        assert_eq!(names(&status.function.receiver_reads), ["status_id_change"]);
        assert_eq!(
            names(&status.function.self_calls),
            ["closed_version_and_status?"]
        );

        let def = by_name(class, "leaf_or_manually_scheduled?");
        assert_eq!(def.kind, FunctionKind::Def);
        assert_eq!(def.line, 10);
        assert_eq!(
            names(&def.function.receiver_reads),
            ["leaf?", "schedule_manually"]
        );
        assert!(def.function.self_calls.is_empty());
    }

    /// The block of `attribute :schedule_manually do … end` is already a
    /// class function with its validator call. Its `if` modifier is read for
    /// facts and flattened: the call and the condition's `receiver_reads` sit
    /// side by side with no pairing. [`crate::extract_attribute_blocks`] keeps
    /// the pairing.
    #[test]
    fn attribute_block_modifier_is_flattened() {
        let body = "class CreateContract < BaseContract
  attribute :schedule_manually do
    validate_has_predecessors_or_children if model.schedule_automatically?
  end
end
";
        let (classes, report) = scan("modifier", body, &["model"]);
        let class = only_class(&classes);
        let block = by_name(class, "attribute:schedule_manually:block");
        assert_eq!(report.blocks_found, 1);
        assert_eq!(
            names(&block.function.self_calls),
            ["validate_has_predecessors_or_children"]
        );
        assert_eq!(
            names(&block.function.receiver_reads),
            ["schedule_automatically?"]
        );
    }

    /// Without `model` as a receiver, `model.leaf?` is a call on whatever the
    /// bare call `model` returns: `model` is a `self_calls` entry and `leaf?`,
    /// a send to a non-self receiver, is not recorded at all.
    #[test]
    fn receivers_option_binds() {
        let (classes, _) = scan("receivers", CONTRACT, &[]);
        let class = only_class(&classes);
        let def = by_name(class, "leaf_or_manually_scheduled?");
        assert!(def.function.receiver_reads.is_empty());
        assert_eq!(names(&def.function.self_calls), ["model"]);
        let status = by_name(class, "attribute:status_id:writable");
        assert!(status.function.receiver_reads.is_empty());
        assert_eq!(
            names(&status.function.self_calls),
            ["model", "closed_version_and_status?"]
        );
    }

    #[test]
    fn assignment_to_a_receiver_is_a_write() {
        let body =
            "class C\n  def rename\n    model.subject = 'x'\n    model.subject\n  end\nend\n";
        let (classes, _) = scan("write", body, &["model"]);
        let f = &by_name(only_class(&classes), "rename").function;
        assert_eq!(names(&f.writes), ["subject"]);
        assert_eq!(names(&f.receiver_reads), ["subject"]);
        let (classes, _) = scan("write_unlisted", body, &[]);
        let f = &by_name(only_class(&classes), "rename").function;
        assert!(f.writes.is_empty());
    }

    #[test]
    fn a_def_inside_a_block_is_not_a_class_function() {
        let body = "class C\n  included do\n    def inner\n      helper\n    end\n  end\n\n  def outer\n    helper\n  end\nend\n";
        let (classes, report) = scan("inner", body, &[]);
        let class = only_class(&classes);
        let listed: Vec<&str> = class.functions.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(listed, ["outer"]);
        assert_eq!(report.defs_found, 1);
        assert_eq!(report.functions_found, 1);
    }

    #[test]
    fn defs_in_a_transparent_block_are_class_functions() {
        let body = "module M\n  included do\n    def inner\n      helper\n    end\n\n    private def hidden\n    end\n  end\n\n  def outer\n  end\nend\n";
        let root = std::env::temp_dir().join(format!(
            "ruff_ruby_spo_class_functions_{}_transparent",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.rb"), body).unwrap();
        let options = FunctionOptions {
            transparent_blocks: vec!["included".to_string()],
            ..FunctionOptions::default()
        };
        let (classes, report) = extract_class_functions(&root, &["a.rb"], &options);
        let _ = fs::remove_dir_all(&root);
        let class = only_class(&classes);
        let listed: Vec<&str> = class.functions.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(listed, ["inner", "hidden", "outer"]);
        assert_eq!(class.functions[0].function.self_calls, ["helper"]);
        assert_eq!(report.defs_found, 3);
    }

    fn ruby_files_under(dir: &Path, root: &Path, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                ruby_files_under(&path, root, out);
            } else if path.extension().is_some_and(|e| e == "rb")
                && let Ok(rel) = path.strip_prefix(root)
            {
                out.push(rel.to_string_lossy().into_owned());
            }
        }
    }

    /// Prints the contract functions of the checkout named by
    /// `OPENPROJECT_SRC`.
    #[test]
    #[ignore = "needs an OpenProject checkout in OPENPROJECT_SRC"]
    #[allow(clippy::print_stderr)]
    fn openproject_contract_functions_report() {
        let Ok(root) = std::env::var("OPENPROJECT_SRC") else {
            return;
        };
        let root = Path::new(&root);
        let mut files = Vec::new();
        ruby_files_under(&root.join("app/contracts"), root, &mut files);
        if let Ok(modules) = fs::read_dir(root.join("modules")) {
            for module in modules.flatten() {
                ruby_files_under(&module.path().join("app/contracts"), root, &mut files);
            }
        }
        files.sort();
        let refs: Vec<&str> = files.iter().map(String::as_str).collect();
        let options = FunctionOptions {
            receivers: vec!["model".to_string()],
            lambda_macros: vec!["attribute".to_string()],
            transparent_blocks: Vec::new(),
        };
        let (classes, report) = extract_class_functions(root, &refs, &options);
        eprintln!("files listed={} {report:#?}", refs.len());
        let lambdas: Vec<&ClassFunction> = classes
            .iter()
            .flat_map(|c| &c.functions)
            .filter(|f| matches!(f.kind, FunctionKind::Lambda { .. }))
            .collect();
        let with_facts = lambdas
            .iter()
            .filter(|f| !f.function.self_calls.is_empty() || !f.function.receiver_reads.is_empty())
            .count();
        eprintln!(
            "lambda functions={} with self_calls or receiver_reads={with_facts}",
            lambdas.len()
        );
        for class in classes
            .iter()
            .filter(|c| c.class == "WorkPackages::BaseContract")
        {
            for f in class
                .functions
                .iter()
                .filter(|f| matches!(f.kind, FunctionKind::Lambda { .. }))
            {
                eprintln!(
                    "{} (line {}) self_calls={:?} receiver_reads={:?}",
                    f.name, f.line, f.function.self_calls, f.function.receiver_reads
                );
            }
        }
    }

    #[test]
    fn private_def_and_positional_lambdas() {
        let body = "class C
  private def hidden
    visible?
  end

  attribute :a, ->(x) { x }, lambda { check }
  attribute 'b', writable: proc { ready? }, other: 1
  attribute writable: Proc.new { fine? }
  other_macro :z, ->(*) { ignored }
end
";
        let (classes, report) = scan("positional", body, &[]);
        let class = only_class(&classes);
        let listed: Vec<&str> = class.functions.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            listed,
            [
                "hidden",
                "attribute:a:arg1",
                "attribute:a:arg2",
                "attribute:b:writable",
                "attribute:_:writable",
            ]
        );
        assert_eq!(report.lambdas_found, 4);
        assert_eq!(report.defs_found, 1);
        assert_eq!(report.blocks_found, 0);
        assert_eq!(
            report.functions_found,
            report.defs_found + report.lambdas_found + report.blocks_found
        );
        assert_eq!(
            names(&by_name(class, "attribute:b:writable").function.self_calls),
            ["ready?"]
        );
    }

    #[test]
    fn nesting_tells_a_nested_class_from_the_compact_form() {
        let nested = "module Queries\n  class UpdateContract < BaseContract\n    def run\n      go\n    end\n  end\nend\n";
        let (classes, _) = scan("nest1", nested, &[]);
        let class = only_class(&classes);
        assert_eq!(class.superclass.as_deref(), Some("BaseContract"));
        assert_eq!(class.nesting, ["Queries::UpdateContract", "Queries"]);

        let compact =
            "class Queries::UpdateContract < BaseContract\n  def run\n    go\n  end\nend\n";
        let (classes, _) = scan("nest2", compact, &[]);
        assert_eq!(only_class(&classes).nesting, ["Queries::UpdateContract"]);
    }

    #[test]
    fn nesting_lists_every_open_scope_and_stops_at_an_absolute_name() {
        let deep = "module A\n  module B\n    class C\n      def run\n        go\n      end\n    end\n  end\nend\n";
        let (classes, _) = scan("nest3", deep, &[]);
        assert_eq!(only_class(&classes).nesting, ["A::B::C", "A::B", "A"]);

        let absolute = "module A\n  class ::D\n    def run\n      go\n    end\n  end\nend\n";
        let (classes, _) = scan("nest4", absolute, &[]);
        let class = only_class(&classes);
        assert_eq!(class.class, "D");
        assert_eq!(class.nesting, ["D", "A"]);
    }
}
