//! The Rails seed-call harvest arm: `db/seeds/*.rb` rows as data.
//!
//! A seed file is a list of calls such as
//! `ObjectManager::Attribute.add(name: 'state_id', screens: { ... })` or
//! `Setting.create_if_not_exists(title: 'X', name: 'x', state: { ... })`.
//! Their arguments are hash literals that define which fields a screen shows
//! per role, the default overviews, field rules and setting defaults. This
//! arm reads each matching call as a [`SeedRow`] so the data is harvested,
//! not hand-copied.
//!
//! # AST walk
//!
//! The whole tree is walked, so calls nested in blocks (`each do … end`),
//! `if`/`unless` bodies or argument positions are found. A call matches when
//! its receiver is a constant path (`A::B`, also `::A::B`) equal to the
//! spec's receiver and its method name equals the spec's method. A call on
//! any other receiver (a local, a method result, no receiver) is not a match.
//!
//! # Literal lowering
//!
//! Strings (including `%w[]` / `%i[]` elements, heredocs and adjacent or
//! interpolation-free `dstr` literals), symbols, integers, floats (source
//! text kept), `true`/`false`/`nil`, arrays and hashes lower to
//! [`SeedValue`]. Everything else (method calls, constants, locals,
//! interpolation, ranges, lambdas, `**splat` entries) becomes
//! [`SeedValue::Unresolved`] carrying its exact source text. Nothing is
//! guessed. A hash key that is not a string, symbol, integer, boolean or
//! `nil` literal becomes [`SeedKey::Unresolved`]. A `**splat` hash entry is one unresolved key
//! plus one unresolved value, both holding the splat's source text.
//!
//! A call on a constant path whose arguments all lower to resolved values
//! (`Role.find_by(name: 'Agent')`) is [`SeedValue::Call`]; a further call on
//! such a value (`….id`, `….pluck(:id)`) is [`SeedValue::Chain`], with empty
//! `args` for a plain attribute read. Nothing is evaluated: the value names
//! what the consumer has to resolve against its own data. An operator, a
//! safe-navigation call, a block, or any argument that is itself unresolved
//! keeps the whole expression [`SeedValue::Unresolved`].
//!
//! With [`SeedOptions::follow_locals`], a bare local read lowers to the value
//! the local was last bound to. A local binds only through `name = <expr>` as
//! a direct statement of a straight-line statement list: the file's top level,
//! a class, module, `def` or `class << self` body, or a parenthesised
//! sequence, reached without passing through any `if`/`unless`/ternary, `case`,
//! loop, block, lambda, `begin`/`rescue`/`ensure` or `&&`/`||`. A later
//! assignment to the same name rebinds it. The name is unbound, and reads stay
//! unresolved, when it is assigned in any other position (inside one of the
//! constructs above, as `x = 1 if c`, `x ||= v`, `a, x = v`, or inside call
//! arguments), when the assigned value is itself unresolved, or when a block
//! or method parameter of the same name shadows it. A class, module, `def` or
//! `class << self` body starts with no bindings and does not leak its own.
//!
//! A hash key that is such a local read becomes a [`SeedKey`] when the bound
//! value is a string, symbol, integer, boolean or `nil`. A value too large to
//! bind (more than 4096 nodes, counting hash keys) leaves the name unbound.
//! Pattern captures (`in {r:}`, `=> r`) assign like any other write.
//!
//! A receiver-less call to one of [`SeedOptions::translation_markers`] with a
//! single string literal argument (gettext's `__('Title')`) lowers to
//! [`SeedValue::Translatable`] holding the source string. The markers are
//! configuration: without them such a call stays unresolved.
//!
//! # Conservation
//!
//! In every [`SeedScanReport`]:
//! `calls_found == rows_fully_literal + rows_with_unresolved` and
//! `calls_found == rows.len()`. `unresolved_values` counts every
//! [`SeedValue::Unresolved`] and [`SeedKey::Unresolved`] in all rows. A file
//! that cannot be read or parsed counts in neither `files_scanned` nor any
//! other figure.
//!
//! No SPO triples are emitted: the shared `Predicate` set has no variant for
//! seed rows, and adding one is outside this arm.

use std::cell::Cell;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use lib_ruby_parser::nodes::{
    And, Arg, Begin as BeginNode, Block, Blockarg, Case, CaseMatch, Class as ClassNode,
    Def as DefNode, Defs as DefsNode, Ensure, For, If, IfMod, IfTernary, KwBegin, Kwarg, Kwoptarg,
    Kwrestarg, Lvasgn, MatchVar, Module as ModuleNode, Numblock, Optarg, Or, Rescue, Restarg,
    SClass as SclassNode, Send as SendNode, Shadowarg, Until, UntilPost, While, WhilePost,
};
use lib_ruby_parser::traverse::visitor::{
    Visitor, visit_and, visit_begin, visit_block, visit_case, visit_case_match, visit_ensure,
    visit_for, visit_if, visit_if_mod, visit_if_ternary, visit_kw_begin, visit_lvasgn,
    visit_numblock, visit_or, visit_rescue, visit_send, visit_until, visit_until_post, visit_while,
    visit_while_post,
};
use lib_ruby_parser::{Node, Parser, ParserOptions};

/// A call to harvest: the constant path of the receiver and the method name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedCallSpec {
    /// Receiver constant path without a leading `::`, e.g. `ObjectManager::Attribute`.
    pub receiver: String,
    /// Method name, e.g. `add`.
    pub method: String,
}

/// A hash key of a seed row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedKey {
    Str(String),
    Sym(String),
    Int(i64),
    Bool(bool),
    Nil,
    /// A key that is not one of the literals above; `source` is its text.
    Unresolved {
        source: String,
    },
}

/// A literal value of a seed row, or the exact text of what is not one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedValue {
    Str(String),
    Sym(String),
    Int(i64),
    /// A float literal, kept as source text.
    Float(String),
    Bool(bool),
    Nil,
    Array(Vec<SeedValue>),
    Hash(Vec<(SeedKey, SeedValue)>),
    /// A string marked for translation (`__('Title')`), holding the source
    /// string, untranslated.
    Translatable(String),
    /// `Receiver.method(args)` on a constant path (no leading `::`); every
    /// argument is resolved (no [`SeedValue::Unresolved`] inside).
    Call {
        receiver: String,
        method: String,
        args: Vec<SeedValue>,
    },
    /// `<base>.method(args)` where `base` is a `Call` or another `Chain` and
    /// every argument is resolved; a plain attribute read (`….id`) has empty
    /// `args`.
    Chain {
        base: Box<SeedValue>,
        method: String,
        args: Vec<SeedValue>,
    },
    /// Any other expression; `source` is its text from the byte range.
    Unresolved {
        source: String,
    },
}

/// One matched call, in source order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedRow {
    pub call: SeedCallSpec,
    pub file: String,
    /// 1-based line of the call.
    pub line: u32,
    /// Positional arguments; a trailing keyword-argument list is one `Hash`.
    pub args: Vec<SeedValue>,
}

/// Counts for one [`extract_seed_rows`] run. See the module doc for the
/// conservation law.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SeedScanReport {
    pub files_scanned: usize,
    pub calls_found: usize,
    pub rows_fully_literal: usize,
    pub rows_with_unresolved: usize,
    pub unresolved_values: usize,
}

/// How literal values are lowered beyond plain Ruby literals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeedOptions {
    /// Receiver-less method names that mark a string for translation
    /// (`["__"]` for gettext-style seeds).
    pub translation_markers: Vec<String>,
    /// Lower a bare local to the value it was bound to; see the module doc
    /// for exactly which assignments bind. Off by default. Only seed-row
    /// extraction follows locals: class macros and class constants ignore it.
    pub follow_locals: bool,
    /// Method names peeled off the end of a class constant's expression before
    /// lowering (`["freeze", "with_indifferent_access"]`); only
    /// [`extract_class_constants`](crate::extract_class_constants) reads it.
    pub transparent_methods: Vec<String>,
}

/// Read every file in `files` (paths relative to `root`) and return one
/// [`SeedRow`] per call matching any of `calls`, files in the given order and
/// calls in source order within a file.
#[must_use]
pub fn extract_seed_rows(
    root: &Path,
    files: &[&str],
    calls: &[SeedCallSpec],
) -> (Vec<SeedRow>, SeedScanReport) {
    extract_seed_rows_with(root, files, calls, &SeedOptions::default())
}

/// [`extract_seed_rows`] with lowering options.
#[must_use]
pub fn extract_seed_rows_with(
    root: &Path,
    files: &[&str],
    calls: &[SeedCallSpec],
    options: &SeedOptions,
) -> (Vec<SeedRow>, SeedScanReport) {
    let mut rows = Vec::new();
    let mut report = SeedScanReport::default();
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
            lowerer: Lowerer::new(&src, options),
            file: rel,
            specs: calls,
            rows: Vec::new(),
            depth: 0,
        };
        collector.statement(&ast);
        for row in collector.rows {
            let unresolved = row.args.iter().map(count_unresolved).sum::<usize>();
            report.calls_found += 1;
            if unresolved == 0 {
                report.rows_fully_literal += 1;
            } else {
                report.rows_with_unresolved += 1;
            }
            report.unresolved_values += unresolved;
            rows.push(row);
        }
    }
    (rows, report)
}

/// Largest value, counted in [`SeedValue`] nodes, that a local may be bound to.
const MAX_BOUND_NODES: usize = 4096;
/// The most substituted cost one top-level lowering may copy out of bound
/// locals; a wide expression reading a large local many times stays unresolved
/// past it instead of allocating every copy.
const MAX_SUBSTITUTED_COST: usize = 4 * MAX_BOUND_NODES;

struct Collector<'a> {
    lowerer: Lowerer<'a>,
    file: &'a str,
    specs: &'a [SeedCallSpec],
    rows: Vec<SeedRow>,
    /// Number of enclosing constructs in which an assignment may not run (or
    /// may run more than once); a local binds only at depth 0.
    depth: usize,
}

/// The locals a subtree assigns anywhere in it, pattern captures included.
#[derive(Default)]
struct AssignedLocals(Vec<String>);

impl Visitor for AssignedLocals {
    fn on_lvasgn(&mut self, node: &Lvasgn) {
        self.0.push(node.name.clone());
        visit_lvasgn(self, node);
    }

    fn on_match_var(&mut self, node: &MatchVar) {
        self.0.push(node.name.clone());
    }

    // A method, class or module body has its own local scope: its assignments
    // never touch the enclosing locals.
    // Header expressions (a superclass, a `def self.` or `class <<` target)
    // still run in the enclosing scope, so they are visited.
    fn on_def(&mut self, _: &DefNode) {}
    fn on_defs(&mut self, node: &DefsNode) {
        self.visit(&node.definee);
    }
    fn on_class(&mut self, node: &ClassNode) {
        self.visit(&node.name);
        if let Some(superclass) = node.superclass.as_deref() {
            self.visit(superclass);
        }
    }
    fn on_module(&mut self, node: &ModuleNode) {
        self.visit(&node.name);
    }
    fn on_s_class(&mut self, node: &SclassNode) {
        self.visit(&node.expr);
    }
}

/// A visitor hook that counts an enclosing construct for [`Collector::depth`].
/// The construct may run more than once or not at all, so every local it
/// assigns is unbound before its body is lowered.
macro_rules! branching {
    ($($method:ident($node:ident, $walk:ident)),* $(,)?) => {
        $(
            fn $method(&mut self, node: &$node) {
                let mut assigned = AssignedLocals::default();
                $walk(&mut assigned, node);
                for name in &assigned.0 {
                    self.unbind(name);
                }
                self.depth += 1;
                $walk(self, node);
                self.depth -= 1;
            }
        )*
    };
}

impl Collector<'_> {
    /// One statement of a straight-line list: `name = <expr>` binds or unbinds
    /// the local, anything else is walked.
    fn statement(&mut self, stmt: &Node) {
        if self.depth == 0
            && let Node::Lvasgn(assign) = stmt
            && let Some(value) = assign.value.as_deref()
        {
            self.visit(value);
            if self.lowerer.options.follow_locals {
                let lowered = self.lowerer.lower(value);
                // A partly unresolved value is not bound: substituting it would copy its
                // unresolved parts into every row that reads the local. An oversized one
                // is not bound either, so repeated `x = [x, x]` cannot grow without limit.
                if count_unresolved(&lowered) == 0 && node_count(&lowered) <= MAX_BOUND_NODES {
                    let cost = substitution_cost(&lowered);
                    self.lowerer
                        .locals
                        .insert(assign.name.clone(), (lowered, cost));
                    return;
                }
            }
            self.lowerer.locals.remove(&assign.name);
        } else {
            self.visit(stmt);
        }
    }

    fn unbind(&mut self, name: &str) {
        self.lowerer.locals.remove(name);
    }

    /// Enter a new local scope (class, module, `def`): no bindings come in.
    fn enter_scope(&mut self) -> (HashMap<String, (SeedValue, usize)>, usize) {
        (
            std::mem::take(&mut self.lowerer.locals),
            std::mem::take(&mut self.depth),
        )
    }

    fn leave_scope(&mut self, saved: (HashMap<String, (SeedValue, usize)>, usize)) {
        self.lowerer.locals = saved.0;
        self.depth = saved.1;
    }
}

impl Visitor for Collector<'_> {
    fn on_send(&mut self, node: &SendNode) {
        if let Some(recv) = node.recv.as_deref()
            && let Some(path) = const_path(recv)
            && let Some(spec) = self
                .specs
                .iter()
                .find(|s| s.receiver == path && s.method == node.method_name)
        {
            // An argument may assign a local that a later argument reads.
            let mut assigned = AssignedLocals::default();
            for arg in &node.args {
                assigned.visit(arg);
            }
            for name in &assigned.0 {
                self.unbind(name);
            }
            self.rows.push(SeedRow {
                call: spec.clone(),
                file: self.file.to_string(),
                line: self.lowerer.line_of(node.expression_l.begin),
                args: node.args.iter().map(|a| self.lowerer.lower(a)).collect(),
            });
        }
        // A paren group in a receiver or argument is an expression, not a statement list.
        self.depth += 1;
        visit_send(self, node);
        self.depth -= 1;
    }

    fn on_begin(&mut self, node: &BeginNode) {
        if self.depth == 0 {
            for stmt in &node.statements {
                self.statement(stmt);
            }
        } else {
            visit_begin(self, node);
        }
    }

    fn on_lvasgn(&mut self, node: &Lvasgn) {
        self.unbind(&node.name);
        visit_lvasgn(self, node);
    }

    fn on_match_var(&mut self, node: &MatchVar) {
        self.unbind(&node.name);
    }

    fn on_class(&mut self, node: &ClassNode) {
        self.visit(&node.name);
        if let Some(superclass) = node.superclass.as_deref() {
            self.visit(superclass);
        }
        let saved = self.enter_scope();
        if let Some(body) = node.body.as_deref() {
            self.statement(body);
        }
        self.leave_scope(saved);
    }

    fn on_module(&mut self, node: &ModuleNode) {
        self.visit(&node.name);
        let saved = self.enter_scope();
        if let Some(body) = node.body.as_deref() {
            self.statement(body);
        }
        self.leave_scope(saved);
    }

    fn on_s_class(&mut self, node: &SclassNode) {
        self.visit(&node.expr);
        let saved = self.enter_scope();
        if let Some(body) = node.body.as_deref() {
            self.statement(body);
        }
        self.leave_scope(saved);
    }

    fn on_def(&mut self, node: &DefNode) {
        let saved = self.enter_scope();
        if let Some(args) = node.args.as_deref() {
            self.visit(args);
        }
        if let Some(body) = node.body.as_deref() {
            self.statement(body);
        }
        self.leave_scope(saved);
    }

    fn on_defs(&mut self, node: &DefsNode) {
        self.visit(&node.definee);
        let saved = self.enter_scope();
        if let Some(args) = node.args.as_deref() {
            self.visit(args);
        }
        if let Some(body) = node.body.as_deref() {
            self.statement(body);
        }
        self.leave_scope(saved);
    }

    fn on_arg(&mut self, node: &Arg) {
        self.unbind(&node.name);
    }

    fn on_optarg(&mut self, node: &Optarg) {
        self.unbind(&node.name);
        self.visit(&node.default);
    }

    fn on_kwarg(&mut self, node: &Kwarg) {
        self.unbind(&node.name);
    }

    fn on_kwoptarg(&mut self, node: &Kwoptarg) {
        self.unbind(&node.name);
        self.visit(&node.default);
    }

    fn on_shadowarg(&mut self, node: &Shadowarg) {
        self.unbind(&node.name);
    }

    fn on_restarg(&mut self, node: &Restarg) {
        if let Some(name) = &node.name {
            self.unbind(name);
        }
    }

    fn on_kwrestarg(&mut self, node: &Kwrestarg) {
        if let Some(name) = &node.name {
            self.unbind(name);
        }
    }

    fn on_blockarg(&mut self, node: &Blockarg) {
        if let Some(name) = &node.name {
            self.unbind(name);
        }
    }

    branching! {
        on_if(If, visit_if),
        on_if_mod(IfMod, visit_if_mod),
        on_if_ternary(IfTernary, visit_if_ternary),
        on_case(Case, visit_case),
        on_case_match(CaseMatch, visit_case_match),
        on_while(While, visit_while),
        on_while_post(WhilePost, visit_while_post),
        on_until(Until, visit_until),
        on_until_post(UntilPost, visit_until_post),
        on_for(For, visit_for),
        on_block(Block, visit_block),
        on_numblock(Numblock, visit_numblock),
        on_kw_begin(KwBegin, visit_kw_begin),
        on_rescue(Rescue, visit_rescue),
        on_ensure(Ensure, visit_ensure),
        on_and(And, visit_and),
        on_or(Or, visit_or),
    }
}

/// Source-aware lowering of AST nodes to [`SeedValue`]s, shared with the
/// class-macro arm.
pub(crate) struct Lowerer<'a> {
    pub(crate) src: &'a str,
    pub(crate) options: &'a SeedOptions,
    /// Locals bound so far; only filled when [`SeedOptions::follow_locals`].
    locals: HashMap<String, (SeedValue, usize)>,
    /// Substituted cost spent by the current top-level [`Lowerer::lower`].
    spent: Cell<usize>,
}

impl<'a> Lowerer<'a> {
    pub(crate) fn new(src: &'a str, options: &'a SeedOptions) -> Self {
        Self {
            src,
            options,
            locals: HashMap::new(),
            spent: Cell::new(0),
        }
    }
}

impl Lowerer<'_> {
    pub(crate) fn line_of(&self, offset: usize) -> u32 {
        let newlines = self
            .src
            .as_bytes()
            .iter()
            .take(offset)
            .filter(|b| **b == b'\n')
            .count();
        u32::try_from(newlines + 1).unwrap_or(u32::MAX)
    }

    pub(crate) fn source_of(&self, node: &Node) -> String {
        let loc = node.expression();
        self.src
            .as_bytes()
            .get(loc.begin..loc.end)
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default()
    }

    fn unresolved(&self, node: &Node) -> SeedValue {
        SeedValue::Unresolved {
            source: self.source_of(node),
        }
    }

    /// Lower one expression; the substitution budget starts afresh.
    pub(crate) fn lower(&self, node: &Node) -> SeedValue {
        self.spent.set(0);
        self.lower_node(node)
    }

    fn lower_node(&self, node: &Node) -> SeedValue {
        match node {
            Node::Str(s) => SeedValue::Str(s.value.to_string_lossy()),
            Node::Dstr(d) => self.concat_parts(node, &d.parts),
            Node::Heredoc(h) => self.concat_parts(node, &h.parts),
            Node::Sym(s) => SeedValue::Sym(s.name.to_string_lossy()),
            Node::Int(i) => {
                parse_int(&i.value).map_or_else(|| self.unresolved(node), SeedValue::Int)
            }
            Node::Float(f) => SeedValue::Float(f.value.clone()),
            Node::True(_) => SeedValue::Bool(true),
            Node::False(_) => SeedValue::Bool(false),
            Node::Nil(_) => SeedValue::Nil,
            Node::Array(a) => {
                SeedValue::Array(a.elements.iter().map(|e| self.lower_node(e)).collect())
            }
            Node::Hash(h) => self.lower_pairs(&h.pairs),
            Node::Kwargs(k) => self.lower_pairs(&k.pairs),
            Node::Lvar(var)
                if self.options.follow_locals
                    && let Some((value, cost)) = self.locals.get(&var.name)
                    && self.spent.get() + cost <= MAX_SUBSTITUTED_COST =>
            {
                self.spent.set(self.spent.get() + cost);
                value.clone()
            }
            Node::Send(call) if let Some(value) = self.lower_call(call) => value,
            Node::Send(call)
                if call.recv.is_none()
                    && self.options.translation_markers.contains(&call.method_name)
                    && let [arg] = call.args.as_slice()
                    && let SeedValue::Str(text) = self.lower_node(arg) =>
            {
                SeedValue::Translatable(text)
            }
            _ => self.unresolved(node),
        }
    }

    /// `Const.method(args)` as [`SeedValue::Call`], or `<call>.method(args)`
    /// as [`SeedValue::Chain`]; `None` unless every argument is resolved and
    /// the method is a plain name (not an operator or a setter).
    fn lower_call(&self, call: &SendNode) -> Option<SeedValue> {
        let recv = call.recv.as_deref()?;
        let name = call.method_name.as_str();
        let body = name.strip_suffix(['?', '!']).unwrap_or(name);
        if body.is_empty() || !body.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return None;
        }
        let args: Vec<SeedValue> = call.args.iter().map(|a| self.lower_node(a)).collect();
        if args.iter().any(|a| count_unresolved(a) > 0) {
            return None;
        }
        if let Some(receiver) = const_path(recv) {
            return Some(SeedValue::Call {
                receiver,
                method: call.method_name.clone(),
                args,
            });
        }
        let base = self.lower_node(recv);
        matches!(base, SeedValue::Call { .. } | SeedValue::Chain { .. }).then(|| SeedValue::Chain {
            base: Box::new(base),
            method: call.method_name.clone(),
            args,
        })
    }

    /// A `dstr`/heredoc is a literal only when every part is a plain string.
    fn concat_parts(&self, whole: &Node, parts: &[Node]) -> SeedValue {
        let mut text = String::new();
        for part in parts {
            match part {
                Node::Str(s) => text.push_str(&s.value.to_string_lossy()),
                Node::Dstr(d) => match self.concat_parts(part, &d.parts) {
                    SeedValue::Str(inner) => text.push_str(&inner),
                    _ => return self.unresolved(whole),
                },
                _ => return self.unresolved(whole),
            }
        }
        SeedValue::Str(text)
    }

    /// A key that is a followed local reads as the key literal it is bound to.
    fn followed_key(&self, key: &Node) -> SeedKey {
        if self.options.follow_locals
            && let Node::Lvar(_) = key
        {
            match self.lower_node(key) {
                SeedValue::Str(s) => return SeedKey::Str(s),
                SeedValue::Sym(s) => return SeedKey::Sym(s),
                SeedValue::Int(n) => return SeedKey::Int(n),
                SeedValue::Bool(b) => return SeedKey::Bool(b),
                SeedValue::Nil => return SeedKey::Nil,
                _ => {}
            }
        }
        SeedKey::Unresolved {
            source: self.source_of(key),
        }
    }

    fn lower_pairs(&self, pairs: &[Node]) -> SeedValue {
        let mut out = Vec::with_capacity(pairs.len());
        for pair in pairs {
            if let Node::Pair(p) = pair {
                let key = match p.key.as_ref() {
                    Node::Sym(s) => SeedKey::Sym(s.name.to_string_lossy()),
                    Node::Str(s) => SeedKey::Str(s.value.to_string_lossy()),
                    Node::True(_) => SeedKey::Bool(true),
                    Node::False(_) => SeedKey::Bool(false),
                    Node::Nil(_) => SeedKey::Nil,
                    Node::Int(i) if let Some(n) = parse_int(&i.value) => SeedKey::Int(n),
                    other => self.followed_key(other),
                };
                out.push((key, self.lower_node(&p.value)));
            } else {
                let source = self.source_of(pair);
                out.push((
                    SeedKey::Unresolved {
                        source: source.clone(),
                    },
                    SeedValue::Unresolved { source },
                ));
            }
        }
        SeedValue::Hash(out)
    }
}

/// `A`, `A::B`, `::A::B` as `A`, `A::B`, `A::B`; `None` for any other receiver.
pub(crate) fn const_path(node: &Node) -> Option<String> {
    let Node::Const(c) = node else { return None };
    match c.scope.as_deref() {
        None | Some(Node::Cbase(_)) => Some(c.name.clone()),
        Some(scope) => Some(format!("{}::{}", const_path(scope)?, c.name)),
    }
}

fn parse_int(text: &str) -> Option<i64> {
    let cleaned: String = text.chars().filter(|c| *c != '_').collect();
    cleaned.parse().ok()
}

/// What substituting a bound value costs: its nodes plus its string bytes in
/// units of 64, so one huge string costs like many small nodes.
fn substitution_cost(value: &SeedValue) -> usize {
    node_count(value) + string_bytes(value) / 64
}

fn string_bytes(value: &SeedValue) -> usize {
    match value {
        SeedValue::Str(s) | SeedValue::Sym(s) | SeedValue::Translatable(s) => s.len(),
        SeedValue::Array(items) | SeedValue::Call { args: items, .. } => {
            items.iter().map(string_bytes).sum()
        }
        SeedValue::Chain { base, args, .. } => {
            string_bytes(base) + args.iter().map(string_bytes).sum::<usize>()
        }
        SeedValue::Hash(pairs) => pairs.iter().map(|(_, v)| string_bytes(v)).sum(),
        _ => 0,
    }
}

fn node_count(value: &SeedValue) -> usize {
    1 + match value {
        SeedValue::Array(items) | SeedValue::Call { args: items, .. } => {
            items.iter().map(node_count).sum()
        }
        SeedValue::Chain { base, args, .. } => {
            node_count(base) + args.iter().map(node_count).sum::<usize>()
        }
        SeedValue::Hash(pairs) => pairs.iter().map(|(_, v)| 1 + node_count(v)).sum(),
        _ => 0,
    }
}

pub(crate) fn count_unresolved(value: &SeedValue) -> usize {
    match value {
        SeedValue::Unresolved { .. } => 1,
        SeedValue::Array(items) | SeedValue::Call { args: items, .. } => {
            items.iter().map(count_unresolved).sum()
        }
        SeedValue::Chain { base, args, .. } => {
            count_unresolved(base) + args.iter().map(count_unresolved).sum::<usize>()
        }
        SeedValue::Hash(pairs) => pairs
            .iter()
            .map(|(k, v)| {
                usize::from(matches!(k, SeedKey::Unresolved { .. })) + count_unresolved(v)
            })
            .sum(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch_dir(case: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ruff_ruby_spo_seeds_{}_{case}", std::process::id()))
    }

    fn spec(receiver: &str, method: &str) -> SeedCallSpec {
        SeedCallSpec {
            receiver: receiver.to_string(),
            method: method.to_string(),
        }
    }

    fn scan(case: &str, body: &str, calls: &[SeedCallSpec]) -> (Vec<SeedRow>, SeedScanReport) {
        let root = scratch_dir(case);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("seed.rb"), body).unwrap();
        let result = extract_seed_rows(&root, &["seed.rb"], calls);
        let _ = fs::remove_dir_all(&root);
        result
    }

    fn sym(s: &str) -> SeedKey {
        SeedKey::Sym(s.to_string())
    }

    fn string(s: &str) -> SeedValue {
        SeedValue::Str(s.to_string())
    }

    fn only_kwargs(row: &SeedRow) -> &Vec<(SeedKey, SeedValue)> {
        assert_eq!(row.args.len(), 1);
        let SeedValue::Hash(pairs) = &row.args[0] else {
            panic!("expected a hash, got {:?}", row.args[0]);
        };
        pairs
    }

    #[test]
    fn matches_receiver_and_lowers_fields() {
        let body = "Setting.create_if_not_exists(title: 'A', name: 'a', state: { value: true })\n\
                    Other.create_if_not_exists(title: 'X')\n\
                    Setting.create_if_not_exists(title: 'B', name: 'b', state: { value: true })\n";
        let (rows, report) = scan("t1", body, &[spec("Setting", "create_if_not_exists")]);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            report,
            SeedScanReport {
                files_scanned: 1,
                calls_found: 2,
                rows_fully_literal: 2,
                rows_with_unresolved: 0,
                unresolved_values: 0,
            }
        );
        assert_eq!(rows[0].line, 1);
        assert_eq!(rows[1].line, 3);
        assert_eq!(
            rows[0].args,
            vec![SeedValue::Hash(vec![
                (sym("title"), string("A")),
                (sym("name"), string("a")),
                (
                    sym("state"),
                    SeedValue::Hash(vec![(sym("value"), SeedValue::Bool(true))])
                ),
            ])]
        );
        assert_eq!(only_kwargs(&rows[1])[1], (sym("name"), string("b")));
    }

    #[test]
    fn non_literal_nested_value_is_unresolved_with_source() {
        let body = "ObjectManager::Attribute.add(name: 'x', data_option: { options: Ticket::State.by_category(state).pluck(:id) })\n";
        let (rows, report) = scan("t2", body, &[spec("ObjectManager::Attribute", "add")]);
        assert_eq!(rows.len(), 1);
        assert_eq!(report.calls_found, 1);
        assert_eq!(report.rows_fully_literal, 0);
        assert_eq!(report.rows_with_unresolved, 1);
        assert_eq!(report.unresolved_values, 1);
        assert_eq!(
            only_kwargs(&rows[0])[1],
            (
                sym("data_option"),
                SeedValue::Hash(vec![(
                    sym("options"),
                    SeedValue::Unresolved {
                        source: "Ticket::State.by_category(state).pluck(:id)".to_string()
                    }
                )])
            )
        );
    }

    #[test]
    fn a_configured_translation_marker_lowers_to_a_translatable_string() {
        let root = scratch_dir("t7");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("seed.rb"),
            "Setting.create_if_not_exists(title: __('Title'), help: __(name), other: x.__('y'))\n",
        )
        .unwrap();
        let calls = [spec("Setting", "create_if_not_exists")];
        let marked = SeedOptions {
            translation_markers: vec!["__".to_string()],
            ..Default::default()
        };
        let (with, with_report) = extract_seed_rows_with(&root, &["seed.rb"], &calls, &marked);
        let (without, without_report) = extract_seed_rows(&root, &["seed.rb"], &calls);
        let _ = fs::remove_dir_all(&root);

        let kw = only_kwargs(&with[0]);
        assert_eq!(
            kw[0],
            (sym("title"), SeedValue::Translatable("Title".to_string()))
        );
        assert_eq!(
            kw[1],
            (
                sym("help"),
                SeedValue::Unresolved {
                    source: "__(name)".to_string()
                }
            )
        );
        assert_eq!(
            kw[2],
            (
                sym("other"),
                SeedValue::Unresolved {
                    source: "x.__('y')".to_string()
                }
            )
        );
        assert_eq!(with_report.unresolved_values, 2);
        assert_eq!(
            only_kwargs(&without[0])[0],
            (
                sym("title"),
                SeedValue::Unresolved {
                    source: "__('Title')".to_string()
                }
            )
        );
        assert_eq!(without_report.unresolved_values, 3);
    }

    #[test]
    fn integer_boolean_and_nil_hash_keys_are_literal() {
        let body = "Setting.create_if_not_exists(name: 'x', options: { true => 'yes', false => 'no', 10 => 'ten', nil => '-', k => 1 })\n";
        let (rows, report) = scan("t8", body, &[spec("Setting", "create_if_not_exists")]);
        assert_eq!(
            only_kwargs(&rows[0])[1],
            (
                sym("options"),
                SeedValue::Hash(vec![
                    (SeedKey::Bool(true), string("yes")),
                    (SeedKey::Bool(false), string("no")),
                    (SeedKey::Int(10), string("ten")),
                    (SeedKey::Nil, string("-")),
                    (
                        SeedKey::Unresolved {
                            source: "k".to_string()
                        },
                        SeedValue::Int(1)
                    ),
                ])
            )
        );
        assert_eq!(report.unresolved_values, 1);
    }

    #[test]
    fn a_local_whose_value_is_partly_unresolved_is_not_bound() {
        let root = scratch_dir("t_partial_local");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("seed.rb"),
            "opts = { a: 1, b: compute }\nSetting.create_if_not_exists(x: opts)\nSetting.create_if_not_exists(y: opts)\n",
        )
        .unwrap();
        let options = SeedOptions {
            follow_locals: true,
            ..Default::default()
        };
        let (rows, report) = extract_seed_rows_with(
            &root,
            &["seed.rb"],
            &[spec("Setting", "create_if_not_exists")],
            &options,
        );
        let _ = fs::remove_dir_all(&root);
        assert_eq!(
            only_kwargs(&rows[0])[0],
            (
                sym("x"),
                SeedValue::Unresolved {
                    source: "opts".to_string()
                }
            )
        );
        assert_eq!(report.unresolved_values, 2);
    }

    #[test]
    fn a_local_assigned_in_a_loop_or_a_preceding_argument_is_not_substituted() {
        let root = scratch_dir("t_loop_local");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("seed.rb"),
            "r = 1\n2.times { Setting.create_if_not_exists(role: r); r = 2 }\nq = 1\nSetting.create_if_not_exists(q = 2, q)\n",
        )
        .unwrap();
        let options = SeedOptions {
            follow_locals: true,
            ..Default::default()
        };
        let (rows, _) = extract_seed_rows_with(
            &root,
            &["seed.rb"],
            &[spec("Setting", "create_if_not_exists")],
            &options,
        );
        let _ = fs::remove_dir_all(&root);
        assert_eq!(
            only_kwargs(&rows[0])[0],
            (
                sym("role"),
                SeedValue::Unresolved {
                    source: "r".to_string()
                }
            )
        );
        assert_eq!(
            rows[1].args[1],
            SeedValue::Unresolved {
                source: "q".to_string()
            }
        );
    }

    #[test]
    fn an_assignment_in_a_nested_method_does_not_unbind_the_outer_local() {
        let root = scratch_dir("t_def_scope");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("seed.rb"),
            "r = 1\nif cond\n  def helper\n    r = 2\n  end\nend\nSetting.create_if_not_exists(role: r)\n",
        )
        .unwrap();
        let options = SeedOptions {
            follow_locals: true,
            ..Default::default()
        };
        let (rows, _) = extract_seed_rows_with(
            &root,
            &["seed.rb"],
            &[spec("Setting", "create_if_not_exists")],
            &options,
        );
        let _ = fs::remove_dir_all(&root);
        assert_eq!(only_kwargs(&rows[0])[0], (sym("role"), SeedValue::Int(1)));
    }

    #[test]
    fn an_assignment_in_a_class_header_unbinds_the_enclosing_local() {
        let root = scratch_dir("t_class_header");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("seed.rb"),
            "r = 1\n2.times do\n  Setting.create_if_not_exists(role: r)\n  class Example < (r = 2; Object)\n  end\nend\n",
        )
        .unwrap();
        let options = SeedOptions {
            follow_locals: true,
            ..Default::default()
        };
        let (rows, _) = extract_seed_rows_with(
            &root,
            &["seed.rb"],
            &[spec("Setting", "create_if_not_exists")],
            &options,
        );
        let _ = fs::remove_dir_all(&root);
        assert_eq!(
            only_kwargs(&rows[0])[0],
            (
                sym("role"),
                SeedValue::Unresolved {
                    source: "r".to_string()
                }
            )
        );
    }

    #[test]
    fn a_wide_expression_of_large_reads_stops_substituting_at_the_budget() {
        let root = scratch_dir("t_wide_reads");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let mut body = String::from("x = [1]\n");
        for _ in 0..10 {
            body.push_str("x = [x, x]\n");
        }
        let reads = vec!["x"; 10_000].join(", ");
        body.push_str("Setting.create_if_not_exists(v: [");
        body.push_str(&reads);
        body.push_str("])\n");
        fs::write(root.join("seed.rb"), body).unwrap();
        let options = SeedOptions {
            follow_locals: true,
            ..Default::default()
        };
        let (rows, _) = extract_seed_rows_with(
            &root,
            &["seed.rb"],
            &[spec("Setting", "create_if_not_exists")],
            &options,
        );
        let _ = fs::remove_dir_all(&root);
        let SeedValue::Array(items) = &only_kwargs(&rows[0])[0].1 else {
            panic!("expected an array");
        };
        let substituted = items
            .iter()
            .filter(|v| !matches!(v, SeedValue::Unresolved { .. }))
            .count();
        // A bound `x` costs 3071 nodes; the budget admits five copies.
        assert_eq!(substituted, MAX_SUBSTITUTED_COST / 3071);
        assert_eq!(items.len(), 10_000);
    }

    #[test]
    fn finds_calls_in_blocks_and_conditionals() {
        let body = "[1, 2].each do |i|\n  Setting.create_if_not_exists(name: 'in_each')\nend\n\
                    if true\n  Setting.create_if_not_exists(name: 'in_if')\nend\n\
                    Setting.create_if_not_exists(name: 'top')\n";
        let (rows, report) = scan("t3", body, &[spec("Setting", "create_if_not_exists")]);
        assert_eq!(report.calls_found, 3);
        let lines: Vec<u32> = rows.iter().map(|r| r.line).collect();
        assert_eq!(lines, vec![2, 5, 7]);
    }

    #[test]
    fn lowers_word_arrays_heredocs_and_negative_integers() {
        let body = "Setting.create_if_not_exists(\n  words: %w[a b],\n  syms: %i[a b],\n  neg: -3,\n  ratio: 1.5,\n  none: nil,\n  adjacent: 'a' 'b',\n  text: <<~TXT,\n    hello\n    world\n  TXT\n  last: 1,\n)\n";
        let (rows, report) = scan("t4", body, &[spec("Setting", "create_if_not_exists")]);
        assert_eq!(report.unresolved_values, 0);
        let pairs = only_kwargs(&rows[0]);
        assert_eq!(pairs[0].1, SeedValue::Array(vec![string("a"), string("b")]));
        assert_eq!(
            pairs[1].1,
            SeedValue::Array(vec![
                SeedValue::Sym("a".to_string()),
                SeedValue::Sym("b".to_string())
            ])
        );
        assert_eq!(pairs[2].1, SeedValue::Int(-3));
        assert_eq!(pairs[3].1, SeedValue::Float("1.5".to_string()));
        assert_eq!(pairs[4].1, SeedValue::Nil);
        assert_eq!(pairs[5].1, string("ab"));
        assert_eq!(pairs[6].1, string("hello\nworld\n"));
        assert_eq!(pairs[7].1, SeedValue::Int(1));
    }

    #[test]
    fn leading_colons_match_and_non_constant_receivers_do_not() {
        let body = "::Setting.create_if_not_exists(name: 'a')\n\
                    setting.create_if_not_exists(name: 'b')\n\
                    Foo::Setting.create_if_not_exists(name: 'c')\n\
                    create_if_not_exists(name: 'd')\n";
        let (rows, report) = scan("t5", body, &[spec("Setting", "create_if_not_exists")]);
        assert_eq!(report.calls_found, 1);
        assert_eq!(rows[0].line, 1);
    }

    #[test]
    fn hash_rocket_keys_and_interpolation() {
        let body = "Setting.create_if_not_exists('k' => 1, :s => \"a#{x}\", y => 2)\n";
        let (rows, report) = scan("t6", body, &[spec("Setting", "create_if_not_exists")]);
        let pairs = only_kwargs(&rows[0]);
        assert_eq!(pairs[0], (SeedKey::Str("k".to_string()), SeedValue::Int(1)));
        assert_eq!(
            pairs[1],
            (
                sym("s"),
                SeedValue::Unresolved {
                    source: "\"a#{x}\"".to_string()
                }
            )
        );
        assert_eq!(
            pairs[2].0,
            SeedKey::Unresolved {
                source: "y".to_string()
            }
        );
        assert_eq!(report.unresolved_values, 2);
        assert_eq!(
            report.calls_found,
            report.rows_fully_literal + report.rows_with_unresolved
        );
    }

    const OVERVIEWS: &str = "overview_role = Role.find_by(name: 'Agent')\n\
Overview.create_if_not_exists(\n\
  name: __('My Assigned Tickets'), link: 'my_assigned', prio: 1000,\n\
  role_ids:  [overview_role.id],\n\
  condition: { 'ticket.state_id' => { operator: 'is', value: Ticket::State.by_category_ids(:open) },\n\
               'ticket.owner_id' => { operator: 'is', pre_condition: 'current_user.id' } },\n\
)\n\
overview_role = Role.find_by(name: 'Customer')\n\
Overview.create_if_not_exists(\n\
  name: __('My Tickets'), link: 'my_tickets', prio: 1100,\n\
  role_ids:  [overview_role.id],\n\
  condition: { 'ticket.state_id' => { operator: 'is', value: Ticket::State.by_category_ids(:open) } },\n\
)\n";

    fn scan_with(
        case: &str,
        body: &str,
        calls: &[SeedCallSpec],
        options: &SeedOptions,
    ) -> (Vec<SeedRow>, SeedScanReport) {
        let root = scratch_dir(case);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("seed.rb"), body).unwrap();
        let result = extract_seed_rows_with(&root, &["seed.rb"], calls, options);
        let _ = fs::remove_dir_all(&root);
        result
    }

    fn following() -> SeedOptions {
        SeedOptions {
            translation_markers: vec!["__".to_string()],
            follow_locals: true,
            ..Default::default()
        }
    }

    fn find_role(name: &str) -> SeedValue {
        SeedValue::Call {
            receiver: "Role".to_string(),
            method: "find_by".to_string(),
            args: vec![SeedValue::Hash(vec![(sym("name"), string(name))])],
        }
    }

    fn role_id(name: &str) -> SeedValue {
        SeedValue::Chain {
            base: Box::new(find_role(name)),
            method: "id".to_string(),
            args: Vec::new(),
        }
    }

    fn field<'r>(row: &'r SeedRow, key: &str) -> &'r SeedValue {
        &only_kwargs(row)
            .iter()
            .find(|(k, _)| *k == sym(key))
            .unwrap()
            .1
    }

    #[test]
    fn followed_locals_resolve_named_calls_and_rebind() {
        let calls = [spec("Overview", "create_if_not_exists")];
        let (rows, report) = scan_with("t9", OVERVIEWS, &calls, &following());
        assert_eq!(rows.len(), 2);
        assert_eq!(
            field(&rows[0], "role_ids"),
            &SeedValue::Array(vec![role_id("Agent")])
        );
        assert_eq!(
            field(&rows[1], "role_ids"),
            &SeedValue::Array(vec![role_id("Customer")])
        );
        let SeedValue::Hash(condition) = field(&rows[0], "condition") else {
            panic!("expected a hash");
        };
        let SeedValue::Hash(state) = &condition[0].1 else {
            panic!("expected a hash");
        };
        assert_eq!(
            state[1],
            (
                sym("value"),
                SeedValue::Call {
                    receiver: "Ticket::State".to_string(),
                    method: "by_category_ids".to_string(),
                    args: vec![SeedValue::Sym("open".to_string())],
                }
            )
        );
        assert_eq!(report.unresolved_values, 0);
        assert_eq!(report.rows_fully_literal, 2);
    }

    #[test]
    fn without_follow_locals_the_local_read_stays_unresolved() {
        let calls = [spec("Overview", "create_if_not_exists")];
        let options = SeedOptions {
            follow_locals: false,
            ..following()
        };
        let (rows, report) = scan_with("t10", OVERVIEWS, &calls, &options);
        let unresolved = SeedValue::Unresolved {
            source: "overview_role.id".to_string(),
        };
        assert_eq!(
            field(&rows[0], "role_ids"),
            &SeedValue::Array(vec![unresolved.clone()])
        );
        assert_eq!(
            field(&rows[1], "role_ids"),
            &SeedValue::Array(vec![unresolved])
        );
        assert_eq!(report.unresolved_values, 2);
    }

    #[test]
    fn only_straight_line_assignments_bind() {
        let calls = [spec("Overview", "create_if_not_exists")];
        let unresolved = |source: &str| SeedValue::Unresolved {
            source: source.to_string(),
        };
        let cases = [
            ("if_branch", "if cond\n  r = Role.find_by(name: 'A')\nend\n"),
            ("modifier", "r = Role.find_by(name: 'A') if cond\n"),
            ("or_assign", "r ||= Role.find_by(name: 'A')\n"),
            ("block", "[1].each { r = Role.find_by(name: 'A') }\n"),
        ];
        for (case, assign) in cases {
            let body = format!("{assign}Overview.create_if_not_exists(role: r)\n");
            let (rows, _) = scan_with(case, &body, &calls, &following());
            assert_eq!(field(&rows[0], "role"), &unresolved("r"), "{case}");
        }
        let body = "r = Role.find_by(name: 'A')\nif cond\n  r = 1\nend\nOverview.create_if_not_exists(role: r)\n";
        let (rows, _) = scan_with("rebound_in_if", body, &calls, &following());
        assert_eq!(field(&rows[0], "role"), &unresolved("r"));
        let body = "r = Role.find_by(name: 'A')\n[1].each do |r|\n  Overview.create_if_not_exists(role: r)\nend\n";
        let (rows, _) = scan_with("shadowed", body, &calls, &following());
        assert_eq!(field(&rows[0], "role"), &unresolved("r"));
        let body =
            "r = Role.find_by(name: 'A')\ndef x\n  Overview.create_if_not_exists(role: r)\nend\n";
        let (rows, _) = scan_with("def_scope", body, &calls, &following());
        assert_eq!(field(&rows[0], "role"), &unresolved("r"));
        let body = "r = Role.find_by(name: 'A')\nOverview.create_if_not_exists(role: r)\n";
        let (rows, _) = scan_with("straight", body, &calls, &following());
        assert_eq!(field(&rows[0], "role"), &find_role("A"));
    }

    #[test]
    fn a_local_bound_to_an_unresolved_value_unbinds() {
        let calls = [spec("Overview", "create_if_not_exists")];
        let body =
            "r = Role.find_by(name: 'A')\nr = compute\nOverview.create_if_not_exists(role: r)\n";
        let (rows, _) = scan_with("t12", body, &calls, &following());
        assert_eq!(
            field(&rows[0], "role"),
            &SeedValue::Unresolved {
                source: "r".to_string()
            }
        );
    }

    #[test]
    fn a_call_with_an_unbound_local_argument_stays_unresolved() {
        let calls = [spec("Overview", "create_if_not_exists")];
        let body = "Overview.create_if_not_exists(a: Foo.bar(baz), b: Foo.bar(1), c: Foo + 1, d: Foo.bar(1).baz = 2)\n";
        let (rows, _) = scan_with("t13", body, &calls, &following());
        assert_eq!(
            field(&rows[0], "a"),
            &SeedValue::Unresolved {
                source: "Foo.bar(baz)".to_string()
            }
        );
        assert_eq!(
            field(&rows[0], "b"),
            &SeedValue::Call {
                receiver: "Foo".to_string(),
                method: "bar".to_string(),
                args: vec![SeedValue::Int(1)],
            }
        );
        assert!(matches!(field(&rows[0], "c"), SeedValue::Unresolved { .. }));
        assert!(matches!(field(&rows[0], "d"), SeedValue::Unresolved { .. }));
    }

    #[test]
    fn a_call_chain_keeps_its_structure() {
        let calls = [spec("Overview", "create_if_not_exists")];
        let body =
            "Overview.create_if_not_exists(ids: Ticket::State.by_category(:open).pluck(:id))\n";
        let (rows, report) = scan_with("t14", body, &calls, &following());
        assert_eq!(report.unresolved_values, 0);
        assert_eq!(
            field(&rows[0], "ids"),
            &SeedValue::Chain {
                base: Box::new(SeedValue::Call {
                    receiver: "Ticket::State".to_string(),
                    method: "by_category".to_string(),
                    args: vec![SeedValue::Sym("open".to_string())],
                }),
                method: "pluck".to_string(),
                args: vec![SeedValue::Sym("id".to_string())],
            }
        );
    }

    fn role_after(case: &str, body: &str) -> SeedValue {
        let calls = [spec("Overview", "create_if_not_exists")];
        let (rows, _) = scan_with(case, body, &calls, &following());
        field(rows.last().unwrap(), "role").clone()
    }

    fn unresolved(source: &str) -> SeedValue {
        SeedValue::Unresolved {
            source: source.to_string(),
        }
    }

    #[test]
    fn pattern_captures_unbind_the_local() {
        let read = "Overview.create_if_not_exists(role: r)\n";
        let cases = [
            ("hash_pattern", "case {r: 2}\nin {r:}\n  READ\nend\n"),
            ("after_case", "case {r: 2}\nin {r:}\nend\nREAD"),
            ("rightward", "{r: 2} => {r:}\nREAD"),
            ("bind_as", "case [2]\nin [Integer => r]\nend\nREAD"),
            ("rest", "case [2]\nin [*r]\nend\nREAD"),
            ("top_level_var", "case 2\nin r\nend\nREAD"),
        ];
        for (case, tail) in cases {
            let body = format!("r = 1\n{}", tail.replace("READ", read));
            assert_eq!(role_after(case, &body), unresolved("r"), "{case}");
        }
        let pinned = "r = 1\ncase 1\nin ^r\n  READ\nend\n".replace("READ", read);
        assert_eq!(role_after("pinned", &pinned), SeedValue::Int(1));
    }

    #[test]
    fn bound_values_are_size_bounded() {
        let mut body = String::from("x = [1]\n");
        for _ in 0..40 {
            body.push_str("x = [x, x]\n");
        }
        body.push_str("Overview.create_if_not_exists(role: x)\n");
        assert_eq!(role_after("doublings", &body), unresolved("x"));
        let small = "x = [1]\nx = [x, x]\nOverview.create_if_not_exists(role: x)\n";
        let one = SeedValue::Array(vec![SeedValue::Int(1)]);
        assert_eq!(
            role_after("small", small),
            SeedValue::Array(vec![one.clone(), one])
        );
    }

    #[test]
    fn an_assignment_in_a_paren_group_inside_call_arguments_unbinds() {
        let read = "Overview.create_if_not_exists(role: r)\n";
        for (case, stmt) in [
            ("arg", "consume((r = 2; nil))\n"),
            ("receiver", "(r = 2; nil).consume\n"),
        ] {
            let body = format!("r = 1\n{stmt}{read}");
            assert_eq!(role_after(case, &body), unresolved("r"), "{case}");
        }
        let standalone = format!("r = 1\n(r = 2; nil)\n{read}");
        assert_eq!(role_after("standalone", &standalone), SeedValue::Int(2));
    }

    #[test]
    fn a_hash_key_read_from_a_followed_local_is_a_seed_key() {
        let calls = [spec("Overview", "create_if_not_exists")];
        let body = "k = :role\nn = 3\nf = 1.5\nu = compute\nOverview.create_if_not_exists({k => 1, n => 2, f => 3, u => 4, z => 5})\n";
        let (rows, _) = scan_with("key_local", body, &calls, &following());
        let SeedValue::Hash(pairs) = &rows[0].args[0] else {
            panic!("expected a hash");
        };
        let keys: Vec<&SeedKey> = pairs.iter().map(|(k, _)| k).collect();
        let key_unresolved = |source: &str| SeedKey::Unresolved {
            source: source.to_string(),
        };
        assert_eq!(
            keys,
            vec![
                &sym("role"),
                &SeedKey::Int(3),
                &key_unresolved("f"),
                &key_unresolved("u"),
                &key_unresolved("z"),
            ]
        );
    }

    /// Run with `ZAMMAD_SRC=/path/to/zammad cargo test -p ruff_ruby_spo
    /// seeds::tests::zammad_seed_measurement -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs ZAMMAD_SRC"]
    #[expect(
        clippy::print_stdout,
        reason = "measurement output is the purpose of this test"
    )]
    fn zammad_seed_measurement() {
        let Ok(src) = std::env::var("ZAMMAD_SRC") else {
            return;
        };
        let root = Path::new(&src);
        let targets = [
            (
                "ObjectManager::Attribute",
                "add",
                "db/seeds/object_manager_attributes.rb",
            ),
            ("Overview", "create_if_not_exists", "db/seeds/overviews.rb"),
            (
                "CoreWorkflow",
                "create_if_not_exists",
                "db/seeds/core_workflow.rb",
            ),
            ("Setting", "create_if_not_exists", "db/seeds/settings.rb"),
        ];
        let mut shapes: std::collections::BTreeMap<String, (usize, String)> =
            std::collections::BTreeMap::new();
        for (receiver, method, file) in targets {
            let options = SeedOptions {
                translation_markers: vec!["__".to_string()],
                follow_locals: true,
                ..Default::default()
            };
            let (rows, report) =
                extract_seed_rows_with(root, &[file], &[spec(receiver, method)], &options);
            println!("{receiver}.{method} {file}: {report:?}");
            assert_eq!(rows.len(), report.calls_found);
            for row in &rows {
                for arg in &row.args {
                    collect_shapes(arg, &row.file, row.line, &mut shapes);
                }
            }
        }
        let mut ranked: Vec<_> = shapes.into_iter().collect();
        ranked.sort_by(|a, b| b.1.0.cmp(&a.1.0).then_with(|| a.0.cmp(&b.0)));
        for (shape, (count, example)) in ranked.iter().take(12) {
            println!("{count:5}  {shape}  e.g. {example}");
        }
    }

    fn collect_shapes(
        value: &SeedValue,
        file: &str,
        line: u32,
        out: &mut std::collections::BTreeMap<String, (usize, String)>,
    ) {
        match value {
            SeedValue::Unresolved { source } => {
                let shape: String = source
                    .chars()
                    .take_while(|c| !matches!(c, '(' | ' ' | '\n'))
                    .collect();
                let example: String = source.chars().take(60).collect();
                let entry = out
                    .entry(shape)
                    .or_insert((0, format!("{file}:{line} `{example}`")));
                entry.0 += 1;
            }
            SeedValue::Array(items) => {
                for item in items {
                    collect_shapes(item, file, line, out);
                }
            }
            SeedValue::Hash(pairs) => {
                for (_, v) in pairs {
                    collect_shapes(v, file, line, out);
                }
            }
            _ => {}
        }
    }
}
