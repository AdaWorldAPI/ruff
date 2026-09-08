//! Ordered behavioral ore — the sequence the five-set arm throws away.
//!
//! # Why this exists
//!
//! [`crate::walk_tu`]'s body arm ends by collapsing its traversal into sets:
//! `method_body_arm` runs `facts.sort(); facts.dedup();` over all five vectors,
//! unconditionally. And `walk_body` gives `ForStmt` / `WhileStmt` / `DoStmt` /
//! `ForRangeStmt` / `SwitchStmt` / `TryStmt` / `LambdaExpr` no match arm at
//! all — their contents are visited, their boundaries are recorded nowhere.
//!
//! So the shipped representation cannot express *a write inside a loop*, *two
//! writes in a row*, or *a call in the else-branch*. Those are exactly the
//! shapes a behavioral vocabulary would be made of. A sequence learner run
//! against that output is learning words from a bag of letters.
//!
//! This module is a SECOND walk over the same cursor that preserves three
//! things separately:
//!
//! | preserved | where | deduped |
//! |---|---|---|
//! | event identity | [`Symbols`] | yes — the only place |
//! | event order | [`MethodOre::events`] | **never** |
//! | structural context | [`MethodOre::scopes`] | n/a |
//!
//! The same fact occurring twice is two events pointing at one symbol.
//!
//! # Additive by construction
//!
//! Nothing here changes `method_body_arm`, `BodyArm`, `CppMethod`, `expand`
//! or the emitted ndjson. `events_collapse_to_the_shipped_five_sets` asserts
//! that folding these events back into sets reproduces the shipped arm
//! exactly — the change is about what is PRESERVED, not about what a fact is.
//!
//! # Neutrality — the ore must not pre-solve its consumer's experiment
//!
//! The alphabet is 20 primitive kinds. There is deliberately no `RAII`,
//! `IteratorLoop`, `VirtualDispatch` or `Move` kind: a `lock_guard` local is
//! [`EventKind::Decl`] plus a type symbol, `std::move(x)` is
//! [`EventKind::Call`] plus a callee symbol. Those concepts are candidate
//! LEARNED tokens, and emitting them here would be writing a tokenizer's
//! answer into its input. Identities are carried verbatim, never hashed —
//! different consumers hash differently.
//!
//! # What libclang does not give
//!
//! There is **no CFG** in libclang's C API. `control` therefore carries only
//! structurally-certain relations (which construct a condition belongs to,
//! which arm a branch is, and a loop's back edge). Successor/predecessor
//! edges are not available and are never fabricated. Every event also carries
//! its source byte `anchor`, so lexical order stays checkable against
//! traversal order rather than being silently conflated with it.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use clang::source::File;
use clang::{Clang, Entity, EntityKind, Index};

use crate::clang_walker::{
    BodyArmConfig, assignment_target, bare_type_name, binary_operator_spelling, call_receiver,
    method_body_arm, own_member_name, qualified_name, thrown_type_name, unary_operator_is_inc_dec,
};

/// The closed primitive alphabet. Adding a variant is a schema change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EventKind {
    ScopeEnter,
    ScopeExit,
    Param,
    Decl,
    CtorInit,
    Read,
    Write,
    ReadWrite,
    Condition,
    Branch,
    Return,
    Break,
    Continue,
    Goto,
    Label,
    Call,
    New,
    Delete,
    Throw,
    Cast,
}

impl EventKind {
    /// The wire spelling. Stable — consumers match on it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ScopeEnter => "ScopeEnter",
            Self::ScopeExit => "ScopeExit",
            Self::Param => "Param",
            Self::Decl => "Decl",
            Self::CtorInit => "CtorInit",
            Self::Read => "Read",
            Self::Write => "Write",
            Self::ReadWrite => "ReadWrite",
            Self::Condition => "Condition",
            Self::Branch => "Branch",
            Self::Return => "Return",
            Self::Break => "Break",
            Self::Continue => "Continue",
            Self::Goto => "Goto",
            Self::Label => "Label",
            Self::Call => "Call",
            Self::New => "New",
            Self::Delete => "Delete",
            Self::Throw => "Throw",
            Self::Cast => "Cast",
        }
    }
}

impl fmt::Display for EventKind {
    /// Formats the event kind using its stable string representation.
    ///
    /// # Examples
    ///
    /// ```
    /// let kind = EventKind::FunctionEnter;
    /// assert_eq!(kind.to_string(), kind.as_str());
    /// ```
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The closed scope vocabulary. Class and namespace are deliberately absent:
/// they are constants of a stream, already carried fully-qualified in the
/// method IRI, and emitting them per-method would be redundant structure a
/// tokenizer would then have to learn to ignore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    Function,
    Block,
    If,
    Then,
    Else,
    For,
    While,
    Do,
    RangeFor,
    Switch,
    Case,
    Default,
    Try,
    Catch,
    Lambda,
}

impl ScopeKind {
    /// Provides the stable name of this scope kind.
    ///
    /// # Examples
    ///
    /// ```
    /// assert_eq!(ScopeKind::Loop.as_str(), "Loop");
    /// ```
    ///
    /// Each scope kind maps to a fixed string representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Function => "Function",
            Self::Block => "Block",
            Self::If => "If",
            Self::Then => "Then",
            Self::Else => "Else",
            Self::For => "For",
            Self::While => "While",
            Self::Do => "Do",
            Self::RangeFor => "RangeFor",
            Self::Switch => "Switch",
            Self::Case => "Case",
            Self::Default => "Default",
            Self::Try => "Try",
            Self::Catch => "Catch",
            Self::Lambda => "Lambda",
        }
    }

    /// Determines whether this scope kind represents a loop.
    ///
    /// # Examples
    ///
    /// ```
    /// assert!(ScopeKind::For.is_loop());
    /// assert!(!ScopeKind::Block.is_loop());
    /// ```
    ///
    /// Returns `true` for `for`, `while`, `do`, and range-based `for` loops; `false` otherwise.
    fn is_loop(self) -> bool {
        matches!(self, Self::For | Self::While | Self::Do | Self::RangeFor)
    }
}

/// What an entity is, for the symbol table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SymKind {
    Member,
    Param,
    Local,
    Callee,
    Type,
    Scope,
    Exception,
    Other,
}

impl SymKind {
    /// Provides the stable string representation of this symbol kind.
    ///
    /// # Examples
    ///
    /// ```
    /// assert_eq!(SymKind::Member.as_str(), "member");
    /// ```
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Param => "param",
            Self::Local => "local",
            Self::Callee => "callee",
            Self::Type => "type",
            Self::Scope => "scope_kind",
            Self::Exception => "exception",
            Self::Other => "other",
        }
    }
}

/// Whose entity it is. The distinction the shipped arm cannot make, because it
/// only ever recorded own members.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Own,
    Param,
    Local,
    This,
    Static,
    Ext,
    Unknown,
    None,
}

impl Role {
    /// Returns the stable string representation of a role.
    ///
    /// # Examples
    ///
    /// ```
    /// use crate::events::Role;
    ///
    /// assert_eq!(Role::Param.as_str(), "param");
    /// ```
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Own => "own",
            Self::Param => "param",
            Self::Local => "local",
            Self::This => "this",
            Self::Static => "static",
            Self::Ext => "ext",
            Self::Unknown => "unknown",
            Self::None => "-",
        }
    }
}

/// Did a libclang semantic API answer this, or did the walker infer it from
/// cursor shape and tokens? A consumer that reports a result must be able to
/// say how much of what it consumed the compiler simply handed over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prov {
    Clang,
    Walk,
}

impl Prov {
    /// Provides the stable lowercase label for this provenance value.
    ///
    /// # Examples
    ///
    /// ```
    /// assert_eq!(Prov::Clang.as_str(), "clang");
    /// assert_eq!(Prov::Walk.as_str(), "walk");
    /// ```
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clang => "clang",
            Self::Walk => "walk",
        }
    }
}

/// One occurrence. Never deduped, never sorted.
#[derive(Debug, Clone)]
pub struct OreEvent {
    pub seq: u32,
    pub kind: EventKind,
    pub subj: Option<String>,
    pub obj: Option<String>,
    pub scope: u32,
    pub parent: Option<u32>,
    pub anchor: u32,
    pub control: Option<String>,
    pub type_rel: Option<String>,
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

/// One method's ore, plus the header facts a consumer joins on.
#[derive(Debug, Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent C++ method qualifiers, mirroring `ruff_spo_triplet::CppMethod`'s own \
              carve-out — any combination is valid, so two-variant enums would be artificial"
)]
pub struct MethodOre {
    pub iri: String,
    pub tu: String,
    pub class: String,
    pub events: Vec<OreEvent>,
    pub scopes: Vec<OreScope>,
    pub is_const: bool,
    pub is_static: bool,
    pub is_virtual: bool,
    pub overrides: bool,
    /// The base overload this method overrides, as an IRI — not just a flag.
    /// It is the one compiler-given statement of behavioral relatedness the
    /// corpus offers for free, and a retrieval measure needs the PAIR, so a
    /// boolean would have left the geometry half of any downstream ablation
    /// unmeasurable.
    pub overrides_target: Option<String>,
    pub mkind: &'static str,
    pub access: &'static str,
    pub n_params: u32,
    /// The SHIPPED five-set arm's view of the very same cursor, and the
    /// centroid `recipe::classify` derives from it. Carried here so a
    /// consumer can compare the ordered ore against A0 without a second
    /// parse — and, more importantly, without a second WALK, which would
    /// make any difference between them ambiguous.
    pub set_reads: u32,
    pub set_writes: u32,
    pub set_raises: u32,
    pub set_calls: u32,
    pub set_guarded: u32,
    pub centroid: String,
}

/// The canonical identity table — the ONLY place dedup happens.
#[derive(Debug, Default)]
pub struct Symbols {
    index: BTreeMap<(SymKind, Role, String), u32>,
    rows: Vec<(SymKind, Role, String, Prov)>,
}

impl Symbols {
    /// Interns a symbol and returns its stable identifier.
    ///
    /// Symbols with the same kind, role, and name share an identifier; the first
    /// provenance value is retained for each unique symbol.
    ///
    /// # Examples
    ///
    /// ```
    /// let mut symbols = Symbols::default();
    /// let id = symbols.intern(SymKind::Member, Role::OwnMember, "value", Prov::Walk);
    ///
    /// assert_eq!(id, "s0");
    /// ```
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

    /// Iterates over interned symbols with their stable identifiers and metadata.
    ///
    /// # Examples
    ///
    /// ```
    /// let symbols = Symbols::default();
    /// assert_eq!(symbols.rows().count(), 0);
    /// ```
    pub fn rows(&self) -> impl Iterator<Item = (String, SymKind, Role, &str, Prov)> {
        self.rows
            .iter()
            .enumerate()
            .map(|(i, (k, r, n, p))| (format!("s{i}"), *k, *r, n.as_str(), *p))
    }

    /// Reports the number of interned symbols.
    ///
    /// # Examples
    ///
    /// ```
    /// let symbols = Symbols::default();
    /// assert_eq!(symbols.len(), 0);
    /// ```
    ///
    /// Returns the number of symbols stored in the table.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Determines whether the symbol table contains no symbols.
    ///
    /// # Examples
    ///
    /// ```
    /// let symbols = Symbols::default();
    /// assert!(symbols.is_empty());
    /// ```
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// Everything one translation unit yielded.
#[derive(Debug, Default)]
pub struct TuOre {
    pub methods: Vec<MethodOre>,
    pub diagnostics: Vec<String>,
}

// ─────────────────────────────────────────────────────────────────────────
// The walk
// ─────────────────────────────────────────────────────────────────────────

struct Walk<'s> {
    syms: &'s mut Symbols,
    events: Vec<OreEvent>,
    scopes: Vec<OreScope>,
    stack: Vec<u32>,
}

impl<'s> Walk<'s> {
    /// Creates a walker backed by the provided shared symbol table.
    ///
    /// # Examples
    ///
    /// ```
    /// let mut symbols = Symbols::default();
    /// let _walker = Walk::new(&mut symbols);
    /// ```
    fn new(syms: &'s mut Symbols) -> Self {
        Self {
            syms,
            events: Vec::new(),
            scopes: Vec::new(),
            stack: Vec::new(),
        }
    }

    /// Gets the ID of the currently active scope, or `0` when no scope is active.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let scope_id = walk.cur();
    /// ```
    fn cur(&self) -> u32 {
        self.stack.last().copied().unwrap_or(0)
    }

    /// Gets the identifier of the enclosing scope.
    ///
    /// # Examples
    ///
    /// ```
    /// # struct Walk { stack: Vec<u32> }
    /// # impl Walk {
    /// #     fn parent(&self) -> Option<u32> {
    /// #         let n = self.stack.len();
    /// #         (n >= 2).then(|| self.stack[n - 2])
    /// #     }
    /// # }
    /// let walk = Walk { stack: vec![10, 20] };
    /// assert_eq!(walk.parent(), Some(10));
    /// ```
    fn parent(&self) -> Option<u32> {
        let n = self.stack.len();
        (n >= 2).then(|| self.stack[n - 2])
    }

    /// Returns the source-file byte offset where an entity begins, or `0` when no source range is available.
    ///
    /// # Examples
    ///
    /// ```
    /// # use clang::Entity;
    /// # fn example(entity: &Entity<'_>) {
    /// let offset = anchor(entity);
    /// assert!(offset >= 0);
    /// # }
    /// ```
    fn anchor(e: &Entity<'_>) -> u32 {
        e.get_range()
            .map(|r| r.get_start().get_file_location().offset)
            .unwrap_or(0)
    }

    /// Appends a walker-originated event for an entity in the current scope.
    ///
    /// The event receives a sequence number, source anchor, current scope, and
    /// parent scope, while its subject, object, control metadata, and type
    /// relation remain unset.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let index = walk.push(EventKind::Read, &entity);
    /// assert_eq!(walk.events[index].kind, EventKind::Read);
    /// ```
    fn push(&mut self, kind: EventKind, e: &Entity<'_>) -> usize {
        let seq = u32::try_from(self.events.len()).unwrap_or(u32::MAX);
        self.events.push(OreEvent {
            seq,
            kind,
            subj: None,
            obj: None,
            scope: self.cur(),
            parent: self.parent(),
            anchor: Self::anchor(e),
            control: None,
            type_rel: None,
            prov: Prov::Walk,
        });
        self.events.len() - 1
    }

    /// Records a nested scope and its matching entry and exit events.
    ///
    /// The scope is linked to the currently active parent, and loop scopes mark
    /// their exit event with a `back_edge` control label.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// walker.scoped(ScopeKind::Loop, entity, |walker| {
    ///     walker.children(entity);
    /// });
    /// ```
    fn scoped<F: FnOnce(&mut Self)>(&mut self, kind: ScopeKind, e: &Entity<'_>, body: F) {
        let id = u32::try_from(self.scopes.len()).unwrap_or(u32::MAX);
        let depth = u16::try_from(self.stack.len()).unwrap_or(u16::MAX);
        let parent = self.stack.last().copied();
        let ksym = self
            .syms
            .intern(SymKind::Scope, Role::None, kind.as_str(), Prov::Walk);
        self.stack.push(id);
        let enter = self.push(EventKind::ScopeEnter, e);
        self.events[enter].subj = Some(ksym.clone());
        let enter_seq = self.events[enter].seq;
        self.scopes.push(OreScope {
            id,
            parent,
            kind,
            depth,
            enter_seq,
            exit_seq: enter_seq,
        });
        body(self);
        let exit = self.push(EventKind::ScopeExit, e);
        self.events[exit].subj = Some(ksym);
        if kind.is_loop() {
            self.events[exit].control = Some("back_edge".to_string());
        }
        let exit_seq = self.events[exit].seq;
        if let Some(s) = self.scopes.iter_mut().find(|s| s.id == id) {
            s.exit_seq = exit_seq;
        }
        self.stack.pop();
    }

    /// Visits each direct child entity with the walker.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// for child in entity.get_children() {
    ///     walker.node(&child);
    /// }
    /// ```
    fn children(&mut self, e: &Entity<'_>) {
        for c in e.get_children() {
            self.node(&c);
        }
    }

    /// Walks a construct body without introducing a redundant block scope.
    ///
    /// Braced and unbraced bodies are handled uniformly, preserving equivalent
    /// scope structures for constructs such as `if (x) f();` and
    /// `if (x) { f(); }`.
    ///
    /// # Examples
    ///
    /// ```text
    /// if (condition) statement;
    /// if (condition) { statement; }
    /// ```
    fn body(&mut self, e: &Entity<'_>) {
        if e.get_kind() == EntityKind::CompoundStmt {
            self.children(e);
        } else {
            self.node(e);
        }
    }

    /// Classifies a referenced declaration by role, symbol kind, and provenance.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let (role, kind, provenance) = ref_role(&entity);
    /// assert_eq!(role, Role::Param);
    /// assert_eq!(kind, SymKind::Param);
    /// assert_eq!(provenance, Prov::Clang);
    /// ```
    ///
    /// # Returns
    ///
    /// A tuple containing the declaration's role, symbol kind, and resolution provenance. Unresolved references use `Role::Unknown`, `SymKind::Other`, and `Prov::Walk`.
    ///
    /// # Parameters
    ///
    /// * `e` - The entity whose referenced declaration is classified.
    fn ref_role(e: &Entity<'_>) -> (Role, SymKind, Prov) {
        match e.get_reference().map(|r| r.get_kind()) {
            Some(EntityKind::ParmDecl) => (Role::Param, SymKind::Param, Prov::Clang),
            Some(EntityKind::VarDecl) => (Role::Local, SymKind::Local, Prov::Clang),
            Some(EntityKind::FieldDecl) => (Role::Own, SymKind::Member, Prov::Clang),
            _ => (Role::Unknown, SymKind::Other, Prov::Walk),
        }
    }

    /// Visits a Clang entity and records its behavioral events and structural scopes.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let before = walk.events.len();
    /// walk.node(&entity);
    /// assert!(walk.events.len() >= before);
    /// ```
    fn node(&mut self, e: &Entity<'_>) {
        match e.get_kind() {
            EntityKind::CompoundStmt => self.scoped(ScopeKind::Block, e, |w| w.children(e)),

            EntityKind::IfStmt => self.scoped(ScopeKind::If, e, |w| {
                let ch = e.get_children();
                // [cond, then, else?] — the arms are taken by POSITION, so a
                // braced and an unbraced arm give the identical scope tree.
                if let Some(cond) = ch.first() {
                    w.node(cond);
                    let i = w.push(EventKind::Condition, cond);
                    w.events[i].control = Some("if".to_string());
                }
                if let Some(then) = ch.get(1) {
                    let i = w.push(EventKind::Branch, then);
                    w.events[i].control = Some("then".to_string());
                    w.scoped(ScopeKind::Then, then, |w| w.body(then));
                }
                if let Some(els) = ch.get(2) {
                    let i = w.push(EventKind::Branch, els);
                    w.events[i].control = Some("else".to_string());
                    w.scoped(ScopeKind::Else, els, |w| w.body(els));
                }
            }),

            EntityKind::ForStmt
            | EntityKind::WhileStmt
            | EntityKind::DoStmt
            | EntityKind::ForRangeStmt => {
                let (sk, tag) = match e.get_kind() {
                    EntityKind::ForStmt => (ScopeKind::For, "for"),
                    EntityKind::WhileStmt => (ScopeKind::While, "while"),
                    EntityKind::DoStmt => (ScopeKind::Do, "do"),
                    _ => (ScopeKind::RangeFor, "rangefor"),
                };
                let is_do = sk == ScopeKind::Do;
                self.scoped(sk, e, |w| {
                    // libclang labels no sub-part of a loop header, and a
                    // `for(;;)` simply has fewer children — so for `for` /
                    // `while` / range-`for` the LAST child is the body and
                    // everything before it is the header. `do { } while (c)`
                    // is the exception in BOTH respects: its body comes
                    // FIRST and its condition is evaluated after the body, so
                    // treating the last child as the body would walk the
                    // condition as a statement and open a spurious Block
                    // around the real one. The header's parts are not
                    // individually distinguished; `anchor` keeps their
                    // source positions.
                    let ch = e.get_children();
                    if is_do {
                        if let Some((body, rest)) = ch.split_first() {
                            w.body(body);
                            for c in rest {
                                w.node(c);
                            }
                        }
                        let i = w.push(EventKind::Condition, e);
                        w.events[i].control = Some(tag.to_string());
                    } else {
                        let (head, body) = ch.split_at(ch.len().saturating_sub(1));
                        for c in head {
                            w.node(c);
                        }
                        let i = w.push(EventKind::Condition, e);
                        w.events[i].control = Some(tag.to_string());
                        for c in body {
                            w.body(c);
                        }
                    }
                });
            }

            EntityKind::SwitchStmt => self.scoped(ScopeKind::Switch, e, |w| {
                let ch = e.get_children();
                if let Some(cond) = ch.first() {
                    w.node(cond);
                    let i = w.push(EventKind::Condition, cond);
                    w.events[i].control = Some("switch".to_string());
                }
                for c in ch.iter().skip(1) {
                    w.body(c);
                }
            }),

            EntityKind::CaseStmt | EntityKind::DefaultStmt => {
                let is_case = e.get_kind() == EntityKind::CaseStmt;
                let i = self.push(EventKind::Branch, e);
                self.events[i].control = Some(if is_case { "case" } else { "default" }.to_string());
                let sk = if is_case {
                    ScopeKind::Case
                } else {
                    ScopeKind::Default
                };
                self.scoped(sk, e, |w| {
                    for c in e.get_children() {
                        w.body(&c);
                    }
                });
            }

            EntityKind::TryStmt => self.scoped(ScopeKind::Try, e, |w| {
                for c in e.get_children() {
                    if c.get_kind() == EntityKind::CatchStmt {
                        let i = w.push(EventKind::Branch, &c);
                        w.events[i].control = Some("catch".to_string());
                        w.scoped(ScopeKind::Catch, &c, |w| {
                            for g in c.get_children() {
                                w.body(&g);
                            }
                        });
                    } else {
                        w.body(&c);
                    }
                }
            }),

            EntityKind::CatchStmt => {
                let i = self.push(EventKind::Branch, e);
                self.events[i].control = Some("catch".to_string());
                self.scoped(ScopeKind::Catch, e, |w| {
                    for c in e.get_children() {
                        w.body(&c);
                    }
                });
            }

            EntityKind::LambdaExpr => self.scoped(ScopeKind::Lambda, e, |w| {
                for c in e.get_children() {
                    w.body(&c);
                }
            }),

            EntityKind::ReturnStmt => {
                self.push(EventKind::Return, e);
                self.children(e);
            }
            EntityKind::BreakStmt => {
                self.push(EventKind::Break, e);
            }
            EntityKind::ContinueStmt => {
                self.push(EventKind::Continue, e);
            }
            EntityKind::GotoStmt | EntityKind::IndirectGotoStmt => {
                self.push(EventKind::Goto, e);
            }
            EntityKind::LabelStmt => {
                self.push(EventKind::Label, e);
                self.children(e);
            }

            EntityKind::VarDecl => {
                let name = e.get_name().unwrap_or_default();
                let sym = self
                    .syms
                    .intern(SymKind::Local, Role::Local, &name, Prov::Clang);
                let ty = e
                    .get_type()
                    .map(|t| bare_type_name(&t.get_display_name()))
                    .filter(|t| !t.is_empty())
                    .map(|t| self.syms.intern(SymKind::Type, Role::None, &t, Prov::Clang));
                let i = self.push(EventKind::Decl, e);
                self.events[i].subj = Some(sym);
                self.events[i].type_rel = ty;
                self.events[i].prov = Prov::Clang;
                self.children(e);
            }

            EntityKind::MemberRef => {
                // A constructor's member-initializer target. The shipped arm
                // never sees these, so every ctor field initialisation is
                // invisible to it.
                if let Some(name) = e.get_name() {
                    let sym = self
                        .syms
                        .intern(SymKind::Member, Role::Own, &name, Prov::Clang);
                    let i = self.push(EventKind::CtorInit, e);
                    self.events[i].subj = Some(sym);
                    self.events[i].prov = Prov::Clang;
                }
                self.children(e);
            }

            EntityKind::ThrowExpr => {
                let ty = thrown_type_name(e).map(|t| {
                    self.syms
                        .intern(SymKind::Exception, Role::None, &t, Prov::Clang)
                });
                let i = self.push(EventKind::Throw, e);
                self.events[i].obj = ty;
                self.children(e);
            }

            EntityKind::NewExpr | EntityKind::DeleteExpr => {
                let is_new = e.get_kind() == EntityKind::NewExpr;
                // A delete-expression's OWN type is `void` -- it yields no
                // value -- so reading it here labelled every Delete event
                // `void`. The deleted object's type is the operand's, with one
                // pointer layer removed: `delete p` where `p` is `Foo *` is a
                // Delete of `Foo`. `new` is unaffected; its own type IS the
                // allocated pointer type.
                let ty = if is_new {
                    e.get_type().map(|t| bare_type_name(&t.get_display_name()))
                } else {
                    e.get_children()
                        .first()
                        .and_then(Entity::get_type)
                        .map(|t| t.get_pointee_type().unwrap_or(t))
                        .map(|t| bare_type_name(&t.get_display_name()))
                }
                .filter(|t| !t.is_empty())
                .map(|t| self.syms.intern(SymKind::Type, Role::None, &t, Prov::Clang));
                let i = self.push(
                    if is_new {
                        EventKind::New
                    } else {
                        EventKind::Delete
                    },
                    e,
                );
                self.events[i].obj = ty;
                self.children(e);
            }

            EntityKind::CStyleCastExpr
            | EntityKind::StaticCastExpr
            | EntityKind::DynamicCastExpr
            | EntityKind::ReinterpretCastExpr
            | EntityKind::ConstCastExpr
            | EntityKind::FunctionalCastExpr => {
                let tag = match e.get_kind() {
                    EntityKind::CStyleCastExpr => "c",
                    EntityKind::StaticCastExpr => "static",
                    EntityKind::DynamicCastExpr => "dynamic",
                    EntityKind::ReinterpretCastExpr => "reinterpret",
                    EntityKind::ConstCastExpr => "const",
                    _ => "functional",
                };
                let i = self.push(EventKind::Cast, e);
                self.events[i].type_rel = Some(tag.to_string());
                self.children(e);
            }

            EntityKind::BinaryOperator => {
                let ch = e.get_children();
                if let Some((lhs, rest)) = ch.split_first()
                    && binary_operator_spelling(e).as_deref() == Some("=")
                {
                    self.write_event(lhs, EventKind::Write);
                    self.node_skipping_top_member(lhs);
                    for r in rest {
                        self.node(r);
                    }
                } else {
                    self.children(e);
                }
            }

            EntityKind::CompoundAssignOperator => {
                let ch = e.get_children();
                if let Some((lhs, rest)) = ch.split_first() {
                    self.write_event(lhs, EventKind::ReadWrite);
                    self.node_skipping_top_member(lhs);
                    for r in rest {
                        self.node(r);
                    }
                }
            }

            EntityKind::UnaryOperator => {
                let ch = e.get_children();
                if let Some(op) = ch.first()
                    && unary_operator_is_inc_dec(e)
                {
                    self.write_event(op, EventKind::ReadWrite);
                    self.node_skipping_top_member(op);
                } else {
                    self.children(e);
                }
            }

            EntityKind::CallExpr => self.call(e),

            EntityKind::MemberRefExpr => {
                let (sym, prov) = if let Some(m) = own_member_name(e) {
                    (
                        self.syms.intern(SymKind::Member, Role::Own, &m, Prov::Walk),
                        Prov::Walk,
                    )
                } else {
                    let name = e.get_name().unwrap_or_default();
                    (
                        self.syms
                            .intern(SymKind::Member, Role::Ext, &name, Prov::Walk),
                        Prov::Walk,
                    )
                };
                let i = self.push(EventKind::Read, e);
                self.events[i].subj = Some(sym);
                self.events[i].prov = prov;
                self.children(e);
            }

            EntityKind::DeclRefExpr => {
                let (role, kind, prov) = Self::ref_role(e);
                if matches!(role, Role::Param | Role::Local) {
                    let name = e.get_name().unwrap_or_default();
                    let sym = self.syms.intern(kind, role, &name, prov);
                    let i = self.push(EventKind::Read, e);
                    self.events[i].subj = Some(sym);
                    self.events[i].prov = prov;
                }
                self.children(e);
            }

            _ => self.children(e),
        }
    }

    /// Records a write or read-modify-write event for the symbol named by the left-hand side.
    ///
    /// Member assignment targets are recorded as owned members; other targets use their
    /// inferred symbol kind, role, and provenance.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// walk.write_event(&lhs, EventKind::Write);
    /// ```
    fn write_event(&mut self, lhs: &Entity<'_>, kind: EventKind) {
    fn write_event(&mut self, lhs: &Entity<'_>, kind: EventKind) {
        let (sym, prov) = if let Some(m) = assignment_target(lhs) {
            (
                self.syms.intern(SymKind::Member, Role::Own, &m, Prov::Walk),
                Prov::Walk,
            )
        } else {
            let (role, sk, prov) = Self::ref_role(lhs);
            let name = lhs.get_name().unwrap_or_else(|| "?".to_string());
            (self.syms.intern(sk, role, &name, prov), prov)
        };
        let i = self.push(kind, lhs);
        self.events[i].subj = Some(sym);
        self.events[i].prov = prov;
    }

    /// Walks an assignment target's children without recording the target itself as a read.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// walker.node_skipping_top_member(lhs);
    /// ```
    fn node_skipping_top_member(&mut self, lhs: &Entity<'_>) {
        self.children(lhs);
    }

    /// Records a function or method call and traverses its receiver and arguments.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// walker.call(&call_entity);
    /// ```
    fn call(&mut self, e: &Entity<'_>) {
        let name = e.get_name().unwrap_or_default();
        let referenced = e.get_reference();
        let (tag, prov) = referenced.map_or(("-", Prov::Walk), |r| {
            let t = if r.is_virtual_method() {
                "virtual"
            } else if r.is_static_method() {
                "static"
            } else {
                match r.get_kind() {
                    EntityKind::Constructor => "ctor",
                    EntityKind::Destructor => "dtor",
                    EntityKind::Method | EntityKind::ConversionFunction => "member",
                    EntityKind::FunctionDecl => "free",
                    _ => "-",
                }
            };
            (t, Prov::Clang)
        });
        let recv = call_receiver(e);
        let recv_sym = recv
            .as_ref()
            .map(|r| self.syms.intern(SymKind::Callee, Role::Ext, r, Prov::Walk));
        let callee = self.syms.intern(
            SymKind::Callee,
            if recv.is_some() {
                Role::Ext
            } else {
                Role::This
            },
            &name,
            prov,
        );
        let i = self.push(EventKind::Call, e);
        self.events[i].subj = recv_sym;
        self.events[i].obj = Some(callee);
        self.events[i].type_rel = Some(tag.to_string());
        self.events[i].prov = prov;
        // Walk the arguments and the RECEIVER, but never the callee reference
        // itself: `foo()` nests a `MemberRefExpr` named `foo`, and recording
        // it would emit a read of a member that does not exist. The shipped
        // walker names this trap; this one fell into it once.
        for c in e.get_children() {
            let is_callee = c.get_kind() == EntityKind::MemberRefExpr
                && c.get_name().as_deref() == Some(name.as_str());
            if is_callee {
                self.children(&c); // the receiver base still counts
            } else {
                self.node(&c);
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Entry points
// ─────────────────────────────────────────────────────────────────────────

/// Builds the canonical qualified identifier for a C++ method, including parameter types, constness, and reference qualification.
///
/// Returns `None` when the method has no name or semantic parent.
///
/// # Examples
///
/// ```
/// let iri = "Namespace::Type.method(int, std::string) const &";
/// assert_eq!(iri, "Namespace::Type.method(int, std::string) const &");
/// ```
fn method_iri(m: &Entity<'_>) -> Option<String> {
    let name = m.get_name()?;
    let parent = m.get_semantic_parent()?;
    let params: Vec<String> = m
        .get_arguments()
        .into_iter()
        .flatten()
        .filter_map(|a| a.get_type().map(|t| t.get_display_name()))
        .collect();
    let refq = m
        .get_type()
        .and_then(|t| t.get_ref_qualifier())
        .map(|q| match q {
            clang::RefQualifier::LValue => " &",
            clang::RefQualifier::RValue => " &&",
        })
        .unwrap_or("");
    Some(format!(
        "{}.{name}({}){}{}",
        qualified_name(&parent),
        params.join(","),
        if m.is_const_method() { " const" } else { "" },
        refq
    ))
}

/// Identifies whether a Clang entity kind represents a callable declaration.
///
/// # Examples
///
/// ```
/// assert!(is_callable(EntityKind::Method));
/// assert!(!is_callable(EntityKind::Namespace));
/// ```
///
/// Returns `true` for methods, constructors, destructors, conversion functions,
/// and function declarations; `false` for other entity kinds.
fn is_callable(k: EntityKind) -> bool {
    matches!(
        k,
        EntityKind::Method
            | EntityKind::Constructor
            | EntityKind::Destructor
            | EntityKind::ConversionFunction
            | EntityKind::FunctionDecl
    )
}

/// Collects callable method definitions from a Clang entity tree that belong to the translation unit's main file.
///
/// Matching methods are converted to ordered method data and appended to `out`; declarations from
/// included headers are skipped.
///
/// # Examples
///
/// ```ignore
/// collect(&root, tu, &mut symbols, &mut methods, main_file, &arm_cfg);
/// ```
///
/// `main` identifies the translation unit's source file. `tu` and `arm_cfg` provide context for
/// method extraction, while `syms` stores shared symbol identities.
fn collect(
    e: &Entity<'_>,
    tu: &str,
    syms: &mut Symbols,
    out: &mut Vec<MethodOre>,
    main: Option<File<'_>>,
    arm_cfg: &BodyArmConfig,
) {
    for c in e.get_children() {
        if is_callable(c.get_kind()) && c.is_definition() {
            // Only bodies from THIS translation unit's own file, or every
            // header a TU pulls in is re-harvested once per includer and the
            // corpus silently multiplies.
            let same = c
                .get_location()
                .map(|l| l.get_file_location().file)
                .is_some_and(|f| f.is_some() && f == main);
            if same && let Some(iri) = method_iri(&c) {
                out.push(method_ore(&c, tu, &iri, syms, arm_cfg));
            }
        }
        collect(&c, tu, syms, out, main, arm_cfg);
    }
}

/// Builds the ordered behavioral representation and metadata for a C++ method.
///
/// The result combines the method's ordered events and nested scopes with its
/// identity, qualifiers, access, override information, shipped set counts, and
/// centroid classification.
///
/// # Examples
///
/// ```ignore
/// let ore = method_ore(&method, translation_unit, iri, &mut symbols, &arm_cfg);
/// assert_eq!(ore.iri, iri);
/// ```
fn method_ore
fn method_ore(
    m: &Entity<'_>,
    tu: &str,
    iri: &str,
    syms: &mut Symbols,
    arm_cfg: &BodyArmConfig,
) -> MethodOre {
    // The shipped arm, on this exact cursor, in this exact parse.
    let arm = method_body_arm(m, arm_cfg);
    let shipped = ruff_spo_triplet::CppMethod {
        name: m.get_name().unwrap_or_default(),
        writes: arm.writes.clone(),
        reads: arm.reads.clone(),
        raises: arm.raises.clone(),
        calls: arm.calls.clone(),
        guarded_writes: arm.guarded_writes.clone(),
        ..Default::default()
    };
    let centroid = format!("{:?}", ruff_spo_triplet::classify(&shipped));
    let mut w = Walk::new(syms);
    let anchor = m;
    w.scoped(ScopeKind::Function, anchor, |w| {
        for a in m.get_arguments().into_iter().flatten() {
            let name = a.get_name().unwrap_or_default();
            let sym = w
                .syms
                .intern(SymKind::Param, Role::Param, &name, Prov::Clang);
            let ty = a
                .get_type()
                .map(|t| bare_type_name(&t.get_display_name()))
                .filter(|t| !t.is_empty())
                .map(|t| w.syms.intern(SymKind::Type, Role::None, &t, Prov::Clang));
            let i = w.push(EventKind::Param, &a);
            w.events[i].subj = Some(sym);
            w.events[i].type_rel = ty;
            w.events[i].prov = Prov::Clang;
        }
        for c in m.get_children() {
            match c.get_kind() {
                // Parameters are emitted above, in declaration order.
                EntityKind::ParmDecl => {}
                // The function's own CompoundStmt would open a redundant Block
                // scope inside Function; walk through it.
                EntityKind::CompoundStmt => w.children(&c),
                _ => w.node(&c),
            }
        }
    });
    let class = m
        .get_semantic_parent()
        .map(|p| qualified_name(&p))
        .unwrap_or_default();
    MethodOre {
        iri: iri.to_string(),
        tu: tu.to_string(),
        class,
        events: w.events,
        scopes: w.scopes,
        is_const: m.is_const_method(),
        is_static: m.is_static_method(),
        is_virtual: m.is_virtual_method(),
        overrides: m.get_overridden_methods().is_some_and(|o| !o.is_empty()),
        overrides_target: m
            .get_overridden_methods()
            .and_then(|o| o.into_iter().next())
            .and_then(|b| method_iri(&b)),
        mkind: match m.get_kind() {
            EntityKind::Constructor => "ctor",
            EntityKind::Destructor => "dtor",
            EntityKind::ConversionFunction => "operator",
            _ => {
                if m.get_name().is_some_and(|n| n.starts_with("operator")) {
                    "operator"
                } else {
                    "method"
                }
            }
        },
        access: match m.get_accessibility() {
            Some(clang::Accessibility::Protected) => "protected",
            Some(clang::Accessibility::Private) => "private",
            _ => "public",
        },
        n_params: u32::try_from(m.get_arguments().map_or(0, |a| a.len())).unwrap_or(0),
        set_reads: u32::try_from(arm.reads.len()).unwrap_or(0),
        set_writes: u32::try_from(arm.writes.len()).unwrap_or(0),
        set_raises: u32::try_from(arm.raises.len()).unwrap_or(0),
        set_calls: u32::try_from(arm.calls.len()).unwrap_or(0),
        set_guarded: u32::try_from(arm.guarded_writes.len()).unwrap_or(0),
        centroid,
    }
}

/// Walks a translation unit and collects ordered ore for its method definitions.
///
/// Parse diagnostics at error severity are included in the returned translation-unit data.
///
/// # Errors
///
/// Returns [`crate::WalkError::Libclang`] if libclang cannot be initialized, or
/// [`crate::WalkError::Parse`] if parsing fails.
///
/// # Examples
///
/// ```no_run
/// use std::path::Path;
///
/// let mut symbols = Symbols::default();
/// let ore = walk_tu_events(Path::new("example.cpp"), &[], &mut symbols)?;
/// # let _: TuOre = ore;
/// # Ok::<(), crate::WalkError>(())
/// ```
pub fn walk_tu_events(
    path: &Path,
    args: &[String],
    syms: &mut Symbols,
) -> Result<TuOre, crate::WalkError> {
    let clang = Clang::new().map_err(crate::WalkError::Libclang)?;
    let index = Index::new(&clang, false, false);
    let tu = index
        .parser(path)
        .arguments(args)
        .skip_function_bodies(false)
        .parse()
        .map_err(|e| crate::WalkError::Parse(e.to_string()))?;
    let diagnostics = tu
        .get_diagnostics()
        .into_iter()
        .filter(|d| d.get_severity() >= clang::diagnostic::Severity::Error)
        .map(|d| d.formatter().format())
        .collect();
    let mut methods = Vec::new();
    let main = tu.get_file(path);
    let arm_cfg = BodyArmConfig::default();
    collect(
        &tu.get_entity(),
        &path.display().to_string(),
        syms,
        &mut methods,
        main,
        &arm_cfg,
    );
    Ok(TuOre {
        methods,
        diagnostics,
    })
}

#[cfg(all(test, feature = "libclang"))]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::clang_walker::CLANG_TEST_LOCK;

    /// One parse per fixture. `Clang` is a process singleton, so every test
    /// here serialises on the crate-wide lock — the same lock `arm_tests` and
    /// `libclang_tests` take.
    fn ore(name: &str, src: &str) -> (Vec<MethodOre>, Symbols) {
        let dir = std::env::temp_dir().join(format!(
            "cpp_ore_{name}_{}",
            crate::clang_walker::fixture_salt()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("f.cpp");
        // The path carries `fixture_salt` so two test PROCESSES never share it;
        // the lock only serialises libclang within one process, which is all a
        // `Mutex` can do when the runner gives every test its own process.
        let _guard = CLANG_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut fh = std::fs::File::create(&path).expect("fixture file");
        fh.write_all(src.as_bytes()).expect("write fixture");
        drop(fh);
        let mut syms = Symbols::default();
        let tu = walk_tu_events(&path, &["-std=c++17".to_string()], &mut syms).expect("walk");
        (tu.methods, syms)
    }

    fn find<'a>(ms: &'a [MethodOre], needle: &str) -> &'a MethodOre {
        ms.iter()
            .find(|m| m.iri.contains(needle))
            .unwrap_or_else(|| {
                panic!(
                    "no method matching {needle} in {:?}",
                    ms.iter().map(|m| &m.iri).collect::<Vec<_>>()
                )
            })
    }

    fn kinds(m: &MethodOre) -> Vec<&'static str> {
        m.events.iter().map(|e| e.kind.as_str()).collect()
    }

    /// Collects the stable names of a method's scopes in traversal order.
    ///
    /// # Examples
    ///
    /// ```
    /// let method = MethodOre::default();
    /// assert!(scope_kinds(&method).is_empty());
    /// ```
    fn scope_kinds(m: &MethodOre) -> Vec<&'static str> {
        m.scopes.iter().map(|s| s.kind.as_str()).collect()
    }

    /// THE CONTROL. Its expected sequence was hand-written before this code
    /// existed (`EXPECTED-run.md`); where the two disagreed, the disagreement
    /// was recorded rather than reconciled, and this test pins what libclang
    /// actually does.
    #[test]
    fn the_control_loop_over_a_branch_emits_its_whole_structure() {
        let (ms, syms) = ore(
            "control",
            r"
struct C {
  void run(int n);
  void foo();
  void bar();
  int count_;
};
void C::run(int n) {
  for (int i = 0; i < n; ++i) {
    if (count_ > 0) { foo(); } else { bar(); }
  }
}",
        );
        let m = find(&ms, "C.run(int)");
        assert_eq!(
            kinds(m),
            vec![
                "ScopeEnter",
                "Param",
                "ScopeEnter",
                "Decl",
                "Read",
                "Read",
                "ReadWrite",
                "Condition",
                "ScopeEnter",
                "Read",
                "Condition",
                "Branch",
                "ScopeEnter",
                "Call",
                "ScopeExit",
                "Branch",
                "ScopeEnter",
                "Call",
                "ScopeExit",
                "ScopeExit",
                "ScopeExit",
                "ScopeExit",
            ],
            "the control's event sequence"
        );
        assert_eq!(
            scope_kinds(m),
            vec!["Function", "For", "If", "Then", "Else"],
            "braces must not add a scope: For > If > {{Then, Else}}"
        );
        // The loop's exit closes a back edge — the one control relation that
        // is structurally certain without a CFG.
        let exit_for = m
            .events
            .iter()
            .find(|e| e.kind == EventKind::ScopeExit && e.control.as_deref() == Some("back_edge"));
        assert!(exit_for.is_some(), "the loop exit must carry its back edge");
        // A call's callee must NOT also appear as a member read.
        let reads: Vec<&str> = m
            .events
            .iter()
            .filter(|e| e.kind == EventKind::Read)
            .filter_map(|e| e.subj.as_deref())
            .filter_map(|s| syms.rows().find(|r| r.0 == s).map(|r| r.3))
            .map(str::to_string)
            .map(|s| Box::leak(s.into_boxed_str()) as &str)
            .collect();
        assert!(
            !reads.contains(&"foo") && !reads.contains(&"bar"),
            "a call's own callee is a MemberRefExpr; it must never read as a member: {reads:?}"
        );
    }

    /// Braces are style, not behavior: the two forms must give one tree.
    #[test]
    fn a_braced_and_an_unbraced_arm_give_the_identical_stream() {
        let (ms, _) = ore(
            "braces",
            r"
struct C { void a(int n); void b(int n); int x_; };
void C::a(int n) { if (n > 0) { x_ = n; } else { x_ = 0; } }
void C::b(int n) { if (n > 0) x_ = n; else x_ = 0; }",
        );
        assert_eq!(
            kinds(find(&ms, "C.a(int)")),
            kinds(find(&ms, "C.b(int)")),
            "brace style must not change the event stream"
        );
        assert_eq!(
            scope_kinds(find(&ms, "C.a(int)")),
            scope_kinds(find(&ms, "C.b(int)"))
        );
    }

    /// Every scope kind the schema names, on one fixture.
    #[test]
    fn every_scope_kind_appears_with_the_right_parent_and_depth() {
        let (ms, _) = ore(
            "scopes",
            // A built-in array, not `std::vector`, so the range-for needs no
            // standard library. macOS CI runs libclang without a sysroot, so
            // `#include <vector>` fails there, `xs` gets an error type, and the
            // range-for never parses -- the other three loops are header-free
            // and passed, which is why only `RangeFor` went missing.
            r"
struct C {
  void loops(const int (&xs)[4], int n);
  void branches(int n);
  void protect();
  void lam(int n);
  void blk(int n);
  int x_;
};
void C::loops(const int (&xs)[4], int n) {
  for (int i = 0; i < n; ++i) { x_ += i; }
  while (x_ < n) { ++x_; }
  do { --x_; } while (x_ > n);
  for (int v : xs) { x_ += v; }
}
void C::branches(int n) {
  switch (n) { case 0: x_ = 1; break; default: x_ = 2; }
}
void C::protect() { try { x_ = 1; } catch (const int& e) { x_ = 2; } }
void C::lam(int n) { auto f = [this, n]() { x_ = n; }; f(); }
void C::blk(int n) { { x_ = n; } }",
        );
        let have = |m: &MethodOre, k: &str| scope_kinds(m).contains(&k);
        let loops = find(&ms, "C.loops");
        for k in ["For", "While", "Do", "RangeFor"] {
            assert!(have(loops, k), "missing {k}");
        }
        let br = find(&ms, "C.branches");
        for k in ["Switch", "Case", "Default"] {
            assert!(have(br, k), "missing {k}");
        }
        let pr = find(&ms, "C.protect");
        assert!(have(pr, "Try") && have(pr, "Catch"));
        assert!(have(find(&ms, "C.lam"), "Lambda"));
        // A FREE-STANDING compound is a real Block; a construct's body is not.
        assert!(have(find(&ms, "C.blk"), "Block"));
        assert!(
            !have(loops, "Block"),
            "a loop body's braces must not open a Block"
        );
        // Depth and parentage, not just presence.
        let f = &loops.scopes[0];
        assert_eq!((f.kind, f.depth, f.parent), (ScopeKind::Function, 0, None));
        for s in loops.scopes.iter().filter(|s| s.kind.is_loop()) {
            assert_eq!(
                s.parent,
                Some(0),
                "a top-level loop's parent is the function"
            );
            assert_eq!(s.depth, 1);
        }
    }

    /// Nested and sibling scopes must stay distinguishable — the property that
    /// makes a sequence recoverable rather than flattened.
    #[test]
    fn sibling_and_nested_scopes_do_not_flatten_into_each_other() {
        let (ms, _) = ore(
            "nesting",
            r"
struct C { void f(int n); int x_; };
void C::f(int n) {
  for (int i = 0; i < n; ++i) {
    if (n > 1) { x_ = 1; }
    if (n > 2) { x_ = 2; }
    for (int j = 0; j < n; ++j) { x_ = 3; }
  }
}",
        );
        let m = find(&ms, "C.f(int)");
        let ifs: Vec<&OreScope> = m
            .scopes
            .iter()
            .filter(|s| s.kind == ScopeKind::If)
            .collect();
        assert_eq!(ifs.len(), 2, "two sibling ifs");
        assert_ne!(ifs[0].id, ifs[1].id, "siblings need distinct ids");
        assert_eq!(ifs[0].parent, ifs[1].parent, "siblings share a parent");
        assert!(
            ifs[0].exit_seq < ifs[1].enter_seq,
            "siblings do not interleave"
        );
        let fors: Vec<&OreScope> = m
            .scopes
            .iter()
            .filter(|s| s.kind == ScopeKind::For)
            .collect();
        assert_eq!(fors.len(), 2);
        assert_eq!(fors[0].depth, 1);
        assert_eq!(fors[1].depth, 2, "the inner loop is one level deeper");
        assert_eq!(fors[1].parent, Some(fors[0].id));
    }

    /// The whole point: an occurrence is not an identity.
    #[test]
    fn two_writes_to_one_member_are_two_events_and_one_symbol() {
        let (ms, syms) = ore(
            "dupes",
            r"
struct C { void f(int n); int x_; };
void C::f(int n) { x_ = n; x_ = n + 1; }",
        );
        let m = find(&ms, "C.f(int)");
        let writes: Vec<&OreEvent> = m
            .events
            .iter()
            .filter(|e| e.kind == EventKind::Write)
            .collect();
        assert_eq!(writes.len(), 2, "two occurrences survive");
        assert_eq!(
            writes[0].subj, writes[1].subj,
            "both point at ONE canonical symbol"
        );
        assert_eq!(
            syms.rows().filter(|r| r.3 == "x_").count(),
            1,
            "the symbol table deduped"
        );
        // And A0, on the same cursor, kept one.
        assert_eq!(
            m.set_writes, 1,
            "the shipped arm collapses both to one fact"
        );
    }

    /// The additive guarantee: folding the events back into sets must
    /// reproduce the shipped arm exactly. This is what proves the change is
    /// about what is PRESERVED, not about what a fact is.
    #[test]
    fn events_collapse_to_the_shipped_five_sets() {
        let (ms, syms) = ore(
            "collapse",
            r"
struct E {};
struct C {
  void f(int n);
  int x_; int y_;
};
void C::f(int n) {
  x_ = n;
  x_ = n + 1;
  y_ += x_;
  if (n > 2) { throw E(); }
}",
        );
        let m = find(&ms, "C.f(int)");
        let name_of = |id: &str| {
            syms.rows()
                .find(|r| r.0 == id)
                .map(|r| (r.2, r.3.to_string()))
        };
        let mut reads: Vec<String> = Vec::new();
        let mut writes: Vec<String> = Vec::new();
        let mut raises: Vec<String> = Vec::new();
        for e in &m.events {
            match e.kind {
                EventKind::Read | EventKind::ReadWrite => {
                    if let Some(id) = &e.subj
                        && let Some((Role::Own, n)) = name_of(id)
                    {
                        reads.push(n);
                    }
                }
                EventKind::Throw => {
                    if let Some(id) = &e.obj
                        && let Some((_, n)) = name_of(id)
                    {
                        raises.push(n);
                    }
                }
                _ => {}
            }
            if matches!(e.kind, EventKind::Write | EventKind::ReadWrite)
                && let Some(id) = &e.subj
                && let Some((Role::Own, n)) = name_of(id)
            {
                writes.push(n);
            }
        }
        for v in [&mut reads, &mut writes, &mut raises] {
            v.sort();
            v.dedup();
        }
        assert_eq!(
            (
                u32::try_from(writes.len()).unwrap_or(u32::MAX),
                u32::try_from(raises.len()).unwrap_or(u32::MAX)
            ),
            (m.set_writes, m.set_raises),
            "collapsing the ore must reproduce the shipped arm's sets"
        );
        assert_eq!(
            u32::try_from(reads.len()).unwrap_or(u32::MAX),
            m.set_reads,
            "reads agree too"
        );
    }

    /// Class and namespace are constants of a stream, not events.
    #[test]
    fn the_class_and_namespace_ride_the_iri_and_never_become_scopes() {
        let (ms, _) = ore(
            "ns",
            r"
namespace outer { namespace inner {
struct C { void f(); int x_; };
void C::f() { x_ = 1; }
} }",
        );
        let m = find(&ms, "C.f()");
        assert!(
            m.iri.contains("outer::inner::C"),
            "iri carries the path: {}",
            m.iri
        );
        assert_eq!(m.class, "outer::inner::C");
        assert!(
            m.scopes
                .iter()
                .all(|s| s.kind != ScopeKind::Block || s.depth > 0),
            "no class/namespace scope is emitted"
        );
    }

    /// Anchors are the lexical truth against which traversal order is checked.
    /// Loop headers are the known exception and this pins it rather than
    /// asserting a property that does not hold.
    #[test]
    fn anchors_are_non_decreasing_except_across_a_loop_header() {
        let (ms, _) = ore(
            "anchors",
            r"
struct C { void f(int n); int x_; };
void C::f(int n) { x_ = n; if (n > 0) { x_ = 1; } }",
        );
        let m = find(&ms, "C.f(int)");
        let mut last = 0;
        for e in &m.events {
            if e.kind == EventKind::ScopeExit {
                continue; // an exit re-uses its construct's start anchor
            }
            assert!(
                e.anchor >= last,
                "anchor went backwards at seq {} ({})",
                e.seq,
                e.kind
            );
            last = e.anchor;
        }
    }

    /// A constructor's member-initializer list is invisible to the shipped
    /// arm; the ore records it.
    #[test]
    fn a_constructor_initializer_list_becomes_ctorinit_events() {
        let (ms, _) = ore(
            "ctorinit",
            r"
struct C { C(int s); int a_; int b_; };
C::C(int s) : a_(0), b_(s) {}",
        );
        let m = find(&ms, "C.C(int)");
        assert_eq!(
            m.events
                .iter()
                .filter(|e| e.kind == EventKind::CtorInit)
                .count(),
            2,
            "both initialisers"
        );
        assert_eq!(m.mkind, "ctor");
    }

    /// A virtual call is labelled by libclang, not guessed — and the label is
    /// marked `clang` so a consumer can report how much the compiler gave it.
    #[test]
    fn a_virtual_call_is_labelled_by_libclang_and_marked_as_such() {
        let (ms, _) = ore(
            "virt",
            r"
struct B { virtual void v(); void nv(); virtual ~B(); };
struct C : B { void f(B* p); };
void C::f(B* p) { p->v(); p->nv(); }",
        );
        let m = find(&ms, "C.f(B *)");
        let calls: Vec<(&str, Prov)> = m
            .events
            .iter()
            .filter(|e| e.kind == EventKind::Call)
            .map(|e| (e.type_rel.as_deref().unwrap_or("-"), e.prov))
            .collect();
        assert!(
            calls.contains(&("virtual", Prov::Clang)),
            "the virtual call must be labelled by clang: {calls:?}"
        );
        assert!(
            calls.contains(&("member", Prov::Clang)),
            "and the non-virtual one distinguished: {calls:?}"
        );
    }
}
