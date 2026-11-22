//! Contract **writability guards** — `attribute :x, writable: <value>`.
//!
//! `OpenProject` contracts declare whether an attribute may be written as a
//! lambda over the contract's model:
//!
//! ```ruby
//! attribute :start_date,
//!           writable: ->(*) { leaf_or_manually_scheduled? }
//!
//! def leaf_or_manually_scheduled?
//!   model.leaf? || model.schedule_manually?
//! end
//! ```
//!
//! This arm lowers each `writable:` value into a small boolean IR
//! ([`GuardExpr`]) over named facts, or records why it cannot
//! ([`GuardOutcome::Unlowered`]). It is extraction only: nothing is executed
//! and no fact is evaluated.
//!
//! # Lowering
//!
//! - `true` / `false` → [`GuardExpr::Const`]; `||`/`or`, `&&`/`and`, `!`/`not`
//!   → `Or` / `And` / `Not`.
//! - A lambda / `lambda {}` / `proc {}` lowers its body, which must be one
//!   expression.
//! - A call chain rooted at `model` (plain or `&.`, no arguments except one
//!   `&:sym` block-pass) becomes a [`GuardExpr::Fact`] with canonical text
//!   (`model.effective_target_versions.any?(&:closed?)`). A no-argument call
//!   directly on a constant (`WorkPackage.work_based_mode?`) is a fact too.
//! - A receiver-less, argument-less call is inlined from the `def` found in the
//!   guard's own class, else its scanned superclasses (lookup always starts at
//!   the class that declares the attribute, as Ruby's dispatch does). Optional
//!   parameters are bound to their default expression; a required parameter,
//!   recursion, or depth beyond [`MAX_INLINE_DEPTH`] is unlowered.
//! - Everything else is unlowered with a reason naming the node kind and a
//!   source snippet. Nothing is guessed.
//!
//! Only attribute calls that are direct statements of a class body are seen
//! (optionally with a trailing `do … end` block).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use lib_ruby_parser::nodes::{Block, Def as DefNode, Send as SendNode};
use lib_ruby_parser::{Node, Parser, ParserOptions};

/// Deepest chain of inlined `def`s; a fifth nested call is unlowered.
const MAX_INLINE_DEPTH: usize = 4;
/// Longest source snippet quoted in an unlowered reason.
const SNIPPET_MAX_CHARS: usize = 60;

/// A boolean expression over named facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardExpr {
    Const(bool),
    /// A named fact, in canonical text (`model.leaf?`).
    Fact(String),
    Not(Box<GuardExpr>),
    And(Box<GuardExpr>, Box<GuardExpr>),
    Or(Box<GuardExpr>, Box<GuardExpr>),
}

/// The result of lowering one `writable:` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardOutcome {
    Lowered(GuardExpr),
    Unlowered { reason: String },
}

/// One `attribute :name, writable: …` declaration and its lowering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractGuard {
    /// Namespaced name of the enclosing class, e.g. `WorkPackages::BaseContract`.
    pub class: String,
    pub attribute: String,
    /// Path relative to the scan root, `/`-separated.
    pub file: String,
    /// 1-based line of the `attribute` call.
    pub line: usize,
    pub outcome: GuardOutcome,
}

/// Conservation ledger of a guard scan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GuardScanReport {
    /// Every `*.rb` file found under the root.
    pub rb_files: usize,
    /// Receiver-less `attribute :sym, …` calls in class bodies.
    pub attribute_calls: usize,
    /// Of those, the ones carrying a `writable:` keyword.
    pub writable_guards: usize,
    pub lowered: usize,
    pub unlowered: usize,
}

/// Lower every `writable:` guard under `root`. Thin wrapper over
/// [`extract_contract_guards_with_report`].
#[must_use]
pub fn extract_contract_guards(root: &Path) -> Vec<ContractGuard> {
    extract_contract_guards_with_report(root).0
}

/// Like [`extract_contract_guards`] but also returns the [`GuardScanReport`].
/// Guards come in sorted file order, then declaration order.
#[must_use]
pub fn extract_contract_guards_with_report(root: &Path) -> (Vec<ContractGuard>, GuardScanReport) {
    let mut paths = Vec::new();
    collect_rb(root, &mut paths);
    paths.sort();
    let files: Vec<Parsed> = paths.iter().map(|p| parse(root, p)).collect();

    let mut report = GuardScanReport {
        rb_files: files.len(),
        ..GuardScanReport::default()
    };
    let mut scan = Scan::default();
    for (index, file) in files.iter().enumerate() {
        if let Some(ast) = &file.ast {
            scan.walk(index, &file.src, ast, &[]);
        }
    }
    report.attribute_calls = scan.attribute_calls;
    report.writable_guards = scan.pending.len();

    let lowerer = Lowerer {
        files: &files,
        classes: &scan.classes,
    };
    let mut guards = Vec::with_capacity(scan.pending.len());
    for pending in &scan.pending {
        let mut frame = Frame {
            file: pending.file,
            class: pending.class.clone(),
            stack: Vec::new(),
            bindings: HashMap::new(),
        };
        let outcome = match lowerer.lower_writable(pending.value, &mut frame) {
            Ok(expr) => {
                report.lowered += 1;
                GuardOutcome::Lowered(expr)
            }
            Err(reason) => {
                report.unlowered += 1;
                GuardOutcome::Unlowered { reason }
            }
        };
        guards.push(ContractGuard {
            class: pending.class.clone(),
            attribute: pending.attribute.clone(),
            file: files[pending.file].rel.clone(),
            line: pending.line,
            outcome,
        });
    }
    (guards, report)
}

struct Parsed {
    rel: String,
    src: String,
    ast: Option<Node>,
}

fn collect_rb(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rb(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rb") {
            out.push(path);
        }
    }
}

fn parse(root: &Path, path: &Path) -> Parsed {
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    let Ok(src) = fs::read_to_string(path) else {
        return Parsed {
            rel,
            src: String::new(),
            ast: None,
        };
    };
    let options = ParserOptions {
        buffer_name: path.display().to_string(),
        ..Default::default()
    };
    let ast = Parser::new(src.as_bytes().to_vec(), options)
        .do_parse()
        .ast
        .map(|boxed| *boxed);
    Parsed { rel, src, ast }
}

#[derive(Clone, Copy)]
struct DefRef<'a> {
    file: usize,
    def: &'a DefNode,
}

struct ClassEntry<'a> {
    /// Superclass as written, with the module nesting it was written in.
    superclass: Option<(Vec<String>, String)>,
    defs: HashMap<String, DefRef<'a>>,
}

struct Pending<'a> {
    class: String,
    file: usize,
    line: usize,
    attribute: String,
    value: &'a Node,
}

#[derive(Default)]
struct Scan<'a> {
    classes: HashMap<String, ClassEntry<'a>>,
    pending: Vec<Pending<'a>>,
    attribute_calls: usize,
}

impl<'a> Scan<'a> {
    /// Recurse through `module` / `class` / `begin` wrappers. `ns` is the
    /// lexical module nesting.
    fn walk(&mut self, file: usize, src: &str, node: &'a Node, ns: &[String]) {
        match node {
            Node::Begin(b) => {
                for stmt in &b.statements {
                    self.walk(file, src, stmt, ns);
                }
            }
            Node::Module(m) => {
                let Some(name) = const_path(&m.name) else {
                    return;
                };
                let mut nested = ns.to_vec();
                nested.push(name);
                if let Some(body) = m.body.as_deref() {
                    self.walk(file, src, body, &nested);
                }
            }
            Node::Class(c) => {
                let Some(local) = const_path(&c.name) else {
                    return;
                };
                let qualified = if ns.is_empty() {
                    local
                } else {
                    format!("{}::{local}", ns.join("::"))
                };
                // An absolute path (`::Base`) is recorded with an empty nesting
                // so `resolve` bypasses lexical lookup.
                let superclass = c.superclass.as_deref().and_then(|sup| {
                    let path = const_path(sup)?;
                    let nesting = if is_absolute(sup) {
                        Vec::new()
                    } else {
                        ns.to_vec()
                    };
                    Some((nesting, path))
                });
                let entry = self
                    .classes
                    .entry(qualified.clone())
                    .or_insert_with(|| ClassEntry {
                        superclass: None,
                        defs: HashMap::new(),
                    });
                if entry.superclass.is_none() {
                    entry.superclass = superclass;
                }
                let nested: Vec<String> = qualified.split("::").map(str::to_string).collect();
                if let Some(body) = c.body.as_deref() {
                    let stmts: Vec<&'a Node> = match body {
                        Node::Begin(b) => b.statements.iter().collect(),
                        other => vec![other],
                    };
                    for stmt in stmts {
                        self.statement(file, src, &qualified, &nested, stmt);
                    }
                }
            }
            _ => {}
        }
    }

    fn statement(&mut self, file: usize, src: &str, class: &str, ns: &[String], stmt: &'a Node) {
        match stmt {
            Node::Def(def) => self.add_def(file, class, def),
            Node::Class(_) | Node::Module(_) => self.walk(file, src, stmt, ns),
            Node::Send(send) if send.recv.is_none() => {
                // `private def foo` registers `foo` all the same.
                if let [Node::Def(def)] = send.args.as_slice() {
                    self.add_def(file, class, def);
                } else if send.method_name == "attribute" {
                    self.attribute_call(file, src, class, send);
                }
            }
            Node::Block(Block { call, .. }) => {
                if let Node::Send(send) = &**call
                    && send.recv.is_none()
                    && send.method_name == "attribute"
                {
                    self.attribute_call(file, src, class, send);
                }
            }
            _ => {}
        }
    }

    fn add_def(&mut self, file: usize, class: &str, def: &'a DefNode) {
        if let Some(entry) = self.classes.get_mut(class) {
            entry.defs.insert(def.name.clone(), DefRef { file, def });
        }
    }

    fn attribute_call(&mut self, file: usize, src: &str, class: &str, send: &'a SendNode) {
        let Some(Node::Sym(sym)) = send.args.first() else {
            return;
        };
        self.attribute_calls += 1;
        let pairs = match send.args.last() {
            Some(Node::Kwargs(k)) => &k.pairs,
            Some(Node::Hash(h)) => &h.pairs,
            _ => return,
        };
        let writable = pairs.iter().find_map(|pair| match pair {
            Node::Pair(p) if matches!(&*p.key, Node::Sym(k) if k.name.to_string_lossy() == "writable") => {
                Some(&*p.value)
            }
            _ => None,
        });
        if let Some(value) = writable {
            self.pending.push(Pending {
                class: class.to_string(),
                file,
                line: line_of(src, send.expression_l.begin),
                attribute: sym.name.to_string_lossy(),
                value,
            });
        }
    }
}

fn line_of(src: &str, offset: usize) -> usize {
    1 + src
        .as_bytes()
        .iter()
        .take(offset)
        .filter(|b| **b == b'\n')
        .count()
}

/// `A::B` for a constant path made only of constants (a leading `::` is
/// dropped; see [`is_absolute`]).
fn const_path(node: &Node) -> Option<String> {
    let Node::Const(c) = node else {
        return None;
    };
    match c.scope.as_deref() {
        None | Some(Node::Cbase(_)) => Some(c.name.clone()),
        Some(scope) => const_path(scope).map(|base| format!("{base}::{}", c.name)),
    }
}

/// True when the constant path is rooted at `::`.
fn is_absolute(node: &Node) -> bool {
    match node {
        Node::Const(c) => match c.scope.as_deref() {
            Some(Node::Cbase(_)) => true,
            Some(scope) => is_absolute(scope),
            None => false,
        },
        _ => false,
    }
}

/// `lambda {}`, `proc {}` or `->`: the call a block hangs off.
fn is_lambda_like(b: &Block) -> bool {
    match &*b.call {
        Node::Lambda(_) => true,
        Node::Send(s) => {
            s.recv.is_none()
                && s.args.is_empty()
                && matches!(s.method_name.as_str(), "lambda" | "proc")
        }
        _ => false,
    }
}

/// Lowering state: which file the node being lowered lives in, the class
/// lookups start from, the inline stack, and bound parameters.
struct Frame {
    file: usize,
    class: String,
    stack: Vec<String>,
    /// Parameter name → what an identifier of that name stands for.
    bindings: HashMap<String, Result<GuardExpr, String>>,
}

struct Lowerer<'a> {
    files: &'a [Parsed],
    classes: &'a HashMap<String, ClassEntry<'a>>,
}

impl Lowerer<'_> {
    /// The `writable:` value itself is a literal or a lambda; a bare call
    /// there would run at class-definition time, not as a predicate. Only this
    /// outer callable is unwrapped: its body is the guard.
    fn lower_writable(&self, value: &Node, st: &mut Frame) -> Result<GuardExpr, String> {
        match value {
            Node::True(_) | Node::False(_) => self.lower(value, st),
            Node::Block(b) if is_lambda_like(b) => match b.body.as_deref() {
                Some(body) => self.lower(body, st),
                None => Err("lambda with an empty body".to_string()),
            },
            _ => Err(self.unsupported(value, st)),
        }
    }

    fn lower(&self, node: &Node, st: &mut Frame) -> Result<GuardExpr, String> {
        match node {
            Node::True(_) => Ok(GuardExpr::Const(true)),
            Node::False(_) => Ok(GuardExpr::Const(false)),
            Node::Or(o) => Ok(GuardExpr::Or(
                Box::new(self.lower(&o.lhs, st)?),
                Box::new(self.lower(&o.rhs, st)?),
            )),
            Node::And(a) => Ok(GuardExpr::And(
                Box::new(self.lower(&a.lhs, st)?),
                Box::new(self.lower(&a.rhs, st)?),
            )),
            Node::Begin(b) => match b.statements.as_slice() {
                [one] => self.lower(one, st),
                many => Err(format!(
                    "{} statements where one expression is required",
                    many.len()
                )),
            },
            // A callable inside a guard body is a returned value (a Proc is
            // truthy), not the predicate.
            Node::Block(b) if is_lambda_like(b) => Err(format!(
                "guard returns a callable: {}",
                self.snippet(node, st)
            )),
            Node::Lvar(l) => match st.bindings.get(&l.name) {
                Some(bound) => bound.clone(),
                None => Err(format!("unbound local `{}`", l.name)),
            },
            Node::Send(s) => self.lower_send(node, s, st),
            Node::CSend(_) => self.chain(node, true, st).map(GuardExpr::Fact),
            _ => Err(self.unsupported(node, st)),
        }
    }

    fn lower_send(
        &self,
        node: &Node,
        send: &SendNode,
        st: &mut Frame,
    ) -> Result<GuardExpr, String> {
        if send.method_name == "!"
            && send.args.is_empty()
            && let Some(recv) = send.recv.as_deref()
        {
            return Ok(GuardExpr::Not(Box::new(self.lower(recv, st)?)));
        }
        if send.recv.is_some() {
            return self.chain(node, true, st).map(GuardExpr::Fact);
        }
        if !send.args.is_empty() {
            return Err(format!("call with arguments: {}", self.snippet(node, st)));
        }
        if send.method_name == "model" {
            return Err("bare `model` is not a fact".to_string());
        }
        self.inline(&send.method_name, st)
    }

    /// Canonical text of a call chain. `top` is true only for the outermost
    /// call, the one place a constant receiver is accepted.
    fn chain(&self, node: &Node, top: bool, st: &Frame) -> Result<String, String> {
        let (recv, name, args) = match node {
            Node::Send(s) => (s.recv.as_deref(), s.method_name.as_str(), &s.args),
            Node::CSend(c) => (Some(&*c.recv), c.method_name.as_str(), &c.args),
            _ => return Err(self.unsupported(node, st)),
        };
        let Some(recv) = recv else {
            if name == "model" && args.is_empty() {
                return Ok("model".to_string());
            }
            return Err(format!(
                "call not rooted at `model`: {}",
                self.snippet(node, st)
            ));
        };
        let suffix = match args.as_slice() {
            [] => String::new(),
            [Node::BlockPass(bp)] => match bp.value.as_deref() {
                Some(Node::Sym(sym)) => format!("(&:{})", sym.name.to_string_lossy()),
                _ => {
                    return Err(format!(
                        "block-pass that is not a symbol: {}",
                        self.snippet(node, st)
                    ));
                }
            },
            _ => {
                return Err(format!("call with arguments: {}", self.snippet(node, st)));
            }
        };
        let base = match recv {
            Node::Lvar(l) => match st.bindings.get(&l.name) {
                Some(Ok(GuardExpr::Fact(text))) => text.clone(),
                Some(Ok(_)) => {
                    return Err(format!("local `{}` is not bound to a fact", l.name));
                }
                Some(Err(reason)) => return Err(reason.clone()),
                None => return Err(format!("unbound local `{}`", l.name)),
            },
            Node::Const(_) if top => const_path(recv).ok_or_else(|| self.unsupported(recv, st))?,
            Node::Send(_) | Node::CSend(_) => self.chain(recv, false, st)?,
            _ => return Err(self.unsupported(recv, st)),
        };
        Ok(format!("{base}.{name}{suffix}"))
    }

    /// Inline `def name`, found from the declaring class upward.
    fn inline(&self, name: &str, st: &mut Frame) -> Result<GuardExpr, String> {
        if st.stack.iter().any(|n| n == name) {
            return Err(format!(
                "recursive inline of `{name}` ({} -> {name})",
                st.stack.join(" -> ")
            ));
        }
        if st.stack.len() >= MAX_INLINE_DEPTH {
            return Err(format!(
                "inline depth limit {MAX_INLINE_DEPTH} reached at `{name}`"
            ));
        }
        let Some(found) = self.find_def(&st.class, name) else {
            return Err(format!(
                "no `def {name}` in `{}` or its scanned superclasses",
                st.class
            ));
        };

        let saved_file = st.file;
        let saved_bindings = std::mem::take(&mut st.bindings);
        st.file = found.file;
        st.stack.push(name.to_string());
        let result = self.lower_def_body(name, found.def, st);
        st.stack.pop();
        st.file = saved_file;
        st.bindings = saved_bindings;
        result
    }

    fn lower_def_body(
        &self,
        name: &str,
        def: &DefNode,
        st: &mut Frame,
    ) -> Result<GuardExpr, String> {
        if let Some(Node::Args(args)) = def.args.as_deref() {
            for param in &args.args {
                match param {
                    Node::Optarg(opt) => {
                        let bound = self.lower(&opt.default, st);
                        st.bindings.insert(opt.name.clone(), bound);
                    }
                    Node::Arg(arg) => {
                        return Err(format!(
                            "required parameter `{}` of `{name}` has no default",
                            arg.name
                        ));
                    }
                    other => {
                        return Err(format!(
                            "unsupported parameter in `{name}`: {}",
                            self.snippet(other, st)
                        ));
                    }
                }
            }
        }
        match def.body.as_deref() {
            Some(body) => self.lower(body, st),
            None => Err(format!("`def {name}` has an empty body")),
        }
    }

    fn find_def(&self, class: &str, name: &str) -> Option<DefRef<'_>> {
        let mut current = class.to_string();
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(current.clone()) {
                return None;
            }
            let entry = self.classes.get(&current)?;
            if let Some(found) = entry.defs.get(name) {
                return Some(*found);
            }
            let (ns, sup) = entry.superclass.as_ref()?;
            current = self.resolve(ns, sup)?;
        }
    }

    /// Resolve a superclass name against the module nesting it was written in,
    /// innermost scope first.
    fn resolve(&self, ns: &[String], sup: &str) -> Option<String> {
        (0..=ns.len()).rev().find_map(|depth| {
            let candidate = if depth == 0 {
                sup.to_string()
            } else {
                format!("{}::{sup}", ns[..depth].join("::"))
            };
            self.classes.contains_key(&candidate).then_some(candidate)
        })
    }

    fn unsupported(&self, node: &Node, st: &Frame) -> String {
        format!(
            "unsupported {}: {}",
            node.str_type(),
            self.snippet(node, st)
        )
    }

    fn snippet(&self, node: &Node, st: &Frame) -> String {
        let loc = node.expression();
        let raw = self.files[st.file]
            .src
            .as_bytes()
            .get(loc.begin..loc.end)
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default();
        let flat = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        if flat.chars().count() > SNIPPET_MAX_CHARS {
            let head: String = flat.chars().take(SNIPPET_MAX_CHARS).collect();
            format!("{head}…")
        } else {
            flat
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use GuardExpr::{And, Const, Fact, Not, Or};
    use std::fmt::Write as _;

    fn fact(text: &str) -> GuardExpr {
        Fact(text.to_string())
    }

    fn scan(case: &str, files: &[(&str, &str)]) -> (Vec<ContractGuard>, GuardScanReport) {
        let root = std::env::temp_dir().join(format!(
            "ruff_ruby_spo_guards_{}_{case}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        for (rel, content) in files {
            let path = root.join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, content).unwrap();
        }
        let out = extract_contract_guards_with_report(&root);
        let _ = fs::remove_dir_all(&root);
        out
    }

    fn outcome<'a>(guards: &'a [ContractGuard], class: &str, attr: &str) -> &'a GuardOutcome {
        &guards
            .iter()
            .find(|g| g.class == class && g.attribute == attr)
            .unwrap_or_else(|| panic!("no guard {class}#{attr}: {guards:?}"))
            .outcome
    }

    fn lowered(guards: &[ContractGuard], class: &str, attr: &str) -> GuardExpr {
        match outcome(guards, class, attr) {
            GuardOutcome::Lowered(e) => e.clone(),
            GuardOutcome::Unlowered { reason } => panic!("{attr} unlowered: {reason}"),
        }
    }

    fn unlowered(guards: &[ContractGuard], class: &str, attr: &str) -> String {
        match outcome(guards, class, attr) {
            GuardOutcome::Unlowered { reason } => reason.clone(),
            GuardOutcome::Lowered(e) => panic!("{attr} unexpectedly lowered: {e:?}"),
        }
    }

    const BASE: &str = r"
module WorkPackages
  # namespace holder
end

class WorkPackages::BaseContract < ModelContract
    attribute :status_id,
              permission: %i[edit_work_packages change_work_package_status],
              writable: ->(*) {
                # If we did not change into the status,
                # mark unwritable if status and version is closed
                model.status_id_change || !closed_version_and_status?
              }
    attribute :done_ratio,
              writable: ->(*) {
                          WorkPackage.work_based_mode?
                        } do
      next
    end
    attribute :derived_done_ratio,
              writable: false
    attribute :start_date,
              writable: ->(*) {
                leaf_or_manually_scheduled?
              } do
      validate_after_soonest_start(:start_date)
    end
    attribute :subject

    def closed_version_and_status?(status = model.status)
      status&.closed? && model.effective_target_versions.any?(&:closed?)
    end

    def leaf_or_manually_scheduled?
      model.leaf? || model.schedule_manually?
    end
end
";

    const CHILD: &str = r"
class WorkPackages::CreateContract < WorkPackages::BaseContract
  attribute :scheduled, writable: ->(*) { leaf_or_manually_scheduled? }
end
";

    #[test]
    fn openproject_snippet_lowers_to_exact_expressions() {
        let (guards, _) = scan("snippet", &[("base.rb", BASE), ("child.rb", CHILD)]);
        let base = "WorkPackages::BaseContract";

        // Disabling parameter-default substitution turns this into `unbound local`.
        assert_eq!(
            lowered(&guards, base, "status_id"),
            Or(
                Box::new(fact("model.status_id_change")),
                Box::new(Not(Box::new(And(
                    Box::new(fact("model.status.closed?")),
                    Box::new(fact("model.effective_target_versions.any?(&:closed?)")),
                )))),
            )
        );
        assert_eq!(
            lowered(&guards, base, "start_date"),
            Or(
                Box::new(fact("model.leaf?")),
                Box::new(fact("model.schedule_manually?")),
            )
        );
        assert_eq!(
            lowered(&guards, base, "done_ratio"),
            fact("WorkPackage.work_based_mode?")
        );
        assert_eq!(lowered(&guards, base, "derived_done_ratio"), Const(false));
    }

    #[test]
    fn location_and_order_are_reported() {
        let (guards, _) = scan("location", &[("base.rb", BASE), ("child.rb", CHILD)]);
        let names: Vec<_> = guards.iter().map(|g| g.attribute.as_str()).collect();
        assert_eq!(
            names,
            [
                "status_id",
                "done_ratio",
                "derived_done_ratio",
                "start_date",
                "scheduled"
            ]
        );
        assert_eq!(guards[0].file, "base.rb");
        // BASE starts with a newline, so `attribute :status_id` is on line 7.
        assert_eq!(guards[0].line, 7);
        assert_eq!(guards[4].file, "child.rb");
        assert_eq!(guards[4].line, 3);
    }

    #[test]
    fn predicate_inherited_from_the_superclass_is_inlined() {
        let (guards, _) = scan("inherit", &[("base.rb", BASE), ("child.rb", CHILD)]);
        assert_eq!(
            lowered(&guards, "WorkPackages::CreateContract", "scheduled"),
            Or(
                Box::new(fact("model.leaf?")),
                Box::new(fact("model.schedule_manually?")),
            )
        );
    }

    #[test]
    fn report_counts_the_ledger() {
        let (_, report) = scan("report", &[("base.rb", BASE), ("child.rb", CHILD)]);
        assert_eq!(
            report,
            GuardScanReport {
                rb_files: 2,
                attribute_calls: 6,
                writable_guards: 5,
                lowered: 5,
                unlowered: 0,
            }
        );
    }

    #[test]
    fn call_with_arguments_is_unlowered() {
        let (guards, report) = scan(
            "args",
            &[(
                "c.rb",
                "class C\n  attribute :a, writable: ->(*) { can?(1) }\n  attribute :b, writable: ->(*) { model.foo(1) }\nend\n",
            )],
        );
        assert_eq!(unlowered(&guards, "C", "a"), "call with arguments: can?(1)");
        assert_eq!(
            unlowered(&guards, "C", "b"),
            "call with arguments: model.foo(1)"
        );
        assert_eq!((report.lowered, report.unlowered), (0, 2));
    }

    #[test]
    fn required_parameter_is_unlowered() {
        let (guards, _) = scan(
            "required",
            &[(
                "c.rb",
                "class C\n  attribute :a, writable: ->(*) { needs?  }\n  def needs?(x)\n    model.a?\n  end\nend\n",
            )],
        );
        let reason = unlowered(&guards, "C", "a");
        assert!(reason.contains("required parameter `x`"), "{reason}");
    }

    #[test]
    fn mutual_recursion_terminates_unlowered() {
        let (guards, _) = scan(
            "recursion",
            &[(
                "c.rb",
                "class C\n  attribute :a, writable: ->(*) { ping? }\n  def ping?\n    pong?\n  end\n  def pong?\n    ping?\n  end\nend\n",
            )],
        );
        let reason = unlowered(&guards, "C", "a");
        assert!(
            reason.contains("recursive") && reason.contains("`ping?`"),
            "{reason}"
        );
    }

    #[test]
    fn inline_depth_is_limited_to_four() {
        let chain = |len: usize| {
            let mut src = String::from("class C\n  attribute :a, writable: ->(*) { d1? }\n");
            for i in 1..=len {
                let body = if i == len {
                    "model.leaf?".to_string()
                } else {
                    format!("d{}?", i + 1)
                };
                let _ = write!(src, "  def d{i}?\n    {body}\n  end\n");
            }
            src.push_str("end\n");
            src
        };
        let (ok, _) = scan("depth4", &[("c.rb", &chain(4))]);
        assert_eq!(lowered(&ok, "C", "a"), fact("model.leaf?"));
        let (too_deep, _) = scan("depth5", &[("c.rb", &chain(5))]);
        let reason = unlowered(&too_deep, "C", "a");
        assert!(reason.contains("depth limit"), "{reason}");
    }

    #[test]
    fn absolute_superclass_bypasses_lexical_lookup() {
        let src = "class Base\n  def ok?\n    model.top?\n  end\nend\n\
                   module N\n  class Base\n    def ok?\n      model.nested?\n    end\n  end\n\
                   class Abs < ::Base\n    attribute :a, writable: ->(*) { ok? }\n  end\n\
                   class Lex < Base\n    attribute :a, writable: ->(*) { ok? }\n  end\nend\n";
        let (guards, _) = scan("abs_super", &[("c.rb", src)]);
        assert_eq!(lowered(&guards, "N::Abs", "a"), fact("model.top?"));
        assert_eq!(lowered(&guards, "N::Lex", "a"), fact("model.nested?"));
    }

    #[test]
    fn only_the_outer_writable_callable_is_unwrapped() {
        let src = "class C\n  attribute :plain, writable: ->(*) { false }\n\
                   attribute :direct, writable: ->(*) { -> { false } }\n\
                   attribute :helper, writable: ->(*) { maker }\n\
                   def maker\n    proc { model.leaf? }\n  end\nend\n";
        let (guards, _) = scan("callable", &[("c.rb", src)]);
        assert_eq!(lowered(&guards, "C", "plain"), Const(false));
        let direct = unlowered(&guards, "C", "direct");
        assert!(direct.contains("guard returns a callable"), "{direct}");
        let helper = unlowered(&guards, "C", "helper");
        assert!(helper.contains("guard returns a callable"), "{helper}");
    }

    #[test]
    fn multiple_statements_and_unknown_shapes_are_unlowered() {
        let (guards, _) = scan(
            "shapes",
            &[(
                "c.rb",
                "class C\n  attribute :a, writable: ->(*) { x = 1; model.leaf? }\n  attribute :b, writable: :sym\n  attribute :c, writable: ->(*) { missing? }\n  attribute :d, writable: proc { model&.owner&.admin? }\n  attribute :e, writable: lambda { not model.leaf? }\nend\n",
            )],
        );
        assert!(unlowered(&guards, "C", "a").contains("2 statements"));
        assert!(unlowered(&guards, "C", "b").starts_with("unsupported sym:"));
        assert!(unlowered(&guards, "C", "c").contains("no `def missing?`"));
        assert_eq!(lowered(&guards, "C", "d"), fact("model.owner.admin?"));
        assert_eq!(
            lowered(&guards, "C", "e"),
            Not(Box::new(fact("model.leaf?")))
        );
    }
}
