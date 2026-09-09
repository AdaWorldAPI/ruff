//! Ordered behavioral ore for the Ruby/Rails frontend — the sequence and the
//! structure that the shipped set arms throw away.
//!
//! # Why this exists
//!
//! `crate::functions`'s body walker ends by collapsing its traversal into
//! sets: `extract_functions_from_body` runs `dedup_in_place` over all six
//! vectors of every [`ruff_spo_triplet::Function`], unconditionally. The
//! walker visits statements in source order and then sorts that order away.
//!
//! So the shipped representation cannot express *a write inside a loop*, *two
//! writes in a row*, or *a call in the else-branch*. It cannot say that a
//! method read a field five times. Those are exactly the shapes a behavioral
//! vocabulary would be made of.
//!
//! The class-body side loses a different thing. `crate::walk::walk_class_body`
//! pushes one [`crate::Declaration`] per macro in source order, and that order
//! is real — Rails executes callbacks in declaration order — but it is an
//! accident of a `Vec`, never an explicit fact, and it does not survive
//! `ruff_spo_triplet::expand`, which emits a flat unordered triple set and
//! drops `Callback::options` (the `if:` / `unless:` / `on:` conditions) on the
//! floor.
//!
//! This module is a SECOND walk that preserves three things separately, in
//! **two arms**:
//!
//! | arm | walks | preserves |
//! |---|---|---|
//! | Ruby ([`MethodOre`]) | `def … end` bodies | language primitives in order, inside a scope tree |
//! | Rails ([`ClassOre::declarations`]) | the class body | the framework declaration stream in order, WITH its options |
//!
//! | preserved | where | deduped |
//! |---|---|---|
//! | identity | [`Symbols`] | yes — the only place |
//! | order | [`MethodOre::events`], [`ClassOre::declarations`] | **never** |
//! | structure | [`MethodOre::scopes`] | n/a |
//!
//! The same fact occurring twice is two events pointing at one symbol.
//!
//! # Two arms, because two vocabularies
//!
//! The Ruby arm's alphabet is the LANGUAGE: a send, an assignment, a
//! condition, a branch, a rescue. The Rails arm's alphabet is the FRAMEWORK:
//! a callback, a validation, a scope, an association. Keeping them apart is
//! what lets a consumer subtract one from the other — framework convention is
//! deterministically collapsible, language behaviour is what remains. Fusing
//! them would make that subtraction impossible to express.
//!
//! # Additive by construction
//!
//! Nothing here changes `extract_functions_from_body`, `walk_class_body`,
//! `Function`, `Declaration`, `ModelGraph` or anything `expand` emits.
//! [`MethodOre`] carries the shipped six-set counts beside the events so a
//! consumer can compare the two views without a second parse — and, more
//! importantly, without a second WALK, which would make any difference
//! between them ambiguous. `events_collapse_to_the_shipped_six_sets` asserts
//! that folding the events back into sets reproduces the shipped arm exactly:
//! the change is about what is PRESERVED, not about what a fact IS.
//!
//! # Neutrality — the ore must not pre-solve its consumer's experiment
//!
//! There is deliberately no `GuardedWrite`, `Traversal` or `Mutator` event
//! kind. A blank-guarded default write is a [`EventKind::Condition`] carrying
//! the predicate's own name, a [`EventKind::Branch`] carrying which arm, and a
//! [`EventKind::Write`] — three primitives whose co-occurrence a consumer may
//! learn to call a guard. An association walk is a [`EventKind::Read`] whose
//! symbol happens to be interned as [`SymKind::Relation`]. Identities are
//! carried verbatim, never hashed; different consumers hash differently.
//!
//! # What lib-ruby-parser does not give
//!
//! There is no CFG. `control` therefore carries only structurally-certain
//! relations: which arm of a conditional a branch is, which construct a
//! condition belongs to, and a loop's back edge. Successor edges are not
//! available and are never fabricated. Every event carries its source byte
//! `anchor`, so lexical order stays checkable against traversal order rather
//! than being silently conflated with it.

use std::collections::BTreeMap;
use std::fmt;

use lib_ruby_parser::{Loc, Node};

use crate::Declaration;

// ─────────────────────────────────────────────────────────────────────────
// Alphabets
// ─────────────────────────────────────────────────────────────────────────

/// The Ruby arm's event alphabet — language primitives only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EventKind {
    /// A scope opened. `subj` is the scope-kind symbol.
    ScopeEnter,
    /// A scope closed. Re-uses its construct's start anchor.
    ScopeExit,
    /// A method parameter, in declaration order.
    Param,
    /// A local-variable binding (`x = …`).
    Decl,
    /// An instance-variable write (`@x = …`) — NOT an attribute write.
    IvarWrite,
    /// An instance-variable read (`@x`).
    IvarRead,
    /// An attribute read (`self.x` / bare `x`).
    Read,
    /// An attribute write (`self.x = …`).
    Write,
    /// A read-modify-write (`self.x += …`, `self.x ||= …`).
    ReadWrite,
    /// A conditional's test expression. `obj` is the predicate callee when the
    /// test is a send, so `blank?` / `present?` survive as identity.
    Condition,
    /// Entry into one arm of a conditional. `control` is `then` / `else` /
    /// `when` / `rescue` / `ensure`.
    Branch,
    /// `return`.
    Return,
    /// `break`.
    Break,
    /// `next`.
    Next,
    /// `redo`.
    Redo,
    /// `retry`.
    Retry,
    /// `yield`.
    Yield,
    /// A method send. `subj` is the receiver symbol when there is one, `obj`
    /// the callee.
    Call,
    /// `raise X` / `raise X.new(…)`. `obj` is the exception type symbol.
    Raise,
    /// `super` / `super(…)`.
    Super,
    /// A constant reference.
    Const,
}

impl EventKind {
    /// The stable wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ScopeEnter => "ScopeEnter",
            Self::ScopeExit => "ScopeExit",
            Self::Param => "Param",
            Self::Decl => "Decl",
            Self::IvarWrite => "IvarWrite",
            Self::IvarRead => "IvarRead",
            Self::Read => "Read",
            Self::Write => "Write",
            Self::ReadWrite => "ReadWrite",
            Self::Condition => "Condition",
            Self::Branch => "Branch",
            Self::Return => "Return",
            Self::Break => "Break",
            Self::Next => "Next",
            Self::Redo => "Redo",
            Self::Retry => "Retry",
            Self::Yield => "Yield",
            Self::Call => "Call",
            Self::Raise => "Raise",
            Self::Super => "Super",
            Self::Const => "Const",
        }
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The Rails arm's event alphabet — one variant per [`Declaration`] family.
/// This is the framework layer, deliberately disjoint from [`EventKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RailsKind {
    Association,
    Validation,
    Callback,
    Concern,
    Attribute,
    Delegation,
    Scope,
    ActsAs,
    DslCall,
    GemDsl,
    DynamicMethod,
    Using,
    /// Single-table inheritance — `inheritance_column`, `abstract_class`.
    Sti,
}

impl RailsKind {
    /// The stable wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Association => "Association",
            Self::Validation => "Validation",
            Self::Callback => "Callback",
            Self::Concern => "Concern",
            Self::Attribute => "Attribute",
            Self::Delegation => "Delegation",
            Self::Scope => "Scope",
            Self::ActsAs => "ActsAs",
            Self::DslCall => "DslCall",
            Self::GemDsl => "GemDsl",
            Self::DynamicMethod => "DynamicMethod",
            Self::Using => "Using",
            Self::Sti => "Sti",
        }
    }
}

impl fmt::Display for RailsKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One scope in a method's tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    Method,
    Block,
    If,
    Then,
    Else,
    Case,
    When,
    While,
    Until,
    For,
    Rescue,
    Ensure,
    Lambda,
}

impl ScopeKind {
    /// The stable wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Method => "Method",
            Self::Block => "Block",
            Self::If => "If",
            Self::Then => "Then",
            Self::Else => "Else",
            Self::Case => "Case",
            Self::When => "When",
            Self::While => "While",
            Self::Until => "Until",
            Self::For => "For",
            Self::Rescue => "Rescue",
            Self::Ensure => "Ensure",
            Self::Lambda => "Lambda",
        }
    }

    /// Loops carry a back edge; the exit event records it.
    fn is_loop(self) -> bool {
        matches!(self, Self::While | Self::Until | Self::For)
    }
}

/// What a symbol names. Identity only — never a behavioural claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SymKind {
    /// An `ActiveRecord` attribute reached through `self` or implicit self.
    Attr,
    /// A declared association name — the same string an `Attr` might carry,
    /// interned separately because the class declared it as a relation.
    Relation,
    /// A method parameter.
    Param,
    /// A local variable.
    Local,
    /// An instance variable.
    Ivar,
    /// A method being called.
    Callee,
    /// A receiver that is not `self`.
    Receiver,
    /// A constant.
    Const,
    /// An exception type.
    Exception,
    /// A scope kind, so a `ScopeEnter` has a subject like any other event.
    Scope,
    Other,
}

impl SymKind {
    /// The stable wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Attr => "attr",
            Self::Relation => "relation",
            Self::Param => "param",
            Self::Local => "local",
            Self::Ivar => "ivar",
            Self::Callee => "callee",
            Self::Receiver => "receiver",
            Self::Const => "const",
            Self::Exception => "exception",
            Self::Scope => "scope_kind",
            Self::Other => "other",
        }
    }
}

/// Where a name resolves. `Own` is the one the six-set fold keys on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    /// Reached through `self` or implicit self — an attribute of this model.
    Own,
    Param,
    Local,
    Ivar,
    /// An explicit non-self receiver.
    Ext,
    None,
}

impl Role {
    /// The stable wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Own => "own",
            Self::Param => "param",
            Self::Local => "local",
            Self::Ivar => "ivar",
            Self::Ext => "ext",
            Self::None => "-",
        }
    }
}

/// How a fact was obtained. `Parser` is a node the AST gave directly; `Walk`
/// is a relation this walker imposed (a scope boundary, a branch arm).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prov {
    Parser,
    Walk,
}

impl Prov {
    /// The stable wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Parser => "parser",
            Self::Walk => "walk",
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Records
// ─────────────────────────────────────────────────────────────────────────

/// One event in a method body, at its position in the stream.
#[derive(Debug, Clone)]
pub struct OreEvent {
    /// Position in this method's stream. Never sorted.
    pub seq: u32,
    pub kind: EventKind,
    /// The acting name, interned.
    pub subj: Option<String>,
    /// The acted-on name, interned.
    pub obj: Option<String>,
    /// The scope this event occurred in.
    pub scope: u32,
    /// That scope's parent, carried so a consumer need not walk the tree.
    pub parent: Option<u32>,
    /// Source byte offset, so lexical order stays checkable against
    /// traversal order rather than silently conflated with it.
    pub anchor: u32,
    /// A structurally-certain relation only: which arm, which construct, a
    /// loop's back edge. Never a successor edge.
    pub control: Option<String>,
    pub prov: Prov,
}

/// One scope in a method's tree.
#[derive(Debug, Clone)]
pub struct OreScope {
    pub id: u32,
    pub parent: Option<u32>,
    pub kind: ScopeKind,
    pub depth: u16,
    pub enter_seq: u32,
    pub exit_seq: u32,
}

/// One declaration in a class body, at its position in the stream.
///
/// This is the Rails arm. The `options` are the reason it exists: a callback's
/// `if:` / `unless:` / `on:` is captured by `crate::walk` and then dropped by
/// `ruff_spo_triplet::expand`, which emits only `phase:target`.
#[derive(Debug, Clone)]
pub struct RailsEvent {
    /// Position in the class body. Rails executes callbacks in this order,
    /// and this ordinal is the whole reason this record exists.
    pub seq: u32,
    /// Which family, for grouping without inspecting the payload.
    pub kind: RailsKind,
    /// The declaration itself, carried verbatim.
    ///
    /// It is NOT re-spelled into strings here. The typed data already exists;
    /// a second projection of it would be a mirror that can drift, and the
    /// authoritative wire spelling (`AssocKind::BelongsTo` → `"belongs_to"`)
    /// already lives at `ruff_spo_triplet::expand`'s `association()`. That it
    /// is private there is a real gap — the enums own no `as_str()` — but the
    /// fix belongs upstream on the enum, read by both, not copied into a
    /// second match here. Everything this arm adds is `seq` and `anchor`;
    /// the options a consumer wants (`if:` / `unless:` / `on:`) are already
    /// on the declaration, and are simply not dropped.
    pub decl: Declaration,
    /// Source byte offset of the macro call.
    pub anchor: u32,
}

/// One method's ordered ore, plus the shipped arm's view of the same body.
#[derive(Debug, Clone)]
pub struct MethodOre {
    /// `<class>.<method>` — the join key.
    pub iri: String,
    pub name: String,
    /// Ruby visibility at the `def` site.
    pub public: bool,
    /// The event stream, in source order. Never sorted.
    pub events: Vec<OreEvent>,
    /// The scope tree.
    pub scopes: Vec<OreScope>,
    /// The SHIPPED six-set arm's view of this same body, carried so a
    /// consumer can compare the two views without a second parse.
    pub set_reads: u32,
    pub set_writes: u32,
    pub set_guarded: u32,
    pub set_raises: u32,
    pub set_traverses: u32,
    pub set_calls: u32,
}

/// One class's ore — both arms.
#[derive(Debug, Clone)]
pub struct ClassOre {
    /// `<namespace>:<Class>`.
    pub iri: String,
    pub name: String,
    /// The Rails arm: the class-body declaration stream, in order.
    pub declarations: Vec<RailsEvent>,
    /// The Ruby arm: one entry per `def`, in declaration order.
    pub methods: Vec<MethodOre>,
}

/// The canonical identity table — the ONLY place dedup happens.
#[derive(Debug, Default)]
pub struct Symbols {
    index: BTreeMap<(SymKind, Role, String), u32>,
    rows: Vec<(SymKind, Role, String, Prov)>,
}

impl Symbols {
    /// Intern a name, returning its stable `s<N>` id.
    pub fn intern(&mut self, kind: SymKind, role: Role, name: &str, prov: Prov) -> String {
        let key = (kind, role, name.to_string());
        if let Some(id) = self.index.get(&key) {
            return format!("s{id}");
        }
        let id = u32::try_from(self.rows.len()).unwrap_or(u32::MAX);
        self.index.insert(key, id);
        self.rows.push((kind, role, name.to_string(), prov));
        format!("s{id}")
    }

    /// Every interned row, in insertion order.
    pub fn rows(&self) -> impl Iterator<Item = (String, SymKind, Role, &str, Prov)> {
        self.rows
            .iter()
            .enumerate()
            .map(|(i, (k, r, n, p))| (format!("s{i}"), *k, *r, n.as_str(), *p))
    }

    /// How many distinct names have been interned.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether nothing has been interned yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The Ruby arm — ordered walk of a method body
// ─────────────────────────────────────────────────────────────────────────

/// Byte offset of a node's start, or 0 where the parser gives no extent.
fn anchor(node: &Node) -> u32 {
    u32::try_from(node_expression_loc(node).begin).unwrap_or(0)
}

/// `expression_l` for the node shapes this walker records. A node whose
/// extent we do not read yields a zero range rather than a wrong one.
fn node_expression_loc(node: &Node) -> Loc {
    match node {
        Node::Send(n) => n.expression_l,
        Node::Block(n) => n.expression_l,
        Node::Begin(n) => n.expression_l,
        Node::KwBegin(n) => n.expression_l,
        Node::Def(n) => n.expression_l,
        Node::If(n) => n.expression_l,
        Node::IfMod(n) => n.expression_l,
        Node::Case(n) => n.expression_l,
        Node::When(n) => n.expression_l,
        Node::While(n) => n.expression_l,
        Node::Until(n) => n.expression_l,
        Node::For(n) => n.expression_l,
        Node::Rescue(n) => n.expression_l,
        Node::RescueBody(n) => n.expression_l,
        Node::Ensure(n) => n.expression_l,
        Node::Return(n) => n.expression_l,
        Node::Break(n) => n.expression_l,
        Node::Next(n) => n.expression_l,
        Node::Redo(n) => n.expression_l,
        Node::Retry(n) => n.expression_l,
        Node::Yield(n) => n.expression_l,
        Node::Super(n) => n.expression_l,
        Node::ZSuper(n) => n.expression_l,
        Node::Lvasgn(n) => n.expression_l,
        Node::Ivasgn(n) => n.expression_l,
        Node::Lvar(n) => n.expression_l,
        Node::Ivar(n) => n.expression_l,
        Node::Const(n) => n.expression_l,
        Node::OrAsgn(n) => n.expression_l,
        Node::AndAsgn(n) => n.expression_l,
        Node::OpAsgn(n) => n.expression_l,
        Node::Lambda(n) => n.expression_l,
        _ => Loc { begin: 0, end: 0 },
    }
}

/// The ordered walk. One instance per method body; `syms` is shared across
/// the whole file so an identity interned in one method is the same id in the
/// next.
struct Walk<'s> {
    syms: &'s mut Symbols,
    known: &'s [String],
    events: Vec<OreEvent>,
    scopes: Vec<OreScope>,
    stack: Vec<u32>,
}

impl<'s> Walk<'s> {
    fn new(syms: &'s mut Symbols, known: &'s [String]) -> Self {
        Self {
            syms,
            known,
            events: Vec::new(),
            scopes: Vec::new(),
            stack: Vec::new(),
        }
    }

    fn cur(&self) -> u32 {
        self.stack.last().copied().unwrap_or(0)
    }

    fn parent(&self) -> Option<u32> {
        let n = self.stack.len();
        (n >= 2).then(|| self.stack[n - 2])
    }

    fn next_seq(&self) -> u32 {
        u32::try_from(self.events.len()).unwrap_or(u32::MAX)
    }

    fn push(
        &mut self,
        kind: EventKind,
        subj: Option<String>,
        obj: Option<String>,
        anchor: u32,
        control: Option<&str>,
        prov: Prov,
    ) {
        let seq = self.next_seq();
        let scope = self.cur();
        let parent = self.parent();
        self.events.push(OreEvent {
            seq,
            kind,
            subj,
            obj,
            scope,
            parent,
            anchor,
            control: control.map(ToString::to_string),
            prov,
        });
    }

    /// Open a scope, emitting its `ScopeEnter`.
    fn enter(&mut self, kind: ScopeKind, at: u32) -> u32 {
        let id = u32::try_from(self.scopes.len()).unwrap_or(u32::MAX);
        let parent = self.stack.last().copied();
        let depth = u16::try_from(self.stack.len()).unwrap_or(u16::MAX);
        let enter_seq = self.next_seq();
        self.scopes.push(OreScope {
            id,
            parent,
            kind,
            depth,
            enter_seq,
            exit_seq: enter_seq,
        });
        self.stack.push(id);
        let sym = self
            .syms
            .intern(SymKind::Scope, Role::None, kind.as_str(), Prov::Walk);
        self.push(EventKind::ScopeEnter, Some(sym), None, at, None, Prov::Walk);
        id
    }

    /// Close the innermost scope, emitting its `ScopeExit`. A loop's exit
    /// carries the back edge — the one control relation a tree walk can state
    /// with certainty.
    fn exit(&mut self, at: u32) {
        let Some(id) = self.stack.pop() else { return };
        let kind = self.scopes[id as usize].kind;
        let seq = self.next_seq();
        self.scopes[id as usize].exit_seq = seq;
        let sym = self
            .syms
            .intern(SymKind::Scope, Role::None, kind.as_str(), Prov::Walk);
        let control = kind.is_loop().then_some("back_edge");
        // The exit belongs to the scope being closed, so record it before the
        // stack pop takes effect for `cur()`.
        let parent = self.stack.last().copied();
        self.events.push(OreEvent {
            seq,
            kind: EventKind::ScopeExit,
            subj: Some(sym),
            obj: None,
            scope: id,
            parent,
            anchor: at,
            control: control.map(ToString::to_string),
            prov: Prov::Walk,
        });
    }

    fn attr(&mut self, name: &str) -> String {
        let kind = if self.known.iter().any(|r| r == name) {
            SymKind::Relation
        } else {
            SymKind::Attr
        };
        self.syms.intern(kind, Role::Own, name, Prov::Parser)
    }

    fn callee(&mut self, name: &str) -> String {
        self.syms
            .intern(SymKind::Callee, Role::None, name, Prov::Parser)
    }
}

impl Walk<'_> {
    /// Walk one body node, preserving order and structure. Mirrors the shape
    /// recognition of `crate::functions`'s set walker exactly — the same
    /// node patterns produce the same facts — and differs only in that every
    /// occurrence is kept, in place, inside the scope it occurred in.
    #[expect(
        clippy::too_many_lines,
        reason = "one match arm per recognised Ruby node shape; splitting it \
                  would hide the alphabet this module exists to define"
    )]
    fn body(&mut self, node: &Node) {
        let at = anchor(node);
        match node {
            Node::Begin(b) => {
                for stmt in &b.statements {
                    self.body(stmt);
                }
            }
            Node::KwBegin(b) => {
                for stmt in &b.statements {
                    self.body(stmt);
                }
            }
            // `raise X` / `raise X.new(…)`.
            Node::Send(s) if s.method_name == "raise" && s.recv.is_none() => {
                let exc = s
                    .args
                    .first()
                    .and_then(crate::functions::exception_type_name)
                    .map(|name| {
                        self.syms
                            .intern(SymKind::Exception, Role::None, &name, Prov::Parser)
                    });
                self.push(EventKind::Raise, None, exc, at, None, Prov::Parser);
                for arg in &s.args {
                    self.body(arg);
                }
            }
            // `self.<x>` — write, mutator call, or read, in that priority
            // order, matching the set walker.
            Node::Send(s) if matches!(s.recv.as_deref(), Some(Node::Self_(_))) => {
                self.self_send(s, at);
            }
            // `self.x ||= v` — a write, and the J1 blank-guard idiom.
            Node::OrAsgn(o) => {
                self.op_assign(&o.recv, at);
                self.body(&o.value);
            }
            // `self.x += v` — a read-modify-write.
            Node::OpAsgn(o) => {
                if let Some(field) = crate::functions::attr_of_self(&o.recv) {
                    let field = field.to_string();
                    let sym = self.attr(&field);
                    self.push(
                        EventKind::ReadWrite,
                        Some(sym),
                        None,
                        at,
                        None,
                        Prov::Parser,
                    );
                }
                self.body(&o.value);
            }
            // `self.x &&= v` — a present-guarded write.
            Node::AndAsgn(a) => {
                self.op_assign(&a.recv, at);
                self.body(&a.value);
            }
            // A general send: relation walk, mutator dispatch, or neither.
            Node::Send(s) => {
                self.general_send(s, at);
            }
            Node::For(f) => {
                if let Some(rel) = crate::functions::node_relation_name(&f.iteratee, self.known) {
                    let sym = self.attr(&rel);
                    self.push(
                        EventKind::Read,
                        Some(sym),
                        None,
                        at,
                        Some("iteratee"),
                        Prov::Parser,
                    );
                }
                self.enter(ScopeKind::For, at);
                self.body(&f.iteratee);
                if let Some(b) = f.body.as_deref() {
                    self.body(b);
                }
                self.exit(at);
            }
            Node::While(w) => {
                self.loop_with_cond(ScopeKind::While, &w.cond, w.body.as_deref(), at);
            }
            Node::Until(u) => {
                self.loop_with_cond(ScopeKind::Until, &u.cond, u.body.as_deref(), at);
            }
            Node::Block(blk) => {
                self.body(&blk.call);
                self.enter(ScopeKind::Block, at);
                if let Some(b) = blk.body.as_deref() {
                    self.body(b);
                }
                self.exit(at);
            }
            Node::Lambda(_) => {
                self.enter(ScopeKind::Lambda, at);
                self.exit(at);
            }
            Node::If(i) => {
                self.conditional(&i.cond, i.if_true.as_deref(), i.if_false.as_deref(), at);
            }
            Node::IfMod(i) => {
                self.conditional(&i.cond, i.if_true.as_deref(), i.if_false.as_deref(), at);
            }
            Node::Case(c) => {
                self.enter(ScopeKind::Case, at);
                if let Some(e) = c.expr.as_deref() {
                    self.body(e);
                }
                for arm in &c.when_bodies {
                    let arm_at = anchor(arm);
                    self.enter(ScopeKind::When, arm_at);
                    self.push(
                        EventKind::Branch,
                        None,
                        None,
                        arm_at,
                        Some("when"),
                        Prov::Walk,
                    );
                    if let Node::When(w) = arm {
                        for p in &w.patterns {
                            self.body(p);
                        }
                        if let Some(b) = w.body.as_deref() {
                            self.body(b);
                        }
                    } else {
                        self.body(arm);
                    }
                    self.exit(arm_at);
                }
                if let Some(b) = c.else_body.as_deref() {
                    let else_at = anchor(b);
                    self.enter(ScopeKind::Else, else_at);
                    self.push(
                        EventKind::Branch,
                        None,
                        None,
                        else_at,
                        Some("else"),
                        Prov::Walk,
                    );
                    self.body(b);
                    self.exit(else_at);
                }
                self.exit(at);
            }
            Node::Rescue(r) => {
                if let Some(b) = r.body.as_deref() {
                    self.body(b);
                }
                for arm in &r.rescue_bodies {
                    let arm_at = anchor(arm);
                    self.enter(ScopeKind::Rescue, arm_at);
                    self.push(
                        EventKind::Branch,
                        None,
                        None,
                        arm_at,
                        Some("rescue"),
                        Prov::Walk,
                    );
                    self.body(arm);
                    self.exit(arm_at);
                }
                if let Some(b) = r.else_.as_deref() {
                    self.body(b);
                }
            }
            Node::RescueBody(r) => {
                if let Some(e) = r.exc_list.as_deref() {
                    self.body(e);
                }
                if let Some(b) = r.body.as_deref() {
                    self.body(b);
                }
            }
            Node::Ensure(e) => {
                if let Some(b) = e.body.as_deref() {
                    self.body(b);
                }
                if let Some(b) = e.ensure.as_deref() {
                    let ens_at = anchor(b);
                    self.enter(ScopeKind::Ensure, ens_at);
                    self.push(
                        EventKind::Branch,
                        None,
                        None,
                        ens_at,
                        Some("ensure"),
                        Prov::Walk,
                    );
                    self.body(b);
                    self.exit(ens_at);
                }
            }
            Node::Return(r) => {
                self.push(EventKind::Return, None, None, at, None, Prov::Parser);
                for arg in &r.args {
                    self.body(arg);
                }
            }
            Node::Break(b) => {
                self.push(EventKind::Break, None, None, at, None, Prov::Parser);
                for arg in &b.args {
                    self.body(arg);
                }
            }
            Node::Next(n) => {
                self.push(EventKind::Next, None, None, at, None, Prov::Parser);
                for arg in &n.args {
                    self.body(arg);
                }
            }
            Node::Redo(_) => self.push(EventKind::Redo, None, None, at, None, Prov::Parser),
            Node::Retry(_) => self.push(EventKind::Retry, None, None, at, None, Prov::Parser),
            Node::Yield(y) => {
                self.push(EventKind::Yield, None, None, at, None, Prov::Parser);
                for arg in &y.args {
                    self.body(arg);
                }
            }
            Node::Super(s) => {
                self.push(EventKind::Super, None, None, at, None, Prov::Parser);
                for arg in &s.args {
                    self.body(arg);
                }
            }
            Node::ZSuper(_) => self.push(EventKind::Super, None, None, at, None, Prov::Parser),
            Node::Lvasgn(a) => {
                let sym = self
                    .syms
                    .intern(SymKind::Local, Role::Local, &a.name, Prov::Parser);
                self.push(EventKind::Decl, Some(sym), None, at, None, Prov::Parser);
                if let Some(v) = a.value.as_deref() {
                    self.body(v);
                }
            }
            Node::Ivasgn(a) => {
                let sym = self
                    .syms
                    .intern(SymKind::Ivar, Role::Ivar, &a.name, Prov::Parser);
                self.push(
                    EventKind::IvarWrite,
                    Some(sym),
                    None,
                    at,
                    None,
                    Prov::Parser,
                );
                if let Some(v) = a.value.as_deref() {
                    self.body(v);
                }
            }
            Node::Ivar(i) => {
                let sym = self
                    .syms
                    .intern(SymKind::Ivar, Role::Ivar, &i.name, Prov::Parser);
                self.push(EventKind::IvarRead, Some(sym), None, at, None, Prov::Parser);
            }
            Node::Const(_) => {
                if let Some(name) = crate::functions::exception_type_name(node) {
                    let sym = self
                        .syms
                        .intern(SymKind::Const, Role::None, &name, Prov::Parser);
                    self.push(EventKind::Const, Some(sym), None, at, None, Prov::Parser);
                }
            }
            _ => {}
        }
    }
}

impl Walk<'_> {
    /// `self.<x>` — write, mutator call, or read. Priority mirrors the set
    /// walker: a trailing `=` makes it a write, an `ActiveRecord` mutator name
    /// makes it a call, anything else that is an attribute identifier is a
    /// read.
    fn self_send(&mut self, s: &lib_ruby_parser::nodes::Send, at: u32) {
        let method = s.method_name.as_str();
        if let Some(base) = method.strip_suffix('=')
            && crate::functions::is_attr_ident(base)
        {
            let base = base.to_string();
            let sym = self.attr(&base);
            self.push(EventKind::Write, Some(sym), None, at, None, Prov::Parser);
            for arg in &s.args {
                self.body(arg);
            }
            return;
        }
        if crate::functions::is_ar_mutator(method) {
            let callee = self.callee(method);
            let recv = self
                .syms
                .intern(SymKind::Receiver, Role::Own, "self", Prov::Parser);
            self.push(
                EventKind::Call,
                Some(recv),
                Some(callee),
                at,
                None,
                Prov::Parser,
            );
            for arg in &s.args {
                self.body(arg);
            }
            return;
        }
        if s.args.is_empty() && crate::functions::is_attr_ident(method) {
            let method = method.to_string();
            let sym = self.attr(&method);
            self.push(EventKind::Read, Some(sym), None, at, None, Prov::Parser);
            return;
        }
        let callee = self.callee(method);
        let recv = self
            .syms
            .intern(SymKind::Receiver, Role::Own, "self", Prov::Parser);
        self.push(
            EventKind::Call,
            Some(recv),
            Some(callee),
            at,
            None,
            Prov::Parser,
        );
        for arg in &s.args {
            self.body(arg);
        }
    }

    /// A send with a non-self receiver, or none. Records the relation walk and
    /// the mutator dispatch the set walker records, then recurses.
    fn general_send(&mut self, s: &lib_ruby_parser::nodes::Send, at: u32) {
        if let Some(rel) = crate::functions::traversed_relation(s, self.known) {
            let sym = self.attr(&rel);
            self.push(
                EventKind::Read,
                Some(sym),
                None,
                at,
                Some("traversal"),
                Prov::Parser,
            );
        } else if s.recv.is_none()
            && crate::functions::is_attr_ident(&s.method_name)
            && s.args.is_empty()
        {
            // A bare identifier read of an attribute — implicit self.
            let name = s.method_name.clone();
            let sym = self.attr(&name);
            self.push(EventKind::Read, Some(sym), None, at, None, Prov::Parser);
            return;
        }
        if crate::functions::is_ar_mutator(&s.method_name) {
            let label = crate::functions::receiver_label(s.recv.as_deref());
            let recv = self
                .syms
                .intern(SymKind::Receiver, Role::Ext, &label, Prov::Parser);
            let callee = self.callee(&s.method_name);
            self.push(
                EventKind::Call,
                Some(recv),
                Some(callee),
                at,
                None,
                Prov::Parser,
            );
        }
        if let Some(recv) = s.recv.as_deref() {
            self.body(recv);
        }
        for arg in &s.args {
            self.body(arg);
        }
    }

    /// `x ||= v` / `x &&= v` on a `self` attribute — a write. The guard
    /// semantics stay out of the alphabet: the co-occurrence is the evidence.
    fn op_assign(&mut self, recv: &Node, at: u32) {
        if let Some(field) = crate::functions::attr_of_self(recv) {
            let field = field.to_string();
            let sym = self.attr(&field);
            self.push(EventKind::Write, Some(sym), None, at, None, Prov::Parser);
        }
    }

    /// A loop whose header carries a condition.
    fn loop_with_cond(&mut self, kind: ScopeKind, cond: &Node, body: Option<&Node>, at: u32) {
        self.enter(kind, at);
        self.condition(cond, "loop_cond");
        if let Some(b) = body {
            self.body(b);
        }
        self.exit(at);
    }

    /// A conditional's test, carrying the predicate's own name so `blank?` /
    /// `present?` survive as identity rather than as an interpretation.
    fn condition(&mut self, cond: &Node, control: &str) {
        let at = anchor(cond);
        let pred = if let Node::Send(s) = cond {
            Some(self.callee(&s.method_name))
        } else {
            None
        };
        let subj = if let Node::Send(s) = cond
            && let Some(field) = s.recv.as_deref().and_then(crate::functions::attr_of_self)
        {
            let field = field.to_string();
            Some(self.attr(&field))
        } else {
            None
        };
        self.push(
            EventKind::Condition,
            subj,
            pred,
            at,
            Some(control),
            Prov::Parser,
        );
        self.body(cond);
    }

    /// `if` / `unless` / the postfix modifier forms — one scope per arm, so a
    /// write in the then-branch is distinguishable from one in the else.
    fn conditional(
        &mut self,
        cond: &Node,
        if_true: Option<&Node>,
        if_false: Option<&Node>,
        at: u32,
    ) {
        self.enter(ScopeKind::If, at);
        self.condition(cond, "if_cond");
        if let Some(b) = if_true {
            let b_at = anchor(b);
            self.enter(ScopeKind::Then, b_at);
            self.push(
                EventKind::Branch,
                None,
                None,
                b_at,
                Some("then"),
                Prov::Walk,
            );
            self.body(b);
            self.exit(b_at);
        }
        if let Some(b) = if_false {
            let b_at = anchor(b);
            self.enter(ScopeKind::Else, b_at);
            self.push(
                EventKind::Branch,
                None,
                None,
                b_at,
                Some("else"),
                Prov::Walk,
            );
            self.body(b);
            self.exit(b_at);
        }
        self.exit(at);
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The Rails arm — the class-body declaration stream, in order
// ─────────────────────────────────────────────────────────────────────────

/// Which family a declaration belongs to. A `match` on the variant, nothing
/// re-spelled: the payload rides along untouched on [`RailsEvent::decl`].
fn rails_kind(decl: &Declaration) -> RailsKind {
    match decl {
        Declaration::Association(_) => RailsKind::Association,
        Declaration::Validation(_) => RailsKind::Validation,
        Declaration::Callback(_) => RailsKind::Callback,
        Declaration::Concern(_) => RailsKind::Concern,
        Declaration::Attribute(_) => RailsKind::Attribute,
        Declaration::Delegation(_) => RailsKind::Delegation,
        Declaration::Scope(_) => RailsKind::Scope,
        Declaration::ActsAs(_) => RailsKind::ActsAs,
        Declaration::DslCall(_) => RailsKind::DslCall,
        Declaration::GemDsl(_) => RailsKind::GemDsl,
        Declaration::DynamicMethod(_) => RailsKind::DynamicMethod,
        Declaration::Using(_) => RailsKind::Using,
        Declaration::Sti(_) => RailsKind::Sti,
    }
}

/// Walk a class body statement by statement, pairing each declaration the
/// SHIPPED router produced with the source position of the statement that
/// produced it.
///
/// Routing is not re-implemented: each statement goes through
/// `crate::walk::walk_class_body`, the one place that decides what a macro
/// means. This arm only records WHERE and IN WHAT ORDER — a statement that
/// yields two declarations yields two events sharing an anchor.
fn rails_arm(body: &Node) -> Vec<RailsEvent> {
    let mut out = Vec::new();
    let stmts: Vec<&Node> = match body {
        Node::Begin(b) => b.statements.iter().collect(),
        single => vec![single],
    };
    for stmt in stmts {
        let at = anchor(stmt);
        let mut decls = Vec::new();
        crate::walk::walk_class_body(stmt, &mut decls);
        for decl in decls {
            let seq = u32::try_from(out.len()).unwrap_or(u32::MAX);
            out.push(RailsEvent {
                seq,
                kind: rails_kind(&decl),
                decl,
                anchor: at,
            });
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────
// Entry points — one parse, two walks
// ─────────────────────────────────────────────────────────────────────────

/// Collect every `def` in a class body, paired with the visibility the
/// shipped walker assigns it, so the Ruby arm covers exactly the same method
/// set the set arm does.
fn defs_of(body: &Node, out: &mut Vec<(String, Node)>) {
    match body {
        Node::Begin(b) => {
            for stmt in &b.statements {
                defs_of(stmt, out);
            }
        }
        Node::KwBegin(b) => {
            for stmt in &b.statements {
                defs_of(stmt, out);
            }
        }
        Node::Block(blk) => {
            if let Some(b) = blk.body.as_deref() {
                defs_of(b, out);
            }
        }
        Node::Def(d) => {
            if let Some(b) = d.body.as_deref() {
                out.push((d.name.clone(), b.clone()));
            }
        }
        Node::Send(s) => {
            // `private def foo … end` — the def rides as an argument.
            for arg in &s.args {
                defs_of(arg, out);
            }
        }
        _ => {}
    }
}

/// Walk one class body into both arms.
///
/// The set counts on each [`MethodOre`] come from the SHIPPED
/// `crate::functions::extract_functions_from_body` run over the very same
/// body node in the very same parse — not a second parse and not a second
/// walk, so any difference between the two views is a difference in what is
/// preserved, never an artefact of having looked twice.
#[must_use]
pub fn class_ore(iri: &str, name: &str, body: &Node, syms: &mut Symbols) -> ClassOre {
    let declarations = rails_arm(body);
    let decls: Vec<Declaration> = declarations.iter().map(|e| e.decl.clone()).collect();
    let known: Vec<String> = decls
        .iter()
        .filter_map(|d| match d {
            Declaration::Association(a) => Some(a.name.clone()),
            _ => None,
        })
        .collect();

    // The shipped arm's verdict on this same body, for the side-by-side.
    let (public_fns, helper_fns) =
        crate::functions::extract_functions_from_body(Some(body), &decls);
    let mut sets: BTreeMap<String, (u32, bool)> = BTreeMap::new();
    let mut set_rows: BTreeMap<String, &ruff_spo_triplet::Function> = BTreeMap::new();
    for f in &public_fns {
        set_rows.insert(f.name.clone(), f);
        sets.insert(f.name.clone(), (0, true));
    }
    for f in &helper_fns {
        set_rows.insert(f.name.clone(), f);
        sets.insert(f.name.clone(), (0, false));
    }

    let mut defs = Vec::new();
    defs_of(body, &mut defs);

    let mut methods = Vec::new();
    for (method_name, def_body) in defs {
        let mut walk = Walk::new(syms, &known);
        walk.enter(ScopeKind::Method, anchor(&def_body));
        walk.body(&def_body);
        walk.exit(anchor(&def_body));
        let shipped = set_rows.get(&method_name);
        let public = sets.get(&method_name).is_none_or(|(_, p)| *p);
        methods.push(MethodOre {
            iri: format!("{iri}.{method_name}"),
            name: method_name,
            public,
            events: walk.events,
            scopes: walk.scopes,
            set_reads: shipped.map_or(0, |f| u32::try_from(f.reads.len()).unwrap_or(u32::MAX)),
            set_writes: shipped.map_or(0, |f| u32::try_from(f.writes.len()).unwrap_or(u32::MAX)),
            set_guarded: shipped.map_or(0, |f| {
                u32::try_from(f.guarded_writes.len()).unwrap_or(u32::MAX)
            }),
            set_raises: shipped.map_or(0, |f| u32::try_from(f.raises.len()).unwrap_or(u32::MAX)),
            set_traverses: shipped
                .map_or(0, |f| u32::try_from(f.traverses.len()).unwrap_or(u32::MAX)),
            set_calls: shipped.map_or(0, |f| u32::try_from(f.calls.len()).unwrap_or(u32::MAX)),
        });
    }

    ClassOre {
        iri: iri.to_string(),
        name: name.to_string(),
        declarations,
        methods,
    }
}

/// Parse one Ruby source and walk every class in it into both arms.
///
/// `namespace` prefixes the class IRI, matching [`crate::extract_with`].
/// Returns an empty vec on a hard parse failure, mirroring the shipped
/// parser's own silent-skip behaviour rather than inventing a second policy.
#[must_use]
pub fn source_ore(src: &str, namespace: &str, syms: &mut Symbols) -> Vec<ClassOre> {
    let options = lib_ruby_parser::ParserOptions {
        buffer_name: "<events>".to_string(),
        ..Default::default()
    };
    let parser = lib_ruby_parser::Parser::new(src.as_bytes().to_vec(), options);
    let Some(ast) = parser.do_parse().ast else {
        return Vec::new();
    };
    let mut out = Vec::new();
    collect(&ast, &[], namespace, syms, &mut out);
    out
}

/// Recurse into `module`/`class`/`begin` wrappers, threading the module
/// namespace exactly as `parse::collect_classes_with_namespace` does, so a
/// class IRI from this arm joins the shipped one.
fn collect(
    node: &Node,
    ns: &[String],
    namespace: &str,
    syms: &mut Symbols,
    out: &mut Vec<ClassOre>,
) {
    match node {
        Node::Begin(b) => {
            for stmt in &b.statements {
                collect(stmt, ns, namespace, syms, out);
            }
        }
        Node::Module(m) => {
            let mut nested = ns.to_vec();
            if let Some(name) = const_name(&m.name) {
                nested.push(name);
            }
            if let Some(body) = m.body.as_deref() {
                collect(body, &nested, namespace, syms, out);
            }
        }
        Node::Class(c) => {
            let local = const_name(&c.name).unwrap_or_default();
            let qualified = if ns.is_empty() {
                local
            } else {
                format!("{}::{local}", ns.join("::"))
            };
            if let Some(body) = c.body.as_deref() {
                let iri = format!("{namespace}:{qualified}");
                out.push(class_ore(&iri, &qualified, body, syms));
                collect(body, ns, namespace, syms, out);
            }
        }
        _ => {}
    }
}

/// A constant node's dotted name, for class and module headers.
fn const_name(node: &Node) -> Option<String> {
    match node {
        Node::Const(c) => {
            let base = c.scope.as_deref().and_then(const_name);
            Some(base.map_or_else(|| c.name.clone(), |b| format!("{b}::{}", c.name)))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ore(src: &str) -> (Vec<ClassOre>, Symbols) {
        let mut syms = Symbols::default();
        let classes = source_ore(src, "op", &mut syms);
        (classes, syms)
    }

    fn method<'a>(c: &'a ClassOre, name: &str) -> &'a MethodOre {
        c.methods
            .iter()
            .find(|m| m.name == name)
            .unwrap_or_else(|| panic!("no method `{name}`"))
    }

    /// Resolve an event's subject to `(kind, role, name)`.
    fn subj_of(syms: &Symbols, e: &OreEvent) -> Option<(SymKind, Role, String)> {
        let id = e.subj.as_ref()?;
        syms.rows()
            .find(|r| &r.0 == id)
            .map(|r| (r.1, r.2, r.3.to_string()))
    }

    // ── the additivity gate ────────────────────────────────────────────

    /// THE gate: folding the ordered events back into sets must reproduce
    /// the shipped six-set arm exactly. What changed is what is PRESERVED,
    /// never what a fact IS. If this fails, the two walkers disagree about
    /// the meaning of a body and the ordered ore is not a superset.
    #[test]
    fn events_collapse_to_the_shipped_six_sets() {
        let (cs, syms) = ore(r"
class Invoice < ApplicationRecord
  belongs_to :project
  def settle
    self.total = 1
    self.total = 2
    self.state = 'x'
    raise ArgumentError if self.total.nil?
    self.save
  end
end");
        let m = method(&cs[0], "settle");

        let mut reads: Vec<String> = Vec::new();
        let mut writes: Vec<String> = Vec::new();
        let mut raises: Vec<String> = Vec::new();
        let mut calls = 0_u32;
        for e in &m.events {
            match e.kind {
                EventKind::Read | EventKind::ReadWrite => {
                    if let Some((_, Role::Own, n)) = subj_of(&syms, e) {
                        reads.push(n);
                    }
                }
                EventKind::Raise => {
                    if let Some(id) = &e.obj
                        && let Some(r) = syms.rows().find(|r| &r.0 == id)
                    {
                        raises.push(r.3.to_string());
                    }
                }
                EventKind::Call => calls += 1,
                _ => {}
            }
            if matches!(e.kind, EventKind::Write | EventKind::ReadWrite)
                && let Some((_, Role::Own, n)) = subj_of(&syms, e)
            {
                writes.push(n);
            }
        }
        // The fold dedups; the stream did not.
        let dedup = |mut v: Vec<String>| {
            v.sort();
            v.dedup();
            v
        };
        assert_eq!(
            u32::try_from(dedup(writes.clone()).len()).unwrap(),
            m.set_writes,
            "folded writes must equal the shipped set"
        );
        assert_eq!(
            u32::try_from(dedup(reads).len()).unwrap(),
            m.set_reads,
            "folded reads must equal the shipped set"
        );
        assert_eq!(
            u32::try_from(dedup(raises).len()).unwrap(),
            m.set_raises,
            "folded raises must equal the shipped set"
        );
        assert_eq!(
            calls, m.set_calls,
            "folded calls must equal the shipped set"
        );
        // ANTI-VACUITY: the stream must have carried MORE than the set did.
        // Two writes to `total` collapse to one set entry; if the raw event
        // count equalled the set count this test would be asserting nothing.
        assert_eq!(writes.len(), 3, "three write events before dedup");
        assert_eq!(m.set_writes, 2, "two distinct fields written");
    }

    // ── order ──────────────────────────────────────────────────────────

    /// The thing `dedup_in_place` destroys: repetition, and its position.
    #[test]
    fn repeated_facts_are_separate_events_pointing_at_one_symbol() {
        let (cs, _) = ore(r"
class M < ApplicationRecord
  def touch_twice
    self.count
    self.count
    self.count
  end
end");
        let m = method(&cs[0], "touch_twice");
        let reads: Vec<&OreEvent> = m
            .events
            .iter()
            .filter(|e| e.kind == EventKind::Read)
            .collect();
        assert_eq!(reads.len(), 3, "three reads, not one");
        let ids: Vec<&Option<String>> = reads.iter().map(|e| &e.subj).collect();
        assert!(
            ids.windows(2).all(|w| w[0] == w[1]),
            "all three point at ONE interned symbol"
        );
        assert_eq!(m.set_reads, 1, "the shipped set still says one");
    }

    /// Source order is preserved, and checkable against the byte anchors
    /// rather than merely assumed.
    #[test]
    fn event_order_follows_source_order() {
        let (cs, syms) = ore(r"
class M < ApplicationRecord
  def ordered
    self.zulu = 1
    self.alpha = 2
    self.mike = 3
  end
end");
        let m = method(&cs[0], "ordered");
        let names: Vec<String> = m
            .events
            .iter()
            .filter(|e| e.kind == EventKind::Write)
            .filter_map(|e| subj_of(&syms, e).map(|(_, _, n)| n))
            .collect();
        // Source order is z, a, m; alphabetical order is a, m, z. A sort or a
        // dedup of this stream would reorder it visibly, which is what makes
        // this assertion a falsifier rather than a coincidence.
        assert_eq!(names, vec!["zulu", "alpha", "mike"]);
        let anchors: Vec<u32> = m
            .events
            .iter()
            .filter(|e| e.kind == EventKind::Write)
            .map(|e| e.anchor)
            .collect();
        assert!(
            anchors.windows(2).all(|w| w[0] < w[1]),
            "anchors strictly increase, so traversal order is not merely asserted to be lexical: {anchors:?}"
        );
    }

    // ── structure ──────────────────────────────────────────────────────

    /// A write inside a loop is distinguishable from one outside it. This is
    /// the second thing the set arm cannot express.
    #[test]
    fn a_write_inside_a_loop_is_in_a_loop_scope() {
        let (cs, syms) = ore(r"
class M < ApplicationRecord
  def tally
    self.before = 1
    while self.more?
      self.inside = 2
    end
    self.after = 3
  end
end");
        let m = method(&cs[0], "tally");
        let scope_kind = |id: u32| m.scopes[id as usize].kind;
        let where_written = |name: &str| {
            m.events
                .iter()
                .filter(|e| e.kind == EventKind::Write)
                .find(|e| subj_of(&syms, e).is_some_and(|(_, _, n)| n == name))
                .map(|e| scope_kind(e.scope))
        };
        assert_eq!(where_written("inside"), Some(ScopeKind::While));
        assert_eq!(where_written("before"), Some(ScopeKind::Method));
        assert_eq!(where_written("after"), Some(ScopeKind::Method));
        assert!(
            m.scopes.iter().any(|s| s.kind == ScopeKind::While),
            "the loop opened a scope"
        );
    }

    /// A call in the else-branch is distinguishable from one in the then.
    /// The third thing the set arm cannot express.
    #[test]
    fn branch_arms_are_separate_scopes() {
        let (cs, syms) = ore(r"
class M < ApplicationRecord
  def route
    if self.ready?
      self.win = 1
    else
      self.lose = 2
    end
  end
end");
        let m = method(&cs[0], "route");
        let arm_of = |name: &str| {
            m.events
                .iter()
                .filter(|e| e.kind == EventKind::Write)
                .find(|e| subj_of(&syms, e).is_some_and(|(_, _, n)| n == name))
                .map(|e| m.scopes[e.scope as usize].kind)
        };
        assert_eq!(arm_of("win"), Some(ScopeKind::Then));
        assert_eq!(arm_of("lose"), Some(ScopeKind::Else));
        let controls: Vec<&str> = m
            .events
            .iter()
            .filter(|e| e.kind == EventKind::Branch)
            .filter_map(|e| e.control.as_deref())
            .collect();
        assert_eq!(controls, vec!["then", "else"]);
    }

    /// A loop's exit carries the back edge — the one control relation a tree
    /// walk can state with certainty. A non-loop scope must NOT carry it,
    /// or the tag would be decoration rather than a fact.
    #[test]
    fn only_a_loop_exit_carries_the_back_edge() {
        let (cs, _) = ore(r"
class M < ApplicationRecord
  def mixed
    while self.more?
      self.a = 1
    end
    if self.cond?
      self.b = 2
    end
  end
end");
        let m = method(&cs[0], "mixed");
        let back_edges: Vec<ScopeKind> = m
            .events
            .iter()
            .filter(|e| e.kind == EventKind::ScopeExit && e.control.as_deref() == Some("back_edge"))
            .map(|e| m.scopes[e.scope as usize].kind)
            .collect();
        assert_eq!(back_edges, vec![ScopeKind::While]);
        assert!(
            m.scopes.iter().any(|s| s.kind == ScopeKind::If),
            "the `if` really is present, so its absence above is a decision"
        );
    }

    /// The guard's own predicate survives as identity, so a consumer can
    /// learn the blank-guard idiom without the ore having pre-named it.
    #[test]
    fn a_condition_carries_its_predicate_and_subject_not_a_verdict() {
        let (cs, syms) = ore(r"
class M < ApplicationRecord
  def defaulted
    self.name = 'x' if self.name.blank?
  end
end");
        let m = method(&cs[0], "defaulted");
        let cond = m
            .events
            .iter()
            .find(|e| e.kind == EventKind::Condition)
            .expect("a condition event");
        let pred = cond
            .obj
            .as_ref()
            .and_then(|id| syms.rows().find(|r| &r.0 == id).map(|r| r.3.to_string()));
        assert_eq!(pred.as_deref(), Some("blank?"), "the predicate is identity");
        assert_eq!(
            subj_of(&syms, cond).map(|(_, _, n)| n).as_deref(),
            Some("name"),
            "and so is the field it guards"
        );
        // NEUTRALITY: no event kind names the idiom.
        assert!(
            !m.events
                .iter()
                .any(|e| format!("{:?}", e.kind).contains("Guard")),
            "the alphabet must not pre-name what a consumer is meant to learn"
        );
    }

    // ── the Rails arm ──────────────────────────────────────────────────

    /// Callback declaration order is explicit, and the conditions that
    /// `ruff_spo_triplet::expand` drops are still attached.
    #[test]
    fn the_rails_arm_keeps_callback_order_and_its_conditions() {
        let (cs, _) = ore(r"
class M < ApplicationRecord
  before_validation :normalize
  before_save :stamp, if: :dirty?
  after_commit :notify, on: :create
end");
        let c = &cs[0];
        let cbs: Vec<&RailsEvent> = c
            .declarations
            .iter()
            .filter(|d| d.kind == RailsKind::Callback)
            .collect();
        assert_eq!(cbs.len(), 3);
        assert!(
            cbs.windows(2).all(|w| w[0].seq < w[1].seq),
            "the chain's order is explicit, not an accident of a Vec"
        );
        let opts: Vec<usize> = cbs
            .iter()
            .map(|e| match &e.decl {
                Declaration::Callback(cb) => cb.options.len(),
                _ => unreachable!("filtered to callbacks"),
            })
            .collect();
        assert_eq!(
            opts,
            vec![0, 1, 1],
            "the `if:` and `on:` conditions survive — expand.rs drops these"
        );
    }

    /// The Rails arm records position, so a declaration can be located in the
    /// file rather than only counted.
    #[test]
    fn the_rails_arm_records_source_position() {
        let (cs, _) = ore(r"
class M < ApplicationRecord
  belongs_to :project
  has_many :entries
end");
        let anchors: Vec<u32> = cs[0].declarations.iter().map(|d| d.anchor).collect();
        assert_eq!(anchors.len(), 2);
        assert!(
            anchors[0] < anchors[1],
            "anchors increase with source position: {anchors:?}"
        );
    }
}
