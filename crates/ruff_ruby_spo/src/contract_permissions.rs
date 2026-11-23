//! Contract **permission declarations** — the `ActiveModel` *contract layer*
//! (`BaseContract`), not the `ActiveRecord` callback/association layer.
//!
//! `OpenProject` contracts say which permission a user needs before an attribute
//! is writable:
//!
//! ```ruby
//! attribute :status_id, permission: %i[edit_work_packages change_work_package_status]
//! default_attribute_permission :add_work_packages
//! attribute_permission :project_id, :move_work_packages
//! attribute_alias :project_phase_definition_id, :project_phase_id
//! ```
//!
//! This arm harvests those declarations per contract class, with source
//! anchors, and resolves them the way `app/contracts/base_contract.rb` does.
//! It is extraction only: nothing is executed, no permission is checked.
//! `writable:` guards are the sibling arm ([`crate::extract_contract_guards`]).
//!
//! # DSL semantics preserved (all cites are `app/contracts/base_contract.rb`)
//!
//! - **nil / false is a no-op** (line 97, `return unless permission`). An
//!   explicit `permission: nil` does not erase a permission inherited from a
//!   superclass, and `permission: false` likewise. No declaration is recorded.
//! - **A value is stored as `Array(permission)`** (line 99), so `:a` and `[:a]`
//!   are the same. The list means *any of* (lines 259-266, `permissions.any?`):
//!   [`PermissionValue::AnyOf`] holds alternatives, never a conjunction. An
//!   empty list is truthy in Ruby, is recorded, and can never be satisfied; it
//!   is not "no permission".
//! - **Later declaration in one class wins** (hash assignment, line 99).
//! - **Resolution order** (lines 251-255): the canonical attribute name (the
//!   name with one trailing `_id` removed), then `<canonical>_id`, then the
//!   `default_attribute_permission` (stored under the key `:default_permission`,
//!   line 93). See [`ContractPermissions::lookup`].
//! - **Subclass overrides superclass per key** (`reverse_merge!`, lines 183-184
//!   and 207): keys are merged, a subclass key wins, the values (arrays) are
//!   replaced whole, never unioned. Because the key is the attribute *as
//!   written*, a parent entry under `:status` shadows a child entry under
//!   `:status_id` at lookup time (canonical name is tried first).
//! - **Only the superclass chain is walked** (lines 203-205): modules are not
//!   traversed by `collect_ancestor_attributes`, and the chain stops *before*
//!   `BaseContract` itself, so declarations written in `BaseContract`'s own
//!   body never count.
//! - **Mixins contribute only through code that runs in the including class**,
//!   i.e. an `ActiveSupport::Concern` `included do … end` block, whose
//!   statements run in the body of the class that calls `include`. They are
//!   applied at the position of the `include` call; within one `include A, B`
//!   call `B` runs first so `A` wins (Ruby includes in reverse order). A
//!   module's own `include` of another concern is applied before its
//!   `included` block (Concern dependencies). Anything else a module could do
//!   (`self.included(base)`, a module that was not scanned, a dynamic
//!   `include`) is an explicit [`ContractGap`], never silence.
//!
//! # Typed uncertainty
//!
//! A permission value that is not a literal symbol or an array of literal
//! symbols (a constant, a call, a splat, a string, a lambda) is
//! [`PermissionValue::Unlowered`] with the source text. Declarations that
//! cannot even be attributed to an attribute (dynamic attribute name,
//! `**opts`, `if`-wrapped or loop-generated calls, unscanned mixins) are
//! [`ContractGap`]s. [`ContractPermissions::lookup`] keeps four outcomes
//! apart: declared, inherited/default, proven absent and unresolved.
//!
//! Reopened classes are merged in sorted file order, not Ruby's load order.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use lib_ruby_parser::Node;
use lib_ruby_parser::nodes::Send as SendNode;
use lib_ruby_parser::traverse::visitor::{Visitor, visit_send};

use crate::guards::{
    ClassEntry, Lowerer, Mixin, MixinArg, Parsed, Scan, collect_rb, const_path, flat_source,
    line_of, parse, snippet_of,
};

/// The name of the class `collect_ancestor_attributes` stops in front of.
const ROOT_CONTRACT: &str = "BaseContract";

/// DSL calls this arm reads.
const DSL: [&str; 4] = [
    "attribute",
    "attribute_permission",
    "default_attribute_permission",
    "attribute_alias",
];

/// What the permission value of one declaration lowers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionValue {
    /// The user needs **one of** these (OR). May be empty (never satisfiable).
    AnyOf(Vec<String>),
    /// Not a literal symbol / symbol array: the permission cannot be known.
    Unlowered {
        /// Why (node kind).
        reason: String,
        /// The value expression as written, whitespace-flattened.
        source_text: String,
    },
}

/// How an attribute permission was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclForm {
    /// `attribute :x, permission: …`
    AttributeOption,
    /// `attribute_permission :x, …`
    AttributePermission,
}

/// Where a declaration is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclSource {
    /// The class or module whose body contains the text.
    pub scope: String,
    /// Path relative to the scan root, `/`-separated.
    pub file: String,
    /// 1-based line of the call.
    pub line: usize,
}

/// One per-attribute permission declaration after override resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionDecl {
    /// The attribute as written (`status_id`, not canonicalised).
    pub attribute: String,
    pub value: PermissionValue,
    pub form: DeclForm,
    pub source: DeclSource,
    /// The class in the superclass chain whose body applied it. For a
    /// mixin-provided declaration this is the including class.
    pub applied_in: String,
    /// Application order across the whole chain; a larger value ran later.
    pub order: usize,
}

/// `default_attribute_permission …`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultPermissionDecl {
    pub value: PermissionValue,
    pub source: DeclSource,
    pub applied_in: String,
    pub order: usize,
}

/// `attribute_alias db, outside` (base_contract.rb:65-69: `aliases[db] = outside`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasDecl {
    pub db: String,
    pub outside: String,
    pub source: DeclSource,
    pub applied_in: String,
    pub order: usize,
}

/// Something that may declare permissions or aliases but could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractGap {
    pub reason: String,
    pub source: DeclSource,
    pub order: usize,
}

/// The permission surface of one contract class, superclass chain resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractPermissions {
    /// Namespaced class name.
    pub class: String,
    /// Effective per-attribute declarations, keyed by attribute as written.
    pub permissions: BTreeMap<String, PermissionDecl>,
    pub default: Option<DefaultPermissionDecl>,
    /// Effective aliases keyed by the `db` attribute.
    pub aliases: BTreeMap<String, AliasDecl>,
    /// Undeterminable contributions, in application order.
    pub gaps: Vec<ContractGap>,
}

/// The four outcomes of [`ContractPermissions::lookup`], never collapsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionState<'a> {
    /// (a) An attribute-specific permission declared by this contract class.
    Explicit(&'a PermissionDecl),
    /// (b) An attribute-specific permission inherited from a superclass.
    Inherited(&'a PermissionDecl),
    /// (b) No attribute-specific entry; the default permission applies.
    Default(&'a DefaultPermissionDecl),
    /// (c) Proven absent: nothing declares a permission anywhere in the
    /// chain and nothing unreadable could have, so no restriction applies
    /// (`base_contract.rb:257`, `next unless permissions`).
    ProvenAbsent,
    /// (d) Cannot be determined safely.
    Unresolved { reason: String },
}

impl ContractPermissions {
    /// Permission requirement of `attribute`, in Ruby's order
    /// (base_contract.rb:251-255): canonical name, then `<canonical>_id`, then
    /// the default.
    ///
    /// An entry whose value is unlowered, or that a later unreadable
    /// declaration could have overridden, gives [`PermissionState::Unresolved`].
    /// When no entry is found, any gap makes the answer unresolved, because an
    /// attribute-specific entry beats the default regardless of order.
    #[must_use]
    pub fn lookup(&self, attribute: &str) -> PermissionState<'_> {
        let canonical = attribute.strip_suffix("_id").unwrap_or(attribute);
        let with_id = format!("{canonical}_id");
        let (found, via_id) = match self.permissions.get(canonical) {
            Some(decl) => (Some(decl), false),
            None => (self.permissions.get(&with_id), true),
        };
        if let Some(decl) = found {
            if let PermissionValue::Unlowered {
                reason,
                source_text,
            } = &decl.value
            {
                return PermissionState::Unresolved {
                    reason: format!("{reason}: {source_text}"),
                };
            }
            // Only the `_id` entry is known: an unreadable declaration of any
            // position could supply the canonical entry, which is looked up
            // first (base_contract.rb:251-255).
            if via_id && let Some(gap) = self.gaps.first() {
                return PermissionState::Unresolved {
                    reason: format!(
                        "an unreadable declaration may supply `{canonical}`, which takes precedence over `{with_id}`: {}",
                        gap.reason
                    ),
                };
            }
            if let Some(gap) = self.gaps.iter().find(|g| g.order > decl.order) {
                return PermissionState::Unresolved {
                    reason: format!(
                        "a later unreadable declaration may override: {}",
                        gap.reason
                    ),
                };
            }
            return if decl.applied_in == self.class {
                PermissionState::Explicit(decl)
            } else {
                PermissionState::Inherited(decl)
            };
        }
        if let Some(gap) = self.gaps.first() {
            return PermissionState::Unresolved {
                reason: format!("an unreadable declaration may supply it: {}", gap.reason),
            };
        }
        match &self.default {
            Some(default) => match &default.value {
                PermissionValue::AnyOf(_) => PermissionState::Default(default),
                PermissionValue::Unlowered {
                    reason,
                    source_text,
                } => PermissionState::Unresolved {
                    reason: format!("default permission {reason}: {source_text}"),
                },
            },
            None => PermissionState::ProvenAbsent,
        }
    }
}

// ---------------------------------------------------------------------------
// Scan-time events
// ---------------------------------------------------------------------------

/// Where a statement is written, by file index (resolved to a path later).
#[derive(Debug, Clone)]
pub(crate) struct SrcRef {
    pub(crate) file: usize,
    pub(crate) scope: String,
    pub(crate) line: usize,
}

/// One statement of a class or module body that matters here, in body order.
#[derive(Debug, Clone)]
pub(crate) enum ContractEvent {
    Permission {
        attribute: String,
        value: PermissionValue,
        form: DeclForm,
        at: SrcRef,
    },
    Default {
        value: PermissionValue,
        at: SrcRef,
    },
    Alias {
        db: String,
        outside: String,
        at: SrcRef,
    },
    /// An `include` call; `mixin` indexes `ClassEntry::mixins`.
    Include {
        mixin: usize,
        at: SrcRef,
    },
    /// An `include` inside `included do … end`; `mixin` indexes
    /// `ClassEntry::included_mixins`.
    IncludeInIncluder {
        mixin: usize,
        at: SrcRef,
    },
    Gap {
        reason: String,
        at: SrcRef,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[expect(clippy::enum_variant_names, reason = "names the body kind")]
enum Ctx {
    ClassBody,
    ModuleBody,
    /// Statements of a module's `included do … end`: they run in the includer.
    IncludedBody,
}

/// Record one direct statement of a class or module body (called by the guard
/// scan for every such statement; it never changes guard output).
pub(crate) fn record_statement(
    entry: &mut ClassEntry<'_>,
    file: usize,
    src: &str,
    scope: &str,
    ns: &[String],
    stmt: &Node,
) {
    let ctx = if entry.is_module {
        Ctx::ModuleBody
    } else {
        Ctx::ClassBody
    };
    // `included do … end` of a module.
    if ctx == Ctx::ModuleBody
        && let Node::Block(block) = stmt
        && let Node::Send(call) = &*block.call
        && call.recv.is_none()
        && call.method_name == "included"
        && call.args.is_empty()
    {
        let mut out = Vec::new();
        match block.body.as_deref() {
            Some(Node::Begin(b)) => {
                for inner in &b.statements {
                    included_statement(entry, file, src, scope, ns, inner, &mut out);
                }
            }
            Some(inner) => included_statement(entry, file, src, scope, ns, inner, &mut out),
            None => {}
        }
        entry.included_events.extend(out);
        return;
    }
    // `def self.included(base)` runs arbitrary code in the includer.
    if ctx == Ctx::ModuleBody
        && let Node::Defs(def) = stmt
        && def.name == "included"
    {
        entry.events.push(ContractEvent::Gap {
            reason: "module defines `self.included`, which can declare attributes".to_string(),
            at: src_ref(file, src, scope, stmt),
        });
        return;
    }
    let mut out = Vec::new();
    classify(stmt, file, src, scope, ctx, &mut out);
    entry.events.extend(out);
}

/// One statement of an `included do … end` block. An `include` there runs in
/// the includer when the block runs, so it is expanded at that position.
fn included_statement(
    entry: &mut ClassEntry<'_>,
    file: usize,
    src: &str,
    scope: &str,
    ns: &[String],
    stmt: &Node,
    out: &mut Vec<ContractEvent>,
) {
    if let Node::Send(send) = stmt
        && receiverless(send)
        && send.method_name == "include"
        && !send.args.is_empty()
    {
        let names = send
            .args
            .iter()
            .map(|arg| match const_path(arg) {
                Some(path) => MixinArg::Const(path),
                None => MixinArg::Dynamic(format!(
                    "dynamic `include` argument `{}`",
                    snippet_of(src, arg)
                )),
            })
            .collect();
        entry.included_mixins.push(Mixin {
            prepend: false,
            ns: ns.to_vec(),
            names,
        });
        out.push(ContractEvent::IncludeInIncluder {
            mixin: entry.included_mixins.len() - 1,
            at: src_ref(file, src, scope, stmt),
        });
        return;
    }
    classify(stmt, file, src, scope, Ctx::IncludedBody, out);
}

/// No receiver, or an explicit `self`: both call the class-level DSL method.
fn receiverless(send: &SendNode) -> bool {
    matches!(send.recv.as_deref(), None | Some(Node::Self_(_)))
}

fn src_ref(file: usize, src: &str, scope: &str, node: &Node) -> SrcRef {
    SrcRef {
        file,
        scope: scope.to_string(),
        line: line_of(src, node.expression().begin),
    }
}

fn classify(
    stmt: &Node,
    file: usize,
    src: &str,
    scope: &str,
    ctx: Ctx,
    out: &mut Vec<ContractEvent>,
) {
    match stmt {
        Node::Def(_) | Node::Class(_) | Node::Module(_) | Node::Defs(_) => {}
        Node::Send(send) if receiverless(send) => {
            if DSL.contains(&send.method_name.as_str()) {
                dsl_call(send, stmt, file, src, scope, ctx, out);
            } else if !matches!(send.method_name.as_str(), "include" | "prepend") {
                nested_gaps(&send.args, file, src, scope, out);
            } else if send.recv.is_some() || ctx == Ctx::IncludedBody {
                // The scan expands only a receiverless `include` of a class or
                // module body (and of an `included` block, handled above).
                out.push(ContractEvent::Gap {
                    reason: format!(
                        "`{}` written here is not expanded; the module may declare permissions",
                        flat_source(src, stmt)
                    ),
                    at: src_ref(file, src, scope, stmt),
                });
            }
        }
        Node::Block(block) => {
            if let Node::Send(send) = &*block.call
                && receiverless(send)
                && DSL.contains(&send.method_name.as_str())
            {
                dsl_call(send, stmt, file, src, scope, ctx, out);
            } else {
                nested_gaps(std::slice::from_ref(stmt), file, src, scope, out);
            }
        }
        other => nested_gaps(std::slice::from_ref(other), file, src, scope, out),
    }
}

/// Gaps for DSL calls hidden below a statement this arm does not unwrap
/// (`if`, `case`, a loop block, …).
fn nested_gaps(nodes: &[Node], file: usize, src: &str, scope: &str, out: &mut Vec<ContractEvent>) {
    let mut finder = DslFinder {
        src,
        found: Vec::new(),
    };
    for node in nodes {
        finder.visit(node);
    }
    for (name, line) in finder.found {
        out.push(ContractEvent::Gap {
            reason: format!(
                "`{name}` is called inside a conditional, loop or other construct this arm does not unwrap"
            ),
            at: SrcRef {
                file,
                scope: scope.to_string(),
                line,
            },
        });
    }
}

struct DslFinder<'s> {
    src: &'s str,
    found: Vec<(String, usize)>,
}

impl Visitor for DslFinder<'_> {
    fn on_send(&mut self, node: &SendNode) {
        if receiverless(node)
            && DSL.contains(&node.method_name.as_str())
            && can_carry_permission(node)
        {
            self.found.push((
                node.method_name.clone(),
                line_of(self.src, node.expression_l.begin),
            ));
        }
        visit_send(self, node);
    }
}

/// False for an `attribute` call that provably carries no permission (only
/// literal options, none of them `permission:`): a hidden one cannot change
/// any permission, so it is no gap. Every other DSL call can.
fn can_carry_permission(send: &SendNode) -> bool {
    if send.method_name != "attribute" {
        return true;
    }
    match send.args.as_slice() {
        [_] => false,
        [_, .., Node::Kwargs(k)] => k.pairs.iter().any(is_permission_or_splat),
        [_, .., Node::Hash(h)] => h.pairs.iter().any(is_permission_or_splat),
        _ => true,
    }
}

fn is_permission_or_splat(pair: &Node) -> bool {
    match pair {
        Node::Pair(p) => {
            !matches!(&*p.key, Node::Sym(k) if k.name.to_string_lossy() != "permission")
        }
        _ => true,
    }
}

/// Result of lowering one permission value.
enum Lowered {
    /// nil / false: Ruby ignores the call (`base_contract.rb:97`).
    Noop,
    Value(PermissionValue),
}

fn lower_permission(node: &Node, src: &str) -> Lowered {
    let unlowered = |reason: &str| {
        Lowered::Value(PermissionValue::Unlowered {
            reason: reason.to_string(),
            source_text: flat_source(src, node),
        })
    };
    match node {
        Node::Nil(_) | Node::False(_) => Lowered::Noop,
        Node::Sym(s) => Lowered::Value(PermissionValue::AnyOf(vec![s.name.to_string_lossy()])),
        Node::Array(a) => {
            let mut names = Vec::new();
            for element in &a.elements {
                match element {
                    Node::Sym(s) => names.push(s.name.to_string_lossy()),
                    other => {
                        return unlowered(&format!(
                            "array element is not a literal symbol ({})",
                            other.str_type()
                        ));
                    }
                }
            }
            Lowered::Value(PermissionValue::AnyOf(names))
        }
        other => unlowered(&format!(
            "permission is not a literal symbol or symbol array ({})",
            other.str_type()
        )),
    }
}

fn dsl_call(
    send: &SendNode,
    whole: &Node,
    file: usize,
    src: &str,
    scope: &str,
    ctx: Ctx,
    out: &mut Vec<ContractEvent>,
) {
    let at = SrcRef {
        file,
        scope: scope.to_string(),
        line: line_of(src, send.expression_l.begin),
    };
    let gap = |reason: String| ContractEvent::Gap {
        reason,
        at: at.clone(),
    };
    if ctx == Ctx::ModuleBody {
        out.push(gap(format!(
            "`{}` is called directly in a module body",
            send.method_name
        )));
        return;
    }
    let sym = |node: Option<&Node>| match node {
        Some(Node::Sym(s)) => Some(s.name.to_string_lossy()),
        _ => None,
    };
    match send.method_name.as_str() {
        "attribute" => {
            let name = sym(send.args.first());
            let pairs = match send.args.last() {
                Some(Node::Kwargs(k)) if send.args.len() >= 2 => Some(&k.pairs),
                Some(Node::Hash(h)) if send.args.len() >= 2 => Some(&h.pairs),
                _ => None,
            };
            let Some(pairs) = pairs else {
                if send.args.len() >= 2 {
                    out.push(gap(format!(
                        "options of `attribute` are not a literal hash: {}",
                        flat_source(src, whole)
                    )));
                }
                return;
            };
            let found = pairs.iter().enumerate().rev().find_map(|(i, pair)| match pair {
                Node::Pair(p)
                    if matches!(&*p.key, Node::Sym(k) if k.name.to_string_lossy() == "permission") =>
                {
                    Some((i, &*p.value))
                }
                _ => None,
            });
            let splat_after = |from: usize| {
                pairs[from..]
                    .iter()
                    .any(|pair| matches!(pair, Node::Kwsplat(_)))
            };
            let Some(name) = name else {
                if found.is_some() || splat_after(0) {
                    out.push(gap(format!(
                        "`attribute` with a non-literal name may carry a permission: {}",
                        flat_source(src, whole)
                    )));
                }
                return;
            };
            match found {
                None if splat_after(0) => out.push(gap(format!(
                    "`**` splat in `attribute :{name}` may carry `permission:`"
                ))),
                None => {}
                Some((i, value)) if splat_after(i + 1) => {
                    out.push(ContractEvent::Permission {
                        attribute: name,
                        value: PermissionValue::Unlowered {
                            reason: "a `**` splat after `permission:` can override it".to_string(),
                            source_text: flat_source(src, value),
                        },
                        form: DeclForm::AttributeOption,
                        at,
                    });
                }
                Some((_, value)) => {
                    if let Lowered::Value(value) = lower_permission(value, src) {
                        out.push(ContractEvent::Permission {
                            attribute: name,
                            value,
                            form: DeclForm::AttributeOption,
                            at,
                        });
                    }
                }
            }
        }
        "attribute_permission" => match (send.args.as_slice(), sym(send.args.first())) {
            ([_, value], Some(name)) => {
                if let Lowered::Value(value) = lower_permission(value, src) {
                    out.push(ContractEvent::Permission {
                        attribute: name,
                        value,
                        form: DeclForm::AttributePermission,
                        at,
                    });
                }
            }
            _ => out.push(gap(format!(
                "`attribute_permission` with a non-literal attribute or unexpected arguments: {}",
                flat_source(src, whole)
            ))),
        },
        "default_attribute_permission" => match send.args.as_slice() {
            [value] => {
                if let Lowered::Value(value) = lower_permission(value, src) {
                    out.push(ContractEvent::Default { value, at });
                }
            }
            _ => out.push(gap(format!(
                "`default_attribute_permission` with unexpected arguments: {}",
                flat_source(src, whole)
            ))),
        },
        _ => match (
            send.args.as_slice(),
            sym(send.args.first()),
            sym(send.args.get(1)),
        ) {
            ([_, _], Some(db), Some(outside)) if db != outside => {
                out.push(ContractEvent::Alias { db, outside, at });
            }
            _ => out.push(gap(format!(
                "`attribute_alias` with non-literal or identical names: {}",
                flat_source(src, whole)
            ))),
        },
    }
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// Harvest the permission declarations of every contract class under `root`.
///
/// One entry per scanned class (not module), sorted by class name. The class
/// named `BaseContract` is skipped: Ruby never collects from it (see the
/// module docs).
#[must_use]
pub fn extract_contract_permissions(root: &Path) -> Vec<ContractPermissions> {
    let mut paths = Vec::new();
    collect_rb(root, &mut paths);
    paths.sort();
    let files: Vec<Parsed> = paths.iter().map(|p| parse(root, p)).collect();
    let mut scan = Scan::default();
    for (index, file) in files.iter().enumerate() {
        if let Some(ast) = &file.ast {
            scan.walk(index, &file.src, ast, &[]);
        }
    }
    let lowerer = Lowerer {
        files: &files,
        classes: &scan.classes,
    };
    let mut names: Vec<&String> = scan
        .classes
        .iter()
        .filter(|(name, e)| !e.is_module && name.as_str() != ROOT_CONTRACT)
        .map(|(name, _)| name)
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|name| resolve_class(&lowerer, name))
        .collect()
}

struct State<'l, 'a> {
    lowerer: &'l Lowerer<'a>,
    out: ContractPermissions,
    order: usize,
}

impl State<'_, '_> {
    fn source(&self, at: &SrcRef) -> DeclSource {
        DeclSource {
            scope: at.scope.clone(),
            file: self
                .lowerer
                .files
                .get(at.file)
                .map_or_else(String::new, |f| f.rel.clone()),
            line: at.line,
        }
    }

    fn tick(&mut self) -> usize {
        self.order += 1;
        self.order
    }

    fn gap(&mut self, reason: String, at: &SrcRef) {
        let source = self.source(at);
        let order = self.tick();
        self.out.gaps.push(ContractGap {
            reason,
            source,
            order,
        });
    }

    fn apply(&mut self, event: &ContractEvent, applied_in: &str) {
        match event {
            ContractEvent::Permission {
                attribute,
                value,
                form,
                at,
            } => {
                let source = self.source(at);
                let order = self.tick();
                // Plain assignment: a later declaration replaces the earlier
                // one whole (base_contract.rb:99).
                self.out.permissions.insert(
                    attribute.clone(),
                    PermissionDecl {
                        attribute: attribute.clone(),
                        value: value.clone(),
                        form: *form,
                        source,
                        applied_in: applied_in.to_string(),
                        order,
                    },
                );
            }
            ContractEvent::Default { value, at } => {
                let source = self.source(at);
                let order = self.tick();
                self.out.default = Some(DefaultPermissionDecl {
                    value: value.clone(),
                    source,
                    applied_in: applied_in.to_string(),
                    order,
                });
            }
            ContractEvent::Alias { db, outside, at } => {
                let source = self.source(at);
                let order = self.tick();
                self.out.aliases.insert(
                    db.clone(),
                    AliasDecl {
                        db: db.clone(),
                        outside: outside.clone(),
                        source,
                        applied_in: applied_in.to_string(),
                        order,
                    },
                );
            }
            ContractEvent::Gap { reason, at } => self.gap(reason.clone(), at),
            ContractEvent::Include { .. } | ContractEvent::IncludeInIncluder { .. } => {}
        }
    }

    /// Apply a class body in order, expanding `include` where it is written.
    fn apply_class(&mut self, class: &str) {
        let Some(entry) = self.lowerer.classes.get(class) else {
            return;
        };
        for event in &entry.events {
            match event {
                ContractEvent::Include { mixin, at } => {
                    let mut stack = Vec::new();
                    self.include(&entry.mixins[*mixin], at, class, &mut stack);
                }
                other => self.apply(other, class),
            }
        }
    }

    fn include(&mut self, mixin: &Mixin, at: &SrcRef, applied_in: &str, stack: &mut Vec<String>) {
        // Ruby includes the arguments of one call in reverse order.
        for arg in mixin.names.iter().rev() {
            match arg {
                MixinArg::Const(name) => match self.lowerer.resolve(&mixin.ns, name) {
                    Some(found) => self.module(&found, applied_in, stack),
                    None => self.gap(
                        format!(
                            "module `{name}` is not scanned; an `included` hook could declare permissions"
                        ),
                        at,
                    ),
                },
                MixinArg::Dynamic(what) => self.gap(what.clone(), at),
            }
        }
    }

    fn module(&mut self, name: &str, applied_in: &str, stack: &mut Vec<String>) {
        if stack.iter().any(|n| n == name) {
            return;
        }
        let Some(entry) = self.lowerer.classes.get(name) else {
            return;
        };
        stack.push(name.to_string());
        // Concern dependencies first, then the `included` block.
        for event in &entry.events {
            match event {
                ContractEvent::Include { mixin, at } => {
                    self.include(&entry.mixins[*mixin], at, applied_in, stack);
                }
                other => self.apply(other, applied_in),
            }
        }
        for event in &entry.included_events {
            match event {
                ContractEvent::IncludeInIncluder { mixin, at } => {
                    self.include(&entry.included_mixins[*mixin], at, applied_in, stack);
                }
                other => self.apply(other, applied_in),
            }
        }
        stack.pop();
    }
}

fn resolve_class(lowerer: &Lowerer<'_>, class: &str) -> ContractPermissions {
    // The superclass chain, subclass first, stopping before `BaseContract`
    // (base_contract.rb:203-205).
    let mut chain = vec![class.to_string()];
    let mut seen: HashSet<String> = HashSet::from([class.to_string()]);
    let mut unscanned: Option<(String, String)> = None;
    let mut current = class.to_string();
    while let Some((ns, sup)) = lowerer
        .classes
        .get(&current)
        .and_then(|e| e.superclass.clone())
    {
        match lowerer.resolve(&ns, &sup) {
            Some(found) if found == ROOT_CONTRACT => break,
            Some(found) => {
                if !seen.insert(found.clone()) {
                    break;
                }
                chain.push(found.clone());
                current = found;
            }
            None => {
                if sup != ROOT_CONTRACT {
                    unscanned = Some((current.clone(), sup));
                }
                break;
            }
        }
    }

    let mut state = State {
        lowerer,
        out: ContractPermissions {
            class: class.to_string(),
            permissions: BTreeMap::new(),
            default: None,
            aliases: BTreeMap::new(),
            gaps: Vec::new(),
        },
        order: 0,
    };
    if let Some((from, sup)) = unscanned {
        // Root-most position: applied before everything else.
        // No file or line: the text lives outside the scan (`file` is empty).
        let at = SrcRef {
            file: usize::MAX,
            scope: from,
            line: 0,
        };
        state.gap(
            format!("superclass `{sup}` is not scanned; it may declare permissions"),
            &at,
        );
    }
    // Root-most first, so a subclass key overwrites: `reverse_merge!` semantics.
    for name in chain.iter().rev() {
        state.apply_class(name);
    }
    state.out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scan(case: &str, files: &[(&str, &str)]) -> Vec<ContractPermissions> {
        let root =
            std::env::temp_dir().join(format!("ruff_ruby_spo_perms_{}_{case}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for (rel, content) in files {
            let path = root.join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, content).unwrap();
        }
        let out = extract_contract_permissions(&root);
        let _ = fs::remove_dir_all(&root);
        out
    }

    fn class<'a>(all: &'a [ContractPermissions], name: &str) -> &'a ContractPermissions {
        all.iter()
            .find(|c| c.class == name)
            .unwrap_or_else(|| panic!("no class {name}"))
    }

    fn any_of(items: &[&str]) -> PermissionValue {
        PermissionValue::AnyOf(items.iter().map(|s| (*s).to_string()).collect())
    }

    fn explicit<'a>(c: &'a ContractPermissions, attr: &str) -> &'a PermissionDecl {
        match c.lookup(attr) {
            PermissionState::Explicit(d) => d,
            other => panic!("{attr}: expected Explicit, got {other:?}"),
        }
    }

    fn inherited<'a>(c: &'a ContractPermissions, attr: &str) -> &'a PermissionDecl {
        match c.lookup(attr) {
            PermissionState::Inherited(d) => d,
            other => panic!("{attr}: expected Inherited, got {other:?}"),
        }
    }

    #[test]
    fn literal_and_array_permissions_with_anchors() {
        let src = "class Base < BaseContract\n  attribute :a, permission: :p\n  attribute :b,\n            permission: %i[x y]\n  attribute :c, permission: [:z]\nend\n";
        let all = scan("literal", &[("c.rb", src)]);
        let c = class(&all, "Base");
        let a = explicit(c, "a");
        assert_eq!(a.value, any_of(&["p"]));
        assert_eq!((a.source.file.as_str(), a.source.line), ("c.rb", 2));
        // An array is a set of alternatives, in written order.
        assert_eq!(explicit(c, "b").value, any_of(&["x", "y"]));
        assert_eq!(explicit(c, "b").source.line, 3);
        assert_eq!(explicit(c, "c").value, any_of(&["z"]));
    }

    #[test]
    fn comment_line_inside_the_statement_does_not_hide_the_permission() {
        // The shape of work_packages/create_contract.rb:40-44.
        let src = "class C < BaseContract\n  attribute :status_id,\n            # Overriding permission from the parent,\n            # because creation needs no status permission.\n            permission: :add_work_packages\nend\n";
        let all = scan("comment", &[("c.rb", src)]);
        assert_eq!(
            explicit(class(&all, "C"), "status_id").value,
            any_of(&["add_work_packages"])
        );
    }

    #[test]
    fn subclass_overrides_and_inherits_per_key() {
        let base = "class Base < BaseContract\n  attribute :status_id, permission: %i[edit change]\n  attribute :parent_id, permission: :manage\n  default_attribute_permission :edit\nend\n";
        let child = "class Child < Base\n  attribute :status_id, permission: :add\n  default_attribute_permission :add\nend\n";
        let all = scan("override", &[("b.rb", base), ("c.rb", child)]);
        let c = class(&all, "Child");
        // Override: replaced whole, never unioned (reverse_merge!).
        let s = explicit(c, "status_id");
        assert_eq!(s.value, any_of(&["add"]));
        assert_eq!(s.source.file, "c.rb");
        // Inherited: untouched key keeps the parent's entry and anchor.
        let p = inherited(c, "parent_id");
        assert_eq!(p.value, any_of(&["manage"]));
        assert_eq!(
            (p.source.file.as_str(), p.applied_in.as_str()),
            ("b.rb", "Base")
        );
        // The default is overridden the same way and applies to the rest.
        match c.lookup("subject") {
            PermissionState::Default(d) => assert_eq!(d.value, any_of(&["add"])),
            other => panic!("{other:?}"),
        }
        // The parent is unaffected by the child.
        assert_eq!(
            explicit(class(&all, "Base"), "status_id").value,
            any_of(&["edit", "change"])
        );
    }

    #[test]
    fn explicit_nil_and_false_keep_the_inherited_permission() {
        // base_contract.rb:97: nothing is recorded for nil / false.
        let base = "class Base < BaseContract\n  attribute :a, permission: :keep\n  attribute :b, permission: :keep\n  attribute :c, permission: :keep\nend\n";
        let child = "class Child < Base\n  attribute :a, permission: nil\n  attribute :b, permission: false\n  attribute :c\nend\n";
        let all = scan("nil", &[("b.rb", base), ("c.rb", child)]);
        let c = class(&all, "Child");
        for attr in ["a", "b", "c"] {
            let d = inherited(c, attr);
            assert_eq!(d.value, any_of(&["keep"]), "{attr}");
            assert_eq!(d.applied_in, "Base");
        }
        // An empty array is NOT a no-op: it replaces and is never satisfiable.
        let child = "class Child < Base\n  attribute :a, permission: []\nend\n";
        let all = scan("empty", &[("b.rb", base), ("c.rb", child)]);
        let d = explicit(class(&all, "Child"), "a");
        assert_eq!(d.value, any_of(&[]));
    }

    #[test]
    fn lookup_order_is_canonical_then_id_then_default() {
        let src = "class C < BaseContract\n  attribute :project_id, permission: :via_id\n  attribute :project, permission: :via_canonical\n  attribute :owner_id, permission: :owner_id_only\n  default_attribute_permission :fallback\nend\n";
        let all = scan("order", &[("c.rb", src)]);
        let c = class(&all, "C");
        // `project_id` canonicalises to `project`, which is tried first.
        assert_eq!(explicit(c, "project_id").value, any_of(&["via_canonical"]));
        assert_eq!(explicit(c, "project").value, any_of(&["via_canonical"]));
        // Only the `_id` key is declared: found as the second candidate.
        assert_eq!(explicit(c, "owner").value, any_of(&["owner_id_only"]));
        assert!(matches!(c.lookup("other"), PermissionState::Default(_)));
        // A parent's canonical key shadows a child's `_id` key.
        let parent = "class P < BaseContract\n  attribute :status, permission: :parent\nend\n";
        let child = "class K < P\n  attribute :status_id, permission: :child\nend\n";
        let all = scan("shadow", &[("p.rb", parent), ("k.rb", child)]);
        assert_eq!(
            inherited(class(&all, "K"), "status_id").value,
            any_of(&["parent"])
        );
    }

    #[test]
    fn attribute_permission_and_aliases_are_harvested() {
        let src = "class C < BaseContract\n  attribute_permission :project_id, :move\n  attribute_alias :project_phase_definition_id, :project_phase_id\nend\nclass D < C\n  attribute_alias :project_phase_definition_id, :other\nend\n";
        let all = scan("alias", &[("c.rb", src)]);
        let c = class(&all, "C");
        assert_eq!(
            explicit(c, "project_id").form,
            DeclForm::AttributePermission
        );
        let alias = &c.aliases["project_phase_definition_id"];
        assert_eq!(alias.outside, "project_phase_id");
        assert_eq!(alias.source.line, 3);
        // A subclass alias for the same db key replaces the parent's.
        let d = class(&all, "D");
        assert_eq!(d.aliases["project_phase_definition_id"].outside, "other");
    }

    #[test]
    fn non_literal_values_are_unlowered_with_source_text() {
        let src = "class C < BaseContract\n  attribute :a, permission: PERM\n  attribute :b, permission: perms_for(:b)\n  attribute :c, permission: [*LIST, :x]\n  attribute :d, permission: \"str\"\n  attribute :e, permission: -> { :x }\n  default_attribute_permission DEFAULT\nend\n";
        let all = scan("unlowered", &[("c.rb", src)]);
        let c = class(&all, "C");
        for (attr, text) in [
            ("a", "PERM"),
            ("b", "perms_for(:b)"),
            ("d", "\"str\""),
            ("e", "-> { :x }"),
        ] {
            match &c.permissions[attr].value {
                PermissionValue::Unlowered { source_text, .. } => assert_eq!(source_text, text),
                other @ PermissionValue::AnyOf(_) => panic!("{attr}: {other:?}"),
            }
            assert!(
                matches!(c.lookup(attr), PermissionState::Unresolved { .. }),
                "{attr}"
            );
        }
        assert!(matches!(
            c.permissions["c"].value,
            PermissionValue::Unlowered { .. }
        ));
        // A non-literal default is unresolved for every attribute that falls to it.
        match c.lookup("other") {
            PermissionState::Unresolved { reason } => {
                assert!(reason.contains("DEFAULT"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn splat_after_permission_is_unlowered_and_before_it_is_not() {
        let src = "class C < BaseContract\n  attribute :a, permission: :p, **opts\n  attribute :b, **opts, permission: :p\n  attribute :c, **opts\nend\n";
        let all = scan("splat", &[("c.rb", src)]);
        let c = class(&all, "C");
        assert!(matches!(
            c.permissions["a"].value,
            PermissionValue::Unlowered { .. }
        ));
        assert_eq!(c.permissions["b"].value, any_of(&["p"]));
        // `**opts` with no `permission:` could carry one: a gap, not absence.
        assert!(c.gaps.iter().any(|g| g.reason.contains("`**` splat")));
    }

    #[test]
    fn mixin_included_block_contributes_in_the_including_class() {
        let concern = "module Guarded\n  extend ActiveSupport::Concern\n  included do\n    attribute :secret, permission: :see_secret\n    default_attribute_permission :from_mixin\n  end\nend\n";
        let class_src = "class C < BaseContract\n  include Guarded\nend\nclass D < C\n  attribute :secret, permission: :own\nend\n";
        let all = scan("mixin", &[("m.rb", concern), ("c.rb", class_src)]);
        let c = class(&all, "C");
        let d = explicit(c, "secret");
        assert_eq!(d.value, any_of(&["see_secret"]));
        // It was installed in the including class, written in the module.
        assert_eq!(
            (d.applied_in.as_str(), d.source.scope.as_str()),
            ("C", "Guarded")
        );
        assert_eq!(d.source.file, "m.rb");
        assert!(matches!(c.lookup("x"), PermissionState::Default(_)));
        // A subclass declaration still overrides the mixin-provided one.
        assert_eq!(explicit(class(&all, "D"), "secret").value, any_of(&["own"]));
    }

    #[test]
    fn mixin_order_follows_ruby() {
        let mods = "module One\n  included do\n    attribute :a, permission: :one\n  end\nend\nmodule Two\n  included do\n    attribute :a, permission: :two\n  end\nend\n";
        // `include One, Two`: Two runs first, One last, so One wins.
        let one_call = "class C < BaseContract\n  include One, Two\nend\n";
        let all = scan("mix_call", &[("m.rb", mods), ("c.rb", one_call)]);
        assert_eq!(explicit(class(&all, "C"), "a").value, any_of(&["one"]));
        // Two calls: the later one runs later and wins.
        let two_calls = "class C < BaseContract\n  include One\n  include Two\nend\n";
        let all = scan("mix_calls", &[("m.rb", mods), ("c.rb", two_calls)]);
        assert_eq!(explicit(class(&all, "C"), "a").value, any_of(&["two"]));
        // A declaration written after the include overwrites it, before loses.
        let after =
            "class C < BaseContract\n  include One\n  attribute :a, permission: :mine\nend\n";
        let all = scan("mix_after", &[("m.rb", mods), ("c.rb", after)]);
        assert_eq!(explicit(class(&all, "C"), "a").value, any_of(&["mine"]));
        let before =
            "class C < BaseContract\n  attribute :a, permission: :mine\n  include One\nend\n";
        let all = scan("mix_before", &[("m.rb", mods), ("c.rb", before)]);
        assert_eq!(explicit(class(&all, "C"), "a").value, any_of(&["one"]));
    }

    #[test]
    fn mixins_that_cannot_be_read_are_gaps_not_silence() {
        let hook = "module Hooked\n  def self.included(base)\n    base.attribute :x, permission: :p\n  end\nend\n";
        let class_src = "class C < BaseContract\n  include Hooked\n  include Elsewhere::Unscanned\n  include Object.const_get(:M)\n  attribute :known, permission: :k\nend\n";
        let all = scan("gaps", &[("h.rb", hook), ("c.rb", class_src)]);
        let c = class(&all, "C");
        let reasons: Vec<&str> = c.gaps.iter().map(|g| g.reason.as_str()).collect();
        assert!(
            reasons.iter().any(|r| r.contains("`self.included`")),
            "{reasons:?}"
        );
        assert!(
            reasons.iter().any(|r| r.contains("`Elsewhere::Unscanned`")),
            "{reasons:?}"
        );
        assert!(
            reasons.iter().any(|r| r.contains("const_get")),
            "{reasons:?}"
        );
        // The gaps ran before `known`, so `known` is still decided ...
        assert_eq!(explicit(c, "known").value, any_of(&["k"]));
        // ... but anything else could have been declared by them.
        assert!(matches!(
            c.lookup("other"),
            PermissionState::Unresolved { .. }
        ));

        // A DSL call written directly in a module body is not understood: a gap.
        let direct = "module Direct\n  attribute :x, permission: :p\nend\nclass M < BaseContract\n  include Direct\nend\n";
        let all = scan("gaps_direct", &[("d.rb", direct)]);
        assert!(matches!(
            class(&all, "M").lookup("x"),
            PermissionState::Unresolved { .. }
        ));

        // A gap written AFTER a declaration could replace it: unresolved.
        let late = "class L < BaseContract\n  attribute :known, permission: :k\n  include Elsewhere::Unscanned\nend\n";
        let all = scan("gaps_late", &[("l.rb", late)]);
        assert!(matches!(
            class(&all, "L").lookup("known"),
            PermissionState::Unresolved { .. }
        ));
    }

    #[test]
    fn an_id_entry_is_unresolved_when_any_gap_could_supply_the_canonical_entry() {
        // The gap runs BEFORE the `_id` declaration, yet it could declare
        // `:status`, which is looked up before `:status_id`.
        let early = "class C < BaseContract\n  include Elsewhere::Unscanned\n  attribute :status_id, permission: :public\nend\n";
        let all = scan("id_gap", &[("c.rb", early)]);
        let c = class(&all, "C");
        assert!(matches!(
            c.lookup("status_id"),
            PermissionState::Unresolved { .. }
        ));
        assert!(matches!(
            c.lookup("status"),
            PermissionState::Unresolved { .. }
        ));
        // An early gap does not shadow a canonical entry declared after it.
        let canonical = "class C < BaseContract\n  include Elsewhere::Unscanned\n  attribute :status, permission: :public\nend\n";
        let all = scan("canon_gap", &[("c.rb", canonical)]);
        assert_eq!(
            explicit(class(&all, "C"), "status_id").value,
            any_of(&["public"])
        );
        // Without any gap the `_id` entry decides.
        let clean = "class C < BaseContract\n  attribute :status_id, permission: :public\nend\n";
        let all = scan("id_clean", &[("c.rb", clean)]);
        assert_eq!(
            explicit(class(&all, "C"), "status").value,
            any_of(&["public"])
        );
    }

    #[test]
    fn an_include_inside_an_included_block_is_expanded_in_the_includer() {
        let mods = "module Inner\n  extend ActiveSupport::Concern\n  included do\n    attribute :secret, permission: :admin\n  end\nend\nmodule Outer\n  extend ActiveSupport::Concern\n  included do\n    include Inner\n  end\nend\n";
        let class_src = "class C < BaseContract\n  include Outer\nend\n";
        let all = scan("nested_include", &[("m.rb", mods), ("c.rb", class_src)]);
        let c = class(&all, "C");
        let d = explicit(c, "secret");
        assert_eq!(d.value, any_of(&["admin"]));
        assert_eq!(d.source.scope, "Inner");
        assert!(c.gaps.is_empty(), "{:?}", c.gaps);
        // An unresolvable include there is a gap, never proven absence.
        let unknown = "module Outer\n  extend ActiveSupport::Concern\n  included do\n    include Elsewhere::Unscanned\n  end\nend\nclass C < BaseContract\n  include Outer\nend\n";
        let all = scan("nested_unknown", &[("m.rb", unknown)]);
        assert!(matches!(
            class(&all, "C").lookup("secret"),
            PermissionState::Unresolved { .. }
        ));
    }

    #[test]
    fn an_explicit_self_receiver_declares_like_a_bare_call() {
        let src = "class C < BaseContract\n  self.attribute :secret, permission: :admin\n  self.default_attribute_permission :edit\nend\n";
        let all = scan("self_recv", &[("c.rb", src)]);
        let c = class(&all, "C");
        assert_eq!(explicit(c, "secret").value, any_of(&["admin"]));
        assert!(matches!(c.lookup("other"), PermissionState::Default(_)));
        // `self.include` is not expanded, so it is a gap.
        let inc = "class C < BaseContract\n  self.include Elsewhere::Unscanned\nend\n";
        let all = scan("self_include", &[("c.rb", inc)]);
        assert!(matches!(
            class(&all, "C").lookup("x"),
            PermissionState::Unresolved { .. }
        ));
        // A receiver other than `self` is not the contract's DSL.
        let other = "class C < BaseContract\n  Other.attribute :secret, permission: :admin\nend\n";
        let all = scan("other_recv", &[("c.rb", other)]);
        assert!(matches!(
            class(&all, "C").lookup("secret"),
            PermissionState::ProvenAbsent
        ));
    }

    #[test]
    fn dsl_calls_in_conditionals_or_loops_are_gaps() {
        let src = "class C < BaseContract\n  if flag?\n    attribute :a, permission: :p\n  end\n  %i[x y].each do |n|\n    attribute n, permission: :q\n  end\n  attribute dyn, permission: :r\nend\n";
        let all = scan("nested", &[("c.rb", src)]);
        let c = class(&all, "C");
        assert!(c.permissions.is_empty());
        assert_eq!(c.gaps.len(), 3, "{:?}", c.gaps);
        assert!(matches!(c.lookup("a"), PermissionState::Unresolved { .. }));
    }

    #[test]
    fn baseline_contract_itself_is_not_collected() {
        // base_contract.rb:203: the chain stops before BaseContract.
        let root = "class BaseContract\n  attribute :a, permission: :ignored\nend\n";
        let child = "class C < BaseContract\nend\nclass D < C\nend\n";
        let all = scan("root", &[("b.rb", root), ("c.rb", child)]);
        assert!(all.iter().all(|c| c.class != "BaseContract"));
        assert!(matches!(
            class(&all, "D").lookup("a"),
            PermissionState::ProvenAbsent
        ));
    }

    /// The focused regression: override, inheritance, nil that keeps the
    /// inherited value, an array (OR) and a dynamic value, in one chain.
    const REGRESSION_BASE: &str = "class Base < BaseContract\n  attribute :overridden, permission: :parent_perm\n  attribute :inherited, permission: :parent_perm\n  attribute :nil_keeps, permission: :parent_perm\n  attribute :either, permission: %i[first second]\n  attribute :dynamic, permission: PermissionTable.for(:dynamic)\nend\n";
    const REGRESSION_CHILD: &str = "class Child < Base\n  attribute :overridden, permission: :child_perm\n  attribute :nil_keeps, permission: nil\nend\n";

    #[test]
    fn regression_fixture_keeps_the_states_apart() {
        let all = scan(
            "regression",
            &[("base.rb", REGRESSION_BASE), ("child.rb", REGRESSION_CHILD)],
        );
        let c = class(&all, "Child");
        // Override (a): the child's own entry replaced the parent's.
        let o = explicit(c, "overridden");
        assert_eq!(
            (&o.value, o.applied_in.as_str()),
            (&any_of(&["child_perm"]), "Child")
        );
        // Inherited (b).
        assert_eq!(inherited(c, "inherited").value, any_of(&["parent_perm"]));
        // nil kept the inherited value: still the parent's, not erased, not Explicit.
        assert_eq!(inherited(c, "nil_keeps").value, any_of(&["parent_perm"]));
        // Array is alternatives (OR).
        assert_eq!(inherited(c, "either").value, any_of(&["first", "second"]));
        // Dynamic value (d): unresolved with the source text, not absence.
        match c.lookup("dynamic") {
            PermissionState::Unresolved { reason } => {
                assert!(reason.contains("PermissionTable.for(:dynamic)"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
        // Nothing declared for this one and nothing unreadable: proven absent (c).
        assert_eq!(c.lookup("undeclared"), PermissionState::ProvenAbsent);
    }

    #[test]
    fn proven_absent_differs_from_unresolved() {
        let clean =
            "class Clean < BaseContract\n  attribute :a\n  attribute :b, writable: false\nend\n";
        let dirty = "class Dirty < BaseContract\n  include Elsewhere::Mixin\n  attribute :a\nend\n";
        let all = scan("absent", &[("a.rb", clean), ("b.rb", dirty)]);
        let absent = class(&all, "Clean").lookup("a");
        let unresolved = class(&all, "Dirty").lookup("a");
        assert_eq!(absent, PermissionState::ProvenAbsent);
        assert!(matches!(unresolved, PermissionState::Unresolved { .. }));
        assert_ne!(absent, unresolved);
        // An unscanned superclass is just as unresolved.
        let orphan = "class Orphan < Elsewhere::Parent\n  attribute :a\nend\n";
        let all = scan("orphan", &[("o.rb", orphan)]);
        assert!(matches!(
            class(&all, "Orphan").lookup("a"),
            PermissionState::Unresolved { .. }
        ));
    }

    /// Permission map of `WorkPackages::CreateContract` as produced by
    /// nexgen's line scanner (`crates/op-contracts/src/work_packages/
    /// generated_create_admission.rs` at nexgen `b70b3e0`), canonical
    /// attribute name -> alternatives.
    const NEXGEN_CREATE: &[(&str, &[&str])] = &[
        ("assigned_to", &["add_work_packages"]),
        ("author", &["add_work_packages"]),
        ("budget", &["add_work_packages"]),
        ("categories", &["add_work_packages"]),
        ("category", &["add_work_packages"]),
        ("created_at", &["add_work_packages"]),
        ("derived_done_ratio", &["add_work_packages"]),
        ("derived_estimated_hours", &["add_work_packages"]),
        ("derived_remaining_hours", &["add_work_packages"]),
        ("description", &["add_work_packages"]),
        ("done_ratio", &["add_work_packages"]),
        ("due_date", &["add_work_packages"]),
        ("duration", &["add_work_packages"]),
        ("estimated_hours", &["add_work_packages"]),
        ("ignore_non_working_days", &["add_work_packages"]),
        ("labels", &["add_work_packages"]),
        ("observed_in_versions", &["assign_versions"]),
        ("parent", &["manage_subtasks"]),
        ("priority", &["add_work_packages"]),
        ("project", &["add_work_packages"]),
        ("project_phase_definition", &["view_project_phases"]),
        ("remaining_hours", &["add_work_packages"]),
        ("responsible", &["add_work_packages"]),
        ("schedule_manually", &["add_work_packages"]),
        ("start_date", &["add_work_packages"]),
        ("status", &["add_work_packages"]),
        ("subject", &["add_work_packages"]),
        ("target_versions", &["assign_versions"]),
        ("type", &["add_work_packages"]),
    ];

    /// Real-corpus gate: `OPENPROJECT_SRC` is a checkout of `OpenProject`.
    #[test]
    #[allow(clippy::print_stderr)]
    fn openproject_create_contract_matches_the_line_scanner() {
        let Ok(root) = std::env::var("OPENPROJECT_SRC") else {
            eprintln!("OPENPROJECT_SRC unset; skipping real-corpus gate");
            return;
        };
        // `app`, not `app/contracts`: contracts include modules from
        // `app/services` (a module that is not scanned is a gap).
        let all = extract_contract_permissions(&Path::new(&root).join("app"));
        let c = class(&all, "WorkPackages::CreateContract");
        eprintln!("gaps: {:#?}", c.gaps);
        let mut different = Vec::new();
        for (attr, expected) in NEXGEN_CREATE {
            let got: Option<Vec<String>> = match c.lookup(attr) {
                PermissionState::Explicit(d) | PermissionState::Inherited(d) => match &d.value {
                    PermissionValue::AnyOf(v) => Some(v.clone()),
                    PermissionValue::Unlowered { .. } => None,
                },
                PermissionState::Default(d) => match &d.value {
                    PermissionValue::AnyOf(v) => Some(v.clone()),
                    PermissionValue::Unlowered { .. } => None,
                },
                PermissionState::ProvenAbsent | PermissionState::Unresolved { .. } => None,
            };
            let want: Vec<String> = expected.iter().map(|s| (*s).to_string()).collect();
            if got.as_ref() != Some(&want) {
                different.push(format!("{attr}: ruff={got:?} scanner={want:?}"));
            }
        }
        // `status_id`: the create contract overrides the base's two-permission
        // list (create_contract.rb:40-44), after a comment line.
        assert_eq!(
            explicit(c, "status_id").value,
            any_of(&["add_work_packages"])
        );
        // No explicit permission may exist that the scanner did not report.
        let canon = |a: &str| a.strip_suffix("_id").unwrap_or(a).to_string();
        for key in c.permissions.keys() {
            assert!(
                NEXGEN_CREATE.iter().any(|(n, _)| *n == canon(key)),
                "ruff found an explicit permission for `{key}` the scanner has no row for"
            );
        }
        assert!(different.is_empty(), "{different:#?}");
    }
}
